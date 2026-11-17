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
    let work_dir = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store.work_dir.clone()
    };
    if work_dir.trim().is_empty() {
        return Err("请先在设置中配置工作目录".to_string());
    }
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
            default_jdk: "a".to_string(),
        }
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
