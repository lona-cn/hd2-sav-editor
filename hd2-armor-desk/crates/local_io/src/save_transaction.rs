//! Save transactions: copies, backups and the explicit write-back.
//!
//! Invariants enforced here:
//! * **I1** — nothing in this module runs unless the user explicitly asked to
//!   write a file.
//! * **I2** — the produced container differs from the source only in the
//!   whitelisted fields plus the two checksums; the codec proves this and the
//!   transaction re-checks it before any bytes leave memory.
//! * **I3** — safe mode rejects a changed source; explicit risk mode rebuilds
//!   requested armor slots on the latest valid source instead of writing stale bytes.
//!
//! A disk-level success is not a claim about the game. The receipts carry the
//! evidence; the UI wording stays inside it.

use std::path::{Path, PathBuf};

use loadout_domain::{encode_intent, HumanReadableDiff, Snapshot};
use sav_codec::{FieldPatch, SaveImage, MAX_INPUT};
use sha2::{Digest, Sha256};

use crate::catalog_store::create_unique_sibling;
use crate::monitor::{iso_stamp, now_stamp, read_bounded, PathIdentity};

/// Result of writing a copy elsewhere.
#[derive(Debug, Clone, PartialEq)]
pub struct CopyReceipt {
    pub target_path: PathBuf,
    pub output_sha256: String,
    pub bytes_written: u64,
    pub written_at: String,
    /// Always false for a copy: the source is untouched by definition.
    pub source_modified: bool,
}

/// Result of writing back to the source file.
#[derive(Debug, Clone, PartialEq)]
pub enum CommitOutcome {
    /// The replacement was verified by reading the file back.
    CommittedVerified,
    /// A precondition failed; the source was not overwritten.
    RejectedBeforeWrite,
    /// The replacement happened, then an external writer changed the file again.
    CommittedButSuperseded,
    /// The OS-level replacement returned an error whose effect cannot be
    /// determined. The backup and any temporary file are kept for inspection.
    Indeterminate,
}

impl CommitOutcome {
    /// Player-facing wording. Never stronger than the evidence.
    pub fn label(&self) -> &'static str {
        match self {
            CommitOutcome::CommittedVerified => "已保存到磁盘（回读一致）",
            CommitOutcome::RejectedBeforeWrite => "未写入：前置检查未通过",
            CommitOutcome::CommittedButSuperseded => "已写入，但随后检测到外部更新",
            CommitOutcome::Indeterminate => "保存结果无法确认，请检查备份",
        }
    }

    /// Whether the file was (or may have been) replaced.
    pub fn may_have_written(&self) -> bool {
        !matches!(self, CommitOutcome::RejectedBeforeWrite)
    }
}

/// Full receipt for a source write-back.
#[derive(Debug, Clone, PartialEq)]
pub struct CommitReceipt {
    pub outcome: CommitOutcome,
    pub source_path: PathBuf,
    pub before_sha256: String,
    pub expected_sha256: String,
    pub output_sha256: String,
    pub backup_path: Option<PathBuf>,
    pub backup_sha256: Option<String>,
    pub committed_at: String,
    /// Human-readable changed fields.
    pub changed: Vec<String>,
    /// Detail message for the diagnostics view.
    pub detail: String,
    /// True when a temporary file was left behind on purpose.
    pub temp_left_in_place: bool,
}

impl CommitReceipt {
    /// Whether this receipt may update the draft baseline.
    pub fn is_verified(&self) -> bool {
        self.outcome == CommitOutcome::CommittedVerified
    }

    /// One-line status for the UI.
    pub fn summary(&self) -> String {
        match &self.backup_path {
            Some(path) => format!(
                "{}；备份：{}",
                self.outcome.label(),
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default()
            ),
            None => self.outcome.label().to_string(),
        }
    }
}

/// A frozen commit request: everything needed to write, captured up front.
#[derive(Debug, Clone)]
pub struct PreparedCommit {
    /// The bytes that will be written.
    pub output: Vec<u8>,
    pub output_sha256: String,
    /// The revision the draft was based on.
    pub expected_sha256: String,
    pub patches: Vec<FieldPatch>,
    /// Explicit slot targets, including a selection equal to the draft baseline.
    pub requested_head_id: Option<u32>,
    pub requested_body_id: Option<u32>,
    /// Human-readable change list.
    pub changed: Vec<String>,
    /// Source path this commit targets.
    pub source_path: PathBuf,
    pub generation: u64,
}

/// Why a transaction could not proceed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommitError {
    #[error("源文件已被外部更新，拒绝覆盖；请重新读取后再编辑")]
    SourceChanged,
    #[error("目标文件已存在：{0}")]
    TargetExists(String),
    #[error("导出目标与源文件是同一个文件，已拒绝")]
    TargetIsSource,
    #[error("生成结果未通过复核：{0}")]
    OutputInvalid(String),
    #[error("备份失败，未触碰源文件：{0}")]
    BackupFailed(String),
    #[error("写入失败：{0}")]
    WriteFailed(String),
    #[error("读取源文件失败：{0}")]
    ReadFailed(String),
    #[error("另一个保存任务正在进行中")]
    Busy,
    #[error("此存档布局未经核验，不能写入")]
    LayoutNotWritable,
}

/// Freeze a commit: build the output bytes and verify them before touching disk.
pub fn prepare_commit(
    source_path: &Path,
    snapshot: &Snapshot,
    image: &SaveImage,
    patches: &[FieldPatch],
    diff: &HumanReadableDiff,
    generation: u64,
) -> Result<PreparedCommit, CommitError> {
    let changed = diff
        .patches
        .iter()
        .map(|patch| {
            format!(
                "偏移 0x{:04X}: {:02X?} → {:02X?}",
                patch.offset.0, patch.before, patch.after
            )
        })
        .collect();
    prepare_commit_from_image(
        source_path,
        &snapshot.sha256,
        image,
        patches,
        (diff.head.to_id, diff.body.to_id),
        changed,
        generation,
    )
}

fn prepare_commit_from_image(
    source_path: &Path,
    expected_sha256: &str,
    image: &SaveImage,
    patches: &[FieldPatch],
    requested_ids: (Option<u32>, Option<u32>),
    changed: Vec<String>,
    generation: u64,
) -> Result<PreparedCommit, CommitError> {
    let (requested_head_id, requested_body_id) = requested_ids;
    if !image.is_writable() {
        return Err(CommitError::LayoutNotWritable);
    }

    // Last gate before any byte is written: the requested patches must lie inside
    // the two armor slots. A bug upstream must not be able to write elsewhere.
    for patch in patches {
        let start = patch.offset.0;
        let allowed = [
            sav_codec::fields::HEAD.0..sav_codec::fields::HEAD.0 + 4,
            sav_codec::fields::BODY.0..sav_codec::fields::BODY.0 + 4,
        ];
        if !allowed.iter().any(|range| range.contains(&start)) {
            return Err(CommitError::OutputInvalid(format!(
                "偏移 0x{start:04X} 不在允许写入的护甲槽范围内"
            )));
        }
    }

    let output = encode_intent(image, patches)
        .map_err(|error| CommitError::OutputInvalid(error.to_string()))?;

    // Independent re-verification of the produced container.
    //
    // The expected payload is the source payload with the requested fields
    // applied. The stored inner checksum also changes, and it lives *inside* the
    // payload, so it is masked out of the comparison and verified separately.
    let check = SaveImage::decode(output.clone())
        .map_err(|error| CommitError::OutputInvalid(error.to_string()))?;
    let mut expected = image.payload().to_vec();
    for patch in patches {
        expected[patch.offset.0..patch.offset.0 + 4].copy_from_slice(&patch.after);
    }

    let hash_range = sav_codec::INNER_CHECKSUM_OFFSET..sav_codec::INNER_CHECKSUM_OFFSET + 4;
    let mut actual_masked = check.payload().to_vec();
    let mut expected_masked = expected.clone();
    if expected.len() >= hash_range.end {
        actual_masked[hash_range.clone()].fill(0);
        expected_masked[hash_range.clone()].fill(0);
    }
    if actual_masked != expected_masked {
        return Err(CommitError::OutputInvalid("正文与预期不一致".into()));
    }
    // The refreshed hash must actually be the correct one for the new payload.
    let recomputed = sav_codec::inner_checksum(&expected) as u32;
    let stored = u32::from_le_bytes(
        check.payload()[hash_range]
            .try_into()
            .map_err(|_| CommitError::OutputInvalid("内层哈希位置越界".into()))?,
    );
    if stored != recomputed {
        return Err(CommitError::OutputInvalid("内层哈希未正确刷新".into()));
    }

    Ok(PreparedCommit {
        output_sha256: sha256_hex(&output),
        output,
        expected_sha256: expected_sha256.to_string(),
        patches: patches.to_vec(),
        requested_head_id,
        requested_body_id,
        changed,
        source_path: source_path.to_path_buf(),
        generation,
    })
}

/// Write a prepared commit to a *new* path, leaving the source untouched.
pub fn save_copy(prepared: &PreparedCommit, target: &Path) -> Result<CopyReceipt, CommitError> {
    if let Ok(target_identity) = PathIdentity::from_path(target) {
        if let Ok(source_identity) = PathIdentity::from_path(&prepared.source_path) {
            if target_identity.same_file(&source_identity) {
                return Err(CommitError::TargetIsSource);
            }
        }
    }
    if target.exists() {
        return Err(CommitError::TargetExists(target.display().to_string()));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| CommitError::WriteFailed(error.to_string()))?;
    }

    // Create with `create_new` so an existing file can never be truncated.
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|error| CommitError::WriteFailed(format!("{}：{error}", target.display())))?;

    if let Err(error) = (|| -> std::io::Result<()> {
        file.write_all(&prepared.output)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })() {
        drop(file);
        let _ = std::fs::remove_file(target);
        return Err(CommitError::WriteFailed(error.to_string()));
    }
    drop(file);

    // Verify what actually landed on disk.
    let written = read_bounded(target, MAX_INPUT).map_err(CommitError::WriteFailed)?;
    if sha256_hex(&written) != prepared.output_sha256 {
        let _ = std::fs::remove_file(target);
        return Err(CommitError::WriteFailed("回读校验不一致".into()));
    }

    Ok(CopyReceipt {
        target_path: target.to_path_buf(),
        output_sha256: prepared.output_sha256.clone(),
        bytes_written: written.len() as u64,
        written_at: iso_stamp(),
        source_modified: false,
    })
}

/// Write a prepared commit back to its source, with backup and conflict checks.
///
/// `backup_root` receives an independent copy of the original bytes. The backup
/// is verified before the source is touched.
pub fn commit_to_source(
    prepared: &PreparedCommit,
    backup_root: &Path,
) -> Result<CommitReceipt, CommitError> {
    let source = &prepared.source_path;

    // 1. Re-read the source and confirm it is still the revision we planned against.
    let current = read_bounded(source, MAX_INPUT).map_err(CommitError::ReadFailed)?;
    let current_sha = sha256_hex(&current);
    if current_sha != prepared.expected_sha256 {
        return Ok(rejected(
            prepared,
            &current_sha,
            "源文件内容与草稿基线不一致",
        ));
    }

    // 2. Independent backup, verified by digest, before anything is replaced.
    std::fs::create_dir_all(backup_root)
        .map_err(|error| CommitError::BackupFailed(error.to_string()))?;
    let stem = source
        .file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "testament_new".into());
    let backup_path = backup_root.join(format!(
        "{stem}_{}_{}.sav",
        now_stamp().replace('.', ""),
        &prepared.expected_sha256[..10]
    ));
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup_path)
            .map_err(|error| {
                CommitError::BackupFailed(format!("{}：{error}", backup_path.display()))
            })?;
        if let Err(error) = (|| -> std::io::Result<()> {
            file.write_all(&current)?;
            file.flush()?;
            file.sync_all()?;
            Ok(())
        })() {
            drop(file);
            return Err(CommitError::BackupFailed(error.to_string()));
        }
    }
    let backup_bytes = read_bounded(&backup_path, MAX_INPUT).map_err(CommitError::BackupFailed)?;
    let backup_sha = sha256_hex(&backup_bytes);
    if backup_sha != prepared.expected_sha256 {
        return Err(CommitError::BackupFailed("备份回读校验不一致".into()));
    }

    // 3. Temporary file in the same directory, so the replacement stays on one volume.
    // `create_new` ensures an attacker-controlled link or stale transaction file
    // can never be truncated or reused.
    let parent = source
        .parent()
        .ok_or_else(|| CommitError::WriteFailed("源文件没有父目录".into()))?;
    let temp_stem = format!(
        "{}.hd2",
        source
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "save".into())
    );
    let (temp_path, mut file) = create_unique_sibling(parent, &temp_stem, ".tmp")
        .map_err(|error| CommitError::WriteFailed(error.to_string()))?;
    use std::io::Write;
    if let Err(error) = (|| -> std::io::Result<()> {
        file.write_all(&prepared.output)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })() {
        drop(file);
        let _ = std::fs::remove_file(&temp_path);
        return Err(CommitError::WriteFailed(error.to_string()));
    }
    drop(file);

    // 4. Re-check the source revision immediately before replacing it.
    let recheck = read_bounded(source, MAX_INPUT).map_err(CommitError::ReadFailed)?;
    let recheck_sha = sha256_hex(&recheck);
    if recheck_sha != prepared.expected_sha256 {
        let _ = std::fs::remove_file(&temp_path);
        return Ok(rejected(prepared, &recheck_sha, "替换前源文件再次变化"));
    }

    // 5. Replace. On Windows this is the documented replacement path; any error
    //    here may leave the outcome undetermined, so the site is preserved.
    match replace_file(&temp_path, source) {
        Ok(()) => {}
        Err(error) => {
            return Ok(CommitReceipt {
                outcome: CommitOutcome::Indeterminate,
                source_path: source.clone(),
                before_sha256: current_sha,
                expected_sha256: prepared.expected_sha256.clone(),
                output_sha256: prepared.output_sha256.clone(),
                backup_path: Some(backup_path.clone()),
                backup_sha256: Some(backup_sha),
                committed_at: iso_stamp(),
                changed: prepared.changed.clone(),
                detail: format!(
                    "替换返回错误：{error}；临时文件 {} 与备份 {} 均已保留",
                    temp_path.display(),
                    backup_path.display()
                ),
                temp_left_in_place: temp_path.exists(),
            });
        }
    }
    let _ = std::fs::remove_file(&temp_path);

    // 6. Read back and compare.
    let after = read_bounded(source, MAX_INPUT)
        .map_err(|error| CommitError::ReadFailed(format!("回读失败：{error}")))?;
    let after_sha = sha256_hex(&after);

    let outcome = if after_sha == prepared.output_sha256 {
        CommitOutcome::CommittedVerified
    } else {
        // Somebody wrote between the replacement and the read-back.
        CommitOutcome::CommittedButSuperseded
    };

    let detail = if outcome == CommitOutcome::CommittedVerified {
        "回读内容与目标一致".to_string()
    } else {
        "替换后检测到外部更新；未自动重试覆盖".to_string()
    };

    Ok(CommitReceipt {
        outcome,
        source_path: source.clone(),
        before_sha256: current_sha,
        expected_sha256: prepared.expected_sha256.clone(),
        output_sha256: prepared.output_sha256.clone(),
        backup_path: Some(backup_path),
        backup_sha256: Some(backup_sha),
        committed_at: iso_stamp(),
        changed: prepared.changed.clone(),
        detail,
        temp_left_in_place: false,
    })
}

/// Explicitly apply the requested armor values to the latest valid source revision.
///
/// Unlike [`commit_to_source`], this mode does not require the file to still match
/// the draft baseline. It decodes the current file, rebuilds only the requested
/// head/body patches on top of those latest bytes, then uses the normal backup,
/// atomic replacement and read-back path. A further change during that short
/// transaction window is still rejected rather than blindly overwritten.
pub fn commit_to_source_force_latest(
    prepared: &PreparedCommit,
    backup_root: &Path,
) -> Result<CommitReceipt, CommitError> {
    let current =
        read_bounded(&prepared.source_path, MAX_INPUT).map_err(CommitError::ReadFailed)?;
    let current_sha = sha256_hex(&current);
    let image = SaveImage::decode(current)
        .map_err(|error| CommitError::OutputInvalid(format!("最新源文件无法解码：{error}")))?;
    if !image.is_writable() {
        return Err(CommitError::LayoutNotWritable);
    }

    let requested_slots = [
        (sav_codec::fields::HEAD, prepared.requested_head_id),
        (sav_codec::fields::BODY, prepared.requested_body_id),
    ];
    let mut patches = Vec::with_capacity(requested_slots.len());
    for (offset, requested_id) in requested_slots {
        let Some(requested_id) = requested_id else {
            continue;
        };
        let start = offset.0;
        let end = start
            .checked_add(4)
            .ok_or_else(|| CommitError::OutputInvalid("护甲槽偏移溢出".into()))?;
        let before: [u8; 4] = image
            .payload()
            .get(start..end)
            .ok_or_else(|| CommitError::OutputInvalid("最新源文件的护甲槽越界".into()))?
            .try_into()
            .map_err(|_| CommitError::OutputInvalid("最新源文件的护甲槽长度错误".into()))?;
        let after = requested_id.to_le_bytes();
        if before != after {
            patches.push(FieldPatch {
                offset,
                before,
                after,
            });
        }
    }

    let changed = patches
        .iter()
        .map(|patch| {
            format!(
                "偏移 0x{:04X}: {:02X?} → {:02X?}",
                patch.offset.0, patch.before, patch.after
            )
        })
        .collect();
    let rebased = prepare_commit_from_image(
        &prepared.source_path,
        &current_sha,
        &image,
        &patches,
        (prepared.requested_head_id, prepared.requested_body_id),
        changed,
        prepared.generation,
    )?;
    let rebased_from_newer_source = current_sha != prepared.expected_sha256;
    let mut receipt = commit_to_source(&rebased, backup_root)?;
    if receipt.is_verified() {
        receipt.detail = if rebased_from_newer_source {
            "已在最新磁盘内容上重新应用护甲选择；风险写入回读一致".into()
        } else {
            "风险写入回读一致".into()
        };
    }
    Ok(receipt)
}

fn rejected(prepared: &PreparedCommit, current_sha: &str, reason: &str) -> CommitReceipt {
    CommitReceipt {
        outcome: CommitOutcome::RejectedBeforeWrite,
        source_path: prepared.source_path.clone(),
        before_sha256: current_sha.to_string(),
        expected_sha256: prepared.expected_sha256.clone(),
        output_sha256: prepared.output_sha256.clone(),
        backup_path: None,
        backup_sha256: None,
        committed_at: iso_stamp(),
        changed: prepared.changed.clone(),
        detail: format!("{reason}；源文件未改动"),
        temp_left_in_place: false,
    }
}

/// Replace `target` with `replacement` on the same volume.
#[cfg(windows)]
fn replace_file(replacement: &Path, target: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let wide = |path: &Path| -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let from = wide(replacement);
    let to = wide(target);
    // `MOVEFILE_REPLACE_EXISTING` is the documented way to replace an existing
    // file atomically on one volume; write-through makes the metadata durable.
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

/// Non-Windows fallback used by the Linux test runs.
#[cfg(not(windows))]
fn replace_file(replacement: &Path, target: &Path) -> Result<(), String> {
    std::fs::rename(replacement, target).map_err(|error| error.to_string())
}

/// Restore a whole snapshot from a backup file.
///
/// This restores *everything*, not just the two armor slots, so the caller must
/// say so explicitly.
pub fn restore_backup(
    backup_path: &Path,
    target: &Path,
    backup_root: &Path,
) -> Result<CommitReceipt, CommitError> {
    let bytes = read_bounded(backup_path, MAX_INPUT).map_err(CommitError::ReadFailed)?;
    // The restored content must be a valid container before it replaces anything.
    SaveImage::decode(bytes.clone())
        .map_err(|error| CommitError::OutputInvalid(error.to_string()))?;
    let expected_sha = sha256_hex(&bytes);

    let current = read_bounded(target, MAX_INPUT).map_err(CommitError::ReadFailed)?;
    let before_sha = sha256_hex(&current);

    // Always keep a copy of what is being replaced, even during a restore.
    std::fs::create_dir_all(backup_root)
        .map_err(|error| CommitError::BackupFailed(error.to_string()))?;
    let safety = backup_root.join(format!(
        "before_restore_{}_{}.sav",
        now_stamp().replace('.', ""),
        &before_sha[..10]
    ));
    std::fs::write(&safety, &current)
        .map_err(|error| CommitError::BackupFailed(error.to_string()))?;

    let prepared = PreparedCommit {
        output: bytes,
        output_sha256: expected_sha.clone(),
        expected_sha256: before_sha.clone(),
        patches: Vec::new(),
        requested_head_id: None,
        requested_body_id: None,
        changed: vec!["恢复整份备份".into()],
        source_path: target.to_path_buf(),
        generation: 0,
    };
    let mut receipt = commit_to_source(&prepared, backup_root)?;
    receipt.changed = vec![format!(
        "恢复整份备份 {}（恢复前副本：{}）",
        backup_path.display(),
        safety.display()
    )];
    Ok(receipt)
}

/// SHA256 helper shared with the monitor.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}
