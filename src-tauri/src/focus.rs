//! 把主窗口抢到前台。
//!
//! Windows 有「前台锁定」策略：后台进程调用 `SetForegroundWindow` 通常会被拒绝，
//! 只让任务栏按钮闪烁。单实例场景下用户刚刚双击了图标，我们却拿不到焦点，
//! 表现就是「双击了没反应」。
//!
//! 这里采用的常规做法是 `AttachThreadInput`：把自己的输入队列临时挂到当前前台线程上，
//! 变成「同一线程」后再设置前台窗口，完成后立即解除挂接。

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    #[link(name = "user32")]
    extern "system" {
        pub fn IsWindow(hwnd: *mut c_void) -> i32;
        pub fn IsIconic(hwnd: *mut c_void) -> i32;
        pub fn ShowWindow(hwnd: *mut c_void, cmd: i32) -> i32;
        pub fn GetForegroundWindow() -> *mut c_void;
        pub fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
        pub fn AttachThreadInput(attach: u32, attach_to: u32, do_attach: i32) -> i32;
        pub fn BringWindowToTop(hwnd: *mut c_void) -> i32;
        pub fn SetForegroundWindow(hwnd: *mut c_void) -> i32;
        pub fn SetFocus(hwnd: *mut c_void) -> *mut c_void;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetCurrentThreadId() -> u32;
    }

    /// `SW_RESTORE`：最小化的窗口同时恢复
    pub const SW_RESTORE: i32 = 9;
}

/// 尽力把窗口放到最前面。失败也不影响主流程，所以不返回错误。
#[cfg(windows)]
pub fn force_foreground(hwnd: isize) {
    unsafe {
        let h = hwnd as *mut std::ffi::c_void;
        if h.is_null() || win::IsWindow(h) == 0 {
            return;
        }
        if win::IsIconic(h) != 0 {
            win::ShowWindow(h, win::SW_RESTORE);
        }

        let current = win::GetCurrentThreadId();
        let foreground = win::GetForegroundWindow();
        let foreground_thread = if foreground.is_null() {
            0
        } else {
            win::GetWindowThreadProcessId(foreground, std::ptr::null_mut())
        };

        let attached = foreground_thread != 0
            && foreground_thread != current
            && win::AttachThreadInput(current, foreground_thread, 1) != 0;

        win::BringWindowToTop(h);
        win::SetForegroundWindow(h);
        win::SetFocus(h);

        if attached {
            win::AttachThreadInput(current, foreground_thread, 0);
        }
    }
}

#[cfg(not(windows))]
pub fn force_foreground(_hwnd: isize) {}
