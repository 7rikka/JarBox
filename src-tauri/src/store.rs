use crate::model::Store;
use std::fs;
use std::path::Path;

pub fn load(path: &Path) -> Store {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| Store::default()),
        Err(_) => Store::default(),
    }
}

pub fn save(path: &Path, store: &Store) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {}", e))?;
    }
    let text = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| format!("写入配置文件失败: {}", e))
}
