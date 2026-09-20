//! UI interaction tests.
//!
//! These build the real `WorkspaceView` inside GPUI's headless test app and
//! dispatch real pointer events at rendered elements, so the click handlers in
//! `workspace.rs` are exercised rather than assumed to work.
//!
//! Scope note: this file covers view state transitions (slot selection, body
//! lock, button enablement, watcher toggling, never writing to a watched file).
//! File dialogs and the save transaction are covered by the `local_io`
//! integration tests, which drive the same functions without a window.

use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    point, px, size, AppContext as _, Modifiers, MouseButton, TestAppContext, VisualTestContext,
};
use hd2_armor_desk::ui::{configure_theme, state::AppState, WorkspaceView};
use loadout_domain::{Catalog, Classification, Item, ItemType};
use local_io::Workspace;
use sav_codec::{fields, SaveImage};

/// A per-test scratch workspace, never the executable's real `workspace` directory.
///
/// Each test gets its own directory: the tests run in parallel and several of
/// them create backups, so a shared root would leak state between them.
static SCRATCH_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn scratch_root() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "hd2-armor-desk-ui-tests/{}-{seq}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&root);
    root
}

/// `HD2_ARMOR_DESK_WORKSPACE` is process-global, so redirected tests must not overlap.
///
/// Holding this for the whole test body is what makes the redirect sound; the
/// tests themselves are fast, so serializing them costs nothing.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Point the app at a private scratch workspace for the duration of one test.
///
/// Production uses `<executable directory>\workspace`; tests set the explicit
/// override before constructing the view so they never touch build artifacts.
struct ScratchEnv {
    previous_workspace: Option<std::ffi::OsString>,
    previous_steam_library: Option<String>,
    root: PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl ScratchEnv {
    fn install() -> ScratchEnv {
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_workspace = std::env::var_os("HD2_ARMOR_DESK_WORKSPACE");
        let previous_steam_library = std::env::var("STEAM_LIBRARY").ok();
        let root = scratch_root();
        // SAFETY: `ENV_LOCK` is held for this test's whole body, so no other
        // test reads or writes these variables concurrently.
        unsafe { std::env::set_var("HD2_ARMOR_DESK_WORKSPACE", &root) };
        ScratchEnv {
            previous_workspace,
            previous_steam_library,
            root,
            _guard: guard,
        }
    }

    /// The workspace root this test is using.
    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for ScratchEnv {
    fn drop(&mut self) {
        match self.previous_workspace.take() {
            Some(value) => unsafe { std::env::set_var("HD2_ARMOR_DESK_WORKSPACE", value) },
            None => unsafe { std::env::remove_var("HD2_ARMOR_DESK_WORKSPACE") },
        }
        match self.previous_steam_library.take() {
            Some(value) => unsafe { std::env::set_var("STEAM_LIBRARY", value) },
            None => unsafe { std::env::remove_var("STEAM_LIBRARY") },
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures")
        .join(name)
}

/// Build the workspace view in a headless test window.
///
/// This mirrors `main` exactly: `gpui_kit::init` installs the component theme
/// and asset source, and the view is wrapped in `Root`, which owns the dialog
/// and notification overlay layers. Without `Root` the first `open_dialog` call
/// panics, so testing it any other way would not exercise the real window.
fn build(cx: &mut TestAppContext) -> (gpui_kit::Entity<WorkspaceView>, VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        configure_theme(cx);
        // Dialogs animate in by default; settling on the first frame keeps the
        // tests deterministic without asserting anything about motion.
        cx.set_reduce_motion(true);
    });
    let mut view: Option<gpui_kit::Entity<WorkspaceView>> = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| WorkspaceView::new(window, cx));
        view = Some(workspace.clone());
        gpui_kit::component::Root::new(workspace, window, cx)
    });
    let view = view.expect("窗口构建时应当建立 WorkspaceView");
    let cx = cx.to_owned();
    (view, cx)
}

/// Open a fixture and wait for the background decode to land.
fn open(view: &gpui_kit::Entity<WorkspaceView>, cx: &mut VisualTestContext, name: &str) {
    let path = fixture(name);
    cx.update(|_, cx| {
        view.update(cx, |view, cx| view.open_initial_path(path, cx));
    });
    cx.run_until_parked();
}

/// Paint one frame so layout facts reflect the latest state.
///
/// `debug_bounds` reads the last *painted* frame; a state change alone does not
/// populate it, so every layout assertion and click goes through here first.
fn render(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.render_frame(cx));
}

/// Click the element registered under `selector`.
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    render(cx);
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("element {selector} was not rendered"));
    let center = point(
        bounds.origin.x + bounds.size.width / 2.0,
        bounds.origin.y + bounds.size.height / 2.0,
    );
    cx.simulate_mouse_move(center, None, Modifiers::default());
    cx.simulate_click(center, Modifiers::default());
    cx.run_until_parked();
}

#[gpui_kit::test]
fn opening_a_save_populates_both_slot_cards(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    let (view, mut cx) = build(cx);
    open(&view, &mut cx, "valid_baseline.bin");

    let (head, body, writable) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        let snapshot = state.snapshot.as_ref().expect("存档应已解码");
        (snapshot.head_id(), snapshot.body_id(), snapshot.writable)
    });

    // Values from tests/fixtures/manifest.json for valid_baseline.bin.
    assert_eq!(head, Some(0x0568_48E9), "头部槽位应读出存档中的 ID");
    assert_eq!(body, Some(0xD346_1392), "身体槽位应读出存档中的 ID");
    assert!(writable, "合成基线是可写布局");

    // Both slot cards and the browser must be laid out.
    render(&mut cx);
    assert!(cx.debug_bounds("card-head").is_some(), "头部卡片应渲染");
    assert!(cx.debug_bounds("card-body").is_some(), "身体卡片应渲染");
    assert!(cx.debug_bounds("action-commit").is_some(), "写回按钮应渲染");
    assert!(cx.debug_bounds("action-undo").is_some(), "撤销按钮应渲染");
}

#[gpui_kit::test]
fn collected_body_armor_can_be_selected_for_the_head_slot(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    let (view, mut cx) = build(cx);
    open(&view, &mut cx, "valid_baseline.bin");

    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.state().update(cx, |state, cx| {
                state.head_query = "0x01C2A674".to_string();
                cx.notify();
            });
        });
    });
    cx.simulate_resize(size(px(1280.0), px(1800.0)));
    click(&mut cx, "pick-armor:0x01C2A674");

    let (selected, status) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        (state.head_item_key(), state.status.clone())
    });
    assert_eq!(selected.as_deref(), Some("armor:0x01C2A674"));
    assert!(
        !status.is_error,
        "目录中的护甲应可被普通模式选择：{status:?}"
    );
}

#[gpui_kit::test]
fn risk_write_rebases_the_head_choice_onto_the_games_new_body_value(cx: &mut TestAppContext) {
    let env = ScratchEnv::install();
    let live = env.root().join("risk_write.sav");
    std::fs::copy(fixture("valid_baseline.bin"), &live).unwrap();

    let (view, mut cx) = build(cx);
    cx.update(|_, cx| {
        view.update(cx, |view, cx| view.open_initial_path(live.clone(), cx));
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.state().update(cx, |state, cx| {
                state.head_query = "0x01C2A674".to_string();
                cx.notify();
            });
        });
    });
    cx.simulate_resize(size(px(1280.0), px(1800.0)));
    click(&mut cx, "pick-armor:0x01C2A674");

    // The game changes body armor after this draft was created.
    std::fs::copy(fixture("valid_body_b01.bin"), &live).unwrap();
    click(&mut cx, "action-commit");
    assert!(
        cx.update(|window, cx| window.has_active_dialog(cx)),
        "写回应先显示模式确认"
    );
    click(&mut cx, "action-force-latest");

    let (verified, dirty, busy) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        (
            state
                .last_commit
                .as_ref()
                .map(|receipt| receipt.is_verified())
                .unwrap_or(false),
            state.is_dirty(),
            state.save_in_flight,
        )
    });
    assert!(verified, "风险写入应完成回读校验");
    assert!(!dirty, "成功写回后草稿应变为干净");
    assert!(!busy, "写回任务应结束");

    let written = SaveImage::decode(std::fs::read(&live).unwrap()).unwrap();
    assert_eq!(
        written.read_u32(fields::HEAD).unwrap(),
        0x01C2_A674,
        "用户选择的头部护甲必须写入"
    );
    assert_eq!(
        written.read_u32(fields::BODY).unwrap(),
        0x61B3_1723,
        "游戏刚切换的身体护甲必须保留"
    );
}

#[gpui_kit::test]
fn reopening_the_same_save_completes_instead_of_stalling(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    let (view, mut cx) = build(cx);
    open(&view, &mut cx, "valid_baseline.bin");
    open(&view, &mut cx, "valid_baseline.bin");

    let status = cx.update(|_, cx| view.read(cx).state().read(cx).status.text.clone());
    assert!(
        status.starts_with("已打开"),
        "same-path reload must complete; got {status:?}"
    );
}

#[gpui_kit::test]
fn steam_discovery_populates_the_first_dialog_it_opens(cx: &mut TestAppContext) {
    let env = ScratchEnv::install();
    let steam = env.root().join("Steam");
    let save = steam
        .join("userdata")
        .join("123456")
        .join("553850")
        .join("remote")
        .join("testament_new.sav");
    std::fs::create_dir_all(save.parent().unwrap()).unwrap();
    std::fs::copy(fixture("valid_baseline.bin"), &save).unwrap();
    // SAFETY: ScratchEnv holds ENV_LOCK until the test ends.
    unsafe { std::env::set_var("STEAM_LIBRARY", &steam) };

    let (view, mut cx) = build(cx);
    render(&mut cx);
    cx.update(|window, cx| window.click("discover", cx));
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| window.has_active_dialog(cx)),
        "discovery must open a dialog on the first click"
    );

    let candidate_index = cx.update(|_, cx| {
        view.read(cx)
            .state()
            .read(cx)
            .candidates
            .iter()
            .position(|candidate| candidate.path == save)
    });
    let candidate_index = candidate_index.expect("the configured Steam library should be found");
    render(&mut cx);
    let selector = match candidate_index {
        0 => "open-cand-0",
        1 => "open-cand-1",
        2 => "open-cand-2",
        other => panic!("unexpected candidate index {other}"),
    };
    cx.update(|window, cx| window.click(selector, cx));
    cx.run_until_parked();
    let opened = cx.update(|_, cx| {
        view.read(cx)
            .state()
            .read(cx)
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.path == save.display().to_string())
            .unwrap_or(false)
    });
    assert!(
        opened,
        "the first dialog must open its discovered candidate"
    );
}

#[gpui_kit::test]
fn body_starts_unlocked_and_can_be_locked_without_writing(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    let (view, mut cx) = build(cx);
    open(&view, &mut cx, "valid_baseline.bin");

    // Opening a save creates a clean draft with both armor slots editable.
    let (has_draft, dirty, body_locked) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        (
            state.draft.is_some(),
            state.is_dirty(),
            state.draft.as_ref().map(|draft| draft.body_locked()),
        )
    });
    assert!(has_draft, "打开存档后应建立草稿");
    assert!(!dirty, "刚打开存档不应有改动");
    assert_eq!(body_locked, Some(false), "身体必须默认解锁");

    // Locking is still available as an explicit protection and must not write.
    click(&mut cx, "toggle-body-lock");

    let (locked_now, dirty, in_flight) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        (
            state.draft.as_ref().map(|draft| draft.body_locked()),
            state.is_dirty(),
            state.save_in_flight,
        )
    });
    assert_eq!(locked_now, Some(true), "点击后身体应锁定");
    assert!(
        !dirty,
        "只锁定不改值：身体仍是存档中的值，不应产生需要写回的改动"
    );
    assert!(!in_flight, "锁定身体绝不能触发写回");
}

#[gpui_kit::test]
fn selecting_the_body_slot_then_the_head_slot_moves_the_target(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    let (view, mut cx) = build(cx);
    open(&view, &mut cx, "valid_baseline.bin");

    let initial = cx.update(|_, cx| view.read(cx).state().read(cx).target_slot);
    assert_eq!(
        initial,
        hd2_armor_desk::ui::state::TargetSlot::Head,
        "默认应在头部槽位选择"
    );

    click(&mut cx, "card-body");
    let after = cx.update(|_, cx| view.read(cx).state().read(cx).target_slot);
    assert_eq!(
        after,
        hd2_armor_desk::ui::state::TargetSlot::Body,
        "点击身体卡片后应切换选择目标"
    );

    click(&mut cx, "card-head");
    let back = cx.update(|_, cx| view.read(cx).state().read(cx).target_slot);
    assert_eq!(
        back,
        hd2_armor_desk::ui::state::TargetSlot::Head,
        "点击头部卡片后应切回头部"
    );
}

#[gpui_kit::test]
fn a_clean_draft_keeps_write_back_disabled(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    let (view, mut cx) = build(cx);
    open(&view, &mut cx, "valid_baseline.bin");

    // Clicking 写回 with no changes must not start a commit.
    click(&mut cx, "action-commit");

    let (in_flight, last_commit) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        (state.save_in_flight, state.last_commit.is_some())
    });
    assert!(!in_flight, "无改动时不应启动写回任务");
    assert!(!last_commit, "无改动时不应产生写回记录");
}

#[gpui_kit::test]
fn the_watcher_toggle_flips_and_never_touches_the_file(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    let (view, mut cx) = build(cx);

    // Watch a copy: the watcher must not modify what it watches.
    let watched = _env.root().join("watch_copy.bin");
    std::fs::copy(fixture("valid_baseline.bin"), &watched).unwrap();
    let before = std::fs::read(&watched).unwrap();

    cx.update(|_, cx| {
        view.update(cx, |view, cx| view.open_initial_path(watched.clone(), cx));
    });
    cx.run_until_parked();

    click(&mut cx, "toolbar-watch");
    let watching = cx.update(|_, cx| view.read(cx).state().read(cx).watching);
    assert!(watching, "点击后应开始监视");

    click(&mut cx, "toolbar-watch");
    let stopped = cx.update(|_, cx| view.read(cx).state().read(cx).watching);
    assert!(!stopped, "再次点击应停止监视");

    assert_eq!(
        std::fs::read(&watched).unwrap(),
        before,
        "只读监视绝不能改动被监视的文件"
    );
}

#[gpui_kit::test]
fn an_unknown_layout_offers_no_write_back(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    let (view, mut cx) = build(cx);
    open(&view, &mut cx, "unknown_layout_valid.bin");

    let (writable, reason) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        let snapshot = state.snapshot.as_ref().expect("存档应已解码");
        (snapshot.writable, snapshot.readonly_reason.clone())
    });
    assert!(!writable, "unknown-layout 必须是只读的");
    assert!(reason.is_some(), "只读必须给出理由");

    // Clicking write-back on a read-only document must not start a task.
    click(&mut cx, "action-commit");
    let in_flight = cx.update(|_, cx| view.read(cx).state().read(cx).save_in_flight);
    assert!(!in_flight, "只读文档绝不能启动写回");
}

#[gpui_kit::test]
fn an_empty_catalog_never_claims_a_type_for_the_head_slot(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    let (view, mut cx) = build(cx);
    // valid_head_armor.bin carries a body-armor ID in the head slot.
    open(&view, &mut cx, "valid_head_armor.bin");

    let (head, claimed) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        (
            state
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.head_id()),
            state.head_is_armor_in_helmet_slot(),
        )
    });
    assert_eq!(head, Some(0xD346_1392));
    assert!(!claimed, "目录为空时不能断言头部是身体护甲，只能显示未知");
}

#[gpui_kit::test]
fn restore_is_reachable_and_whole_file_scoped(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    let (view, mut cx) = build(cx);
    open(&view, &mut cx, "valid_baseline.bin");

    // The whole-file restore is a distinct, reachable action. It must not be
    // confused with the card's head-only "恢复普通头盔" button.
    render(&mut cx);
    assert!(
        cx.debug_bounds("toolbar-restore").is_some(),
        "恢复整份备份按钮应存在"
    );

    // Opening the dialog with no backups must not start a task or touch disk.
    let save_path = fixture("valid_baseline.bin");
    let before = std::fs::read(&save_path).unwrap();
    click(&mut cx, "toolbar-restore");

    let (in_flight, backup_count) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        (state.save_in_flight, state.workspace.list_backups().len())
    });
    assert!(!in_flight, "仅打开恢复对话框不应启动恢复任务");
    assert_eq!(backup_count, 0, "scratch 工作区不应有备份");
    assert_eq!(
        std::fs::read(&save_path).unwrap(),
        before,
        "打开恢复对话框绝不能改动源文件"
    );
}

#[gpui_kit::test]
fn recording_a_restore_rebuilds_the_document_from_disk(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    let (view, mut cx) = build(cx);

    // A copy we are allowed to overwrite, seeded with the baseline save.
    let live = _env.root().join("restore_live.bin");
    std::fs::copy(fixture("valid_baseline.bin"), &live).unwrap();

    cx.update(|_, cx| {
        view.update(cx, |view, cx| view.open_initial_path(live.clone(), cx));
    });
    cx.run_until_parked();

    let (head_before, sha_before) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        let snapshot = state.snapshot.as_ref().expect("应已打开");
        (snapshot.head_id(), snapshot.sha256.clone())
    });
    assert_eq!(head_before, Some(0x0568_48E9), "初始头部 ID");

    // A backup holding a different save. `restore_backup` itself is covered by
    // the local_io transaction tests; this test is about the UI layer's
    // `record_restore` rebuilding the open document from what is now on disk.
    let backup_root = cx.update(|_, cx| view.read(cx).state().read(cx).workspace.backups_dir());
    std::fs::create_dir_all(&backup_root).unwrap();
    let backup = backup_root.join("known_backup.sav");
    std::fs::copy(fixture("valid_head_armor.bin"), &backup).unwrap();

    let receipt = local_io::restore_backup(&backup, &live, &backup_root).expect("恢复应当成功");

    // The restored file must be the backup byte for byte, and a copy of what it
    // replaced must have been kept.
    assert_eq!(
        std::fs::read(&live).unwrap(),
        std::fs::read(&backup).unwrap(),
        "恢复后源文件应与备份逐字节相同"
    );
    let names: Vec<String> = std::fs::read_dir(&backup_root)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        names.iter().any(|name| name.starts_with("before_restore_")),
        "恢复前必须留下副本，实际内容：{names:?}"
    );

    // Recording it must rebuild the snapshot and draft from disk, not patch the
    // old ones: the whole file changed, so any pending edit is meaningless.
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.state().update(cx, |state, _| {
                let generation = state.reader.generation().0;
                state.record_restore(generation, receipt);
            });
        });
    });

    let (head_after, sha_after, dirty, has_draft, in_flight) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        let snapshot = state.snapshot.as_ref().expect("恢复后应仍有快照");
        (
            snapshot.head_id(),
            snapshot.sha256.clone(),
            state.is_dirty(),
            state.draft.is_some(),
            state.save_in_flight,
        )
    });

    assert_eq!(
        head_after,
        Some(0xD346_1392),
        "恢复后快照应反映备份中的头部 ID（valid_head_armor.bin）"
    );
    assert_ne!(sha_after, sha_before, "恢复后 SHA-256 应更新");
    assert!(has_draft, "恢复后应重建草稿");
    assert!(!dirty, "恢复后草稿应为干净状态");
    assert!(!in_flight, "恢复完成后不应仍在写回中");
}

#[gpui_kit::test]
fn stale_restore_receipt_does_not_replace_a_new_document(cx: &mut TestAppContext) {
    let env = ScratchEnv::install();
    let first = env.root().join("first.bin");
    let second = env.root().join("second.bin");
    std::fs::copy(fixture("valid_baseline.bin"), &first).unwrap();
    std::fs::copy(fixture("valid_baseline.bin"), &second).unwrap();

    let (view, mut cx) = build(cx);
    cx.update(|_, cx| {
        view.update(cx, |view, cx| view.open_initial_path(first.clone(), cx));
    });
    cx.run_until_parked();
    let first_generation = cx.update(|_, cx| view.read(cx).state().read(cx).reader.generation().0);

    let backup_root = env.root().join("backups");
    std::fs::create_dir_all(&backup_root).unwrap();
    let backup = backup_root.join("different.sav");
    std::fs::copy(fixture("valid_head_armor.bin"), &backup).unwrap();
    let receipt = local_io::restore_backup(&backup, &first, &backup_root).expect("恢复应成功");

    cx.update(|_, cx| {
        view.update(cx, |view, cx| view.open_initial_path(second.clone(), cx));
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.state().update(cx, |state, _| {
                state.record_restore(first_generation, receipt)
            });
        });
    });

    let (path, head, busy) = cx.update(|_, cx| {
        let state = view.read(cx).state().read(cx);
        let snapshot = state.snapshot.as_ref().expect("第二份存档应保持打开");
        (
            snapshot.path.clone(),
            snapshot.head_id(),
            state.save_in_flight,
        )
    });
    assert_eq!(PathBuf::from(path), second);
    assert_eq!(head, Some(0x0568_48E9));
    assert!(!busy, "旧任务完成后应释放忙碌状态");
}

#[gpui_kit::test]
fn first_run_seeds_the_bundled_v2_catalog(cx: &mut TestAppContext) {
    let env = ScratchEnv::install();
    cx.update(gpui_kit::init);
    let state = cx.new(|_| AppState::new());

    let (items, armors, verified_armors, catalog_path) = cx.update(|cx| {
        let state = state.read(cx);
        (
            state.catalog.len(),
            state
                .catalog
                .items()
                .iter()
                .filter(|item| item.item_type == ItemType::Armor)
                .count(),
            state
                .catalog
                .items()
                .iter()
                .filter(|item| {
                    item.item_type == ItemType::Armor
                        && item.classification == Classification::UserVerified
                })
                .count(),
            state.workspace.catalog_path(),
        )
    });
    assert_eq!(items, 93, "应完整加载由 schema 1 转换的内置目录");
    assert_eq!(armors, 69, "内置目录应包含已观察到的身体护甲");
    assert_eq!(
        verified_armors, 69,
        "本人手工采集的身体护甲应直接标记为已确认"
    );
    assert_eq!(catalog_path, env.root().join("catalog.v2.json"));
    let persisted = std::fs::read_to_string(catalog_path).expect("首次启动应落盘内置目录");
    assert_eq!(Catalog::from_document_json(&persisted).unwrap().len(), 93);
}

#[gpui_kit::test]
fn existing_portable_catalog_takes_precedence_over_the_bundle(cx: &mut TestAppContext) {
    let env = ScratchEnv::install();
    let workspace = Workspace::open(env.root()).unwrap();
    let mut item = Item::new(ItemType::Armor, 0xAABB_CCDD, Classification::UserVerified);
    item.display_name = "玩家自定义护甲".to_string();
    workspace
        .save_catalog(&Catalog::from_items(vec![item]).unwrap())
        .unwrap();

    cx.update(gpui_kit::init);
    let state = cx.new(|_| AppState::new());
    cx.update(|cx| {
        let catalog = &state.read(cx).catalog;
        assert_eq!(catalog.len(), 1, "已有目录不能与内置目录强制合并");
        assert!(catalog.get("armor:0xAABBCCDD").is_some());
    });
}

#[gpui_kit::test]
fn existing_seeded_catalog_promotes_collected_armors_to_user_verified(cx: &mut TestAppContext) {
    let env = ScratchEnv::install();
    let workspace = Workspace::open(env.root()).unwrap();
    let mut item = Item::new(
        ItemType::Armor,
        0x01C2_A674,
        Classification::InferredFromBodyObservation,
    );
    item.display_name = "AD-26 血刃科技".to_string();
    workspace
        .save_catalog(&Catalog::from_items(vec![item]).unwrap())
        .unwrap();

    cx.update(gpui_kit::init);
    let state = cx.new(|_| AppState::new());
    cx.update(|cx| {
        let item = state
            .read(cx)
            .catalog
            .get("armor:0x01C2A674")
            .expect("采集目录条目应保留");
        assert_eq!(item.classification, Classification::UserVerified);
    });

    let persisted = workspace.load_catalog().unwrap();
    assert_eq!(
        persisted
            .get("armor:0x01C2A674")
            .expect("升级后的目录应保存")
            .classification,
        Classification::UserVerified
    );
}

#[gpui_kit::test]
fn app_state_defaults_are_safe(cx: &mut TestAppContext) {
    let _env = ScratchEnv::install();
    cx.update(gpui_kit::init);
    let state = cx.new(|_| AppState::new());

    let (watching, in_flight, has_snapshot, has_draft, diagnostics) = cx.update(|cx| {
        let state = state.read(cx);
        (
            state.watching,
            state.save_in_flight,
            state.snapshot.is_some(),
            state.draft.is_some(),
            state.show_diagnostics,
        )
    });

    assert!(!watching, "默认不监视");
    assert!(!in_flight, "默认没有写回任务");
    assert!(!has_snapshot, "默认没有打开存档");
    assert!(!has_draft, "默认没有草稿");
    assert!(!diagnostics, "诊断默认收起");

    // Keep the imports used by the click helper honest about their types.
    let _ = MouseButton::Left;
    let _ = px(0.0);
}
