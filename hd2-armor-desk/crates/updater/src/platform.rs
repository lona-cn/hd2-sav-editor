use std::ffi::OsStr;

use crate::UpdateError;

pub fn show_update_failure(message: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONERROR, MB_YESNO};

    let text = format!("{message}\n\n是否打开 GitHub Releases 页面手动下载？");
    let text: Vec<u16> = OsStr::new(&text).encode_wide().chain(Some(0)).collect();
    let title: Vec<u16> = OsStr::new("HD2 Armor Desk 更新失败")
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONERROR,
        )
    };
    if result == IDYES {
        let _ = open::that_detached(crate::RELEASES_PAGE_URL);
    }
}

pub struct ParentProcessHandle(windows_sys::Win32::Foundation::HANDLE);

impl Drop for ParentProcessHandle {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

pub fn wait_for_parent_process(pid: u32) -> Result<ParentProcessHandle, UpdateError> {
    use windows_sys::Win32::System::Threading::OpenProcess;
    const PROCESS_SYNCHRONIZE: u32 = 0x0010_0000;
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return Err(UpdateError::Install(format!(
            "无法等待主程序退出：{}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(ParentProcessHandle(handle))
}

pub fn wait_for_process(handle: ParentProcessHandle) -> Result<(), UpdateError> {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};
    let result = unsafe { WaitForSingleObject(handle.0, INFINITE) };
    if result == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err(UpdateError::Install(format!(
            "等待主程序退出失败：{}",
            std::io::Error::last_os_error()
        )))
    }
}
