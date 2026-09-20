//! Restricted discovery of Steam save candidates.
//!
//! Only the documented layout is probed:
//! `<steam>\userdata\<account>\553850\remote\testament_new.sav`.
//! No recursive disk scan, and no guessing which account is "current".

use std::path::{Path, PathBuf};

/// Application directory used by this workflow.
pub const APP_ID: &str = "553850";
/// Save file name.
pub const SAVE_NAME: &str = "testament_new.sav";

/// One candidate save file.
#[derive(Debug, Clone, PartialEq)]
pub struct SaveCandidate {
    pub path: PathBuf,
    /// The numeric account directory name. Never treated as a SteamID64.
    pub account_dir: String,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: u64,
}

impl SaveCandidate {
    /// Player-facing description, including size and modification time.
    pub fn describe(&self) -> String {
        format!(
            "账号目录 {} · {} 字节 · 修改于 {}",
            self.account_dir,
            self.size,
            format_epoch(self.modified)
        )
    }
}

/// Locations worth probing, in priority order.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    // Windows registry is the authoritative location, but reading it needs the
    // `windows` crate; the common install paths cover the default cases and the
    // user can always pick a file by hand.
    for base in [
        r"C:\Program Files (x86)\Steam",
        r"C:\Program Files\Steam",
        r"D:\Steam",
        r"E:\Steam",
    ] {
        let path = PathBuf::from(base);
        if path.is_dir() {
            roots.push(path);
        }
    }

    if let Ok(library) = std::env::var("STEAM_LIBRARY") {
        let path = PathBuf::from(library);
        if path.is_dir() {
            roots.push(path);
        }
    }
    roots
}

/// Find candidates under a Steam root without scanning the whole disk.
pub fn find_candidates(steam_root: &Path) -> Vec<SaveCandidate> {
    let userdata = steam_root.join("userdata");
    let Ok(entries) = std::fs::read_dir(&userdata) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let account_path = entry.path();
        if !account_path.is_dir() {
            continue;
        }
        let account_dir = entry.file_name().to_string_lossy().to_string();
        // Account directories are numeric; anything else is not a candidate.
        if account_dir.is_empty() || !account_dir.chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        let save = account_path.join(APP_ID).join("remote").join(SAVE_NAME);
        if let Ok(metadata) = std::fs::metadata(&save) {
            if !metadata.is_file() {
                continue;
            }
            candidates.push(SaveCandidate {
                path: save,
                account_dir: account_dir.clone(),
                size: metadata.len(),
                modified: metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0),
            });
        }
    }
    // Deterministic order; the user chooses, so no "newest wins" default.
    candidates.sort_by(|a, b| a.account_dir.cmp(&b.account_dir));
    candidates
}

/// Discover candidates across all known Steam roots.
pub fn discover_all() -> Vec<SaveCandidate> {
    let mut all = Vec::new();
    for root in steam_roots() {
        all.extend(find_candidates(&root));
    }
    all
}

/// Format seconds since the epoch without pulling in a date library.
pub fn format_epoch(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

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

/// Whether a path looks like a HD2 save we should offer to open.
pub fn looks_like_save(path: &Path) -> bool {
    path.extension()
        .map(|extension| {
            extension.eq_ignore_ascii_case("sav") || extension.eq_ignore_ascii_case("bin")
        })
        .unwrap_or(false)
}
