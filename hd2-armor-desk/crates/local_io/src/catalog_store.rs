//! Atomic workspace stores: catalog, presets, settings, backups, receipts.
//!
//! Every write goes through a temporary file in the same directory followed by a
//! rename, so a crash mid-write cannot leave a truncated document. Nothing here
//! ever touches the game's save file.

use std::path::{Path, PathBuf};

use loadout_domain::{Catalog, PresetStore, MAX_IMPORT_BYTES};

use crate::monitor::{iso_stamp, read_bounded};

const MAX_PRESET_BYTES: usize = 4 * 1024 * 1024;
const MAX_SETTINGS_BYTES: usize = 1024 * 1024;

/// Failure while reading or writing a workspace document.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum StoreError {
    #[error("工作区路径不可用：{0}")]
    Path(String),
    #[error("读取失败：{0}")]
    Read(String),
    #[error("写入失败：{0}")]
    Write(String),
    #[error("内容无法解析：{0}")]
    Parse(String),
    #[error("拒绝用空表覆盖已有目录：{0}")]
    RefuseToClobber(String),
    #[error("已有文件无法读取，迁移中止：{0}")]
    MigrationBlocked(String),
}

/// A workspace directory holding catalogs, presets, backups and logs.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

fn default_root_for_executable(executable: &Path) -> PathBuf {
    executable
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("workspace")
}

impl Workspace {
    /// Open (creating if needed) a workspace rooted at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Workspace, StoreError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|error| {
            StoreError::Path(format!("无法创建工作区 {}：{error}", root.display()))
        })?;
        Ok(Workspace { root })
    }

    /// Default portable workspace: `<executable directory>\workspace`.
    ///
    /// `HD2_ARMOR_DESK_WORKSPACE` is an explicit override used by isolated
    /// tests and portable launchers; normal application runs use the executable
    /// location and never write to `%LOCALAPPDATA%`.
    pub fn default_root() -> PathBuf {
        if let Some(root) = std::env::var_os("HD2_ARMOR_DESK_WORKSPACE") {
            return PathBuf::from(root);
        }
        std::env::current_exe()
            .map(|path| default_root_for_executable(&path))
            .unwrap_or_else(|_| PathBuf::from("workspace"))
    }

    /// Workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of the v2 catalog document.
    pub fn catalog_path(&self) -> PathBuf {
        self.root.join("catalog.v2.json")
    }

    /// Path of the preset document.
    pub fn presets_path(&self) -> PathBuf {
        self.root.join("presets.json")
    }

    /// Path of the settings document.
    pub fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// Directory holding backups of real saves.
    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// Directory holding commit receipts.
    pub fn receipts_dir(&self) -> PathBuf {
        self.root.join("receipts")
    }

    /// Directory holding copies of imported source catalogs, untouched.
    pub fn imports_dir(&self) -> PathBuf {
        self.root.join("catalog.imports")
    }

    /// Directory for local logs.
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Load the catalog, or an empty catalog when the file does not exist.
    pub fn load_catalog(&self) -> Result<Catalog, StoreError> {
        let path = self.catalog_path();
        if !path.is_file() {
            return Ok(Catalog::default());
        }
        let bytes = read_bounded(&path, MAX_IMPORT_BYTES).map_err(StoreError::Read)?;
        let text = String::from_utf8(bytes)
            .map_err(|error| StoreError::Parse(format!("目录不是 UTF-8：{error}")))?;
        Catalog::from_document_json(&text).map_err(StoreError::Parse)
    }

    /// Save the catalog atomically.
    pub fn save_catalog(&self, catalog: &Catalog) -> Result<(), StoreError> {
        if catalog.is_empty() && self.catalog_path().is_file() {
            // Refuse to replace a non-empty catalog with an empty one.
            let existing = self.load_catalog()?;
            if !existing.is_empty() {
                return Err(StoreError::RefuseToClobber(
                    self.catalog_path().display().to_string(),
                ));
            }
        }
        let json = catalog
            .to_document_json(&iso_stamp())
            .map_err(StoreError::Parse)?;
        atomic_write(&self.catalog_path(), json.as_bytes())
    }

    /// Load presets, or an empty store.
    pub fn load_presets(&self) -> Result<PresetStore, StoreError> {
        let path = self.presets_path();
        if !path.is_file() {
            return Ok(PresetStore::default());
        }
        let bytes = read_bounded(&path, MAX_PRESET_BYTES).map_err(StoreError::Read)?;
        let text = String::from_utf8(bytes)
            .map_err(|error| StoreError::Parse(format!("预设不是 UTF-8：{error}")))?;
        PresetStore::from_json(&text).map_err(StoreError::Parse)
    }

    /// Save presets atomically.
    pub fn save_presets(&self, presets: &PresetStore) -> Result<(), StoreError> {
        let json = presets.to_json().map_err(StoreError::Parse)?;
        atomic_write(&self.presets_path(), json.as_bytes())
    }

    /// Copy an imported source document into the workspace for auditing.
    ///
    /// The user's original file is only read, never modified.
    pub fn archive_import(
        &self,
        source_name: &str,
        bytes: &[u8],
        stamp: &str,
    ) -> Result<PathBuf, StoreError> {
        std::fs::create_dir_all(self.imports_dir())
            .map_err(|error| StoreError::Write(error.to_string()))?;
        let safe = sanitize_file_name(source_name);
        let target = self.imports_dir().join(format!("{stamp}_{safe}"));
        atomic_write(&target, bytes)?;
        Ok(target)
    }

    /// Read a settings value.
    pub fn setting(&self, key: &str) -> Option<String> {
        let bytes = read_bounded(&self.settings_path(), MAX_SETTINGS_BYTES).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        value.get(key)?.as_str().map(str::to_string)
    }

    /// Write a settings value.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let mut map: serde_json::Map<String, serde_json::Value> =
            read_bounded(&self.settings_path(), MAX_SETTINGS_BYTES)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
        map.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
        let json = serde_json::to_string_pretty(&serde_json::Value::Object(map))
            .map_err(|error| StoreError::Parse(error.to_string()))?;
        atomic_write(&self.settings_path(), json.as_bytes())
    }

    /// List existing backups, newest first.
    pub fn list_backups(&self) -> Vec<BackupEntry> {
        let mut entries = Vec::new();
        let Ok(read_dir) = std::fs::read_dir(self.backups_dir()) else {
            return entries;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            entries.push(BackupEntry {
                path: path.clone(),
                file_name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default(),
                size: metadata.len(),
                modified: metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0),
            });
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.modified));
        entries
    }
}

/// One backup file on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct BackupEntry {
    pub path: PathBuf,
    pub file_name: String,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: u64,
}

/// Create a unique sibling without ever opening an existing path for writing.
pub(crate) fn create_unique_sibling(
    parent: &Path,
    stem: &str,
    suffix: &str,
) -> std::io::Result<(PathBuf, std::fs::File)> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let epoch_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for _ in 0..128 {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{stem}.{}.{epoch_nanos:x}.{id:x}{suffix}",
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "无法创建唯一临时文件",
    ))
}

/// Write bytes atomically: temp file in the same directory, sync, then rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::Path(format!("{} 没有父目录", path.display())))?;
    std::fs::create_dir_all(parent).map_err(|error| StoreError::Write(error.to_string()))?;

    let stem = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "doc".into());
    let (temp, mut file) = create_unique_sibling(parent, &stem, ".tmp")
        .map_err(|error| StoreError::Write(error.to_string()))?;
    let write_result = (|| -> std::io::Result<()> {
        use std::io::Write;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(error) = write_result {
        drop(file);
        let _ = std::fs::remove_file(&temp);
        return Err(StoreError::Write(format!("{}：{error}", temp.display())));
    }
    drop(file);

    // Keep a readable copy of the previous version rather than losing it.
    if path.is_file() {
        let previous = path.with_extension("previous");
        let _ = std::fs::copy(path, &previous);
    }

    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        StoreError::Write(format!("替换 {} 失败：{error}", path.display()))
    })
}

/// Make an arbitrary string safe to use as a file name.
pub fn sanitize_file_name(name: &str) -> String {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let mut out: String = name
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            ch if (ch as u32) < 0x20 => '_',
            ch => ch,
        })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        out = "export".into();
    }
    let stem = out
        .split('.')
        .next()
        .unwrap_or("export")
        .to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        out = format!("_{out}");
    }
    // Bound the length while keeping the extension.
    if out.chars().count() > 120 {
        out = out.chars().take(120).collect();
    }
    out
}

/// Build a default export file name for a loadout copy.
pub fn default_export_name(preset_name: Option<&str>, stamp: &str) -> String {
    let base = preset_name
        .map(sanitize_file_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "双甲配装".to_string());
    sanitize_file_name(&format!("{base}_{stamp}.sav"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_workspace_is_contained_beside_the_executable() {
        let executable = PathBuf::from("portable").join("hd2-armor-desk.exe");
        assert_eq!(
            default_root_for_executable(&executable),
            PathBuf::from("portable").join("workspace")
        );
    }
}
