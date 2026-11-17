use crate::model::{JarEntry, JarEntryContent};
use crate::proc::decode_bytes;
use std::fs::File;
use std::io::Read;

fn kind_of(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.ends_with('/') {
        return "dir";
    }
    if lower.ends_with("manifest.mf") || lower.ends_with(".mf") {
        return "config";
    }
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "yml" | "yaml" | "properties" | "json" | "xml" | "conf" | "cfg" | "ini" | "toml" | "env"
        | "txt" | "factories" | "imports" | "sql" | "sh" | "bat" | "prefs" | "service" => "config",
        "class" => "class",
        "jar" | "so" | "dll" | "dylib" => "lib",
        _ => "other",
    }
}

fn rank(kind: &str) -> u8 {
    match kind {
        "config" => 0,
        "other" => 1,
        "lib" => 3,
        "class" => 4,
        _ => 5,
    }
}

pub fn list_entries(path: &str) -> Result<Vec<JarEntry>, String> {
    let file = File::open(path).map_err(|e| format!("无法打开文件: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("不是有效的 JAR/ZIP 文件: {}", e))?;
    let mut items: Vec<JarEntry> = Vec::new();
    for i in 0..archive.len() {
        let entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().to_string();
        let is_dir = entry.is_dir();
        items.push(JarEntry {
            kind: kind_of(&name).to_string(),
            name,
            size: entry.size(),
            is_dir,
        });
    }
    items.sort_by(|a, b| {
        rank(&a.kind)
            .cmp(&rank(&b.kind))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(items)
}

pub fn read_entry(path: &str, name: &str, max_bytes: usize) -> Result<JarEntryContent, String> {
    let file = File::open(path).map_err(|e| format!("无法打开文件: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("不是有效的 JAR/ZIP 文件: {}", e))?;
    let entry = archive
        .by_name(name)
        .map_err(|_| format!("JAR 中不存在条目: {}", name))?;
    let size = entry.size();
    let max = if max_bytes == 0 { 512 * 1024 } else { max_bytes };
    let mut buf: Vec<u8> = Vec::new();
    entry
        .take(max as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("读取条目失败: {}", e))?;
    let truncated = buf.len() > max;
    if truncated {
        buf.truncate(max);
    }
    let (text, encoding) = decode_bytes(&buf);
    Ok(JarEntryContent {
        name: name.to_string(),
        content: text,
        size,
        truncated,
        binary: encoding == "binary",
        encoding: encoding.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sample_jar(tag: &str) -> std::path::PathBuf {
        let bytes = include_bytes!("../../testdata/jars-demo.jar");
        let path = std::env::temp_dir().join(format!("jarbox-test-{}.jar", tag));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        path
    }

    #[test]
    fn lists_config_entries_first() {
        let p = sample_jar("list");
        let entries = list_entries(p.to_str().unwrap()).unwrap();
        assert!(!entries.is_empty());
        assert_eq!(entries[0].kind, "config", "配置文件应排在最前面");
        assert!(entries.iter().any(|e| e.name.ends_with("application.properties")));
        assert!(entries.iter().any(|e| e.name.ends_with("DemoApp.class")));
    }

    #[test]
    fn reads_text_entry() {
        let p = sample_jar("read");
        let c = read_entry(p.to_str().unwrap(), "application.properties", 0).unwrap();
        assert!(!c.binary);
        assert!(c.content.contains("server.port=8080"));
    }

    #[test]
    fn rejects_non_zip() {
        let p = std::env::temp_dir().join("jarbox-bad.jar");
        std::fs::write(&p, b"not a zip").unwrap();
        assert!(list_entries(p.to_str().unwrap()).is_err());
    }
}
