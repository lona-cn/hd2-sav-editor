//! Transaction tests with fault injection: the paths that decide whether a real
//! save can be lost.
//!
//! Acceptance criteria E01-E07, A03, A07, A08, U03, U04.

use std::path::{Path, PathBuf};

use loadout_domain::{
    encode_intent, preview_intent, resolve_intent, Catalog, Classification, Item, ItemRef,
    ItemType, LoadoutIntent, SlotIntent, Snapshot,
};
use local_io::{
    commit_to_source, commit_to_source_force_latest, prepare_commit, restore_backup, save_copy,
    sha256_hex, CommitOutcome,
};
use sav_codec::{fields, FieldPatch, SaveImage};

const OFFSET_HEAD: usize = fields::HEAD.0;
const OFFSET_BODY: usize = fields::BODY.0;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../../tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hd2_txn_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn snapshot_from(path: &Path, image: &SaveImage) -> Snapshot {
    Snapshot {
        raw: image.raw().to_vec(),
        payload: image.payload().to_vec(),
        sha256: sha256_hex(&std::fs::read(path).unwrap()),
        captured_at: "test".into(),
        path: path.display().to_string(),
        writable: image.is_writable(),
        readonly_reason: image.support().detail().map(str::to_string),
        outer_crc32: image.outer_crc32(),
        inner_low32: image.inner_low32(),
    }
}

fn catalog_with_armor() -> Catalog {
    let mut item = Item::new(ItemType::Armor, 0xD346_1392, Classification::UserVerified);
    item.display_name = "FS-55 蹂躏者".into();
    Catalog::from_items(vec![item]).unwrap()
}

fn armor_ref() -> ItemRef {
    ItemRef {
        item_key: Item::make_key(ItemType::Armor, 0xD346_1392),
        id_u32: 0xD346_1392,
        item_type: ItemType::Armor,
        label_snapshot: "FS-55 蹂躏者".into(),
    }
}

/// Prepare a real head-armor commit against a fixture on disk.
fn prepare_head_armor(path: &Path) -> (SaveImage, local_io::PreparedCommit) {
    let raw = std::fs::read(path).unwrap();
    let image = SaveImage::decode(raw).unwrap();
    let snapshot = snapshot_from(path, &image);
    let catalog = catalog_with_armor();
    let intent = LoadoutIntent {
        head: SlotIntent::Set { item: armor_ref() },
        body: SlotIntent::Keep,
    };
    let patches = resolve_intent(&snapshot, &catalog, &intent).unwrap();
    let diff = preview_intent(&snapshot, &catalog, &intent).unwrap();
    let prepared = prepare_commit(path, &snapshot, &image, &patches, &diff, 1).unwrap();
    (image, prepared)
}

/// E01/A03: a copy lands byte-identical to the produced container and the
/// source is untouched.
#[test]
fn save_copy_writes_the_new_bytes_and_leaves_the_source_alone() {
    let dir = temp_dir("copy");
    let source = dir.join("testament_new.sav");
    std::fs::write(&source, fixture("valid_baseline.bin")).unwrap();
    let source_before = std::fs::read(&source).unwrap();

    let (_image, prepared) = prepare_head_armor(&source);
    let target = dir.join("my_loadout.sav");
    let receipt = save_copy(&prepared, &target).unwrap();

    assert!(!receipt.source_modified, "a copy never modifies the source");
    let written = std::fs::read(&target).unwrap();
    assert_eq!(sha256_hex(&written), prepared.output_sha256);
    assert_eq!(
        std::fs::read(&source).unwrap(),
        source_before,
        "the source must be byte-identical after a save-as"
    );

    // The copy must decode and carry the requested head.
    let decoded = SaveImage::decode(written).unwrap();
    assert_eq!(
        decoded.read_u32(fields::HEAD).unwrap(),
        0xD346_1392,
        "the copied loadout must contain the selected head"
    );
    assert_eq!(
        decoded.read_u32(fields::BODY).unwrap(),
        SaveImage::decode(source_before.clone())
            .unwrap()
            .read_u32(fields::BODY)
            .unwrap(),
        "the body must stay at the source value"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// E02: an existing target is never silently overwritten.
#[test]
fn save_copy_refuses_an_existing_target() {
    let dir = temp_dir("copy_exists");
    let source = dir.join("testament_new.sav");
    std::fs::write(&source, fixture("valid_baseline.bin")).unwrap();
    let (_image, prepared) = prepare_head_armor(&source);

    let target = dir.join("already_there.sav");
    std::fs::write(&target, b"important user data").unwrap();
    let error = save_copy(&prepared, &target).unwrap_err();
    assert!(
        error.to_string().contains("已存在"),
        "expected an exists error, got {error}"
    );
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"important user data",
        "the existing file must be untouched"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// E02: exporting onto the source path is refused outright.
#[test]
fn save_copy_refuses_to_target_the_source_itself() {
    let dir = temp_dir("copy_self");
    let source = dir.join("testament_new.sav");
    std::fs::write(&source, fixture("valid_baseline.bin")).unwrap();
    let (_image, prepared) = prepare_head_armor(&source);

    let error = save_copy(&prepared, &source).unwrap_err();
    assert!(
        matches!(error, local_io::CommitError::TargetIsSource),
        "expected TargetIsSource, got {error}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// E03/A07: a committed write-back verifies by read-back and leaves a verified backup.
#[test]
fn commit_writes_back_with_a_verified_backup() {
    let dir = temp_dir("commit");
    let source = dir.join("testament_new.sav");
    let original = fixture("valid_baseline.bin");
    std::fs::write(&source, &original).unwrap();
    let original_sha = sha256_hex(&original);

    let (_image, prepared) = prepare_head_armor(&source);
    let backups = dir.join("backups");
    let receipt = commit_to_source(&prepared, &backups).unwrap();

    assert_eq!(
        receipt.outcome,
        CommitOutcome::CommittedVerified,
        "detail: {}",
        receipt.detail
    );
    assert!(receipt.is_verified());

    // The backup must hold the pre-commit bytes, verified by digest.
    let backup_path = receipt.backup_path.clone().expect("a backup path");
    let backup_bytes = std::fs::read(&backup_path).unwrap();
    assert_eq!(sha256_hex(&backup_bytes), original_sha);
    assert_eq!(
        receipt.backup_sha256.as_deref(),
        Some(original_sha.as_str())
    );

    // The source now holds the new container.
    let after = std::fs::read(&source).unwrap();
    assert_eq!(sha256_hex(&after), prepared.output_sha256);
    let decoded = SaveImage::decode(after).unwrap();
    assert_eq!(decoded.read_u32(fields::HEAD).unwrap(), 0xD346_1392);

    // No temporary files left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no temp files should remain");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn commit_never_reuses_or_truncates_a_preexisting_temp_file() {
    let dir = temp_dir("preexisting_temp");
    let source = dir.join("testament_new.sav");
    std::fs::write(&source, fixture("valid_baseline.bin")).unwrap();
    let (_image, prepared) = prepare_head_armor(&source);

    let occupied = dir.join(format!(".testament_new.sav.hd2_{}.tmp", std::process::id()));
    std::fs::write(&occupied, b"do not overwrite").unwrap();

    let receipt = commit_to_source(&prepared, &dir.join("backups")).unwrap();
    assert_eq!(receipt.outcome, CommitOutcome::CommittedVerified);
    assert_eq!(
        std::fs::read(&occupied).unwrap(),
        b"do not overwrite",
        "a preexisting temp path must never be truncated or moved"
    );
}

/// E04: if the source changed after the draft was taken, the write is refused
/// and the file is not touched.
#[test]
fn commit_refuses_when_the_source_changed_since_the_draft() {
    let dir = temp_dir("conflict");
    let source = dir.join("testament_new.sav");
    std::fs::write(&source, fixture("valid_baseline.bin")).unwrap();

    let (_image, prepared) = prepare_head_armor(&source);

    // The game (or another tool) writes the file after the draft was captured.
    let external = fixture("valid_head_b01.bin");
    std::fs::write(&source, &external).unwrap();
    let external_sha = sha256_hex(&external);

    let backups = dir.join("backups");
    let receipt = commit_to_source(&prepared, &backups).unwrap();

    assert_eq!(receipt.outcome, CommitOutcome::RejectedBeforeWrite);
    assert!(!receipt.is_verified());
    assert_eq!(
        sha256_hex(&std::fs::read(&source).unwrap()),
        external_sha,
        "the external revision must survive untouched"
    );
    assert!(
        receipt.backup_path.is_none(),
        "nothing was written, so no backup is claimed"
    );
    assert!(
        !backups.exists(),
        "a refused commit must not create a backup directory"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Risk mode deliberately accepts a stale draft, but rebases its requested slot
/// values onto the latest valid file instead of replacing that file wholesale.
#[test]
fn force_latest_preserves_an_external_body_change_while_applying_the_head() {
    let dir = temp_dir("force_latest");
    let source = dir.join("testament_new.sav");
    std::fs::write(&source, fixture("valid_baseline.bin")).unwrap();
    let (_image, prepared) = prepare_head_armor(&source);

    // Simulate the game changing body armor after the draft was created.
    let external = fixture("valid_body_b01.bin");
    let external_sha = sha256_hex(&external);
    std::fs::write(&source, &external).unwrap();

    let receipt =
        commit_to_source_force_latest(&prepared, &dir.join("backups")).expect("risk write");
    assert_eq!(receipt.outcome, CommitOutcome::CommittedVerified);
    assert_eq!(receipt.before_sha256, external_sha);
    assert!(
        receipt.detail.contains("最新磁盘内容"),
        "receipt must disclose the rebase: {}",
        receipt.detail
    );

    let written = SaveImage::decode(std::fs::read(&source).unwrap()).unwrap();
    assert_eq!(
        written.read_u32(fields::HEAD).unwrap(),
        0xD346_1392,
        "the requested head armor must be applied"
    );
    assert_eq!(
        written.read_u32(fields::BODY).unwrap(),
        0x61B3_1723,
        "the game's newer body armor must survive a head-only risk write"
    );

    let backup = receipt.backup_path.expect("risk write must still back up");
    assert_eq!(
        std::fs::read(backup).unwrap(),
        external,
        "the backup must contain the latest file that was replaced"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn force_latest_reapplies_an_explicit_body_choice_equal_to_the_old_baseline() {
    let dir = temp_dir("force_explicit_body");
    let source = dir.join("testament_new.sav");
    let baseline = fixture("valid_baseline.bin");
    std::fs::write(&source, &baseline).unwrap();
    let image = SaveImage::decode(baseline).unwrap();
    let snapshot = snapshot_from(&source, &image);
    let catalog = catalog_with_armor();
    let intent = LoadoutIntent {
        head: SlotIntent::Keep,
        body: SlotIntent::Set { item: armor_ref() },
    };
    let patches = resolve_intent(&snapshot, &catalog, &intent).unwrap();
    assert!(
        patches.is_empty(),
        "the explicit body choice equals the old baseline"
    );
    let diff = preview_intent(&snapshot, &catalog, &intent).unwrap();
    let prepared = prepare_commit(&source, &snapshot, &image, &patches, &diff, 1).unwrap();

    std::fs::write(&source, fixture("valid_body_b01.bin")).unwrap();
    let receipt =
        commit_to_source_force_latest(&prepared, &dir.join("backups")).expect("risk write");
    assert_eq!(receipt.outcome, CommitOutcome::CommittedVerified);

    let written = SaveImage::decode(std::fs::read(&source).unwrap()).unwrap();
    assert_eq!(
        written.read_u32(fields::BODY).unwrap(),
        0xD346_1392,
        "risk mode must honor the explicit body choice, not the newer game value"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// E05/W04: a no-op commit produces a byte-identical file and still leaves a backup.
#[test]
fn no_op_commit_is_byte_identical() {
    let dir = temp_dir("noop");
    let source = dir.join("testament_new.sav");
    let original = fixture("valid_baseline.bin");
    std::fs::write(&source, &original).unwrap();

    let image = SaveImage::decode(original.clone()).unwrap();
    let snapshot = snapshot_from(&source, &image);
    // Keeping everything produces no patches.
    let catalog = catalog_with_armor();
    let intent = LoadoutIntent {
        head: SlotIntent::Keep,
        body: SlotIntent::Keep,
    };
    let patches = resolve_intent(&snapshot, &catalog, &intent).unwrap();
    assert!(patches.is_empty());
    let diff = preview_intent(&snapshot, &catalog, &intent).unwrap();

    let prepared = prepare_commit(&source, &snapshot, &image, &patches, &diff, 1).unwrap();
    assert_eq!(
        sha256_hex(&prepared.output),
        snapshot.sha256,
        "a no-op must re-encode to the identical container"
    );

    let receipt = commit_to_source(&prepared, &dir.join("backups")).unwrap();
    assert_eq!(receipt.outcome, CommitOutcome::CommittedVerified);
    assert_eq!(std::fs::read(&source).unwrap(), original);

    let _ = std::fs::remove_dir_all(&dir);
}

/// U04: a read-only layout can never be prepared for writing.
#[test]
fn unknown_layout_cannot_be_prepared_for_writing() {
    let dir = temp_dir("unknown");
    let source = dir.join("unknown.sav");
    std::fs::write(&source, fixture("unknown_layout_valid.bin")).unwrap();

    let raw = std::fs::read(&source).unwrap();
    let image = SaveImage::decode(raw).unwrap();
    assert!(!image.is_writable(), "the unknown layout must be read-only");

    let snapshot = snapshot_from(&source, &image);
    let catalog = catalog_with_armor();
    let intent = LoadoutIntent {
        head: SlotIntent::Set { item: armor_ref() },
        body: SlotIntent::Keep,
    };
    // The refusal happens at the very first step: no patch is even produced.
    let resolve_error = resolve_intent(&snapshot, &catalog, &intent).unwrap_err();
    assert!(
        resolve_error.to_string().contains("只读") || resolve_error.to_string().contains("布局"),
        "unknown layout must be refused at intent resolution, got {resolve_error}"
    );

    // The preview refuses too, so the UI cannot even describe a write here.
    assert!(
        preview_intent(&snapshot, &catalog, &intent).is_err(),
        "a read-only layout must not produce a write preview"
    );

    // And even with an empty patch set, preparing a write is refused.
    let writable_image = SaveImage::decode(fixture("valid_baseline.bin")).unwrap();
    let writable_snapshot = snapshot_from(&source, &writable_image);
    let empty_diff =
        preview_intent(&writable_snapshot, &catalog, &LoadoutIntent::default()).unwrap();
    let error = prepare_commit(
        Path::new("fixture.sav"),
        &snapshot,
        &image,
        &[],
        &empty_diff,
        1,
    )
    .unwrap_err();
    assert!(matches!(error, local_io::CommitError::LayoutNotWritable));
    assert_eq!(
        std::fs::read(&source).unwrap(),
        fixture("unknown_layout_valid.bin")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// E06: restoring a full backup replaces everything and keeps a safety copy.
#[test]
fn restore_replaces_everything_and_keeps_a_safety_copy() {
    let dir = temp_dir("restore");
    let source = dir.join("testament_new.sav");
    let baseline = fixture("valid_baseline.bin");
    std::fs::write(&source, fixture("valid_head_and_body.bin")).unwrap();

    // Write the baseline into a backup file, then restore from it.
    let backup_file = dir.join("saved_backup.sav");
    std::fs::write(&backup_file, &baseline).unwrap();

    let backups = dir.join("backups");
    let receipt = restore_backup(&backup_file, &source, &backups).unwrap();
    assert_eq!(
        receipt.outcome,
        CommitOutcome::CommittedVerified,
        "{}",
        receipt.detail
    );
    assert_eq!(std::fs::read(&source).unwrap(), baseline);

    // A copy of what was replaced must exist.
    let safety: Vec<_> = std::fs::read_dir(&backups)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("before_restore_"))
        .collect();
    assert_eq!(safety.len(), 1, "exactly one safety copy: {safety:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Restoring from a corrupt backup is refused before the source is touched.
#[test]
fn restore_refuses_a_corrupt_backup() {
    let dir = temp_dir("restore_bad");
    let source = dir.join("testament_new.sav");
    let current = fixture("valid_head_and_body.bin");
    std::fs::write(&source, &current).unwrap();

    let corrupt = dir.join("corrupt_backup.sav");
    std::fs::write(&corrupt, b"not a save at all").unwrap();

    let error = restore_backup(&corrupt, &source, &dir.join("backups")).unwrap_err();
    assert!(
        matches!(error, local_io::CommitError::OutputInvalid(_)),
        "expected an invalid-backup error, got {error}"
    );
    assert_eq!(
        std::fs::read(&source).unwrap(),
        current,
        "a rejected restore must not touch the save"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A03/A04: the prepared container differs from the source only inside the
/// whitelisted ranges.
#[test]
fn prepared_output_differs_only_in_whitelisted_ranges() {
    let dir = temp_dir("whitelist");
    let source = dir.join("testament_new.sav");
    let original = fixture("valid_baseline.bin");
    std::fs::write(&source, &original).unwrap();

    let (_image, prepared) = prepare_head_armor(&source);
    let before = SaveImage::decode(original.clone()).unwrap();
    let after = SaveImage::decode(prepared.output.clone()).unwrap();

    // Payload: only the head field may differ.
    let payload_before = before.payload();
    let payload_after = after.payload();
    assert_eq!(payload_before.len(), payload_after.len());
    let differing: Vec<usize> = (0..payload_before.len())
        .filter(|index| payload_before[*index] != payload_after[*index])
        .collect();
    let hash_range = sav_codec::INNER_CHECKSUM_OFFSET..sav_codec::INNER_CHECKSUM_OFFSET + 4;
    for index in &differing {
        assert!(
            (OFFSET_HEAD..OFFSET_HEAD + 4).contains(index) || hash_range.contains(index),
            "payload byte {index:#x} is outside the head range and the checksum field"
        );
    }
    // Only the head and the inner checksum may move.
    let head_and_hash: Vec<usize> = differing
        .iter()
        .copied()
        .filter(|index| hash_range.contains(index))
        .collect();
    assert!(
        differing.len() - head_and_hash.len() <= 4,
        "at most the four head bytes may change outside the checksum"
    );
    assert!(!differing.is_empty(), "the head must actually change");
    assert_eq!(
        &payload_before[OFFSET_BODY..OFFSET_BODY + 4],
        &payload_after[OFFSET_BODY..OFFSET_BODY + 4],
        "body must be untouched"
    );

    // Container: the outer CRC (0x0C) and the inner hash inside block 0 change.
    let differing_container: Vec<usize> = (0..original.len().min(prepared.output.len()))
        .filter(|index| original[*index] != prepared.output[*index])
        .collect();
    assert!(
        !differing_container.is_empty(),
        "checksums must be refreshed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A08: with a change in a later block, block 0 is re-emitted so the inner hash
/// stays correct, and the result decodes cleanly.
#[test]
fn later_block_edit_refreshes_the_inner_hash() {
    let raw = fixture("valid_later_block_edit.bin");
    let image = SaveImage::decode(raw.clone()).unwrap();
    let payload = image.payload().to_vec();

    // Find a payload offset that lives in block 1 rather than block 0.
    let later = sav_codec::container::BLOCK_SIZE;
    assert!(payload.len() > later + 8, "fixture must span two blocks");
    let target = later + 4;
    let new_value =
        u32::from_le_bytes(payload[target..target + 4].try_into().unwrap()).wrapping_add(1);

    let patches = vec![FieldPatch {
        offset: sav_codec::PayloadOffset(target),
        before: payload[target..target + 4].try_into().unwrap(),
        after: new_value.to_le_bytes(),
    }];

    // The codec only accepts whitelisted offsets, so drive it through the domain
    // whitelist check to prove a non-whitelisted later-block edit is refused.
    let snapshot = Snapshot {
        raw: image.raw().to_vec(),
        payload: payload.clone(),
        sha256: sha256_hex(&raw),
        captured_at: "test".into(),
        path: "fixture".into(),
        writable: true,
        readonly_reason: None,
        outer_crc32: image.outer_crc32(),
        inner_low32: image.inner_low32(),
    };
    let catalog = catalog_with_armor();
    let intent = LoadoutIntent {
        head: SlotIntent::Keep,
        body: SlotIntent::Keep,
    };
    let empty = resolve_intent(&snapshot, &catalog, &intent).unwrap();
    assert!(empty.is_empty());

    // The codec is deliberately byte-level, so it *can* encode an arbitrary
    // in-range patch. The refusal must come from the transaction gate, which is
    // the last thing between a bug and the user's save file.
    let diff = preview_intent(&snapshot, &catalog, &intent).unwrap();
    let error = prepare_commit(
        Path::new("fixture.sav"),
        &snapshot,
        &image,
        &patches,
        &diff,
        1,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("不在允许写入"),
        "a patch outside the armor slots must be refused by the commit gate, got {error}"
    );

    // And the codec itself does refresh the inner hash when it is used directly.
    let encoded = encode_intent(&image, &patches).unwrap();
    let decoded = SaveImage::decode(encoded).unwrap();
    assert_eq!(
        decoded.read_u32(sav_codec::PayloadOffset(target)).unwrap(),
        new_value,
        "the codec applies in-range patches faithfully"
    );
}

/// E07: concurrent saves are serialised by the caller; here we prove two
/// sequential commits against the same base behave correctly (the second is
/// refused because the base moved).
#[test]
fn second_commit_on_a_stale_base_is_refused() {
    let dir = temp_dir("double_commit");
    let source = dir.join("testament_new.sav");
    std::fs::write(&source, fixture("valid_baseline.bin")).unwrap();
    let backups = dir.join("backups");

    let (_image, first) = prepare_head_armor(&source);
    let receipt = commit_to_source(&first, &backups).unwrap();
    assert_eq!(receipt.outcome, CommitOutcome::CommittedVerified);

    // The same prepared commit is now stale: its expected hash no longer matches.
    let second = commit_to_source(&first, &backups).unwrap();
    assert_eq!(
        second.outcome,
        CommitOutcome::RejectedBeforeWrite,
        "a stale base must never be written twice"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
