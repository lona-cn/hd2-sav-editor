use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest as _, Sha256};
use zip::ZipArchive;

use crate::UpdateError;

pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 16;
const MAX_VERSION_BYTES: u64 = 4096;

pub const MANAGED_FILES: [&str; 6] = [
    "hd2-armor-desk.exe",
    "hd2-armor-desk-updater.exe",
    "README.md",
    "LICENSE",
    "docs/images/armor-desk-overview.png",
    "VERSION.txt",
];

const ALLOWED_DIRECTORIES: [&str; 2] = ["docs/", "docs/images/"];

pub fn release_version(tag: &str) -> Result<&str, UpdateError> {
    let version = tag
        .strip_prefix("release-")
        .ok_or_else(|| UpdateError::InvalidPackage("版本标签格式不正确".to_string()))?;
    let bytes = version.as_bytes();
    if bytes.len() != 23
        || bytes[8] != b'-'
        || &bytes[18..] != b"-UTC8"
        || !bytes[..8].iter().all(u8::is_ascii_digit)
        || !bytes[9..18].iter().all(u8::is_ascii_digit)
    {
        return Err(UpdateError::InvalidPackage(
            "版本标签格式不正确".to_string(),
        ));
    }
    Ok(version)
}

pub fn sha256_file(path: &Path) -> Result<String, UpdateError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn extract_release_archive(
    archive_path: &Path,
    destination: &Path,
    release_tag: &str,
) -> Result<(), UpdateError> {
    let mut stage_created = false;
    let result =
        extract_release_archive_inner(archive_path, destination, release_tag, &mut stage_created);
    if result.is_err() && stage_created {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

fn extract_release_archive_inner(
    archive_path: &Path,
    destination: &Path,
    release_tag: &str,
    stage_created: &mut bool,
) -> Result<(), UpdateError> {
    let expected_version = release_version(release_tag)?;
    let metadata = fs::metadata(archive_path)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_ARCHIVE_BYTES {
        return Err(UpdateError::InvalidPackage(
            "发布包大小异常，已拒绝解包".to_string(),
        ));
    }
    if destination.exists() {
        return Err(UpdateError::InvalidPackage(
            "更新暂存目录已存在，拒绝覆盖".to_string(),
        ));
    }

    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(UpdateError::InvalidPackage(
            "发布包文件数量超出限制".to_string(),
        ));
    }

    let mut seen = HashSet::with_capacity(MANAGED_FILES.len());
    let mut unpacked_bytes = 0_u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name();
        if entry.is_symlink() {
            return Err(UpdateError::InvalidPackage(format!(
                "发布包包含不允许的符号链接：{name}"
            )));
        }
        if entry.is_dir() {
            if !ALLOWED_DIRECTORIES.contains(&name) {
                return Err(UpdateError::InvalidPackage(format!(
                    "发布包包含不允许的目录：{name}"
                )));
            }
            continue;
        }
        if !MANAGED_FILES.contains(&name) {
            return Err(UpdateError::InvalidPackage(format!(
                "发布包包含不允许的文件路径：{name}"
            )));
        }
        if !seen.insert(name.to_string()) {
            return Err(UpdateError::InvalidPackage(format!(
                "发布包包含重复文件：{name}"
            )));
        }
        if entry.size() > MAX_UNPACKED_BYTES {
            return Err(UpdateError::InvalidPackage(format!(
                "发布包文件超出解压大小限制：{name}"
            )));
        }
        unpacked_bytes = unpacked_bytes
            .checked_add(entry.size())
            .ok_or_else(|| UpdateError::InvalidPackage("解压大小溢出".to_string()))?;
        if unpacked_bytes > MAX_UNPACKED_BYTES {
            return Err(UpdateError::InvalidPackage(
                "发布包总解压大小超出限制".to_string(),
            ));
        }
    }
    for required in MANAGED_FILES {
        if !seen.contains(required) {
            return Err(UpdateError::InvalidPackage(format!(
                "发布包缺少必需文件：{required}"
            )));
        }
    }

    let mut version_file = archive.by_name("VERSION.txt")?;
    if version_file.size() > MAX_VERSION_BYTES {
        return Err(UpdateError::InvalidPackage("版本信息文件异常".to_string()));
    }
    let mut version_text = String::new();
    version_file.read_to_string(&mut version_text)?;
    if !version_text
        .lines()
        .any(|line| line == format!("Version: {expected_version}"))
    {
        return Err(UpdateError::InvalidPackage(
            "发布包版本与 GitHub Release 标签不一致".to_string(),
        ));
    }
    drop(version_file);

    fs::create_dir(destination)?;
    *stage_created = true;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let target = destination.join(entry.name());
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        let mut copied = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            copied = copied
                .checked_add(count as u64)
                .ok_or_else(|| UpdateError::InvalidPackage("解压大小溢出".to_string()))?;
            if copied > entry.size() || copied > MAX_UNPACKED_BYTES {
                return Err(UpdateError::InvalidPackage(format!(
                    "解压文件超过声明大小：{}",
                    entry.name()
                )));
            }
            output.write_all(&buffer[..count])?;
        }
        if copied != entry.size() {
            return Err(UpdateError::InvalidPackage(format!(
                "解压文件长度不匹配：{}",
                entry.name()
            )));
        }
        output.sync_all()?;
    }
    Ok(())
}

pub fn extract_updater_binary(
    archive_path: &Path,
    destination: &Path,
    release_tag: &str,
) -> Result<(), UpdateError> {
    extract_release_archive(archive_path, destination, release_tag)
}

pub(crate) fn ensure_safe_child(root: &Path, child: &Path) -> Result<(), UpdateError> {
    let root = root.canonicalize()?;
    let child = child.canonicalize()?;
    if !child.starts_with(&root) || child == root {
        return Err(UpdateError::InvalidPackage(
            "更新路径超出程序目录".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn hash_and_sync(path: &Path) -> Result<String, UpdateError> {
    let hash = sha256_file(path)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()?;
    Ok(hash)
}

pub fn validate_digest(digest: &str) -> Result<(), UpdateError> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(UpdateError::InvalidPackage(
            "更新包 SHA-256 格式无效".to_string(),
        ));
    }
    Ok(())
}
