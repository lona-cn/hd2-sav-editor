use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::archive::{ensure_safe_child, hash_and_sync, sha256_file, MANAGED_FILES};
use crate::UpdateError;

const UPDATE_ROOT: &str = ".hd2-update";
const OWNER_FILE: &str = "owner";
const OWNER_CONTENTS: &[u8] = b"hd2-sav-editor-update-staging-v1";
const JOURNAL_FILE: &str = "install.json";
const COMMITTED_FILE: &str = "committed";
const ROLLED_BACK_FILE: &str = "rolled-back";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInstallReceipt {
    pub release_tag: String,
    pub installed_files: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    release_tag: String,
    files: Vec<JournalFile>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalFile {
    relative_path: String,
    had_original: bool,
    backup_sha256: Option<String>,
    installed_sha256: String,
}

pub struct InstallLock {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.handle);
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

pub fn acquire_install_lock(install_dir: &Path) -> Result<InstallLock, UpdateError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject, INFINITE};

        let canonical = install_dir.canonicalize()?;
        let lock_id = Sha256Path::digest(canonical.as_os_str().encode_wide());
        let name: Vec<u16> = format!("Local\\HD2ArmorDeskUpdate-{lock_id}")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(UpdateError::Install(format!(
                "无法创建更新互斥锁：{}",
                io::Error::last_os_error()
            )));
        }
        let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            unsafe { CloseHandle(handle) };
            return Err(UpdateError::Install(format!(
                "等待更新互斥锁失败：{}",
                io::Error::last_os_error()
            )));
        }
        Ok(InstallLock { handle })
    }
    #[cfg(not(windows))]
    {
        let _ = install_dir;
        Ok(InstallLock {})
    }
}

pub fn create_update_transaction_dir(
    install_dir: &Path,
    transaction_id: &str,
) -> Result<PathBuf, UpdateError> {
    if transaction_id.is_empty()
        || transaction_id.len() > 80
        || !transaction_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(UpdateError::Install("更新事务编号无效".to_string()));
    }
    let install_dir = install_dir.canonicalize()?;
    let update_root = ensure_update_root(&install_dir, true)?;
    let transaction_dir = update_root.join(transaction_id);
    fs::create_dir(&transaction_dir)
        .map_err(|error| UpdateError::Install(format!("无法创建更新暂存目录：{error}")))?;
    Ok(transaction_dir)
}

pub fn validate_update_transaction_dir(
    install_dir: &Path,
    transaction_dir: &Path,
) -> Result<PathBuf, UpdateError> {
    let install_dir = install_dir.canonicalize()?;
    let update_root = ensure_update_root(&install_dir, false)?;
    let transaction_dir = transaction_dir.canonicalize()?;
    ensure_safe_child(&update_root, &transaction_dir)?;
    if transaction_dir.parent() != Some(update_root.as_path()) {
        return Err(UpdateError::Install("更新事务目录层级无效".to_string()));
    }
    Ok(transaction_dir)
}

pub fn cleanup_update_staging(install_dir: &Path) -> Result<(), UpdateError> {
    let install_dir = install_dir.canonicalize()?;
    let update_root_path = install_dir.join(UPDATE_ROOT);
    match fs::symlink_metadata(&update_root_path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(UpdateError::Install("更新暂存路径不是普通目录".to_string()))
        }
        Ok(_) => {}
    }
    let update_root = ensure_update_root(&install_dir, false)?;
    for entry in fs::read_dir(&update_root)? {
        let entry = entry?;
        if entry.file_name() == OWNER_FILE {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(UpdateError::Install(
                "更新暂存中包含重解析点，拒绝清理".to_string(),
            ));
        }
        if metadata.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

pub fn write_update_health_marker(
    install_dir: &Path,
    marker: &Path,
    release_tag: &str,
) -> Result<(), UpdateError> {
    crate::archive::release_version(release_tag)?;
    let transaction_dir = marker
        .parent()
        .ok_or_else(|| UpdateError::Install("更新启动标记路径无效".to_string()))?;
    let transaction_dir = validate_update_transaction_dir(install_dir, transaction_dir)?;
    if marker.file_name().and_then(|name| name.to_str()) != Some("health") {
        return Err(UpdateError::Install("更新启动标记路径无效".to_string()));
    }
    let journal = read_journal(&transaction_dir)?;
    if journal.release_tag != release_tag
        || marker_is_valid(&transaction_dir.join(COMMITTED_FILE))?
        || marker_is_valid(&transaction_dir.join(ROLLED_BACK_FILE))?
    {
        return Err(UpdateError::Install(
            "更新启动标记与安装事务不匹配".to_string(),
        ));
    }
    write_new_contents(marker, release_tag.as_bytes())
}

fn ensure_update_root(install_dir: &Path, create: bool) -> Result<PathBuf, UpdateError> {
    let update_root = install_dir.join(UPDATE_ROOT);
    match fs::symlink_metadata(&update_root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(UpdateError::Install("更新暂存路径不是普通目录".to_string())),
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            fs::create_dir(&update_root)?;
            if let Err(error) = write_new_contents(&update_root.join(OWNER_FILE), OWNER_CONTENTS) {
                let _ = fs::remove_dir(&update_root);
                return Err(error);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(UpdateError::Install("更新暂存目录不存在".to_string()))
        }
        Err(error) => return Err(error.into()),
    }
    let owner = update_root.join(OWNER_FILE);
    let owner_metadata = fs::symlink_metadata(&owner)?;
    if !owner_metadata.is_file()
        || owner_metadata.file_type().is_symlink()
        || owner_metadata.len() != OWNER_CONTENTS.len() as u64
        || fs::read(&owner)? != OWNER_CONTENTS
    {
        return Err(UpdateError::Install(
            "更新暂存目录所有权标记无效".to_string(),
        ));
    }
    update_root.canonicalize().map_err(Into::into)
}

pub fn recover_incomplete_updates(install_dir: &Path) -> Result<usize, UpdateError> {
    let install_dir = install_dir.canonicalize()?;
    let update_root_path = install_dir.join(UPDATE_ROOT);
    match fs::symlink_metadata(&update_root_path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(UpdateError::Install("更新暂存路径不是普通目录".to_string()))
        }
        Ok(_) => {}
    }
    let update_root = ensure_update_root(&install_dir, false)?;
    let mut recovered = 0;
    for entry in fs::read_dir(&update_root)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let transaction_dir = entry.path();
        let journal_path = transaction_dir.join(JOURNAL_FILE);
        let journal_metadata = match fs::symlink_metadata(&journal_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !journal_metadata.is_file() || journal_metadata.file_type().is_symlink() {
            return Err(UpdateError::Install("更新事务记录无效".to_string()));
        }
        let committed = marker_is_valid(&transaction_dir.join(COMMITTED_FILE))?;
        let rolled_back = marker_is_valid(&transaction_dir.join(ROLLED_BACK_FILE))?;
        if committed && rolled_back {
            return Err(UpdateError::Install("更新事务状态冲突".to_string()));
        }
        if committed || rolled_back {
            continue;
        }
        ensure_safe_child(&update_root, &transaction_dir)?;
        rollback_install(&install_dir, &transaction_dir)?;
        recovered += 1;
    }
    Ok(recovered)
}

struct Sha256Path;

impl Sha256Path {
    #[cfg(windows)]
    fn digest(wide: impl IntoIterator<Item = u16>) -> String {
        use sha2::{Digest as _, Sha256};
        let mut hasher = Sha256::new();
        for unit in wide {
            hasher.update(unit.to_le_bytes());
        }
        format!("{:x}", hasher.finalize())
    }
}

pub fn apply_staged_release(
    install_dir: &Path,
    staged_dir: &Path,
    transaction_dir: &Path,
    release_tag: &str,
) -> Result<UpdateInstallReceipt, UpdateError> {
    let install_dir = install_dir.canonicalize()?;
    let transaction_dir = validate_update_transaction_dir(&install_dir, transaction_dir)?;
    ensure_safe_child(&transaction_dir, staged_dir)?;
    crate::archive::release_version(release_tag)?;

    let mut journal = Journal {
        release_tag: release_tag.to_string(),
        files: Vec::with_capacity(MANAGED_FILES.len()),
    };
    let backup_root = transaction_dir.join("backup");
    fs::create_dir_all(&backup_root)?;

    for relative_path in MANAGED_FILES {
        let source = staged_dir.join(relative_path);
        let source_meta = fs::symlink_metadata(&source).map_err(|error| {
            UpdateError::InvalidPackage(format!("发布包文件缺失或不可读 {relative_path}：{error}"))
        })?;
        if !source_meta.is_file() || source_meta.file_type().is_symlink() {
            return Err(UpdateError::InvalidPackage(format!(
                "发布包文件类型异常：{relative_path}"
            )));
        }
        let source_sha256 = sha256_file(&source)?;
        let target = install_dir.join(relative_path);
        validate_target_path(&install_dir, &target)?;
        let target_state = match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => true,
            Ok(_) => {
                return Err(UpdateError::Install(format!(
                    "安装目标不是普通文件：{relative_path}"
                )))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        let backup_sha256 = if target_state {
            let backup = backup_root.join(relative_path);
            if let Some(parent) = backup.parent() {
                fs::create_dir_all(parent)?;
            }
            let original_sha256 = sha256_file(&target)?;
            fs::copy(&target, &backup)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&backup)?
                .sync_all()?;
            if sha256_file(&backup)? != original_sha256 {
                return Err(UpdateError::Install(format!(
                    "旧文件备份校验失败：{relative_path}"
                )));
            }
            Some(original_sha256)
        } else {
            None
        };
        journal.files.push(JournalFile {
            relative_path: relative_path.to_string(),
            had_original: target_state,
            backup_sha256,
            installed_sha256: source_sha256,
        });
    }

    let journal_path = transaction_dir.join(JOURNAL_FILE);
    write_new_journal(&journal_path, &journal)?;

    let install_result = install_files(&install_dir, staged_dir, &journal);
    if let Err(error) = install_result {
        let rollback_result = rollback(&install_dir, &transaction_dir, &journal);
        if let Err(rollback_error) = rollback_result {
            return Err(UpdateError::Install(format!(
                "更新失败：{error}；自动回滚也失败：{rollback_error}。旧文件和更新暂存区已保留"
            )));
        }
        write_marker(&transaction_dir.join(ROLLED_BACK_FILE))?;
        return Err(UpdateError::Install(format!(
            "更新未完成，已恢复旧版本：{error}"
        )));
    }

    Ok(UpdateInstallReceipt {
        release_tag: release_tag.to_string(),
        installed_files: journal.files.len(),
    })
}

pub fn commit_install(transaction_dir: &Path) -> Result<(), UpdateError> {
    let _journal = read_journal(transaction_dir)?;
    if marker_is_valid(&transaction_dir.join(ROLLED_BACK_FILE))? {
        return Err(UpdateError::Install("无法提交已回滚的更新".to_string()));
    }
    write_marker(&transaction_dir.join(COMMITTED_FILE))
}

pub fn rollback_install(install_dir: &Path, transaction_dir: &Path) -> Result<(), UpdateError> {
    if marker_is_valid(&transaction_dir.join(COMMITTED_FILE))? {
        return Err(UpdateError::Install("无法回滚已提交的更新".to_string()));
    }
    let journal = read_journal(transaction_dir)?;
    rollback(install_dir, transaction_dir, &journal)?;
    write_marker(&transaction_dir.join(ROLLED_BACK_FILE))
}

fn install_files(
    install_dir: &Path,
    staged_dir: &Path,
    journal: &Journal,
) -> Result<(), UpdateError> {
    for file in &journal.files {
        let relative = Path::new(&file.relative_path);
        let source = staged_dir.join(relative);
        let target = install_dir.join(relative);
        validate_target_path(install_dir, &target)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        replace_file(&source, &target)?;
        let installed_hash = hash_and_sync(&target)?;
        if installed_hash != file.installed_sha256 {
            return Err(UpdateError::Install(format!(
                "安装后文件校验失败：{}",
                file.relative_path
            )));
        }
    }
    Ok(())
}

fn rollback(
    install_dir: &Path,
    transaction_dir: &Path,
    journal: &Journal,
) -> Result<(), UpdateError> {
    let backup_root = transaction_dir.join("backup");
    let mut errors = Vec::new();
    for file in journal.files.iter().rev() {
        let relative = Path::new(&file.relative_path);
        let target = install_dir.join(relative);
        if let Err(error) = validate_target_path(install_dir, &target) {
            errors.push(format!("{}：{error}", file.relative_path));
            continue;
        }
        if file.had_original {
            let backup = backup_root.join(relative);
            let backup_metadata = match fs::symlink_metadata(&backup) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    metadata
                }
                _ => {
                    errors.push(format!("缺少有效回滚备份：{}", file.relative_path));
                    continue;
                }
            };
            let backup_hash = match sha256_file(&backup) {
                Ok(hash) => hash,
                Err(error) => {
                    errors.push(format!("读取回滚备份失败 {}：{error}", file.relative_path));
                    continue;
                }
            };
            if backup_metadata.len() > 128 * 1024 * 1024
                || file.backup_sha256.as_deref() != Some(backup_hash.as_str())
            {
                errors.push(format!("回滚备份校验失败：{}", file.relative_path));
                continue;
            }
            if let Some(parent) = target.parent() {
                if let Err(error) = fs::create_dir_all(parent) {
                    errors.push(format!("{}：{error}", file.relative_path));
                    continue;
                }
            }
            let temporary_name = file.relative_path.replace('/', "-");
            let restore = transaction_dir.join(format!("restore-{temporary_name}.tmp"));
            if let Err(error) = fs::copy(&backup, &restore)
                .and_then(|_| {
                    OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&restore)?
                        .sync_all()
                })
                .and_then(|_| replace_file(&restore, &target))
            {
                errors.push(format!("{}：{error}", file.relative_path));
            }
        } else if let Err(error) = remove_if_exists(&target) {
            errors.push(format!("{}：{error}", file.relative_path));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(UpdateError::Install(errors.join("；")))
    }
}

fn validate_target_path(install_dir: &Path, target: &Path) -> Result<(), UpdateError> {
    let relative = target
        .strip_prefix(install_dir)
        .map_err(|_| UpdateError::Install("安装目标超出程序目录".to_string()))?;
    let mut cursor = install_dir.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        cursor.push(component.as_os_str());
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(UpdateError::Install(format!(
                    "安装目录包含重解析点或非目录：{}",
                    cursor.display()
                )))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn write_new_journal(path: &Path, journal: &Journal) -> Result<(), UpdateError> {
    let bytes = serde_json::to_vec(journal)?;
    let temporary = path.with_file_name(format!("{JOURNAL_FILE}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    Ok(())
}

fn read_journal(transaction_dir: &Path) -> Result<Journal, UpdateError> {
    let path = transaction_dir.join(JOURNAL_FILE);
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 32 * 1024 {
        return Err(UpdateError::Install("更新事务记录无效".to_string()));
    }
    let journal: Journal = serde_json::from_slice(&fs::read(path)?)?;
    let mut seen = HashSet::with_capacity(journal.files.len());
    if crate::archive::release_version(&journal.release_tag).is_err()
        || journal.files.len() != MANAGED_FILES.len()
        || journal.files.iter().any(|file| {
            !MANAGED_FILES.contains(&file.relative_path.as_str())
                || !seen.insert(file.relative_path.as_str())
                || file.had_original != file.backup_sha256.is_some()
        })
    {
        return Err(UpdateError::Install("更新事务记录文件清单无效".to_string()));
    }
    Ok(journal)
}

fn write_marker(path: &Path) -> Result<(), UpdateError> {
    write_new_contents(path, b"ok")
}

fn write_new_contents(path: &Path, contents: &[u8]) -> Result<(), UpdateError> {
    let temporary = path.with_file_name(format!(
        "{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("marker")
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    Ok(())
}

fn marker_is_valid(path: &Path) -> Result<bool, UpdateError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 128 {
        return Err(UpdateError::Install("更新事务标记无效".to_string()));
    }
    Ok(fs::read(path)? == b"ok")
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let source_wide = wide(source);
    let target_wide = wide(target);
    let result = if target.exists() {
        unsafe {
            ReplaceFileW(
                target_wide.as_ptr(),
                source_wide.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        }
    } else {
        unsafe {
            MoveFileExW(
                source_wide.as_ptr(),
                target_wide.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    if target.exists() {
        fs::remove_file(target)?;
    }
    fs::rename(source, target)
}
