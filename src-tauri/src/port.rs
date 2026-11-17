//! 从程序日志里识别服务实际监听的端口
//!
//! 端口不再由用户在新建服务时手填（容易和真实值不一致），改为在服务启动后
//! 从它自己的日志里读出来。这里只读日志尾部（最新内容），并从最后一行往前找，
//! 这样服务重启换了端口也能拿到最新值。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// 只看日志尾部这么多字节，足够覆盖一次启动过程
const TAIL: u64 = 96 * 1024;

pub fn detect(path: &Path) -> Option<u16> {
    let len = std::fs::metadata(path).ok()?.len();
    if len == 0 {
        return None;
    }
    let mut f = File::open(path).ok()?;
    let start = len.saturating_sub(TAIL);
    if start > 0 {
        f.seek(SeekFrom::Start(start)).ok()?;
    }
    let mut buf = Vec::with_capacity(TAIL as usize);
    f.take(TAIL).read_to_end(&mut buf).ok()?;
    let (text, _) = crate::proc::decode_bytes(&buf);
    detect_in_text(&text)
}

pub fn detect_in_text(text: &str) -> Option<u16> {
    for line in text.lines().rev() {
        if let Some(p) = detect_in_line(line) {
            return Some(p);
        }
    }
    None
}

/// 单行里找端口：先找「port」关键字，再找 host:port 形式
pub fn detect_in_line(line: &str) -> Option<u16> {
    let lower = line.to_ascii_lowercase();
    if let Some(p) = after_key(&lower, "port") {
        return Some(p);
    }
    if let Some(p) = after_key(&lower, "端口") {
        return Some(p);
    }
    for host in [
        "localhost:",
        "0.0.0.0:",
        "127.0.0.1:",
        "[::]:",
        "[::1]:",
        "://",
    ] {
        if let Some(p) = after_key(&lower, host) {
            return Some(p);
        }
    }
    None
}

/// 取 `key` 之后出现的第一个合法端口号，允许中间夹着 `(s)`、`:`、`=`、空格、引号
fn after_key(lower: &str, key: &str) -> Option<u16> {
    let mut from = 0usize;
    while let Some(idx) = lower[from..].find(key) {
        let abs = from + idx;
        let rest = &lower[abs + key.len()..];
        let trimmed = rest.trim_start_matches(|c: char| {
            matches!(
                c,
                ' ' | ':' | '=' | '(' | ')' | '[' | ']' | '"' | '\'' | '/' | 's' | '#' | ','
            )
        });
        let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() && digits.len() <= 5 {
            if let Ok(v) = digits.parse::<u32>() {
                if (1..=65535).contains(&v) {
                    return Some(v as u16);
                }
            }
        }
        from = abs + key.len();
        if from >= lower.len() {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_boot_tomcat_lines() {
        assert_eq!(
            detect_in_line("Tomcat started on port 8080 (http) with context path ''"),
            Some(8080)
        );
        assert_eq!(
            detect_in_line("Tomcat initialized with port(s): 9090 (http)"),
            Some(9090)
        );
        assert_eq!(detect_in_line("Netty started on port 8888"), Some(8888));
    }

    #[test]
    fn host_port_style_lines() {
        assert_eq!(
            detect_in_line("Listening on: http://0.0.0.0:3000"),
            Some(3000)
        );
        assert_eq!(
            detect_in_line("  ➜  Local:   http://localhost:5173/"),
            Some(5173)
        );
    }

    #[test]
    fn ignores_lines_without_port() {
        assert_eq!(detect_in_line("Started DemoApplication in 3.21 seconds"), None);
        assert_eq!(
            detect_in_line("2026-11-17 09:37:34.123  INFO 12345 --- [nio-8080-exec-1] hello"),
            None
        );
        assert_eq!(detect_in_line("important: 12345 items processed"), None);
    }

    #[test]
    fn newest_line_wins() {
        let text = "Tomcat started on port 8080 (http)\n\
                    later restart\n\
                    Tomcat started on port 9091 (http)\n";
        assert_eq!(detect_in_text(text), Some(9091));
    }

    #[test]
    fn reads_real_file_tail() {
        let dir = std::env::temp_dir().join("jarbox-port-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("app.log");
        std::fs::write(
            &file,
            "2026-11-17 09:30:01.201  INFO 1024 --- [main] o.s.b.w.embedded.tomcat.TomcatWebServer  : Tomcat initialized with port(s): 18080 (http)\n\
             2026-11-17 09:30:02.887  INFO 1024 --- [main] o.s.b.w.embedded.tomcat.TomcatWebServer  : Tomcat started on port 18080 (http) with context path ''\n\
             2026-11-17 09:30:02.889  INFO 1024 --- [main] c.e.demo.DemoApplication                 : Started DemoApplication in 2.4 seconds\n",
        )
        .unwrap();
        assert_eq!(detect(&file), Some(18080));
        let _ = std::fs::remove_file(&file);
    }
}
