//! Process-wide single-instance guard for the desktop application.

use std::fmt;

const INSTANCE_MUTEX_NAME: &str =
    "Local\\lona-cn.hd2-sav-editor.7B465F3B-FF4E-45B2-AE67-E891BB832A64";

#[derive(Debug)]
pub enum AcquireError {
    AlreadyRunning,
    Os(std::io::Error),
}

impl fmt::Display for AcquireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AcquireError::AlreadyRunning => formatter.write_str("应用已在运行"),
            AcquireError::Os(error) => write!(formatter, "无法创建应用实例锁：{error}"),
        }
    }
}

impl std::error::Error for AcquireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AcquireError::AlreadyRunning => None,
            AcquireError::Os(error) => Some(error),
        }
    }
}

pub struct SingleInstanceGuard {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl SingleInstanceGuard {
    pub fn acquire() -> Result<Self, AcquireError> {
        acquire_named(INSTANCE_MUTEX_NAME)
    }
}

#[cfg(windows)]
fn acquire_named(name: &str) -> Result<SingleInstanceGuard, AcquireError> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let wide_name: Vec<u16> = std::ffi::OsStr::new(name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide_name` is NUL-terminated and remains alive for the call.
    // A null security descriptor requests the current process's default ACL.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide_name.as_ptr()) };
    if handle.is_null() {
        return Err(AcquireError::Os(std::io::Error::last_os_error()));
    }
    // `GetLastError` must be read immediately after CreateMutexW: a valid
    // handle plus ERROR_ALREADY_EXISTS means another process won the race.
    let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_running {
        // SAFETY: `handle` was returned by CreateMutexW and is owned here.
        unsafe {
            CloseHandle(handle);
        }
        return Err(AcquireError::AlreadyRunning);
    }
    Ok(SingleInstanceGuard { handle })
}

#[cfg(not(windows))]
fn acquire_named(_name: &str) -> Result<SingleInstanceGuard, AcquireError> {
    Ok(SingleInstanceGuard {})
}

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        // SAFETY: the guard uniquely owns this live CreateMutexW handle.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

pub fn notify_startup_blocked(error: &AcquireError) {
    let message = match error {
        AcquireError::AlreadyRunning => {
            "Armor Desk 已在运行。\n\n请使用现有窗口，避免重复占用系统资源。".to_string()
        }
        AcquireError::Os(error) => {
            format!("无法确认 Armor Desk 是否已在运行。为避免多开，本次启动已取消。\n\n{error}")
        }
    };
    show_message(&message);
}

#[cfg(windows)]
fn show_message(message: &str) {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND,
    };

    let text: Vec<u16> = std::ffi::OsStr::new(message)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let caption: Vec<u16> = std::ffi::OsStr::new("Armor Desk")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: both strings are NUL-terminated and live for the duration of the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(windows))]
fn show_message(message: &str) {
    eprintln!("{message}");
}
#[cfg(all(test, windows))]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::{acquire_named, AcquireError};

    static NAME_SEQUENCE: AtomicU32 = AtomicU32::new(0);

    #[test]
    fn named_guard_blocks_a_second_holder_and_releases_on_drop() {
        let sequence = NAME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "Local\\lona-cn.hd2-sav-editor.test.{}.{}",
            std::process::id(),
            sequence
        );

        let first = acquire_named(&name).expect("first process should acquire the named guard");
        assert!(
            matches!(acquire_named(&name), Err(AcquireError::AlreadyRunning)),
            "a second holder must be rejected while the first guard is alive"
        );

        drop(first);
        let _next = acquire_named(&name).expect("dropping the guard should release the name");
    }
}
