mod autostart;
mod commands;
mod focus;
mod jar;
mod model;
mod port;
mod proc;
mod store;

use model::{AppState, JdkConfig};
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};
use uuid::Uuid;

fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// 单实例：再次双击时把已经运行的那个实例唤到前台（含从托盘隐藏状态恢复），
/// 而不是再开一个窗口去抢同一份 store.json 和同一批 Java 进程。
fn bring_to_front(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        // 先让窗口「存在且可见」：关闭按钮只是把它藏进托盘
        let _ = win.show();
        let _ = win.unminimize();
        // 再抢焦点：系统可能拒绝后台进程的前台请求，用 AttachThreadInput 兜底
        #[cfg(windows)]
        if let Ok(hwnd) = win.hwnd() {
            focus::force_foreground(hwnd.0 as isize);
        }
        let _ = win.set_focus();
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "打开主界面", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出 JarBox", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &sep, &quit])?;

    let mut builder = TrayIconBuilder::with_id("jarbox-tray")
        .tooltip("JarBox · JAR 服务管理器")
        .menu(&menu)
        .show_menu_on_left_click(false);

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "quit" => {
                // 先记录退出时的运行状态，下次启动按它恢复
                {
                    let st = app.state::<AppState>();
                    commands::save_session(&st);
                }
                // 遵循设置：关闭「退出时停止服务」就保留后台 Java 进程，
                // 下次启动 JarBox 时会重新认领它们
                if commands::should_stop_on_exit(app) {
                    commands::shutdown_all(app);
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        // 单实例插件必须最先注册：它是第二个实例启动时唯一的拦截点。
        // 第二个进程会在这里通知已运行的实例然后自行退出，绝不会走到 setup，
        // 所以不会重复加载配置、也不会重复认领 Java 进程。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            bring_to_front(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("jarbox"));
            let _ = std::fs::create_dir_all(&data_dir);
            let _ = std::fs::create_dir_all(data_dir.join("logs"));
            let store_path = data_dir.join("store.json");
            let mut cfg = store::load(&store_path);

            // 首次启动时自动探测 JAVA_HOME
            if cfg.jdks.is_empty() {
                if let Ok(home) = std::env::var("JAVA_HOME") {
                    if proc::java_binary(&home).is_ok() {
                        let id = Uuid::new_v4().to_string();
                        cfg.default_jdk = id.clone();
                        cfg.jdks.push(JdkConfig {
                            id,
                            name: "自动检测 JDK".to_string(),
                            path: home,
                        });
                        let _ = store::save(&store_path, &cfg);
                    }
                }
            }

            let autostart = cfg.autostart;
            app.manage(AppState {
                store: Mutex::new(cfg),
                running: Mutex::new(HashMap::new()),
                last_exit: Mutex::new(HashMap::new()),
                data_dir,
                sys: Mutex::new(sysinfo::System::new().ok()),
                ports: Mutex::new(HashMap::new()),
            });

            // 开机自启：与配置保持同步（release 构建生效）
            let _ = commands::apply_autostart(app.handle(), autostart);

            // 认领上一次会话遗留的 Java 进程（管理器退出时未停止的服务）
            let dead = {
                let st = app.state::<AppState>();
                commands::adopt_orphans(&st)
            };

            // 恢复上次退出时仍在运行的服务：接管不到（进程已随重启消失）的重新拉起来
            {
                let st = app.state::<AppState>();
                let resume = st
                    .store
                    .lock()
                    .map(|g| g.resume_on_start)
                    .unwrap_or(true);
                if resume {
                    let ids = commands::restore_candidates(&st, &dead);
                    if !ids.is_empty() {
                        commands::auto_start_services(app.handle().clone(), ids);
                    }
                }
            }

            // 接管来的进程没有等待线程，起一个巡检线程同步它们的状态
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    commands::reap_adopted(&handle);
                });
            }

            build_tray(app)?;

            // 小屏幕/高缩放下自动收缩窗口，避免超出屏幕
            if let Some(win) = app.get_webview_window("main") {
                if let Ok(Some(mon)) = win.current_monitor() {
                    let msize = mon.size();
                    let wsize = win.outer_size().unwrap_or(*msize);
                    let max_w = (msize.width as f64 * 0.92) as u32;
                    let max_h = (msize.height as f64 * 0.9) as u32;
                    if wsize.width > max_w || wsize.height > max_h {
                        let _ = win.set_size(tauri::PhysicalSize::new(
                            wsize.width.min(max_w),
                            wsize.height.min(max_h),
                        ));
                        let _ = win.center();
                    }
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // 关闭按钮 → 最小化到托盘，保持后台服务运行
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::list_services,
            commands::service_stats,
            commands::save_settings,
            commands::update_settings,
            commands::detect_java,
            commands::scan_jdks,
            commands::import_jars,
            commands::list_work_jars,
            commands::upsert_service,
            commands::delete_service,
            commands::start_service,
            commands::stop_service,
            commands::list_jar_entries,
            commands::read_jar_entry,
            commands::list_logs,
            commands::read_log,
            commands::open_path,
            commands::set_autostart,
            commands::quit_app,
            commands::open_data_dir,
        ])
        .run(tauri::generate_context!())
        .expect("JarBox 启动失败");
}
