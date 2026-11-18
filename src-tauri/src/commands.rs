use crate::model::*;
use crate::proc;
use crate::store;
use crate::jar;
use chrono::Local;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// 内部辅助
// ---------------------------------------------------------------------------

fn now_str() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn persist(state: &AppState) -> Result<(), String> {
    let store = state.store.lock().map_err(|e| e.to_string())?.clone();
    store::save(&state.store_path(), &store)
}

/// 运行时记录文件：记录「哪个服务 → 哪个 pid」，管理器重启后可据此认领进程
fn runtime_path(state: &AppState) -> PathBuf {
    state.data_dir.join("running.json")
}

/// 把当前运行中的服务写入 running.json（进程退出/停止/退出应用时同步清理）
pub(crate) fn persist_running(state: &AppState) {
    let records: Vec<RuntimeRecord> = {
        let running = match state.running.lock() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        let store = match state.store.lock() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        running
            .iter()
            .filter_map(|(id, r)| {
                let svc = store.services.iter().find(|s| &s.id == id)?;
                Some(RuntimeRecord {
                    service_id: id.clone(),
                    pid: r.pid,
                    started_at: r.started_at,
                    log_path: r.log_path.to_string_lossy().to_string(),
                    cmdline: r.cmdline.clone(),
                    jar_path: svc.jar_path.clone(),
                })
            })
            .collect()
    };
    let path = runtime_path(state);
    if records.is_empty() {
        let _ = fs::remove_file(&path);
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(&records) {
        let _ = fs::write(&path, text);
    }
}

/// 退出快照：只记「哪些服务是运行中的」，不关心进程号，用于下次启动恢复。
/// 除了正常退出时调用，服务启动/停止/自行退出时也会同步刷新，
/// 保证管理进程被强杀（任务管理器、断电、蓝屏）时快照仍是最后一次真实状态。
fn session_path(state: &AppState) -> PathBuf {
    state.data_dir.join("last_session.json")
}

/// 在退出（或停机）前调用：把当时仍在跑的服务存下来
pub(crate) fn save_session(state: &AppState) {
    let entries: Vec<SessionEntry> = {
        let running = match state.running.lock() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        let store = match state.store.lock() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        running
            .iter()
            .filter_map(|(id, r)| {
                let svc = store.services.iter().find(|s| &s.id == id)?;
                Some(SessionEntry {
                    id: id.clone(),
                    name: svc.name.clone(),
                    jar_path: svc.jar_path.clone(),
                    started_at: r.started_at,
                })
            })
            .collect()
    };
    let snap = SessionSnapshot {
        saved_at: Local::now().timestamp_millis(),
        services: entries,
    };
    let path = session_path(state);
    if snap.services.is_empty() {
        let _ = fs::remove_file(&path);
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(&snap) {
        let _ = fs::write(&path, text);
    }
}

fn load_session(state: &AppState) -> SessionSnapshot {
    fs::read_to_string(session_path(state))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// 合并「快照里的服务」与「进程已消失的记录」，去掉重复和仍然在跑的
fn merge_restore_ids(snap: &[String], dead: &[String], alive: &HashSet<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in snap.iter().chain(dead.iter()) {
        if !alive.contains(id) && !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
}

/// 算出「上次退出时在跑、但现在已经不在了」的服务，用于自动拉起。
/// `dead` 是 running.json 里 pid 已经消失的记录（断电/重启/被强杀的情况）。
pub(crate) fn restore_candidates(state: &AppState, dead: &[String]) -> Vec<String> {
    let snap = load_session(state);
    let store = match state.store.lock() {
        Ok(g) => g.clone(),
        Err(_) => return Vec::new(),
    };
    let alive: HashSet<String> = state
        .running
        .lock()
        .map(|g| g.keys().cloned().collect())
        .unwrap_or_default();

    let snap_ids: Vec<String> = snap.services.iter().map(|e| e.id.clone()).collect();
    let mut out = merge_restore_ids(&snap_ids, dead, &alive);
    // 服务被删了、或者 JAR 已经不在磁盘上 → 不启动（启动必然失败）
    out.retain(|id| {
        store
            .services
            .iter()
            .find(|s| &s.id == id)
            .map(|s| Path::new(&s.jar_path).is_file())
            .unwrap_or(false)
    });
    out
}

fn append_auto_log(state: &AppState, line: &str) {
    use std::io::Write;
    let path = state.data_dir.join("auto_start.log");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {}", now_str(), line);
    }
}

/// 启动恢复：延迟一会儿（等主窗口建好），逐个拉起上次运行中的服务
pub fn auto_start_services(app: AppHandle, ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(1200));
        let state = app.state::<AppState>();
        append_auto_log(
            &state,
            &format!("开始恢复上次运行中的 {} 个服务: {:?}", ids.len(), ids),
        );
        let mut started: Vec<String> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for id in ids {
            let name = state
                .store
                .lock()
                .ok()
                .and_then(|g| g.services.iter().find(|s| s.id == id).map(|s| s.name.clone()))
                .unwrap_or_else(|| id.clone());
            match start_impl(&app, &state, &id) {
                Ok(_) => {
                    append_auto_log(&state, &format!("已启动 {} ({})", name, id));
                    started.push(name);
                }
                Err(e) => {
                    append_auto_log(&state, &format!("启动失败 {} ({})：{}", name, id, e));
                    failed.push(format!("{}：{}", name, e));
                }
            }
            // 逐个启动，避免同时拉起多个 JVM 抢占端口/CPU
            thread::sleep(std::time::Duration::from_millis(350));
        }
        // 恢复完把快照刷新成当前真实运行集合
        save_session(&state);
        let _ = app.emit(
            "auto-start",
            serde_json::json!({ "started": started, "failed": failed }),
        );
    });
}

/// 校验某个 pid 是否仍是「当初那个 java -jar <jar_path>」，
/// 避免 pid 被系统回收后误认成一个无关进程。
fn pid_owns_jar(sys: &mut sysinfo::System, pid: u32, jar_path: &str) -> bool {
    let target = sysinfo::Pid::from_u32(pid);
    // 注意：refresh_processes 不刷新命令行，必须用 specifics 指定
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[target]),
        true,
        sysinfo::ProcessRefreshKind::everything(),
    );
    let proc_ = match sys.process(target) {
        Some(p) => p,
        None => return false,
    };
    let name = proc_.name().to_string_lossy().to_lowercase();
    if !name.starts_with("java") {
        return false;
    }
    let cmd = proc_
        .cmd()
        .iter()
        .map(|c| c.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let norm = |s: &str| s.replace('/', "\\").to_lowercase();
    norm(&cmd).contains(&norm(jar_path))
}

/// 启动时认领上一次会话遗留（管理器退出/崩溃时仍在运行）的 Java 进程。
/// 返回「记录里有、但进程已经不在」的服务 id —— 这些就是需要自动重启的对象。
pub(crate) fn adopt_orphans(state: &AppState) -> Vec<String> {
    let path = runtime_path(state);
    let records: Vec<RuntimeRecord> = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut sys = match sysinfo::System::new() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let services = match state.store.lock() {
        Ok(g) => g.services.clone(),
        Err(_) => return Vec::new(),
    };
    let mut adopted: HashMap<String, Running> = HashMap::new();
    let mut dead: Vec<String> = Vec::new();
    for r in records {
        // 服务已被删除 → 不认领（进程交给用户自行处理）
        let svc = match services.iter().find(|s| s.id == r.service_id) {
            Some(s) => s,
            None => continue,
        };
        // JAR 已换过路径的记录视为失效
        if Path::new(&svc.jar_path).to_string_lossy().to_lowercase()
            != Path::new(&r.jar_path).to_string_lossy().to_lowercase()
        {
            continue;
        }
        if !pid_owns_jar(&mut sys, r.pid, &r.jar_path) {
            // 进程没了（重启电脑、崩溃、被任务管理器结束）→ 下次可自动拉起
            if !dead.contains(&r.service_id) {
                dead.push(r.service_id.clone());
            }
            continue;
        }
        adopted.insert(
            r.service_id.clone(),
            Running {
                pid: r.pid,
                started_at: r.started_at,
                stop_requested: false,
                log_path: PathBuf::from(r.log_path),
                cmdline: r.cmdline,
                adopted: true,
            },
        );
    }
    if let Ok(mut running) = state.running.lock() {
        for (id, r) in adopted {
            running.insert(id, r);
        }
    }

    // 兜底：记录文件缺失/丢失时，直接扫描系统里正在跑的 java -jar，
    // 按 JAR 路径匹配到本管理器的服务就一并接管
    adopt_by_scan(state, &mut sys, &services);

    // 把失效记录清掉，只留下真正接管到的进程。
    // 这里刻意不同步退出快照：启动阶段 running 可能还是空的，
    // 一同步就会把上次退出时的快照抹掉，后面的恢复流程就没有依据了。
    persist_running(state);
    dead
}

/// 通过扫描进程表认领「命令行里 -jar 指向某个服务 JAR」的 java 进程
fn adopt_by_scan(state: &AppState, sys: &mut sysinfo::System, services: &[Service]) {
    // 需要命令行才能判断 -jar 指向哪个包
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::everything(),
    );
    let already: HashMap<String, u32> = state
        .running
        .lock()
        .map(|g| g.iter().map(|(k, v)| (k.clone(), v.pid)).collect())
        .unwrap_or_default();
    let norm = |s: &str| s.replace('/', "\\").to_lowercase();

    let mut found: HashMap<String, Running> = HashMap::new();
    for (pid, p) in sys.processes() {
        let name = p.name().to_string_lossy().to_lowercase();
        if !name.starts_with("java") {
            continue;
        }
        let args: Vec<String> = p
            .cmd()
            .iter()
            .map(|c| c.to_string_lossy().to_string())
            .collect();
        // 找到 "-jar" 后面那一个参数；可能是相对路径，需用进程工作目录还原
        let jar_raw = match args.iter().position(|a| a == "-jar").and_then(|i| args.get(i + 1)) {
            Some(a) => a.clone(),
            None => continue,
        };
        let mut jar_pb = PathBuf::from(&jar_raw);
        if !jar_pb.is_absolute() {
            if let Some(cwd) = p.cwd() {
                jar_pb = cwd.join(&jar_pb);
            }
        }
        let jar_arg = norm(&jar_pb.to_string_lossy());
        let svc = match services.iter().find(|s| norm(&s.jar_path) == jar_arg) {
            Some(s) => s,
            None => continue,
        };
        let pid_u32 = pid.as_u32();
        if already.get(&svc.id) == Some(&pid_u32) || found.contains_key(&svc.id) {
            continue;
        }
        let started_at = (p.start_time() as i64) * 1000;
        let log_path = newest_log(log_dir_for(svc)).unwrap_or_default();
        found.insert(
            svc.id.clone(),
            Running {
                pid: pid_u32,
                started_at: if started_at > 0 {
                    started_at
                } else {
                    Local::now().timestamp_millis()
                },
                stop_requested: false,
                log_path,
                cmdline: args.join(" "),
                adopted: true,
            },
        );
    }
    if found.is_empty() {
        return;
    }
    if let Ok(mut running) = state.running.lock() {
        for (id, r) in found {
            running.insert(id, r);
        }
    }
}

/// 取日志目录里最新的一个文件，用于接管进程后指向「当前日志」
fn newest_log(dir: PathBuf) -> Option<PathBuf> {
    let mut best: Option<(i64, PathBuf)> = None;
    for e in fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let modified = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
            best = Some((modified, p));
        }
    }
    best.map(|(_, p)| p)
}

fn snapshot(state: &AppState) -> (Store, HashMap<String, Running>, HashMap<String, LastExit>) {
    let store = state.store.lock().map(|g| g.clone()).unwrap_or_default();
    let running = state.running.lock().map(|g| g.clone()).unwrap_or_default();
    let exits = state.last_exit.lock().map(|g| g.clone()).unwrap_or_default();
    (store, running, exits)
}

/// 端口：运行中看当前日志，已停止时看最近一次日志。
/// 找到后按「本次启动 + 日志文件」缓存，避免每次轮询重复读日志
/// （服务日志写了很久以后，启动那行会被挤出尾部，就不该再重新扫）
fn detect_port(state: &AppState, s: &Service, run: Option<&Running>) -> Option<u16> {
    let (started, log) = match run {
        Some(r) => (r.started_at, r.log_path.clone()),
        None => (0, newest_log(log_dir_for(s))?),
    };
    if let Ok(g) = state.ports.lock() {
        if let Some((c_started, c_log, port)) = g.get(&s.id) {
            if *c_started == started && *c_log == log {
                return Some(*port);
            }
        }
    }
    let port = crate::port::detect(&log)?;
    if let Ok(mut g) = state.ports.lock() {
        g.insert(s.id.clone(), (started, log, port));
    }
    Some(port)
}

fn view_for(
    state: &AppState,
    s: &Service,
    running: &HashMap<String, Running>,
    exits: &HashMap<String, LastExit>,
    store: &Store,
) -> ServiceView {
    let run = running.get(&s.id);
    let jdk = store.jdks.iter().find(|j| j.id == s.jdk_id);
    let jdk_path = jdk.map(|j| j.path.clone()).unwrap_or_default();
    let jdk_ok = if jdk_path.is_empty() {
        false
    } else {
        proc::java_binary(&jdk_path).is_ok()
    };
    let exit = exits.get(&s.id);
    let detected_port = detect_port(state, s, run);
    ServiceView {
        service: s.clone(),
        status: if run.is_some() { "running" } else { "stopped" }.to_string(),
        pid: run.map(|r| r.pid),
        started_at: run.map(|r| r.started_at),
        exit_code: exit.and_then(|e| e.code),
        exited_at: exit.map(|e| e.at),
        stopped_by_user: exit.map(|e| e.stopped).unwrap_or(false),
        adopted: run.map(|r| r.adopted).unwrap_or(false),
        detected_port,
        log_dir: log_dir_for(s).to_string_lossy().to_string(),
        current_log: run
            .map(|r| r.log_path.to_string_lossy().to_string())
            .unwrap_or_default(),
        jar_exists: Path::new(&s.jar_path).is_file(),
        jdk_path,
        jdk_ok,
        cmdline: run.map(|r| r.cmdline.clone()).unwrap_or_default(),
    }
}

fn views(state: &AppState) -> Vec<ServiceView> {
    let (store, running, exits) = snapshot(state);
    store
        .services
        .iter()
        .map(|s| view_for(state, s, &running, &exits, &store))
        .collect()
}

fn view_one(state: &AppState, id: &str) -> Option<ServiceView> {
    views(state).into_iter().find(|v| v.service.id == id)
}

/// 日志与 JAR 同目录：<jar 所在目录>/logs
fn log_dir_for(service: &Service) -> PathBuf {
    let jar = PathBuf::from(&service.jar_path);
    jar.parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("logs")
}

fn resolve_java(store: &Store, service: &Service) -> Result<PathBuf, String> {
    let jdk = store
        .jdks
        .iter()
        .find(|j| j.id == service.jdk_id)
        .ok_or_else(|| "该服务未绑定 JDK，请先编辑服务选择 JDK".to_string())?;
    proc::java_binary(&jdk.path).map_err(|e| format!("JDK「{}」不可用: {}", jdk.name, e))
}

pub(crate) fn apply_autostart(app: &AppHandle, enabled: bool) -> Result<(), String> {
    // 开发模式下不写入注册表，避免把 target/debug 的可执行文件注册为开机启动
    let force = std::env::var("JARBOX_AUTOSTART").ok().as_deref() == Some("1");
    if cfg!(debug_assertions) && !force {
        return Ok(());
    }
    let result = crate::autostart::apply(app, enabled);
    // 记录结果，便于排查安全软件拦截等问题
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = fs::write(
            dir.join("autostart.log"),
            format!(
                "{} enabled={} status=[{}] result={:?}\n",
                now_str(),
                enabled,
                crate::autostart::status(app).summary(),
                result
            ),
        );
    }
    result
}

fn import_impl(state: &AppState, paths: Vec<String>) -> Result<Vec<String>, String> {
    let (work_dir, import_mode) = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        (store.work_dir.clone(), store.import_mode.clone())
    };
    if work_dir.trim().is_empty() {
        return Err("请先在设置中配置工作目录".to_string());
    }
    let moving = import_mode == "move";
    let base = PathBuf::from(&work_dir);
    fs::create_dir_all(&base).map_err(|e| format!("创建目录失败: {}", e))?;

    let mut out = Vec::new();
    for raw in paths {
        let src = PathBuf::from(raw.trim());
        if !src.is_file() {
            continue;
        }
        let file_name = src
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown.jar".to_string());
        let stem = src
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "jar".to_string());

        // 一个 JAR 一个独立目录：<工作目录>/<jar 名>/<jar 文件>
        let mut dir = base.join(&stem);
        let mut dest = dir.join(&file_name);
        let mut n = 2;
        let mut same_file = false;
        loop {
            if !dest.exists() {
                break;
            }
            let same = fs::canonicalize(&src)
                .ok()
                .zip(fs::canonicalize(&dest).ok())
                .map(|(a, b)| a == b)
                .unwrap_or(false);
            if same {
                same_file = true;
                break;
            }
            dir = base.join(format!("{} ({})", stem, n));
            dest = dir.join(&file_name);
            n += 1;
        }
        if !dest.exists() {
            fs::create_dir_all(&dir).map_err(|e| format!("创建目录 {} 失败: {}", dir.display(), e))?;
            fs::copy(&src, &dest).map_err(|e| format!("复制 {} 失败: {}", file_name, e))?;
        }
        // 移动模式：复制成功后删掉源文件。
        // 源和目标本来就是同一个文件时（从工作目录里再导入一次）绝不能删。
        if moving && !same_file && src != dest {
            fs::remove_file(&src).map_err(|e| {
                format!(
                    "已复制到工作目录，但删除原文件失败：{}（{}）",
                    src.display(),
                    e
                )
            })?;
        }
        out.push(dest.to_string_lossy().to_string());
    }
    if out.is_empty() {
        return Err("没有找到可导入的 JAR 文件".to_string());
    }
    Ok(out)
}

fn start_impl(app: &AppHandle, state: &AppState, id: &str) -> Result<ServiceView, String> {
    {
        let running = state.running.lock().map_err(|e| e.to_string())?;
        if running.contains_key(id) {
            return Err("服务已在运行中".to_string());
        }
    }
    let (store, _running, _exits) = snapshot(state);
    let service = store
        .services
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .ok_or_else(|| "服务不存在".to_string())?;

    let jar_path = PathBuf::from(&service.jar_path);
    if !jar_path.is_file() {
        return Err(format!("JAR 文件不存在: {}", service.jar_path));
    }
    let java = resolve_java(&store, &service)?;
    let cwd = if service.work_dir.trim().is_empty() {
        jar_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        PathBuf::from(service.work_dir.trim())
    };
    if !cwd.exists() {
        fs::create_dir_all(&cwd).map_err(|e| format!("工作目录不可用: {}", e))?;
    }

    let jvm_args = proc::split_args(&service.jvm_args);
    let prog_args = proc::split_args(&service.program_args);
    let ts = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let log_path = log_dir_for(&service).join(format!("{}.log", ts));
    let cmdline = format!(
        "{} {} -jar {} {}",
        java.to_string_lossy(),
        service.jvm_args.trim(),
        service.jar_path,
        service.program_args.trim()
    );
    let banner = format!(
        "===== JarBox · {} =====",
        service.name
    ) + &format!(
        "\n启动时间: {}\nJDK: {}\n命令: {}\n工作目录: {}\n{}\n",
        now_str(),
        java.to_string_lossy(),
        cmdline.trim(),
        cwd.to_string_lossy(),
        "-".repeat(64)
    );

    let launched = proc::launch(
        &java,
        &jvm_args,
        &jar_path,
        &prog_args,
        &cwd,
        &log_path,
        &banner,
    )?;
    let pid = launched.child.id();
    let started_at = Local::now().timestamp_millis();

    {
        let mut running = state.running.lock().map_err(|e| e.to_string())?;
        running.insert(
            id.to_string(),
            Running {
                pid,
                started_at,
                stop_requested: false,
                log_path: log_path.clone(),
                cmdline: cmdline.clone(),
                adopted: false,
            },
        );
    }
    persist_running(state);
    // 运行集合一变就刷新退出快照。这样即使管理器被任务管理器强杀、
    // 来不及走正常退出流程，下次启动仍能按「最后一次真实状态」恢复。
    save_session(state);

    let mut child = launched.child;
    let sink = launched.log;
    let app2 = app.clone();
    let sid = id.to_string();
    let name = service.name.clone();
    thread::spawn(move || {
        let status = child.wait();
        let code = status.ok().and_then(|s| s.code());
        let state2 = app2.state::<AppState>();
        let stop_requested = {
            let mut running = match state2.running.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let flag = running.get(&sid).map(|r| r.stop_requested).unwrap_or(false);
            running.remove(&sid);
            flag
        };
        persist_running(&state2);
        save_session(&state2);
        if let Ok(mut exits) = state2.last_exit.lock() {
            exits.insert(
                sid.clone(),
                LastExit {
                    code,
                    at: Local::now().timestamp_millis(),
                    stopped: stop_requested,
                },
            );
        }
        if let Ok(mut w) = sink.lock() {
            use std::io::Write;
            let _ = writeln!(
                w,
                "\n===== JarBox: {} 进程已退出（退出码 {}） {} =====\n",
                name,
                code.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
                now_str()
            );
            let _ = w.flush();
        }
        let _ = app2.emit(
            "service-exit",
            serde_json::json!({ "id": sid, "name": name, "code": code }),
        );
    });

    view_one(state, id).ok_or_else(|| "服务不存在".to_string())
}

fn stop_impl(state: &AppState, id: &str) -> Result<(), String> {
    let pid = {
        let mut running = state.running.lock().map_err(|e| e.to_string())?;
        match running.get_mut(id) {
            Some(r) => {
                r.stop_requested = true;
                r.pid
            }
            None => return Err("服务当前未在运行".to_string()),
        }
    };
    let r = proc::kill_tree(pid);
    // 接管的进程没有等待线程，这里直接收尾
    let adopted = state
        .running
        .lock()
        .ok()
        .and_then(|g| g.get(id).map(|x| x.adopted))
        .unwrap_or(false);
    if adopted {
        thread::sleep(std::time::Duration::from_millis(300));
        if let Ok(mut running) = state.running.lock() {
            running.remove(id);
        }
        if let Ok(mut exits) = state.last_exit.lock() {
            exits.insert(
                id.to_string(),
                LastExit {
                    code: None,
                    at: Local::now().timestamp_millis(),
                    stopped: true,
                },
            );
        }
    }
    persist_running(state);
    save_session(state);
    r
}

fn list_logs_impl(state: &AppState, service_id: &str) -> Result<Vec<LogFileInfo>, String> {
    let service = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store.services.iter().find(|s| s.id == service_id).cloned()
    };
    let dir = match service {
        Some(s) => log_dir_for(&s),
        None => return Ok(Vec::new()),
    };
    let _ = fs::create_dir_all(&dir);
    let current = state
        .running
        .lock()
        .ok()
        .and_then(|g| g.get(service_id).map(|r| r.log_path.clone()));
    let mut out: Vec<LogFileInfo> = Vec::new();
    if dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let is_current = current
                    .as_ref()
                    .map(|c| c == &path)
                    .unwrap_or(false);
                out.push(LogFileInfo {
                    name: path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    path: path.to_string_lossy().to_string(),
                    size: meta.len(),
                    modified,
                    is_current,
                });
            }
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    Ok(out)
}

fn line_level(lower: &str) -> Option<&'static str> {
    if lower.contains("error") || lower.contains("fatal") || lower.contains("exception") {
        Some("error")
    } else if lower.contains("warn") {
        Some("warn")
    } else if lower.contains("info") {
        Some("info")
    } else if lower.contains("debug") {
        Some("debug")
    } else if lower.contains("trace") {
        Some("trace")
    } else {
        None
    }
}

fn read_log_impl(
    state: &AppState,
    service_id: &str,
    file: &str,
    keyword: &str,
    levels: &[String],
    limit: usize,
) -> Result<LogResult, String> {
    let service = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store.services.iter().find(|s| s.id == service_id).cloned()
    };
    let dir = match service {
        Some(s) => log_dir_for(&s),
        None => return Err("服务不存在".to_string()),
    };
    let name = Path::new(file)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| "非法的日志文件名".to_string())?;
    let path = dir.join(&name);
    if !path.is_file() {
        return Err(format!("日志文件不存在: {}", name));
    }
    let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    const MAX_READ: u64 = 32 * 1024 * 1024;
    let mut f = fs::File::open(&path).map_err(|e| format!("打开日志失败: {}", e))?;
    if size > MAX_READ {
        use std::io::Seek;
        let _ = f.seek(std::io::SeekFrom::End(-(MAX_READ as i64)));
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)
        .map_err(|e| format!("读取日志失败: {}", e))?;
    let all_lines = crate::proc::decode_lines(&buf);

    let kw = keyword.trim().to_lowercase();
    let want: Vec<String> = levels.iter().map(|l| l.to_lowercase()).collect();
    let filtering = !want.is_empty();
    let limit = if limit == 0 { 2000 } else { limit };

    let mut counts = LogCounts::default();
    let mut selected: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut matched = 0usize;
    let mut prev_kept = false;

    for raw in all_lines {
        let line = if raw.contains('\u{1b}') {
            crate::proc::strip_ansi(&raw)
        } else {
            raw
        };
        if line.is_empty() && total == 0 {
            continue;
        }
        total += 1;
        if total > 500_000 {
            break;
        }
        let lower = line.to_lowercase();
        let lvl = line_level(&lower);
        match lvl {
            Some("error") => counts.error += 1,
            Some("warn") => counts.warn += 1,
            Some("info") => counts.info += 1,
            Some("debug") => counts.debug += 1,
            Some("trace") => counts.trace += 1,
            _ => {}
        }

        let level_ok = if !filtering {
            true
        } else if let Some(l) = lvl {
            want.iter().any(|w| w == l)
        } else {
            // 续行跟随上一行的筛选结果，保留异常堆栈等上下文
            prev_kept
        };
        let keyword_ok = kw.is_empty() || lower.contains(&kw);
        let keep = level_ok && keyword_ok;
        if keep {
            matched += 1;
            selected.push(line);
            if selected.len() > limit {
                selected.remove(0);
            }
        }
        prev_kept = keep;
    }
    counts.total = total as i64;

    Ok(LogResult {
        file: name,
        path: path.to_string_lossy().to_string(),
        size,
        truncated: matched > limit,
        matched,
        total,
        lines: selected,
        counts,
    })
}

fn scan_jdks_impl() -> Vec<JdkConfig> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(home) = std::env::var("JAVA_HOME") {
        if !home.trim().is_empty() {
            roots.push(PathBuf::from(home));
        }
    }
    let mut bases: Vec<PathBuf> = Vec::new();
    for env_key in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Ok(v) = std::env::var(env_key) {
            bases.push(PathBuf::from(v));
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        bases.push(PathBuf::from(profile));
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    for b in &bases {
        dirs.push(b.join("Java"));
        dirs.push(b.join("Eclipse Adoptium"));
        dirs.push(b.join("AdoptOpenJDK"));
        dirs.push(b.join("Microsoft"));
        dirs.push(b.join("Zulu"));
        dirs.push(b.join("Amazon Corretto"));
        dirs.push(b.join("BellSoft"));
        dirs.push(b.join(".jdks"));
        dirs.push(b.join("Programs").join("Eclipse Adoptium"));
    }
    for d in dirs {
        if d.is_dir() {
            if let Ok(entries) = fs::read_dir(&d) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        roots.push(p);
                    }
                }
            }
        }
    }
    // 某些安装目录（如 Microsoft、BellSoft）还有一层 jdk-17.0.x 子目录，多探一层
    let mut extra = Vec::new();
    for r in &roots {
        if r.join("bin").join("java.exe").is_file() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(r) {
            for e in entries.flatten() {
                let p = e.path();
                if p.join("bin").join("java.exe").is_file() {
                    extra.push(p);
                }
            }
        }
    }
    roots.extend(extra);

    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for r in roots {
        if !r.join("bin").is_dir() {
            continue;
        }
        if proc::java_binary(&r.to_string_lossy()).is_err() {
            continue;
        }
        let canonical = fs::canonicalize(&r)
            .unwrap_or_else(|_| r.clone())
            .to_string_lossy()
            .to_string();
        if seen.iter().any(|s| s.eq_ignore_ascii_case(&canonical)) {
            continue;
        }
        seen.push(canonical);
        let name = r
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "JDK".to_string());
        out.push(JdkConfig {
            id: Uuid::new_v4().to_string(),
            name,
            path: r.to_string_lossy().to_string(),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Tauri 命令
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn bootstrap(app: AppHandle, state: State<'_, AppState>) -> Result<Bootstrap, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?.clone();
    let autostart_enabled = crate::autostart::status(&app).enabled();
    Ok(Bootstrap {
        store,
        services: views(&state),
        data_dir: state.data_dir.to_string_lossy().to_string(),
        logs_root: state.logs_root().to_string_lossy().to_string(),
        version: app.package_info().version.to_string(),
        autostart_enabled,
        autostart_skipped: cfg!(debug_assertions),
    })
}

#[tauri::command]
pub async fn list_services(state: State<'_, AppState>) -> Result<Vec<ServiceView>, String> {
    Ok(views(&state))
}

/// 采集运行中服务的 CPU / 内存占用
#[tauri::command]
pub async fn service_stats(state: State<'_, AppState>) -> Result<Vec<ServiceStat>, String> {
    let running = state.running.lock().map_err(|e| e.to_string())?.clone();
    if running.is_empty() {
        return Ok(Vec::new());
    }
    let pids: Vec<sysinfo::Pid> = running
        .values()
        .map(|r| sysinfo::Pid::from_u32(r.pid))
        .collect();
    let mut guard = state.sys.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        *guard = sysinfo::System::new().ok();
    }
    let sys = match guard.as_mut() {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(pids.as_slice()), true);
    let now = Local::now().timestamp_millis();
    let mut out = Vec::new();
    for (id, r) in running.iter() {
        if let Some(p) = sys.process(sysinfo::Pid::from_u32(r.pid)) {
            out.push(ServiceStat {
                id: id.clone(),
                pid: r.pid,
                cpu: (p.cpu_usage() * 10.0).round() / 10.0,
                memory: p.memory(),
                virtual_memory: p.virtual_memory(),
                uptime_ms: now - r.started_at,
            });
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    work_dir: String,
    jdks: Vec<JdkConfig>,
    autostart: bool,
    stop_on_exit: bool,
    default_jdk: String,
) -> Result<SaveResult, String> {
    let wt = work_dir.trim().to_string();
    if !wt.is_empty() {
        fs::create_dir_all(PathBuf::from(&wt)).map_err(|e| format!("工作目录不可用: {}", e))?;
    }
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.work_dir = wt;
        store.jdks = jdks;
        store.autostart = autostart;
        store.stop_on_exit = stop_on_exit;
        store.default_jdk = default_jdk;
    }
    persist(&state)?;
    // 开机自启失败不应该让整次保存失败：配置已经落盘，只是自启没生效，
    // 把原因作为 warning 带回前端提示
    let autostart_warning = apply_autostart(&app, autostart).err();
    let store = state.store.lock().map_err(|e| e.to_string())?.clone();
    Ok(SaveResult {
        store,
        autostart_warning,
        autostart_enabled: crate::autostart::status(&app).enabled(),
    })
}

/// 设置页「改一项就存一项」的补丁：只有显式给出的字段才会被写入。
///
/// 这样做的两个好处：一是并发改动不会互相覆盖（只提交自己那一项），
/// 二是只有真的动了「开机自启」才会去碰注册表——改个 JDK 名字不会触发安全软件弹窗。
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub work_dir: Option<String>,
    pub jdks: Option<Vec<JdkConfig>>,
    pub autostart: Option<bool>,
    pub stop_on_exit: Option<bool>,
    pub resume_on_start: Option<bool>,
    pub import_mode: Option<String>,
    pub keep_jar_versions: Option<u32>,
    pub default_jdk: Option<String>,
}

/// 把补丁应用到 Store 上；返回 Some(autostart) 表示这次改动需要同步系统启动项。
fn apply_settings_patch(store: &mut Store, patch: SettingsPatch) -> Option<bool> {
    if let Some(wd) = patch.work_dir {
        store.work_dir = wd.trim().to_string();
    }
    if let Some(jdks) = patch.jdks {
        store.jdks = jdks;
        // 默认 JDK 被删掉时自动落到第一个，别留一个指向不存在项的 id
        if !store.jdks.iter().any(|j| j.id == store.default_jdk) {
            store.default_jdk = store.jdks.first().map(|j| j.id.clone()).unwrap_or_default();
        }
    }
    if let Some(id) = patch.default_jdk {
        if id.is_empty() || store.jdks.iter().any(|j| j.id == id) {
            store.default_jdk = id;
        }
    }
    if let Some(v) = patch.stop_on_exit {
        store.stop_on_exit = v;
    }
    if let Some(v) = patch.resume_on_start {
        store.resume_on_start = v;
    }
    if let Some(mode) = patch.import_mode {
        // 只接受这两个值，避免前端传错把导入行为变成意料之外的状态
        store.import_mode = if mode == "move" { "move" } else { "copy" }.to_string();
    }
    if let Some(n) = patch.keep_jar_versions {
        store.keep_jar_versions = n.clamp(1, 200);
    }
    if let Some(v) = patch.autostart {
        store.autostart = v;
        return Some(v);
    }
    None
}

#[tauri::command]
pub async fn update_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    patch: SettingsPatch,
) -> Result<SaveResult, String> {
    // 工作目录要能建出来才允许保存，避免存进去一个不可用的路径
    if let Some(wd) = patch.work_dir.clone() {
        let wt = wd.trim().to_string();
        if !wt.is_empty() {
            fs::create_dir_all(PathBuf::from(&wt))
                .map_err(|e| format!("工作目录不可用: {}", e))?;
        }
    }

    let autostart_change = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        apply_settings_patch(&mut store, patch)
    };

    persist(&state)?;
    // 自启失败不影响设置落盘，只把原因作为 warning 带回
    let autostart_warning = match autostart_change {
        Some(enabled) => apply_autostart(&app, enabled).err(),
        None => None,
    };
    let store = state.store.lock().map_err(|e| e.to_string())?.clone();
    Ok(SaveResult {
        store,
        autostart_warning,
        autostart_enabled: crate::autostart::status(&app).enabled(),
    })
}

#[tauri::command]
pub async fn detect_java(path: String) -> Result<JavaInfo, String> {
    Ok(proc::detect_java(&path))
}

#[tauri::command]
pub async fn scan_jdks() -> Result<Vec<JdkConfig>, String> {
    Ok(scan_jdks_impl())
}

#[tauri::command]
pub async fn import_jars(
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> Result<Vec<String>, String> {
    import_impl(&state, paths)
}

#[tauri::command]
pub async fn upsert_service(
    state: State<'_, AppState>,
    service: Service,
) -> Result<Service, String> {
    let mut s = service;
    if s.name.trim().is_empty() {
        return Err("请填写服务名称".to_string());
    }
    if s.jar_path.trim().is_empty() {
        return Err("请选择 JAR 文件".to_string());
    }
    if !Path::new(&s.jar_path).is_file() {
        return Err(format!("JAR 文件不存在: {}", s.jar_path));
    }
    if s.jdk_id.trim().is_empty() {
        return Err("请选择运行该服务使用的 JDK".to_string());
    }
    s.name = s.name.trim().to_string();
    s.work_dir = s.work_dir.trim().to_string();
    s.jvm_args = s.jvm_args.trim().to_string();
    s.program_args = s.program_args.trim().to_string();
    s.note = s.note.trim().to_string();
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        if s.id.trim().is_empty() {
            s.id = Uuid::new_v4().to_string();
            s.created_at = now_str();
            store.services.push(s.clone());
        } else {
            let idx = store
                .services
                .iter()
                .position(|x| x.id == s.id)
                .ok_or_else(|| "服务不存在".to_string())?;
            store.services[idx] = s.clone();
        }
    }
    persist(&state)?;
    Ok(s)
}

#[tauri::command]
pub async fn delete_service(
    state: State<'_, AppState>,
    id: String,
    delete_logs: bool,
) -> Result<(), String> {
    if state
        .running
        .lock()
        .map(|g| g.contains_key(&id))
        .unwrap_or(false)
    {
        stop_impl(&state, &id)?;
        thread::sleep(std::time::Duration::from_millis(400));
    }
    let (removed_dir, log_dir_shared) = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        let target = store.services.iter().find(|s| s.id == id).cloned();
        store.services.retain(|s| s.id != id);
        let dir = target.as_ref().map(log_dir_for);
        let shared = match (&dir, target.as_ref()) {
            (Some(_), Some(t)) => store.services.iter().any(|s| {
                let a = PathBuf::from(&s.jar_path);
                let b = PathBuf::from(&t.jar_path);
                a.parent() == b.parent()
            }),
            _ => false,
        };
        (dir, shared)
    };
    if let Ok(mut exits) = state.last_exit.lock() {
        exits.remove(&id);
    }
    persist(&state)?;
    if delete_logs && !log_dir_shared {
        if let Some(dir) = removed_dir {
            let _ = fs::remove_dir_all(dir);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn start_service(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<ServiceView, String> {
    start_impl(&app, &state, &id)
}

#[tauri::command]
pub async fn stop_service(state: State<'_, AppState>, id: String) -> Result<(), String> {
    stop_impl(&state, &id)
}

// ---------------------------------------------------------------------------
// JAR 版本管理：重启、替换、历史版本回滚
// ---------------------------------------------------------------------------

/// 备份目录：与 JAR 同级的 bak
fn jar_bak_dir(jar: &Path) -> PathBuf {
    jar.parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("bak")
}

/// 等进程真正消失。刚停止的服务还握着 JAR 的文件句柄，这时动文件会失败。
fn wait_process_exit(pid: u32, timeout: Duration) -> bool {
    let start = Instant::now();
    let target = sysinfo::Pid::from_u32(pid);
    let mut sys = match sysinfo::System::new() {
        Ok(s) => s,
        Err(_) => return false,
    };
    loop {
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
        if sys.process(target).is_none() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        thread::sleep(Duration::from_millis(120));
    }
}

/// 备份文件名：原名 + 时间戳；同一秒内重复备份时追加序号，避免覆盖
fn backup_name_for(jar: &Path, dir: &Path) -> String {
    let stem = jar
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "jar".to_string());
    let ext = jar
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "jar".to_string());
    let ts = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut name = format!("{}.{}.{}", stem, ts, ext);
    let mut n = 2;
    while dir.join(&name).exists() {
        name = format!("{}-{}.{}.{}", stem, ts, n, ext);
        n += 1;
    }
    name
}

/// 是不是同一个文件（大小写、相对路径都不影响判断；文件不存在时返回 false）
fn same_file(a: &Path, b: &Path) -> bool {
    fs::canonicalize(a)
        .ok()
        .zip(fs::canonicalize(b).ok())
        .map(|(x, y)| x == y)
        .unwrap_or(false)
}

/// 从备份名还原原始包名：`app.20261118-105500.jar` → `app.jar`。
/// 备份名是「原名.时间戳.扩展名」，所以把时间戳那段去掉就是它当年的文件名。
fn original_name_from_backup(name: &str) -> String {
    let dot = match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => i,
        _ => return name.to_string(),
    };
    let (stem, ext) = (&name[..dot], &name[dot + 1..]);
    // 时间戳固定 8 位日期 + '-' + 6 位时间，共 15 个字符
    if stem.len() <= 16 {
        return name.to_string();
    }
    let split = stem.len() - 15;
    let (head, tag) = stem.split_at(split);
    if !head.ends_with('.') || !is_timestamp_tag(tag) {
        return name.to_string();
    }
    format!("{}.{}", head.trim_end_matches('.'), ext)
}

fn is_timestamp_tag(tag: &str) -> bool {
    let b = tag.as_bytes();
    b.len() == 15
        && b[8] == b'-'
        && b[..8].iter().all(|c| c.is_ascii_digit())
        && b[9..].iter().all(|c| c.is_ascii_digit())
}

/// 移动文件并重试：进程刚退出时句柄可能还没释放（Windows 特有的坑）
fn rename_with_retry(from: &Path, to: &Path, timeout: Duration) -> Result<(), String> {
    let start = Instant::now();
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if start.elapsed() >= timeout {
                    return Err(format!("移动 {} 失败：{}", from.display(), e));
                }
                thread::sleep(Duration::from_millis(150));
            }
        }
    }
}

/// 内容指纹（FNV-1a 64，流式读，几十 MB 的包也不吃内存）。
/// 用来判断两个 JAR 是不是同一个版本——文件名会带时间戳，但内容不会变。
fn file_hash(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut file = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            return Some(hash);
        }
        for &b in &buf[..n] {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

fn is_jar_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".jar") || lower.ends_with(".war")
}

/// 去重键：内容指纹 + 原始文件名。
/// 内容一样但名字不同的，在用户眼里就是两个版本（比如把同一个包改名成 v3、v4 试），不能合并；
/// 只有「内容 + 名字」都一样的两份备份，才是真正重复的那一条。
fn dedupe_key(name: &str, hash: u64) -> (u64, String) {
    (hash, original_name_from_backup(name).to_lowercase())
}

/// 在 dir 里找和 file 完全重复的备份（内容相同，且原始文件名也相同）
fn find_identical_in(dir: &Path, file: &Path, size: u64, hash: u64) -> Option<PathBuf> {
    let self_key = dedupe_key(
        &file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        hash,
    );
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !is_jar_name(&name) || same_file(&path, file) {
            continue;
        }
        if entry.metadata().map(|m| m.len()).unwrap_or(0) != size {
            continue;
        }
        if file_hash(&path).map(|h| dedupe_key(&name, h)) == Some(self_key.clone()) {
            return Some(path);
        }
    }
    None
}

/// 把 file 收进 bak。bak 里已经有内容相同的包时不再新增一份——来回切换版本时
/// 每次都会备份「切换前的包」，如果不去重，同一个版本会越攒越多。
/// 返回 (备份路径, 是否新建)；新建的那份在失败时需要挪回去，去重的那份不用动。
fn stash_to_backup(file: &Path, bak_dir: &Path) -> Result<(PathBuf, bool), String> {
    let len = fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    if len > 0 {
        if let Some(hash) = file_hash(file) {
            if let Some(existing) = find_identical_in(bak_dir, file, len, hash) {
                // 内容已经在 bak 里了，当前这份直接删掉，不再多占一个版本
                fs::remove_file(file).map_err(|e| format!("清理旧 JAR 失败：{}", e))?;
                return Ok((existing, false));
            }
        }
    }
    let name = backup_name_for(file, bak_dir);
    let path = bak_dir.join(name);
    rename_with_retry(file, &path, Duration::from_secs(6))?;
    Ok((path, true))
}

/// 备份名里的时间戳：`app.20261118-105500.jar` → `20261118-105500`，取不到就给空串
fn backup_stamp(name: &str) -> String {
    let dot = match name.rfind('.') {
        Some(i) => i,
        None => return String::new(),
    };
    let stem = &name[..dot];
    if stem.len() < 16 {
        return String::new();
    }
    let split = stem.len() - 15;
    let tag = &stem[split..];
    if stem.as_bytes()[split - 1] == b'.' && is_timestamp_tag(tag) {
        tag.to_string()
    } else {
        String::new()
    }
}

/// 只保留最近 keep 个备份，顺带合并内容重复的备份，返回被删掉的数量。
/// 「最近」按备份名里的时间戳排——备份沿用原文件的修改时间，光看 mtime 分不出先后
/// （同一个 jar 反复备份时 mtime 全都一样）。
fn prune_backups(dir: &Path, keep: usize) -> usize {
    let mut items: Vec<(PathBuf, String, std::time::SystemTime)> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if !is_jar_name(&name) {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let stamp = backup_stamp(&name);
            items.push((path, stamp, modified));
        }
    }
    // 新的在前（先比备份时间戳，再比 mtime），超出的删掉；
    // 同一个版本（内容 + 原名都一样）备份了多份时只留最新的一个
    items.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
    let mut removed = 0;
    let mut seen: Vec<(u64, String)> = Vec::new();
    for (path, _, _) in items {
        let over_limit = seen.len() >= keep.max(1);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let duplicate = match file_hash(&path) {
            Some(h) => {
                let key = dedupe_key(&name, h);
                if seen.contains(&key) {
                    true
                } else {
                    seen.push(key);
                    false
                }
            }
            None => false,
        };
        if over_limit || duplicate {
            if fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// 只做文件层面的替换：当前包移进 bak → 新包按**自己的文件名**放到 dest → 校验大小 → 清理超量备份。
/// dest 与当前包同名时就是原路径，不同名时由调用方负责把服务的 jar_path 指向新文件。
/// 返回 (备份路径, 是否新建了备份, 清理数量)；bak 里已有同一版本时不再重复备份。
/// 与进程/服务无关，方便单测覆盖「备份没丢、回滚可用」这些最容易出错的地方。
fn swap_files(
    current: &Path,
    src: &Path,
    dest: &Path,
    keep: usize,
) -> Result<(PathBuf, bool, usize), String> {
    if same_file(src, current) {
        return Err("源文件就是服务当前使用的 JAR".to_string());
    }
    let bak_dir = jar_bak_dir(current);
    fs::create_dir_all(&bak_dir).map_err(|e| format!("创建备份目录失败：{}", e))?;

    // 源文件已经在目标位置（用户把新包放进服务目录后再选它）：只挪旧包，不用复制
    let src_in_place = same_file(src, dest);

    // 1) 目标位置已被另一个文件占用（目录里留着的同名旧版本）：也收进 bak，不覆盖
    if !src_in_place && dest != current && dest.is_file() {
        let _ = stash_to_backup(dest, &bak_dir)?;
    }

    // 2) 当前包移进 bak（同一分区下 rename 是原子的，比复制快得多）
    let (backup, created) = if current.is_file() {
        stash_to_backup(current, &bak_dir)?
    } else {
        (PathBuf::new(), false)
    };

    // 3) 新包就位；失败就把旧包放回去，不能让服务没有 JAR
    if !src_in_place {
        let fail = |msg: String| -> String {
            let _ = fs::remove_file(dest);
            if backup.is_file() {
                if created {
                    let _ = fs::rename(&backup, current);
                } else {
                    // 备份没新建（内容是 bak 里已有的那份），复制回去即可
                    let _ = fs::copy(&backup, current);
                }
            }
            msg
        };
        if let Err(e) = fs::copy(src, dest) {
            return Err(fail(format!("写入新 JAR 失败：{}", e)));
        }
        // 校验大小：防止复制中途失败留下半个包
        let src_len = fs::metadata(src).map(|m| m.len()).unwrap_or(0);
        let dst_len = fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        if src_len == 0 || dst_len != src_len {
            return Err(fail("写入的新 JAR 大小与原文件不一致，已回滚".to_string()));
        }
    }

    // 4) 按保留策略清理旧备份（内容相同的备份合并成一份，不动的版本一个都不少）
    let pruned = prune_backups(&bak_dir, keep);
    Ok((backup, created, pruned))
}

/// 替换 / 回滚的公共流程：
/// 停服务 → 换文件 → 按需重启
/// `dest_name` 决定新包在服务目录里叫什么：替换时用新包自己的文件名，
/// 回滚时用备份名还原出来的原始文件名；都为空则沿用源文件名。
fn swap_jar_impl(
    app: &AppHandle,
    state: &AppState,
    id: &str,
    src: &Path,
    dest_name: Option<&str>,
) -> Result<ReplaceResult, String> {
    let service = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store
            .services
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| "服务不存在".to_string())?
    };
    let target = PathBuf::from(&service.jar_path);
    if !src.is_file() {
        return Err("要使用的 JAR 文件不存在".to_string());
    }
    let ext = src
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext != "jar" && ext != "war" {
        return Err("只支持 .jar / .war 文件".to_string());
    }
    if target.is_file() && same_file(src, &target) {
        return Err("选中的就是这个服务当前使用的 JAR，无需替换".to_string());
    }

    // 新包在服务目录里的目标路径：保留自己的文件名，不再改成旧包的名字
    let name = match dest_name.map(|n| n.trim()).filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None => src
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
    };
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
    {
        return Err("目标 JAR 文件名不合法".to_string());
    }
    let dir = target
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败：{}", e))?;
    let dest = dir.join(&name);

    let keep = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store.keep_jar_versions.max(1) as usize
    };

    let pid = {
        let running = state.running.lock().map_err(|e| e.to_string())?;
        running.get(id).map(|r| r.pid)
    };
    let was_running = pid.is_some();
    if let Some(pid) = pid {
        stop_impl(state, id)?;
        if !wait_process_exit(pid, Duration::from_secs(8)) {
            return Err("服务进程没有在 8 秒内退出，JAR 仍被占用，请稍后重试".to_string());
        }
        thread::sleep(Duration::from_millis(200));
    }

    // 1) 换文件：当前包进 bak，新包按自己的名字就位，顺带清理超量备份
    let (backup, created, pruned) = swap_files(&target, src, &dest, keep)?;

    // 2) 文件名变了就把服务指向新文件（内存与磁盘都同步，重启才会用上新包）
    if dest != target {
        let updated = {
            match state.store.lock() {
                Ok(mut store) => match store.services.iter_mut().find(|s| s.id == id) {
                    Some(s) => {
                        s.jar_path = dest.to_string_lossy().to_string();
                        true
                    }
                    None => false,
                },
                Err(_) => false,
            }
        };
        if updated {
            // 文件已经换好了，配置写盘失败不影响本次运行，下次保存会纠正
            let _ = persist(state);
        }
    }

    let (restarted, restart_error) = if was_running {
        match start_impl(app, state, id) {
            Ok(_) => (true, None),
            Err(e) => (false, Some(e)),
        }
    } else {
        (false, None)
    };
    let view = view_one(state, id).ok_or_else(|| "服务不存在".to_string())?;
    // 备份可能刚被清理掉（内容和新包一样时没必要留），那就没什么可提示的
    let kept = backup.is_file();
    Ok(ReplaceResult {
        service: view,
        backup: if kept {
            backup.to_string_lossy().to_string()
        } else {
            String::new()
        },
        // 内容是 bak 里已有的那份，没有新增备份
        deduped: kept && !created,
        restarted,
        restart_error,
        pruned,
    })
}

#[tauri::command]
pub async fn restart_service(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<ServiceView, String> {
    let pid = {
        let running = state.running.lock().map_err(|e| e.to_string())?;
        running.get(&id).map(|r| r.pid)
    };
    if let Some(pid) = pid {
        stop_impl(&state, &id)?;
        wait_process_exit(pid, Duration::from_secs(8));
        // 端口释放与文件句柄回收需要一点时间，太急会撞上「地址已占用」
        thread::sleep(Duration::from_millis(300));
    }
    start_impl(&app, &state, &id)
}

/// 导入一个新 JAR 覆盖当前版本：新包保留自己的文件名，旧包进 bak
#[tauri::command]
pub async fn replace_jar(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    new_path: String,
) -> Result<ReplaceResult, String> {
    // dest_name 传 None：直接用新包自己的文件名，不再改成旧包的名字
    swap_jar_impl(&app, &state, &id, Path::new(new_path.trim()), None)
}

/// 列出某个服务的历史 JAR 备份（bak 目录）
#[tauri::command]
pub async fn list_jar_backups(
    state: State<'_, AppState>,
    id: String,
) -> Result<JarBackups, String> {
    let service = {
        let (store, _running, _exits) = snapshot(&state);
        store
            .services
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| "服务不存在".to_string())?
    };
    let jar = PathBuf::from(&service.jar_path);
    let dir = jar_bak_dir(&jar);
    let keep = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store.keep_jar_versions
    };
    // 打开列表时顺手整理一次：合并内容重复的备份、超量的按设置清掉，
    // 这样界面上看到的条数和磁盘上的文件始终是一回事
    if dir.is_dir() {
        prune_backups(&dir, keep.max(1) as usize);
    }
    let mut items: Vec<JarBackup> = Vec::new();
    if dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let lower = name.to_lowercase();
                if !(lower.ends_with(".jar") || lower.ends_with(".war")) {
                    continue;
                }
                let meta = entry.metadata().ok();
                items.push(JarBackup {
                    name,
                    path: path.to_string_lossy().to_string(),
                    size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    modified: meta
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0),
                });
            }
        }
    }
    // 新的在前面，方便一眼看到最近的几个版本
    items.sort_by(|a, b| b.modified.cmp(&a.modified));
    let current_meta = fs::metadata(&jar).ok();
    Ok(JarBackups {
        current_name: jar
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        current_size: current_meta.as_ref().map(|m| m.len()).unwrap_or(0),
        current_modified: current_meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        current_exists: jar.is_file(),
        dir: dir.to_string_lossy().to_string(),
        keep,
        items,
    })
}

/// 回滚到某个历史备份：当前包同样会先进 bak，来回切换不会丢版本
#[tauri::command]
pub async fn rollback_jar(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<ReplaceResult, String> {
    let trimmed = name.trim();
    // 只接受纯文件名，挡住 ../ 之类的路径穿越
    if trimmed.is_empty() || trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..")
    {
        return Err("备份文件名不合法".to_string());
    }
    let service = {
        let (store, _running, _exits) = snapshot(&state);
        store
            .services
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| "服务不存在".to_string())?
    };
    let dir = jar_bak_dir(Path::new(&service.jar_path));
    let src = dir.join(trimmed);
    if !src.is_file() {
        return Err("备份文件不存在".to_string());
    }
    // 备份名带时间戳，恢复时用它的原始文件名（app.20261118-105500.jar → app.jar）
    let dest_name = original_name_from_backup(trimmed);
    swap_jar_impl(&app, &state, &id, &src, Some(&dest_name))
}


#[tauri::command]
pub async fn list_jar_entries(jar_path: String) -> Result<Vec<JarEntry>, String> {
    jar::list_entries(&jar_path)
}

#[tauri::command]
pub async fn read_jar_entry(
    jar_path: String,
    name: String,
) -> Result<JarEntryContent, String> {
    jar::read_entry(&jar_path, &name, 512 * 1024)
}

#[tauri::command]
pub async fn list_logs(
    state: State<'_, AppState>,
    service_id: String,
) -> Result<Vec<LogFileInfo>, String> {
    list_logs_impl(&state, &service_id)
}

#[tauri::command]
pub async fn read_log(
    state: State<'_, AppState>,
    service_id: String,
    file: String,
    keyword: String,
    levels: Vec<String>,
    limit: usize,
) -> Result<LogResult, String> {
    read_log_impl(&state, &service_id, &file, &keyword, &levels, limit)
}

#[tauri::command]
pub async fn list_work_jars(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let work_dir = state
        .store
        .lock()
        .map_err(|e| e.to_string())?
        .work_dir
        .clone();
    if work_dir.trim().is_empty() {
        return Ok(Vec::new());
    }
    let base = PathBuf::from(work_dir);
    let mut out = Vec::new();

    let mut push_jars = |dir: &Path| {
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_file()
                    && p.extension()
                        .map(|x| x.to_string_lossy().to_lowercase() == "jar")
                        .unwrap_or(false)
                {
                    out.push(p.to_string_lossy().to_string());
                }
            }
        }
    };

    // 工作目录根 + 每个 JAR 的独立子目录 + 旧版 jars 目录
    push_jars(&base);
    if let Ok(entries) = fs::read_dir(&base) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                push_jars(&p);
            }
        }
    }
    out.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
    out.dedup();
    Ok(out)
}

#[tauri::command]
pub async fn open_path(path: String, reveal: bool) -> Result<(), String> {
    let _ = reveal;
    proc::reveal(&path)
}

#[tauri::command]
pub async fn set_autostart(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<bool, String> {
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.autostart = enabled;
    }
    persist(&state)?;
    apply_autostart(&app, enabled)?;
    Ok(crate::autostart::status(&app).enabled())
}

/// 巡检被接管的进程：它们没有子进程等待线程，需要主动发现退出并更新状态
pub fn reap_adopted(app: &AppHandle) {
    let state = app.state::<AppState>();
    let entries: Vec<(String, u32, bool)> = match state.running.lock() {
        Ok(g) => g
            .iter()
            .filter(|(_, r)| r.adopted)
            .map(|(id, r)| (id.clone(), r.pid, r.stop_requested))
            .collect(),
        Err(_) => return,
    };
    if entries.is_empty() {
        return;
    }
    let pids: Vec<sysinfo::Pid> = entries
        .iter()
        .map(|(_, pid, _)| sysinfo::Pid::from_u32(*pid))
        .collect();
    let mut guard = match state.sys.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if guard.is_none() {
        *guard = sysinfo::System::new().ok();
    }
    let sys = match guard.as_mut() {
        Some(s) => s,
        None => return,
    };
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(pids.as_slice()), true);
    let alive: Vec<sysinfo::Pid> = pids
        .iter()
        .copied()
        .filter(|p| sys.process(*p).is_some())
        .collect();
    drop(guard);

    let mut changed: Vec<(String, bool)> = Vec::new();
    {
        let mut running = match state.running.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        for (id, pid, stop_requested) in entries {
            if !alive.contains(&sysinfo::Pid::from_u32(pid)) {
                running.remove(&id);
                changed.push((id, stop_requested));
            }
        }
    }
    if changed.is_empty() {
        return;
    }
    for (id, stop_requested) in &changed {
        if let Ok(mut exits) = state.last_exit.lock() {
            exits.insert(
                id.clone(),
                LastExit {
                    code: None,
                    at: Local::now().timestamp_millis(),
                    stopped: *stop_requested,
                },
            );
        }
        let _ = app.emit(
            "service-exit",
            serde_json::json!({ "id": id, "code": null }),
        );
    }
    persist_running(&state);
    save_session(&state);
}

pub fn shutdown_all(app: &AppHandle) {
    let state = app.state::<AppState>();
    let pids: Vec<u32> = state
        .running
        .lock()
        .map(|g| g.values().map(|r| r.pid).collect())
        .unwrap_or_default();
    for pid in pids {
        let _ = proc::kill_tree(pid);
    }
    if let Ok(mut running) = state.running.lock() {
        running.clear();
    }
    persist_running(&state);
    thread::sleep(std::time::Duration::from_millis(200));
}

#[tauri::command]
pub fn should_stop_on_exit(app: &AppHandle) -> bool {
    app.state::<AppState>()
        .store
        .lock()
        .map(|g| g.stop_on_exit)
        .unwrap_or(true)
}

#[tauri::command]
pub async fn quit_app(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    // 先记下「退出这一刻谁在运行」——即使下面会把进程停掉，
    // 下次启动也能按这份快照把服务恢复起来
    save_session(&state);
    let stop = state.store.lock().map(|g| g.stop_on_exit).unwrap_or(true);
    if stop {
        shutdown_all(&app);
    }
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub async fn open_data_dir(state: State<'_, AppState>) -> Result<(), String> {
    proc::reveal(&state.data_dir.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Store {
        Store {
            work_dir: "D:/old".to_string(),
            jdks: vec![
                JdkConfig {
                    id: "a".to_string(),
                    name: "A".to_string(),
                    path: "D:/a".to_string(),
                },
                JdkConfig {
                    id: "b".to_string(),
                    name: "B".to_string(),
                    path: "D:/b".to_string(),
                },
            ],
            services: Vec::new(),
            autostart: false,
            stop_on_exit: true,
            resume_on_start: true,
            import_mode: "copy".to_string(),
            keep_jar_versions: 10,
            default_jdk: "a".to_string(),
        }
    }

    #[test]
    fn backup_name_never_collides() {
        let dir = std::env::temp_dir().join(format!("jarbox-bak-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let jar = dir.join("demo.jar");
        let first = backup_name_for(&jar, &dir);
        assert!(first.starts_with("demo."), "{}", first);
        assert!(first.ends_with(".jar"), "{}", first);
        fs::write(dir.join(&first), b"x").unwrap();
        // 同一秒内再备份一次，名字必须不同，否则会覆盖掉上一个版本
        let second = backup_name_for(&jar, &dir);
        assert_ne!(first, second);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_the_newest_backups() {
        let dir = std::env::temp_dir().join(format!("jarbox-prune-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        for i in 0..6 {
            fs::write(dir.join(format!("demo.{}.jar", i)), format!("v{}", i)).unwrap();
            // 拉开修改时间，避免同一时间戳导致排序不稳定
            thread::sleep(Duration::from_millis(30));
        }
        let removed = prune_backups(&dir, 2);
        assert_eq!(removed, 4);
        let left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(left.len(), 2);
        // 最新的两个（4 和 5）应当留下
        assert!(left.iter().any(|n| n == "demo.5.jar"), "{:?}", left);
        assert!(left.iter().any(|n| n == "demo.4.jar"), "{:?}", left);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_drops_duplicate_content() {
        let dir = std::env::temp_dir().join(format!("jarbox-prune-dup-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        // 同一个版本被备份了两次（内容完全相同），旧的那份应当清掉
        fs::write(dir.join("app.20260101-000001.jar"), b"same").unwrap();
        thread::sleep(Duration::from_millis(30));
        fs::write(dir.join("app.20260101-000002.jar"), b"same").unwrap();
        thread::sleep(Duration::from_millis(30));
        fs::write(dir.join("app.20260101-000003.jar"), b"other").unwrap();

        let removed = prune_backups(&dir, 10);
        assert_eq!(removed, 1);
        let left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(left.len(), 2, "{:?}", left);
        // 留的是较新的那份
        assert!(left.iter().any(|n| n == "app.20260101-000002.jar"), "{:?}", left);
        assert!(!left.iter().any(|n| n == "app.20260101-000001.jar"), "{:?}", left);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_only_merges_truly_identical_entries() {
        // 用户拿同一份 jar 改个名当新版本试：内容相同但名字不同，是用户眼里的两个版本，必须都留着；
        // 只有「内容 + 原名」都一样的，才是重复的那一条
        let root = std::env::temp_dir().join(format!("jarbox-prune-same-{}", Uuid::new_v4()));
        let bak = root.join("bak");
        fs::create_dir_all(&bak).unwrap();
        let current = root.join("app-v4.jar");
        fs::write(&current, b"SAME-BYTES").unwrap();
        fs::write(bak.join("app-v3.20260101-000001.jar"), b"SAME-BYTES").unwrap();
        fs::write(bak.join("app-v4.20260101-000002.jar"), b"SAME-BYTES").unwrap();
        fs::write(bak.join("app-v3.20260101-000003.jar"), b"SAME-BYTES").unwrap();

        let removed = prune_backups(&bak, 10);
        assert_eq!(removed, 1, "只合并那条真正重复的");
        assert!(
            bak.join("app-v3.20260101-000003.jar").is_file(),
            "同名版本留最新的那条"
        );
        assert!(
            bak.join("app-v4.20260101-000002.jar").is_file(),
            "名字不同的版本不能被合并"
        );
        assert!(current.is_file(), "当前包不能被动");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_files_backs_up_and_rolls_back() {
        let dir = std::env::temp_dir().join(format!("jarbox-swap-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("demo.jar");
        fs::write(&target, b"VERSION-1").unwrap();

        // 第一次替换：同名包，旧包进 bak，新包落到原路径
        let new1 = dir.join("new1.jar");
        fs::write(&new1, b"VERSION-2-longer").unwrap();
        let (backup, created, pruned) = swap_files(&target, &new1, &target, 5).unwrap();
        assert_eq!(pruned, 0);
        assert!(created);
        assert_eq!(fs::read(&target).unwrap(), b"VERSION-2-longer");
        assert_eq!(fs::read(&backup).unwrap(), b"VERSION-1");

        // 用备份回滚：内容换回去，且备份文件本身不能被搬走，否则来回切换会丢版本
        let (backup2, _, _) = swap_files(&target, &backup, &target, 5).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"VERSION-1");
        assert!(backup.is_file(), "回滚后旧备份必须仍然存在");
        assert_eq!(fs::read(&backup2).unwrap(), b"VERSION-2-longer");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn switching_back_and_forth_does_not_duplicate_backups() {
        let root = std::env::temp_dir().join(format!("jarbox-dedupe-{}", Uuid::new_v4()));
        let svc = root.join("svc");
        fs::create_dir_all(&svc).unwrap();
        let v1 = svc.join("app.jar");
        let v2 = svc.join("app-v2.jar");
        fs::write(&v1, b"VERSION-1").unwrap();
        fs::write(&v2, b"VERSION-2").unwrap();
        let bak = svc.join("bak");

        // 把 current 换成 dir 里内容为 content 的那个包
        let find = |content: &[u8]| -> PathBuf {
            for e in fs::read_dir(&bak).unwrap().flatten() {
                let p = e.path();
                if fs::read(&p).unwrap() == content {
                    return p;
                }
            }
            panic!("bak 里找不到对应版本");
        };

        // 第一次：v1 → v2（新包本来就在服务目录里，直接顶掉）
        swap_files(&v1, &v2, &v2, 50).unwrap();
        assert_eq!(fs::read(&v2).unwrap(), b"VERSION-2");

        // 来回切 3 轮：bak 里最多就是这两个版本，不该越攒越多
        for round in 0..3 {
            let src = find(b"VERSION-1");
            swap_files(&v2, &src, &v1, 50).unwrap();
            assert_eq!(fs::read(&v1).unwrap(), b"VERSION-1");
            assert_eq!(
                fs::read_dir(&bak).unwrap().flatten().count(),
                2,
                "第 {} 轮切回 v1 后 bak 条目数",
                round + 1
            );

            let src = find(b"VERSION-2");
            swap_files(&v1, &src, &v2, 50).unwrap();
            assert_eq!(fs::read(&v2).unwrap(), b"VERSION-2");
            assert_eq!(
                fs::read_dir(&bak).unwrap().flatten().count(),
                2,
                "第 {} 轮切回 v2 后 bak 条目数",
                round + 1
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_files_keeps_the_new_file_name() {
        let root = std::env::temp_dir().join(format!("jarbox-keep-{}", Uuid::new_v4()));
        let svc = root.join("svc");
        let incoming = root.join("incoming");
        fs::create_dir_all(&svc).unwrap();
        fs::create_dir_all(&incoming).unwrap();
        let current = svc.join("app.jar");
        fs::write(&current, b"V1").unwrap();
        let src = incoming.join("app-2.0.jar");
        fs::write(&src, b"V2-new-build").unwrap();

        // 新包文件名不同：就按它自己的名字落地，不能被迫改成 app.jar
        let dest = svc.join("app-2.0.jar");
        let (backup, _, _) = swap_files(&current, &src, &dest, 5).unwrap();
        assert!(!current.exists(), "旧名字不该再留在服务目录里");
        assert_eq!(fs::read(&dest).unwrap(), b"V2-new-build");
        assert_eq!(fs::read(&backup).unwrap(), b"V1");
        // 替换是复制语义，用户选的源文件不能被搬走
        assert!(src.is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_files_reuses_a_file_already_in_place() {
        // 用户先把新包放进服务目录，再选它替换：不能自己复制自己，也不能把它当旧包挪走
        let dir = std::env::temp_dir().join(format!("jarbox-inplace-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let current = dir.join("app.jar");
        fs::write(&current, b"V1").unwrap();
        let src = dir.join("app-v2.jar");
        fs::write(&src, b"V2").unwrap();

        let (backup, _, _) = swap_files(&current, &src, &src, 5).unwrap();
        assert_eq!(fs::read(&src).unwrap(), b"V2");
        assert_eq!(fs::read(&backup).unwrap(), b"V1");
        assert!(!current.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollback_restores_the_original_file_name() {
        assert_eq!(
            original_name_from_backup("app.20261118-105500.jar"),
            "app.jar"
        );
        assert_eq!(
            original_name_from_backup("order-service.20261118-105500.war"),
            "order-service.war"
        );
        assert_eq!(
            original_name_from_backup("my.app.20261118-105500.jar"),
            "my.app.jar"
        );
        // 不像备份名的原样返回
        assert_eq!(original_name_from_backup("app-v2.jar"), "app-v2.jar");
        assert_eq!(original_name_from_backup("app.jar"), "app.jar");
    }

    #[test]
    fn swap_files_prunes_old_backups() {
        let dir = std::env::temp_dir().join(format!("jarbox-swap-prune-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("demo.jar");
        fs::write(&target, b"v0").unwrap();
        for i in 1..=4 {
            let src = dir.join(format!("new{}.jar", i));
            fs::write(&src, format!("content-{}", i)).unwrap();
            let (_, _, pruned) = swap_files(&target, &src, &target, 2).unwrap();
            if i >= 3 {
                assert!(pruned >= 1, "第 {} 次替换后应清理旧备份", i);
            }
            let left = fs::read_dir(dir.join("bak"))
                .unwrap()
                .flatten()
                .filter(|e| e.path().is_file())
                .count();
            assert_eq!(left, std::cmp::min(i as usize, 2), "第 {} 次后 bak 数量", i);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_only_touches_the_given_field() {
        let mut s = sample();
        let changed = apply_settings_patch(
            &mut s,
            SettingsPatch {
                stop_on_exit: Some(false),
                ..Default::default()
            },
        );
        assert_eq!(changed, None);
        assert!(!s.stop_on_exit);
        // 其他字段一个都不能动
        assert_eq!(s.work_dir, "D:/old");
        assert_eq!(s.default_jdk, "a");
        assert!(!s.autostart);
        assert_eq!(s.jdks.len(), 2);
    }

    #[test]
    fn autostart_patch_asks_for_sync() {
        let mut s = sample();
        let changed = apply_settings_patch(
            &mut s,
            SettingsPatch {
                autostart: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(changed, Some(true));
        assert!(s.autostart);
    }

    #[test]
    fn removing_default_jdk_falls_back_to_first() {
        let mut s = sample();
        let jdks = vec![JdkConfig {
            id: "b".to_string(),
            name: "B".to_string(),
            path: "D:/b".to_string(),
        }];
        apply_settings_patch(
            &mut s,
            SettingsPatch {
                jdks: Some(jdks),
                ..Default::default()
            },
        );
        assert_eq!(s.default_jdk, "b");
        assert_eq!(s.jdks.len(), 1);
    }

    #[test]
    fn removing_all_jdks_clears_default() {
        let mut s = sample();
        apply_settings_patch(
            &mut s,
            SettingsPatch {
                jdks: Some(Vec::new()),
                ..Default::default()
            },
        );
        assert_eq!(s.default_jdk, "");
    }

    #[test]
    fn unknown_default_jdk_is_ignored() {
        let mut s = sample();
        apply_settings_patch(
            &mut s,
            SettingsPatch {
                default_jdk: Some("ghost".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(s.default_jdk, "a");
    }

    #[test]
    fn work_dir_is_trimmed() {
        let mut s = sample();
        apply_settings_patch(
            &mut s,
            SettingsPatch {
                work_dir: Some("  D:/new  ".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(s.work_dir, "D:/new");
    }

    #[test]
    fn resume_patch_toggles_flag() {
        let mut s = sample();
        assert!(s.resume_on_start);
        apply_settings_patch(
            &mut s,
            SettingsPatch {
                resume_on_start: Some(false),
                ..Default::default()
            },
        );
        assert!(!s.resume_on_start);
        // 不传就不动
        apply_settings_patch(&mut s, SettingsPatch::default());
        assert!(!s.resume_on_start);
    }

    #[test]
    fn restore_merges_snapshot_and_dead_records() {
        let snap = vec!["a".to_string(), "b".to_string()];
        let dead = vec!["b".to_string(), "c".to_string()];
        let alive: HashSet<String> = ["b".to_string()].into_iter().collect();
        // b 仍在运行 → 不重复启动；a 来自快照、c 来自失效记录
        assert_eq!(
            merge_restore_ids(&snap, &dead, &alive),
            vec!["a".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn session_snapshot_survives_roundtrip() {
        let snap = SessionSnapshot {
            saved_at: 1_700_000_000_000,
            services: vec![SessionEntry {
                id: "svc-1".to_string(),
                name: "订单".to_string(),
                jar_path: "D:/svc/a.jar".to_string(),
                started_at: 1_700_000_000_123,
            }],
        };
        let text = serde_json::to_string(&snap).unwrap();
        let back: SessionSnapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(back.services.len(), 1);
        assert_eq!(back.services[0].id, "svc-1");
        assert_eq!(back.saved_at, 1_700_000_000_000);
    }

    #[test]
    fn store_without_resume_field_defaults_to_true() {
        // 老版本 store.json 里没有 resumeOnStart，反序列化后应当默认为 true
        let json = r#"{"workDir":"D:/w","jdks":[],"services":[],"autostart":true,
                       "stopOnExit":true,"defaultJdk":""}"#;
        let s: Store = serde_json::from_str(json).unwrap();
        assert!(s.resume_on_start);
    }
}
