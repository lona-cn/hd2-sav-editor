//! Stable read-only monitoring of the save file.
//!
//! The watcher never writes. Its job is to publish a *validated* snapshot only
//! after the file has stopped changing, and to attach a generation so a late
//! result from a previously opened document can never be applied to the current
//! one.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use loadout_domain::Snapshot;
use sav_codec::{DecodeError, SaveImage};
use sha2::{Digest, Sha256};

/// Application ceiling for a save file.
pub const MAX_SAVE_BYTES: u64 = 16 * 1024 * 1024;
/// How long a candidate revision must stay stable before it is accepted.
pub const SETTLE_WINDOW: Duration = Duration::from_millis(250);
/// Default polling interval.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Identity of a document. Incremented whenever a different path is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DocumentGeneration(pub u64);

/// A stable, normalized file identity. Not just the file name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathIdentity {
    /// Canonicalized absolute path.
    pub canonical: PathBuf,
    /// Volume serial and file index when the platform provides them.
    pub file_id: Option<(u64, u64)>,
}

impl PathIdentity {
    /// Build from a path, resolving symlinks and case where possible.
    pub fn from_path(path: &Path) -> Result<PathIdentity, String> {
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("无法解析路径：{error}"))?;
        let file_id = platform::file_identity(&canonical);
        Ok(PathIdentity { canonical, file_id })
    }

    /// Whether two identities refer to the same file.
    pub fn same_file(&self, other: &PathIdentity) -> bool {
        if let (Some(left), Some(right)) = (self.file_id, other.file_id) {
            return left == right;
        }
        self.canonical == other.canonical
    }
}

/// What a monitoring poll produced.
#[derive(Debug, Clone)]
pub enum MonitorEvent {
    /// A validated, stable snapshot. Carries its generation.
    Snapshot {
        generation: DocumentGeneration,
        identity: PathIdentity,
        snapshot: Box<Snapshot>,
        /// True when the raw bytes differ from the previously accepted revision.
        changed: bool,
    },
    /// The file is present but not yet stable (or not yet valid). Keep the last
    /// good snapshot and retry.
    Pending {
        generation: DocumentGeneration,
        reason: String,
        /// True when this looks like an in-progress game write.
        likely_write_in_progress: bool,
    },
    /// A hard read failure.
    ReadError {
        generation: DocumentGeneration,
        message: String,
    },
}

impl MonitorEvent {
    /// Generation this event belongs to.
    pub fn generation(&self) -> DocumentGeneration {
        match self {
            MonitorEvent::Snapshot { generation, .. } => *generation,
            MonitorEvent::Pending { generation, .. } => *generation,
            MonitorEvent::ReadError { generation, .. } => *generation,
        }
    }
}

/// Tracks the settle window for candidate revisions.
#[derive(Debug, Default)]
pub struct StabilityGate {
    candidate: Option<(String, Instant)>,
}

impl StabilityGate {
    /// Feed a candidate revision digest; returns true once it has been stable
    /// for at least `window`.
    pub fn ready(&mut self, digest: &str, now: Instant, window: Duration) -> bool {
        match &self.candidate {
            Some((known, since)) if known == digest => now.duration_since(*since) >= window,
            _ => {
                self.candidate = Some((digest.to_string(), now));
                false
            }
        }
    }

    /// Forget the current candidate, e.g. when switching documents.
    pub fn reset(&mut self) {
        self.candidate = None;
    }
}

/// Polls one path and produces validated snapshots.
#[derive(Debug, Default)]
pub struct StableReader {
    gate: StabilityGate,
    accepted_digest: Option<String>,
    identity: Option<PathIdentity>,
    generation: DocumentGeneration,
}

impl StableReader {
    /// Switch to a different document, bumping the generation and dropping state.
    pub fn open(&mut self, path: &Path) -> Result<PathIdentity, String> {
        let identity = PathIdentity::from_path(path)?;
        if self
            .identity
            .as_ref()
            .map(|current| !current.same_file(&identity))
            .unwrap_or(true)
        {
            self.generation = DocumentGeneration(self.generation.0 + 1);
            self.gate.reset();
            self.accepted_digest = None;
        }
        self.identity = Some(identity.clone());
        Ok(identity)
    }

    /// Current generation.
    pub fn generation(&self) -> DocumentGeneration {
        self.generation
    }

    /// Current path identity, if a document is open.
    pub fn identity(&self) -> Option<&PathIdentity> {
        self.identity.as_ref()
    }

    /// Poll once. `now` is injected so tests control the settle window.
    pub fn poll(&mut self, now: Instant) -> Option<MonitorEvent> {
        let identity = self.identity.clone()?;
        let generation = self.generation;

        let bytes = match stable_read(&identity.canonical) {
            Ok(bytes) => bytes,
            Err(ReadFailure::Unstable(reason)) => {
                return Some(MonitorEvent::Pending {
                    generation,
                    reason,
                    likely_write_in_progress: true,
                });
            }
            Err(ReadFailure::Hard(message)) => {
                return Some(MonitorEvent::ReadError {
                    generation,
                    message,
                });
            }
        };

        let digest = sha256_hex(&bytes);
        if self.accepted_digest.as_deref() == Some(digest.as_str()) {
            // Same revision we already published: nothing to do.
            return None;
        }

        if !self.gate.ready(&digest, now, SETTLE_WINDOW) {
            return Some(MonitorEvent::Pending {
                generation,
                reason: "等待文件写入稳定".into(),
                likely_write_in_progress: true,
            });
        }

        match SaveImage::decode(bytes) {
            Ok(image) => {
                self.accepted_digest = Some(digest.clone());
                let snapshot = Snapshot {
                    raw: image.raw().to_vec(),
                    payload: image.payload().to_vec(),
                    sha256: digest,
                    captured_at: now_stamp(),
                    path: identity.canonical.display().to_string(),
                    writable: image.is_writable(),
                    readonly_reason: image.support().detail().map(str::to_string),
                    outer_crc32: image.outer_crc32(),
                    inner_low32: image.inner_low32(),
                };
                Some(MonitorEvent::Snapshot {
                    generation,
                    identity,
                    snapshot: Box::new(snapshot),
                    changed: true,
                })
            }
            Err(error) => {
                let transient = error.is_transient();
                let message = describe_decode_error(&error);
                if transient {
                    Some(MonitorEvent::Pending {
                        generation,
                        reason: message,
                        likely_write_in_progress: true,
                    })
                } else {
                    // A non-transient failure is still retried, but it is reported
                    // as an error so the UI can show a persistent state.
                    Some(MonitorEvent::ReadError {
                        generation,
                        message,
                    })
                }
            }
        }
    }

    /// Seed the accepted revision so a freshly opened document is not re-published.
    pub fn seed_accepted(&mut self, digest: impl Into<String>) {
        self.accepted_digest = Some(digest.into());
    }
}

/// Why a read failed.
#[derive(Debug)]
pub enum ReadFailure {
    /// The file is changing; retry.
    Unstable(String),
    /// A real error.
    Hard(String),
}

/// Read a file without ever retaining more than `limit + 1` bytes.
///
/// The metadata check rejects ordinary oversized files before allocation; the
/// streaming cap also catches a file that grows after the metadata was read.
pub fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;

    let file = std::fs::File::open(path)
        .map_err(|error| format!("无法打开 {}：{error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("无法读取文件信息：{error}"))?;
    if metadata.len() > limit as u64 {
        return Err(format!(
            "文件 {} 字节，超过 {} 字节上限",
            metadata.len(),
            limit
        ));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取失败：{error}"))?;
    if bytes.len() > limit {
        return Err(format!("文件读取时增长，超过 {limit} 字节上限"));
    }
    Ok(bytes)
}

/// Read a file with metadata checks on both sides.
///
/// Deliberately short-lived: the handle is not held, so the game can rename or
/// replace the file at any time.
pub fn stable_read(path: &Path) -> Result<Vec<u8>, ReadFailure> {
    let before = std::fs::metadata(path)
        .map_err(|error| ReadFailure::Hard(format!("无法读取文件信息：{error}")))?;
    if before.len() > MAX_SAVE_BYTES {
        return Err(ReadFailure::Hard(format!(
            "文件 {} 字节，超过 {} 字节上限",
            before.len(),
            MAX_SAVE_BYTES
        )));
    }
    let bytes =
        std::fs::read(path).map_err(|error| ReadFailure::Hard(format!("读取失败：{error}")))?;
    let after = std::fs::metadata(path)
        .map_err(|error| ReadFailure::Hard(format!("无法重新读取文件信息：{error}")))?;

    let same_size = before.len() == after.len();
    let same_time = modified(&before) == modified(&after);
    let complete = bytes.len() as u64 == after.len();
    if !(same_size && same_time && complete) {
        return Err(ReadFailure::Unstable(
            "文件在读取过程中发生变化，稍后重试".into(),
        ));
    }
    Ok(bytes)
}

fn modified(metadata: &std::fs::Metadata) -> Option<std::time::SystemTime> {
    metadata.modified().ok()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Player-facing description of a decode failure.
pub fn describe_decode_error(error: &DecodeError) -> String {
    match error {
        DecodeError::BadOuterCrc { .. } => "文件尚未完整写入或已损坏；保留上一份有效状态".into(),
        DecodeError::BadInnerHash { .. } => "正文完整性校验失败；该文件不能写入".into(),
        DecodeError::UnrecognizedMagic | DecodeError::UnsupportedFlags => {
            "不是本工具支持的存档容器".into()
        }
        DecodeError::TruncatedOrOversized { .. } => "文件长度异常".into(),
        DecodeError::NonZeroPadding => "终端填充区非零，文件结构异常".into(),
        DecodeError::TrailingBytes { .. } => "存在未解析的尾部数据".into(),
        other => other.to_string(),
    }
}

/// Timestamp helper shared by the I/O layer.
pub fn now_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}Z", now.as_secs(), now.subsec_millis())
}

/// Timestamp helper with a readable UTC form.
pub fn iso_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Days since epoch to civil date (Howard Hinnant's algorithm), no dependencies.
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Convert days since 1970-01-01 to a civil date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Platform-specific file identity.
pub mod platform {
    use std::path::Path;

    /// Volume serial and file index, when available.
    #[cfg(windows)]
    pub fn file_identity(path: &Path) -> Option<(u64, u64)> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        };

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // Read-only access with full sharing: never blocks the game's own writes.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
        unsafe {
            CloseHandle(handle);
        }
        if ok == 0 {
            return None;
        }
        let index = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
        Some((info.dwVolumeSerialNumber as u64, index))
    }

    /// Non-Windows builds have no portable file index here.
    #[cfg(not(windows))]
    pub fn file_identity(_path: &Path) -> Option<(u64, u64)> {
        None
    }
}
