//! 开机自启
//!
//! Windows 上直接读写 `HKCU\...\Run`：用户级启动项不需要管理员权限，
//! 也不会像旧实现那样「先试 HKLM、失败再回退」，把「拒绝访问」这种
//! 底层错误原样抛给用户。所有失败都会翻译成中文可操作提示。

use tauri::AppHandle;

pub const APP_NAME: &str = "JarBox";

#[cfg(windows)]
mod win {
    use super::APP_NAME;
    use std::io::ErrorKind;
    use winreg::enums::*;
    use winreg::{RegKey, HKEY};

    const RUN_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    const APPROVED_KEY: &str =
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    /// 任务管理器「启动」里的启用标记
    const ENABLED_FLAG: [u8; 12] = [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    /// 把 win32 错误翻译成用户能看懂的提示
    fn describe(action: &str, err: &std::io::Error) -> String {
        let code = err.raw_os_error().unwrap_or(0) as u32;
        let hint = match code {
            5 | 0x8007_0005 => {
                "被系统策略或安全软件拦截了（拒绝访问）。可以把 JarBox 加入安全软件白名单，\
                 或右键 JarBox「以管理员身份运行」后再试"
            }
            3 => "路径不存在",
            2 => "注册表项不存在",
            _ => "注册表写入失败",
        };
        format!("{}失败：{}（错误码 0x{:08X}）—— {}", action, err, code, hint)
    }

    fn exe_command() -> Result<String, String> {
        let exe = std::env::current_exe().map_err(|e| format!("获取程序路径失败：{}", e))?;
        Ok(format!("\"{}\"", exe.to_string_lossy()))
    }

    fn run_value(hive: HKEY) -> Option<String> {
        RegKey::predef(hive)
            .open_subkey_with_flags(RUN_KEY, KEY_READ)
            .ok()
            .and_then(|k| k.get_value::<String, _>(APP_NAME).ok())
    }

    /// 任务管理器里是否被禁用（数据末尾 8 字节全 0 表示启用）
    fn task_manager_enabled(hive: HKEY) -> bool {
        let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(APPROVED_KEY, KEY_READ) else {
            return true;
        };
        match key.get_raw_value(APP_NAME) {
            Ok(v) if v.bytes.len() >= 8 => v.bytes.iter().rev().take(8).all(|b| *b == 0),
            _ => true,
        }
    }

    pub fn status() -> super::AutostartStatus {
        let user_entry = run_value(HKEY_CURRENT_USER).is_some();
        let system_entry = run_value(HKEY_LOCAL_MACHINE).is_some();
        let blocked = !task_manager_enabled(HKEY_CURRENT_USER);
        super::AutostartStatus {
            user_entry,
            system_entry,
            blocked,
            exe: exe_command().unwrap_or_default(),
        }
    }

    pub fn enable() -> Result<(), String> {
        let cmd = exe_command()?;
        let (run, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(RUN_KEY)
            .map_err(|e| describe("写用户启动项", &e))?;
        run.set_value(APP_NAME, &cmd)
            .map_err(|e| describe("写用户启动项", &e))?;

        // 读回校验：有些安全软件会「返回成功但其实把值回滚掉」
        let written = run_value(HKEY_CURRENT_USER);
        if written.as_deref() != Some(cmd.as_str()) {
            return Err(
                "启动项写入后校验失败：注册表里没有找到 JarBox。多半是安全软件静默拦截了启动项，\
                 请把 JarBox 加入白名单后重试"
                    .to_string(),
            );
        }

        // 任务管理器里的「启用」标记，失败不影响开机自启，忽略即可
        let _ = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(APPROVED_KEY)
            .and_then(|(k, _)| {
                k.set_raw_value(
                    APP_NAME,
                    &winreg::RegValue {
                        vtype: REG_BINARY,
                        bytes: std::borrow::Cow::Borrowed(&ENABLED_FLAG[..]),
                    },
                )
            });

        // 防御性清理：机器上若有同名的系统级启动项（例如手工添加过），
        // 删掉避免与用户级项重复触发；非管理员删不掉，失败无所谓
        if run_value(HKEY_LOCAL_MACHINE).is_some() {
            let _ = RegKey::predef(HKEY_LOCAL_MACHINE)
                .open_subkey_with_flags(RUN_KEY, KEY_WRITE)
                .and_then(|k| k.delete_value(APP_NAME));
        }
        Ok(())
    }

    pub fn disable() -> Result<(), String> {
        match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_WRITE) {
            Ok(key) => match key.delete_value(APP_NAME) {
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(describe("删除用户启动项", &e)),
            },
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(describe("打开启动项注册表", &e)),
        }

        let _ = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(APPROVED_KEY, KEY_WRITE)
            .and_then(|k| k.delete_value(APP_NAME));

        // 系统级同名项：非管理员删不掉，这里尽力而为，残留由调用方提示
        let _ = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(RUN_KEY, KEY_WRITE)
            .and_then(|k| k.delete_value(APP_NAME));
        Ok(())
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartStatus {
    /// 用户级启动项（HKCU）是否存在
    pub user_entry: bool,
    /// 系统级启动项（HKLM）是否有残留
    pub system_entry: bool,
    /// 是否被任务管理器禁用
    pub blocked: bool,
    /// 注册到启动项里的命令
    pub exe: String,
}

impl AutostartStatus {
    pub fn enabled(&self) -> bool {
        (self.user_entry || self.system_entry) && !self.blocked
    }

    pub fn summary(&self) -> String {
        format!(
            "user={} system={} blocked={} exe={}",
            self.user_entry, self.system_entry, self.blocked, self.exe
        )
    }
}

pub fn status(app: &AppHandle) -> AutostartStatus {
    #[cfg(windows)]
    {
        let _ = app;
        win::status()
    }
    #[cfg(not(windows))]
    {
        use tauri_plugin_autostart::ManagerExt;
        let enabled = app.autolaunch().is_enabled().unwrap_or(false);
        AutostartStatus {
            user_entry: enabled,
            system_entry: false,
            blocked: false,
            exe: String::new(),
        }
    }
}

/// 应用开机自启设置。成功返回 Ok，失败返回可直接展示给用户的中文原因。
pub fn apply(app: &AppHandle, enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = app;
        if enabled {
            win::enable()?;
            // 关掉「退出时停止服务」之类并不会影响自启，这里只做最终校验
            if !win::status().enabled() {
                return Err("启动项已写入，但任务管理器里处于禁用状态，请到「任务管理器 → 启动」中启用 JarBox".to_string());
            }
            Ok(())
        } else {
            win::disable()?;
            let st = win::status();
            if st.enabled() {
                return Err(
                    "仍检测到系统级启动项（HKLM），普通权限删不掉。请以管理员身份运行 JarBox 后关闭，\
                     或在「任务管理器 → 启动」里禁用 JarBox"
                        .to_string(),
                );
            }
            Ok(())
        }
    }
    #[cfg(not(windows))]
    {
        use tauri_plugin_autostart::ManagerExt;
        let manager = app.autolaunch();
        if enabled {
            manager
                .enable()
                .map_err(|e| format!("注册开机自启失败：{}", e))
        } else {
            manager
                .disable()
                .map_err(|e| format!("取消开机自启失败：{}", e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_enabled_needs_entry_and_not_blocked() {
        let mut st = AutostartStatus::default();
        assert!(!st.enabled());
        st.user_entry = true;
        assert!(st.enabled());
        st.blocked = true;
        assert!(!st.enabled());
    }

    #[cfg(windows)]
    #[test]
    fn status_reads_without_panicking() {
        // 只读探测，不修改注册表
        let st = win::status();
        assert!(!st.exe.is_empty());
    }
}
