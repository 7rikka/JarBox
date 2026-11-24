use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// 持久化模型
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct JdkConfig {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub id: String,
    pub name: String,
    pub jar_path: String,
    pub jdk_id: String,
    #[serde(default)]
    pub jvm_args: String,
    #[serde(default)]
    pub program_args: String,
    #[serde(default)]
    pub work_dir: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Store {
    #[serde(default)]
    pub work_dir: String,
    #[serde(default)]
    pub jdks: Vec<JdkConfig>,
    #[serde(default)]
    pub services: Vec<Service>,
    #[serde(default = "default_true")]
    pub autostart: bool,
    #[serde(default = "default_true")]
    pub stop_on_exit: bool,
    /// 启动时恢复「上次退出时仍在运行」的服务
    #[serde(default = "default_true")]
    pub resume_on_start: bool,
    /// 导入 JAR 的方式：copy（复制，保留源文件）/ move（移动，删除源文件）
    #[serde(default = "default_import_mode")]
    pub import_mode: String,
    /// 替换 JAR 时在 bak 目录里保留的历史版本个数
    #[serde(default = "default_keep_jar_versions")]
    pub keep_jar_versions: u32,
    /// 日志按天分割：跨天自动换新文件
    #[serde(default)]
    pub log_split_by_day: bool,
    /// 日志按大小分割
    #[serde(default)]
    pub log_split_by_size: bool,
    /// 按大小分割的阈值（MB）
    #[serde(default = "default_log_split_size_mb")]
    pub log_split_size_mb: u32,
    #[serde(default)]
    pub default_jdk: String,
}

fn default_true() -> bool {
    true
}

fn default_import_mode() -> String {
    "copy".to_string()
}

fn default_keep_jar_versions() -> u32 {
    10
}

/// 按大小分割的默认阈值：50 MB
fn default_log_split_size_mb() -> u32 {
    50
}

impl Default for Store {
    fn default() -> Self {
        Self {
            work_dir: String::new(),
            jdks: Vec::new(),
            services: Vec::new(),
            autostart: true,
            stop_on_exit: true,
            resume_on_start: true,
            import_mode: default_import_mode(),
            keep_jar_versions: default_keep_jar_versions(),
            log_split_by_day: false,
            log_split_by_size: false,
            log_split_size_mb: default_log_split_size_mb(),
            default_jdk: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 运行时状态
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Running {
    pub pid: u32,
    pub started_at: i64,
    pub stop_requested: bool,
    pub log_path: PathBuf,
    pub cmdline: String,
    /// 是否为重启管理器后「接管」的遗留进程（不是本次会话启动的）
    pub adopted: bool,
}

/// 落盘的运行时记录，用于管理器退出/崩溃后重启时重新认领进程
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRecord {
    pub service_id: String,
    pub pid: u32,
    pub started_at: i64,
    pub log_path: String,
    pub cmdline: String,
    pub jar_path: String,
}

#[derive(Clone, Debug)]
pub struct LastExit {
    pub code: Option<i32>,
    pub at: i64,
    /// 是否由用户在 JarBox 中手动停止（而不是进程自己崩溃）
    pub stopped: bool,
}

/// 退出时的运行快照：下次启动据此把「上次在跑」的服务重新拉起来
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub saved_at: i64,
    pub services: Vec<SessionEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntry {
    pub id: String,
    pub name: String,
    pub jar_path: String,
    pub started_at: i64,
}

pub struct AppState {
    pub store: Mutex<Store>,
    pub running: Mutex<HashMap<String, Running>>,
    pub last_exit: Mutex<HashMap<String, LastExit>>,
    pub data_dir: PathBuf,
    pub sys: Mutex<Option<sysinfo::System>>,
    /// 已识别出的端口缓存：服务 id -> (本次启动时间, 日志文件, 端口)
    pub ports: Mutex<HashMap<String, (i64, PathBuf, u16)>>,
}

impl AppState {
    pub fn logs_root(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
    pub fn store_path(&self) -> PathBuf {
        self.data_dir.join("store.json")
    }
}

// ---------------------------------------------------------------------------
// 前端视图模型
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServiceView {
    #[serde(flatten)]
    pub service: Service,
    pub status: String,
    pub pid: Option<u32>,
    pub started_at: Option<i64>,
    pub exit_code: Option<i32>,
    pub exited_at: Option<i64>,
    pub stopped_by_user: bool,
    pub adopted: bool,
    /// 从启动日志里识别出的监听端口（用户不再手填）
    pub detected_port: Option<u16>,
    pub log_dir: String,
    pub current_log: String,
    pub jar_exists: bool,
    pub jdk_path: String,
    pub jdk_ok: bool,
    pub cmdline: String,
}

/// 替换 JAR 的结果：新包已就位、旧包已备份
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceResult {
    pub service: ServiceView,
    /// 备份文件的完整路径（位于 JAR 同目录的 bak 子目录）
    pub backup: String,
    /// 替换前服务是否在运行；为 true 时替换后已自动重启
    pub restarted: bool,
    /// 替换成功但重启失败时的原因
    pub restart_error: Option<String>,
    /// 因超过「保留历史版本个数」而被清理掉的旧备份数量
    pub pruned: usize,
    /// bak 里已存在同一版本、这次没有再新增备份
    #[serde(default)]
    pub deduped: bool,
}

/// 一个历史 JAR 备份（bak 目录里的文件）
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JarBackup {
    pub name: String,
    pub path: String,
    pub size: u64,
    /// 毫秒时间戳
    pub modified: i64,
}

/// 某个服务的版本清单：当前包 + 历史备份
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JarBackups {
    pub current_name: String,
    pub current_size: u64,
    pub current_modified: i64,
    pub current_exists: bool,
    pub dir: String,
    /// 当前设置的保留个数
    pub keep: u32,
    pub items: Vec<JarBackup>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub store: Store,
    pub services: Vec<ServiceView>,
    pub data_dir: String,
    pub logs_root: String,
    pub version: String,
    pub autostart_enabled: bool,
    pub autostart_skipped: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SaveResult {
    pub store: Store,
    /// 设置已保存，但开机自启没能生效时的说明
    pub autostart_warning: Option<String>,
    /// 保存后真实的自启状态（注册表为准）
    pub autostart_enabled: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JarEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub kind: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JarEntryContent {
    pub name: String,
    pub content: String,
    pub size: u64,
    pub truncated: bool,
    pub binary: bool,
    pub encoding: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LogFileInfo {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub modified: i64,
    pub is_current: bool,
    /// 这个文件覆盖的起始时间（毫秒时间戳，取自文件名里的启动时间）
    pub started_at: i64,
    /// 覆盖的结束时间：下一个分片的起始时间；0 表示还在写
    pub ended_at: i64,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct LogCounts {
    pub total: i64,
    pub error: i64,
    pub warn: i64,
    pub info: i64,
    pub debug: i64,
    pub trace: i64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LogResult {
    pub file: String,
    pub path: String,
    pub size: u64,
    pub lines: Vec<String>,
    pub matched: usize,
    pub total: usize,
    pub truncated: bool,
    pub counts: LogCounts,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JavaInfo {
    pub ok: bool,
    pub version: String,
    pub binary: String,
    pub message: String,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStat {
    pub id: String,
    pub pid: u32,
    pub cpu: f32,
    pub memory: u64,
    pub virtual_memory: u64,
    pub uptime_ms: i64,
}
