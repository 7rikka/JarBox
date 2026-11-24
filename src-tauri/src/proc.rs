use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::model::JavaInfo;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 隐藏子进程的控制台窗口（Windows）
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

/// 把用户填写的 JDK 路径（JDK 根目录 / bin 目录 / java 可执行文件）解析为 java 可执行文件
pub fn java_binary(input: &str) -> Result<PathBuf, String> {
    let raw = input.trim().trim_matches('"');
    if raw.is_empty() {
        return Err("未配置 JDK".to_string());
    }
    let p = Path::new(raw);
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    let candidates = [
        p.join("bin").join("java.exe"),
        p.join("bin").join("java"),
        p.join("java.exe"),
        p.join("java"),
    ];
    for c in candidates {
        if c.is_file() {
            return Ok(c);
        }
    }
    Err(format!("未在 {} 中找到 java 可执行文件", raw))
}

/// 执行 java -version 获取版本信息
pub fn detect_java(input: &str) -> JavaInfo {
    let bin = match java_binary(input) {
        Ok(b) => b,
        Err(e) => {
            return JavaInfo {
                ok: false,
                version: String::new(),
                binary: String::new(),
                message: e,
            }
        }
    };
    let mut cmd = Command::new(&bin);
    cmd.arg("-version");
    no_window(&mut cmd);
    match cmd.output() {
        Ok(out) => {
            let raw = if out.stderr.is_empty() { out.stdout } else { out.stderr };
            let (text, _) = decode_bytes(&raw);
            let version = text
                .lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_string();
            JavaInfo {
                ok: true,
                version,
                binary: bin.to_string_lossy().to_string(),
                message: "Java 可用".to_string(),
            }
        }
        Err(e) => JavaInfo {
            ok: false,
            version: String::new(),
            binary: bin.to_string_lossy().to_string(),
            message: format!("执行 java -version 失败: {}", e),
        },
    }
}

/// 按 shell 习惯拆分参数，支持单/双引号
pub fn split_args(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                } else {
                    cur.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                    started = true;
                } else if c.is_whitespace() {
                    if started {
                        out.push(cur.clone());
                        cur.clear();
                        started = false;
                    }
                } else {
                    cur.push(c);
                    started = true;
                }
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// 宽松解码：优先 UTF-8，失败则按 GBK（中文 Windows 下的 Java 日志）
pub fn decode_bytes(buf: &[u8]) -> (String, &'static str) {
    if buf.is_empty() {
        return (String::new(), "utf-8");
    }
    if buf.iter().take(8192).any(|b| *b == 0) {
        return (String::from_utf8_lossy(buf).to_string(), "binary");
    }
    match std::str::from_utf8(buf) {
        Ok(s) => (s.to_string(), "utf-8"),
        Err(_) => {
            let (cow, _, _) = encoding_rs::GBK.decode(buf);
            (cow.to_string(), "gbk")
        }
    }
}

/// 按行宽松解码：同一份日志里可能同时存在 UTF-8（我们写入的抬头）
/// 与 GBK（中文 Windows 下 Java 进程的真实输出），逐行分别判断编码。
pub fn decode_lines(buf: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in buf.split(|b| *b == b'\n') {
        let line = if raw.last() == Some(&b'\r') {
            &raw[..raw.len() - 1]
        } else {
            raw
        };
        if line.is_empty() {
            out.push(String::new());
        } else {
            let (text, _) = decode_bytes(line);
            out.push(text);
        }
    }
    if out.last().map(|l| l.is_empty()).unwrap_or(false) {
        out.pop();
    }
    out
}

/// 去掉终端颜色控制序列
pub fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() || n == 'm' {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

pub struct Launch {
    pub child: Child,
    /// 日志写入器：进程退出后用它补一行退出信息
    pub sink: std::sync::Arc<LogSink>,
}

/// 日志分割策略
#[derive(Clone, Debug, Default)]
pub struct LogPolicy {
    /// 按天分割：跨天自动换新文件
    pub split_by_day: bool,
    /// 按大小分割的阈值（字节），0 表示不按大小分割
    pub split_size: u64,
}

impl LogPolicy {
    pub fn rotating(&self) -> bool {
        self.split_by_day || self.split_size > 0
    }
}

enum SinkMode {
    /// 不分割：进程直接把 stdout/stderr 写进这个文件。
    /// JarBox 退出后 JVM 仍然能继续写，重启管理器时才能无损接管日志。
    Direct { file: fs::File },
    /// 分割：由 JarBox 转发写入，可以随时换到新文件
    Rolling {
        file: fs::File,
        path: PathBuf,
        day: String,
        written: u64,
    },
}

/// 日志写入器：对外只是一个「往哪儿写」的黑盒，内部决定要不要分割。
pub struct LogSink {
    dir: PathBuf,
    policy: LogPolicy,
    /// 服务名，用于分割时的续接提示
    name: String,
    state: std::sync::Mutex<SinkMode>,
}

impl LogSink {
    pub fn write_line(&self, s: &str) {
        let mut buf = String::from(s);
        if !buf.ends_with('\n') {
            buf.push('\n');
        }
        self.write_bytes(buf.as_bytes());
    }

    pub fn write_bytes(&self, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }
        let mut g = match self.state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };

        // 只有 Rolling 模式才可能换文件；Direct 要保持句柄稳定
        let (day_changed, size_exceeded) = match &*g {
            SinkMode::Rolling { day, written, .. } => {
                let today = day_key();
                let by_day = self.policy.split_by_day && day != &today;
                let by_size = self.policy.split_size > 0
                    && *written > 0
                    && *written + buf.len() as u64 > self.policy.split_size;
                (by_day, by_size)
            }
            SinkMode::Direct { .. } => (false, false),
        };

        if day_changed || size_exceeded {
            let reason = if day_changed { "跨天" } else { "大小" };
            self.rotate(&mut g, reason);
        }

        match &mut *g {
            SinkMode::Direct { file, .. } => {
                let _ = file.write_all(buf);
            }
            SinkMode::Rolling { file, written, .. } => {
                let _ = file.write_all(buf);
                *written += buf.len() as u64;
            }
        }
    }

    /// 换一个新文件，并写一行续接提示（不然日志看着会像突然中断）
    fn rotate(&self, st: &mut SinkMode, reason: &str) {
        let old_path = match st {
            SinkMode::Direct { .. } => return,
            SinkMode::Rolling { file, path, .. } => {
                let _ = file.flush();
                let _ = file.sync_data();
                path.clone()
            }
        };
        let (path, file) = match create_log_file(&self.dir) {
            Some(v) => v,
            None => return,
        };
        let mut w = &file;
        let _ = writeln!(
            w,
            "===== JarBox · {}: 日志按{}分割，续接于 {}（上一段：{}）=====",
            self.name,
            reason,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            old_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        );
        let _ = w.flush();
        // 续接提示本身不计入配额，否则阈值很小时会立刻再次超限、反复换文件
        *st = SinkMode::Rolling {
            file,
            path,
            day: day_key(),
            written: 0,
        };
    }
}

fn day_key() -> String {
    chrono::Local::now().format("%Y%m%d").to_string()
}

/// 新建一个按时间命名的日志文件（同一秒内重复时加序号）
fn create_log_file(dir: &Path) -> Option<(PathBuf, fs::File)> {
    if fs::create_dir_all(dir).is_err() {
        return None;
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    for i in 0..50 {
        let name = if i == 0 {
            format!("{}.log", stamp)
        } else {
            format!("{}-{}.log", stamp, i + 1)
        };
        let path = dir.join(name);
        if path.exists() {
            continue;
        }
        match fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => return Some((path, f)),
            Err(_) => return None,
        }
    }
    None
}

/// 文件名里的起始时间戳 `20261124-101530.log` → 毫秒；解析不了返回 None
/// 同一秒内换新文件时会写成 `20261124-101530-2.log`，末尾序号不影响起始时间。
pub fn log_start_ms(name: &str) -> Option<i64> {
    use chrono::TimeZone;
    let mut stem = name.strip_suffix(".log")?;
    if let Some((base, tail)) = stem.rsplit_once('-') {
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) && tail.len() <= 2 {
            stem = base;
        }
    }
    let naive = chrono::NaiveDateTime::parse_from_str(stem, "%Y%m%d-%H%M%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(stem, "%Y%m%d-%H%M"))
        .ok()?;
    chrono::Local
        .from_local_datetime(&naive)
        .single()
        .map(|d| d.timestamp_millis())
}

/// 算这次启动该写哪个文件。
/// - 不分割：每次启动新建一个文件
/// - 按天分割：当天已经有日志就接着写（一天一个文件），否则新建
pub fn initial_log_path(dir: &Path, split_by_day: bool) -> Option<PathBuf> {
    if split_by_day {
        let today = day_key();
        let mut best: Option<(i64, PathBuf)> = None;
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let path = e.path();
                if !path.is_file() {
                    continue;
                }
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if !name.starts_with(&today) || !name.ends_with(".log") {
                    continue;
                }
                let start = log_start_ms(&name).unwrap_or(0);
                if best.as_ref().map(|(t, _)| start > *t).unwrap_or(true) {
                    best = Some((start, path));
                }
            }
        }
        if let Some((_, path)) = best {
            if fs::OpenOptions::new().create(true).append(true).open(&path).is_ok() {
                return Some(path);
            }
        }
    }
    create_log_file(dir).map(|(p, _)| p)
}

/// 启动 java -jar 进程。
/// 不分割时把 stdout/stderr 直接重定向到文件：管理器退出（崩溃/被强杀）后，
/// Java 仍能继续写日志，重新打开管理器时才能无损接管这些进程。
/// 开启分割后只能由 JarBox 转发写入（管道），才能在中途换文件。
pub fn launch(
    bin: &Path,
    jvm_args: &[String],
    jar: &Path,
    prog_args: &[String],
    cwd: &Path,
    log_path: &Path,
    policy: &LogPolicy,
    name: &str,
    banner: &str,
) -> Result<Launch, String> {
    let dir = log_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建日志目录失败: {}", e))?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("创建日志文件失败: {}", e))?;
    let existing = fs::metadata(log_path).map(|m| m.len()).unwrap_or(0);

    let mode = if policy.rotating() {
        SinkMode::Rolling {
            file,
            path: log_path.to_path_buf(),
            day: day_key(),
            written: existing,
        }
    } else {
        SinkMode::Direct { file }
    };
    let sink = std::sync::Arc::new(LogSink {
        dir: dir.clone(),
        policy: policy.clone(),
        name: name.to_string(),
        state: std::sync::Mutex::new(mode),
    });
    sink.write_line(banner);

    let mut cmd = Command::new(bin);
    cmd.args(jvm_args)
        .arg("-jar")
        .arg(jar)
        .args(prog_args)
        .current_dir(cwd)
        .stdin(Stdio::null());
    if policy.rotating() {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    } else {
        // 直接给文件句柄：JarBox 退出后 JVM 也能继续写
        let w = match &*sink.state.lock().map_err(|e| e.to_string())? {
            SinkMode::Direct { file, .. } => file.try_clone().map_err(|e| format!("日志句柄复制失败: {}", e))?,
            SinkMode::Rolling { file, .. } => file.try_clone().map_err(|e| format!("日志句柄复制失败: {}", e))?,
        };
        let w2 = w.try_clone().map_err(|e| format!("日志句柄复制失败: {}", e))?;
        cmd.stdout(Stdio::from(w)).stderr(Stdio::from(w2));
    }
    no_window(&mut cmd);

    let mut child = cmd.spawn().map_err(|e| format!("启动进程失败: {}", e))?;

    if policy.rotating() {
        if let Some(out) = child.stdout.take() {
            pump(out, sink.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump(err, sink.clone());
        }
    }

    Ok(Launch { child, sink })
}

/// 把管道里的字节转发给 LogSink（分割模式下唯一的写入通道）
fn pump<R: std::io::Read + Send + 'static>(mut src: R, sink: std::sync::Arc<LogSink>) {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => sink.write_bytes(&buf[..n]),
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

/// 结束进程树（java 可能派生多个线程/子进程）
pub fn kill_tree(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T", "/F"]);
        no_window(&mut cmd);
        let out = cmd.output().map_err(|e| format!("调用 taskkill 失败: {}", e))?;
        if out.status.success() {
            return Ok(());
        }
        let (msg, _) = decode_bytes(&out.stderr);
        let lower = msg.to_lowercase();
        if lower.contains("not found") || msg.contains("没有") || msg.contains("找不到") {
            // 进程已自行退出
            Ok(())
        } else {
            Err(format!("结束进程失败: {}", msg.trim()))
        }
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("kill");
        cmd.args(["-9", &pid.to_string()]);
        no_window(&mut cmd);
        let _ = cmd.status();
        Ok(())
    }
}

/// 在资源管理器中定位文件/文件夹
pub fn reveal(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("路径不存在: {}", path));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // explorer.exe 自己解析命令行：交给 std 自动加引号时，
        // 带空格的路径会让整段 `/select,C:\a b\x.jar` 被一对引号包住，
        // explorer 认不出这个参数，就会退回默认目录（我的文档）。
        // 所以用 raw_arg 只给路径本身加引号，并统一成正斜杠 → 反斜杠。
        let mut target = path.replace('/', "\\");
        if p.is_dir() {
            while target.len() > 3 && target.ends_with('\\') {
                target.pop();
            }
        }
        let mut cmd = Command::new("explorer");
        if p.is_dir() {
            cmd.raw_arg(format!("\"{}\"", target));
        } else {
            cmd.raw_arg(format!("/select,\"{}\"", target));
        }
        cmd.spawn()
            .map_err(|e| format!("打开资源管理器失败: {}", e))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg("-R").arg(path).spawn();
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open")
            .arg(if p.is_dir() { path.to_string() } else { p.parent().map(|x| x.to_string_lossy().to_string()).unwrap_or_default() })
            .spawn();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn split_args_keeps_quoted_spaces() {
        assert_eq!(
            split_args("-Xmx1g -Dfoo=\"a b\" --flag='x y'"),
            vec!["-Xmx1g", "-Dfoo=a b", "--flag=x y"]
        );
        assert_eq!(split_args("   "), Vec::<String>::new());
        assert_eq!(split_args("-jar a.jar"), vec!["-jar", "a.jar"]);
    }

    #[test]
    fn decode_lines_handles_mixed_encodings() {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice("===== JarBox · 演示服务 =====\n".as_bytes());
        let gbk = encoding_rs::GBK.encode("[INFO] 启动完成，端口 8080\n").0.to_vec();
        buf.extend_from_slice(&gbk);
        let lines = decode_lines(&buf);
        assert!(lines[0].contains("演示服务"), "utf8 行解码失败: {}", lines[0]);
        assert!(lines[1].contains("启动完成"), "gbk 行解码失败: {}", lines[1]);
    }

    #[test]
    fn strip_ansi_removes_color_sequences() {
        assert_eq!(strip_ansi("\u{1b}[32mINFO\u{1b}[0m ready"), "INFO ready");
    }

    #[test]
    fn log_start_ms_reads_the_file_name() {
        // 文件名里的时间就是这一段的起始时间，本地时区
        let ms = log_start_ms("20261124-101530.log").unwrap();
        let back = chrono::Local.timestamp_millis_opt(ms).unwrap();
        assert_eq!(back.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-11-24 10:15:30");
        // 同一秒换了多个文件时，序号不能把时间解析搞坏
        assert_eq!(log_start_ms("20261124-101530-2.log"), Some(ms));
        assert!(log_start_ms("不是日志.log").is_none());
    }

    /// 造一个只用于测试的 sink（不启动进程），直接往里写字节看是否换文件
    fn test_sink(dir: &Path, policy: LogPolicy) -> (std::sync::Arc<LogSink>, PathBuf) {
        let path = initial_log_path(dir, policy.split_by_day).unwrap();
        let file = fs::OpenOptions::new().create(true).append(true).open(&path).unwrap();
        let mode = if policy.rotating() {
            let written = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            SinkMode::Rolling {
                file,
                path: path.clone(),
                day: day_key(),
                written,
            }
        } else {
            SinkMode::Direct { file }
        };
        let sink = std::sync::Arc::new(LogSink {
            dir: dir.to_path_buf(),
            policy,
            name: "测试服务".to_string(),
            state: std::sync::Mutex::new(mode),
        });
        (sink, path)
    }

    fn log_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn rolling_sink_splits_by_size() {
        let dir = std::env::temp_dir().join(format!("jarbox-log-size-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let (sink, first) = test_sink(
            &dir,
            LogPolicy {
                split_by_day: false,
                split_size: 100,
            },
        );
        // 每行 40 字节：写到第三行时超过 100，应该换一个新文件
        for _ in 0..6 {
            sink.write_bytes(&[b'x'; 40]);
        }
        let files = log_files(&dir);
        assert_eq!(files.len(), 3, "应分成 3 个文件: {:?}", files);
        assert!(first.is_file());
        let total: u64 = files
            .iter()
            .map(|n| fs::metadata(dir.join(n)).unwrap().len())
            .sum();
        // 换文件时会补一行续接提示，所以总字节只会比 240 多
        assert!(total >= 240, "分片里不能丢日志: {}", total);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rolling_sink_keeps_one_file_per_day() {
        let dir = std::env::temp_dir().join(format!("jarbox-log-day-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let policy = LogPolicy {
            split_by_day: true,
            split_size: 0,
        };
        let (sink, path) = test_sink(&dir, policy.clone());
        sink.write_line("第一行");
        // 同一天再启动一次：应当接着写同一个文件
        let again = initial_log_path(&dir, true).unwrap();
        assert_eq!(again, path, "按天分割时同一天要续写同一个文件");
        drop(sink);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn direct_sink_never_splits() {
        let dir = std::env::temp_dir().join(format!("jarbox-log-direct-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let (sink, _first) = test_sink(&dir, LogPolicy::default());
        for _ in 0..50 {
            sink.write_bytes(&[b'y'; 1024]);
        }
        assert_eq!(log_files(&dir).len(), 1, "不分割时只有一个文件");
        let _ = fs::remove_dir_all(&dir);
    }
}
