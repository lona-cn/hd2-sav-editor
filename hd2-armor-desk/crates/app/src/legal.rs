//! First-run legal-risk acknowledgement persisted per Windows user.

use std::io;

pub const NOTICE_TITLE: &str = "使用前必须了解：法律与数据风险";
pub const NOTICE_INTRO: &str = "本项目是非官方同人工具，与 Arrowhead Game Studios、Sony Interactive Entertainment、Valve 或 HELLDIVERS 2 的其他权利方没有隶属、授权或背书关系。";
pub const NOTICE_SAVE_RISK: &str = "修改游戏存档可能违反游戏最终用户许可协议、服务条款、平台规则或你所在地区的法律，并可能导致存档损坏、云存档冲突、游戏异常、功能失效、账号限制或封禁。你有责任在使用前自行确认适用规则并保留可靠备份。";
pub const NOTICE_RESPONSIBILITY: &str = "使用本工具产生的法律风险、账号风险、数据损失及其他后果均由用户自行承担。本项目作者和贡献者不保证工具适合任何特定用途，也不对直接或间接损失负责。本段不是法律意见。";

const REGISTRY_SUBKEY: &str = "Software\\lona-cn\\HD2ArmorDesk";
const REGISTRY_VALUE: &str = "LegalRiskAcknowledgedVersion";
const ACKNOWLEDGEMENT_VERSION: u32 = 1;

/// Whether the current Windows user has acknowledged this notice version.
pub fn is_acknowledged() -> io::Result<bool> {
    read_acknowledged_from(REGISTRY_SUBKEY)
}

/// Persist acknowledgement for the current notice version.
pub fn acknowledge() -> io::Result<()> {
    write_acknowledged_to(REGISTRY_SUBKEY)
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;

    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn read_acknowledged_from(subkey: &str) -> io::Result<bool> {
    use windows_sys::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
    };
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let subkey = wide(subkey);
    let value_name = wide(REGISTRY_VALUE);
    let mut value = 0_u32;
    let mut value_size = std::mem::size_of::<u32>() as u32;
    // SAFETY: both strings are NUL-terminated. `value` is writable for
    // `value_size` bytes, and every pointer remains live for the call.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value_name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut value_size,
        )
    };
    match status {
        ERROR_SUCCESS => {
            Ok(value_size == std::mem::size_of::<u32>() as u32 && value == ACKNOWLEDGEMENT_VERSION)
        }
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(false),
        code => Err(io::Error::from_raw_os_error(code as i32)),
    }
}

#[cfg(windows)]
fn write_acknowledged_to(subkey: &str) -> io::Result<()> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD,
        REG_OPTION_NON_VOLATILE,
    };

    let subkey = wide(subkey);
    let value_name = wide(REGISTRY_VALUE);
    let mut key = std::ptr::null_mut();
    // SAFETY: `subkey` is NUL-terminated, output storage is valid, and the
    // default security descriptor is requested with a null pointer.
    let create_status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if create_status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(create_status as i32));
    }

    let version = ACKNOWLEDGEMENT_VERSION;
    // SAFETY: `key` is owned and live, `value_name` is NUL-terminated, and
    // `version` is readable for the supplied four-byte length.
    let write_status = unsafe {
        RegSetValueExW(
            key,
            value_name.as_ptr(),
            0,
            REG_DWORD,
            (&version as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    // SAFETY: `key` is the live handle returned by RegCreateKeyExW and is
    // closed exactly once here, regardless of the write result.
    let close_status = unsafe { RegCloseKey(key) };

    if write_status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(write_status as i32));
    }
    if close_status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(close_status as i32));
    }
    Ok(())
}

#[cfg(not(windows))]
fn read_acknowledged_from(_subkey: &str) -> io::Result<bool> {
    Ok(false)
}

#[cfg(not(windows))]
fn write_acknowledged_to(_subkey: &str) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;

    use windows_sys::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};

    use super::{read_acknowledged_from, wide, write_acknowledged_to};

    static KEY_SEQUENCE: AtomicU32 = AtomicU32::new(0);

    #[test]
    fn acknowledgement_round_trips_through_current_user_registry() {
        let sequence = KEY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let subkey = format!(
            "Software\\lona-cn-hd2-sav-editor-tests-{}-{sequence}",
            std::process::id()
        );
        assert!(!read_acknowledged_from(&subkey).unwrap());

        write_acknowledged_to(&subkey).unwrap();
        assert!(read_acknowledged_from(&subkey).unwrap());

        let subkey_wide = wide(&subkey);
        // SAFETY: the path is NUL-terminated and names only this test's unique key.
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, subkey_wide.as_ptr()) };
        assert_eq!(status, ERROR_SUCCESS);
    }
}
