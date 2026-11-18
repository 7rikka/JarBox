use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::io::Write;

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
    /// 日志文件句柄（用于在进程退出后追加一行退出信息）
    pub log: std::sync::Arc<std::sync::Mutex<fs::File>>,
}

/// 启动 java -jar 进程，并把 stdout/stderr 直接重定向到日志文件。
/// 走文件句柄而不是管道：管理器退出（崩溃/被强杀）后，Java 仍能继续写日志，
/// 重新打开管理器时才能无损接管这些进程。
pub fn launch(
    bin: &Path,
    jvm_args: &[String],
    jar: &Path,
    prog_args: &[String],
    cwd: &Path,
    log_path: &Path,
    banner: &str,
) -> Result<Launch, String> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建日志目录失败: {}", e))?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("创建日志文件失败: {}", e))?;
    let _ = writeln!(file, "{}", banner);
    let _ = file.flush();

    let out_file = file.try_clone().map_err(|e| format!("日志句柄复制失败: {}", e))?;
    let err_file = file.try_clone().map_err(|e| format!("日志句柄复制失败: {}", e))?;

    let mut cmd = Command::new(bin);
    cmd.args(jvm_args)
        .arg("-jar")
        .arg(jar)
        .args(prog_args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file));
    no_window(&mut cmd);

    let child = cmd.spawn().map_err(|e| format!("启动进程失败: {}", e))?;

    Ok(Launch {
        child,
        log: std::sync::Arc::new(std::sync::Mutex::new(file)),
    })
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
}
