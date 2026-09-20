//! Monitor tests: settle window, half-written files, rename-replace, generation
//! isolation, and the guarantee that monitoring never writes.
//!
//! Acceptance criteria D01, D02, D05, D07, W01, W03.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use local_io::{
    read_bounded, DocumentGeneration, MonitorEvent, StabilityGate, StableReader, SETTLE_WINDOW,
};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hd2_monitor_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn baseline_bytes() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/valid_baseline.bin"
    ))
    .unwrap()
}

fn head_b01_bytes() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/valid_head_b01.bin"
    ))
    .unwrap()
}

#[test]
fn bounded_reader_rejects_an_oversized_file_before_returning_bytes() {
    let dir = temp_dir("bounded_read");
    let path = dir.join("oversized.bin");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(1025).unwrap();

    let error = read_bounded(&path, 1024).unwrap_err();
    assert!(error.contains("超过"));
}

/// The stability gate must not accept a revision until it has stopped changing.
#[test]
fn stability_gate_requires_a_quiet_window() {
    let mut gate = StabilityGate::default();
    let start = Instant::now();

    assert!(!gate.ready("rev-a", start, SETTLE_WINDOW));
    assert!(
        !gate.ready("rev-a", start + Duration::from_millis(100), SETTLE_WINDOW),
        "still inside the settle window"
    );
    assert!(
        gate.ready("rev-a", start + Duration::from_millis(300), SETTLE_WINDOW),
        "quiet for longer than the window"
    );

    // A new revision restarts the window.
    let later = start + Duration::from_millis(400);
    assert!(!gate.ready("rev-b", later, SETTLE_WINDOW));
    assert!(!gate.ready("rev-b", later + Duration::from_millis(100), SETTLE_WINDOW));
    assert!(gate.ready("rev-b", later + Duration::from_millis(300), SETTLE_WINDOW));
}

/// W01/D01: a half-written file is reported pending; the last good snapshot stays.
#[test]
fn half_written_file_keeps_last_good_and_then_publishes() {
    let dir = temp_dir("half_written");
    let path = dir.join("testament_new.sav");
    std::fs::write(&path, baseline_bytes()).unwrap();

    let mut reader = StableReader::default();
    reader.open(&path).unwrap();

    let start = Instant::now();
    // First poll: the revision is new, so the settle window is not satisfied yet.
    let first = reader.poll(start).expect("a pending event");
    assert!(matches!(first, MonitorEvent::Pending { .. }));

    // Now a half-written file appears.
    let full = head_b01_bytes();
    std::fs::write(&path, &full[..full.len() / 2]).unwrap();
    let half = reader.poll(start + Duration::from_millis(400)).unwrap();
    assert!(
        matches!(
            half,
            MonitorEvent::Pending {
                likely_write_in_progress: true,
                ..
            } | MonitorEvent::ReadError { .. }
        ),
        "a truncated file must never be published as a snapshot: {half:?}"
    );

    // The complete file lands and stays stable.
    std::fs::write(&path, &full).unwrap();
    let pending = reader.poll(start + Duration::from_millis(500)).unwrap();
    assert!(matches!(pending, MonitorEvent::Pending { .. }));

    let settled = reader
        .poll(start + Duration::from_millis(900))
        .expect("snapshot after the settle window");
    match settled {
        MonitorEvent::Snapshot { snapshot, .. } => {
            assert_eq!(snapshot.sha256.len(), 64);
            assert!(snapshot.writable);
        }
        other => panic!("expected a snapshot, got {other:?}"),
    }

    // The same revision is not published twice.
    assert!(reader.poll(start + Duration::from_millis(1200)).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// D02: the game replaces the file by rename; the tool must follow it.
#[test]
fn rename_replace_is_followed_without_holding_a_handle() {
    let dir = temp_dir("rename_replace");
    let path = dir.join("testament_new.sav");
    std::fs::write(&path, baseline_bytes()).unwrap();

    let mut reader = StableReader::default();
    reader.open(&path).unwrap();
    let start = Instant::now();
    let _ = reader.poll(start);
    let published = reader.poll(start + Duration::from_millis(400)).unwrap();
    assert!(matches!(published, MonitorEvent::Snapshot { .. }));

    // Simulate the game's write: write a sibling then rename it over the target.
    let staged = dir.join("testament_new.sav.tmp");
    std::fs::write(&staged, head_b01_bytes()).unwrap();
    std::fs::rename(&staged, &path).unwrap();

    let pending = reader.poll(start + Duration::from_millis(600)).unwrap();
    assert!(matches!(pending, MonitorEvent::Pending { .. }));
    let settled = reader.poll(start + Duration::from_millis(1000)).unwrap();
    match settled {
        MonitorEvent::Snapshot { changed, .. } => assert!(changed),
        other => panic!("expected the replaced revision, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// D05/W03: a late event from a previously opened document carries the old generation.
#[test]
fn switching_documents_bumps_generation_and_old_events_are_identifiable() {
    let dir = temp_dir("generation");
    let path_a = dir.join("a.sav");
    let path_b = dir.join("b.sav");
    std::fs::write(&path_a, baseline_bytes()).unwrap();
    std::fs::write(&path_b, head_b01_bytes()).unwrap();

    let mut reader = StableReader::default();
    reader.open(&path_a).unwrap();
    let start = Instant::now();
    let _ = reader.poll(start);
    let event_a = reader.poll(start + Duration::from_millis(400)).unwrap();
    let generation_a = event_a.generation();

    reader.open(&path_b).unwrap();
    let generation_b = reader.generation();
    assert!(
        generation_b > generation_a,
        "opening another document must bump the generation"
    );

    // An event produced for A must be rejectable by comparing generations.
    let is_stale =
        |event: &MonitorEvent, current: DocumentGeneration| event.generation() != current;
    assert!(is_stale(&event_a, generation_b));
    assert!(!is_stale(&event_a, generation_a));

    let _ = reader.poll(start + Duration::from_millis(500));
    let event_b = reader.poll(start + Duration::from_millis(900)).unwrap();
    assert_eq!(event_b.generation(), generation_b);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Re-opening the same path does not bump the generation.
#[test]
fn reopening_the_same_path_keeps_the_generation() {
    let dir = temp_dir("same_path");
    let path = dir.join("same.sav");
    std::fs::write(&path, baseline_bytes()).unwrap();

    let mut reader = StableReader::default();
    reader.open(&path).unwrap();
    let first = reader.generation();
    reader.open(&path).unwrap();
    assert_eq!(reader.generation(), first);

    let _ = std::fs::remove_dir_all(&dir);
}

/// U01/D07: monitoring must never modify the file it watches.
#[test]
fn monitoring_never_writes_to_the_watched_file() {
    let dir = temp_dir("read_only");
    let path = dir.join("testament_new.sav");
    std::fs::write(&path, baseline_bytes()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let before_meta = std::fs::metadata(&path).unwrap().modified().unwrap();

    let mut reader = StableReader::default();
    reader.open(&path).unwrap();
    let start = Instant::now();
    for step in 0..6 {
        let _ = reader.poll(start + Duration::from_millis(step * 300));
    }

    let after = std::fs::read(&path).unwrap();
    let after_meta = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(before, after, "watched file bytes must not change");
    assert_eq!(before_meta, after_meta, "mtime must not change");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A missing file is a hard, reportable error rather than a silent no-op.
#[test]
fn missing_file_reports_a_read_error() {
    let dir = temp_dir("missing");
    let path = dir.join("absent.sav");
    // Create then delete so the identity can be resolved once.
    std::fs::write(&path, baseline_bytes()).unwrap();
    let mut reader = StableReader::default();
    reader.open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();

    let event = reader.poll(Instant::now()).unwrap();
    assert!(
        matches!(event, MonitorEvent::ReadError { .. }),
        "expected a read error, got {event:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The path identity distinguishes two different files with the same name.
#[test]
fn path_identity_is_not_just_the_file_name() {
    let dir = temp_dir("identity");
    let first = dir.join("one").join("testament_new.sav");
    let second = dir.join("two").join("testament_new.sav");
    std::fs::create_dir_all(first.parent().unwrap()).unwrap();
    std::fs::create_dir_all(second.parent().unwrap()).unwrap();
    std::fs::write(&first, baseline_bytes()).unwrap();
    std::fs::write(&second, baseline_bytes()).unwrap();

    let identity_a = local_io::PathIdentity::from_path(&first).unwrap();
    let identity_b = local_io::PathIdentity::from_path(&second).unwrap();
    assert!(
        !identity_a.same_file(&identity_b),
        "identical file names in different directories are different documents"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
