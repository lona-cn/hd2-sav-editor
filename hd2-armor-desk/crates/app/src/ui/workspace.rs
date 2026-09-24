//! The main window: two armor cards, a searchable catalog, presets and the save
//! transaction controls.
//!
//! Threading contract:
//! * the view owns no I/O state — everything lives in [`AppState`];
//! * background work is spawned on the entity's context and results are applied
//!   through [`WorkspaceView::apply_background`], which re-checks the generation;
//! * `window.spawn` is used when a dialog needs the window for notifications.

use std::path::PathBuf;

use crate::legal::{self, NOTICE_INTRO, NOTICE_RESPONSIBILITY, NOTICE_SAVE_RISK, NOTICE_TITLE};
use crate::update::{
    self, ReleaseInfo, UpdateCheck, UpdateState, CURRENT_RELEASE_TAG, RELEASES_PAGE_URL,
};
use gpui_kit::base::{Disableable as _, StyledExt as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::Root;
use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    div, prelude::FluentBuilder as _, px, rgb, App, AppContext as _, Context, Div, Entity,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window,
};
use loadout_domain::{HumanReadableDiff, ItemType, LoadoutIntent, SlotIntent, Snapshot};

use super::state::{
    apply_import, format_bytes, list_backups, player_usable, preset_from_draft, slot_accepts,
    slot_rejection, spawn_commit, spawn_copy, spawn_discover, spawn_import_catalog,
    spawn_open_save, spawn_prepare, spawn_restore, spawn_watch, AppState, BackgroundResult,
    CandidateRow, CommitMode, StatusLine, TargetSlot,
};

const BG: u32 = 0x08151D;
const CARD_BG: u32 = 0x10212A;
const CARD_RAISED: u32 = 0x192F39;
const CARD_BORDER: u32 = 0x30434C;
const TEXT: u32 = 0xF1F3EE;
const MUTED: u32 = 0xA5B4B9;
const SUBTLE: u32 = 0x789098;
const ACCENT: u32 = 0xFFE710;
const ACCENT_DIM: u32 = 0x34371B;
const DANGER: u32 = 0xF06B67;
const DANGER_DIM: u32 = 0x321B1E;
const OK: u32 = 0x65D49A;
const OK_DIM: u32 = 0x173226;
const INFO: u32 = 0x74BED0;

type LegalAcknowledgementWriter = fn() -> std::io::Result<()>;

fn legal_notice_section(
    number: &'static str,
    heading: &'static str,
    text: &'static str,
    accent: u32,
) -> Div {
    div()
        .flex()
        .items_start()
        .gap_3()
        .p_4()
        .rounded_md()
        .border_1()
        .border_color(rgb(CARD_BORDER))
        .bg(rgb(CARD_BG))
        .child(
            div()
                .w(px(30.0))
                .h(px(30.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(accent))
                .text_color(rgb(BG))
                .text_sm()
                .font_semibold()
                .child(number),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(rgb(TEXT))
                        .child(heading),
                )
                .child(div().text_sm().text_color(rgb(MUTED)).child(text)),
        )
}

pub struct WorkspaceView {
    state: Entity<AppState>,
    search_input: Entity<InputState>,
    preset_name_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
    legal_acknowledgement_required: bool,
    pending_initial_path: Option<PathBuf>,
}

#[derive(Clone)]
struct CommitRequest {
    snapshot: Snapshot,
    diff: HumanReadableDiff,
    generation: u64,
}

struct BrowserRow {
    key: String,
    name: String,
    type_label: String,
    id_hex: String,
    passive_name: Option<String>,
    passive_description: Option<String>,
    usable: bool,
    selected: bool,
}

fn begin_commit(
    state_entity: Entity<AppState>,
    request: CommitRequest,
    mode: CommitMode,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let generation = request.generation;
    let source = PathBuf::from(&request.snapshot.path);
    let patches = request.diff.patches.clone();
    let snapshot = request.snapshot;
    let diff = request.diff;
    let task = state_entity.update(cx, |state, cx| {
        if state.save_in_flight || state.reader.generation().0 != generation {
            return None;
        }
        state.save_in_flight = true;
        state.status = StatusLine::info(match mode {
            CommitMode::Safe => "正在校验安全写回内容…".to_string(),
            CommitMode::ForceLatest => {
                "正在准备风险写入，将基于最新磁盘内容重新应用护甲选择…".to_string()
            }
        });
        cx.notify();
        Some(spawn_prepare(
            source, snapshot, patches, diff, generation, mode, cx,
        ))
    });
    let Some(task) = task else {
        return false;
    };

    let state_for_commit = state_entity.clone();
    window
        .spawn(cx, async move |window_cx| {
            let message = match task.await {
                Ok(prepared) => {
                    let source = prepared.source_path.clone();
                    let commit_task = state_for_commit.update(window_cx, |state, cx| {
                        if state.reader.generation().0 != prepared.generation {
                            state.save_in_flight = false;
                            return None;
                        }
                        let backups = state.workspace.backups_dir();
                        state.status = StatusLine::info(match mode {
                            CommitMode::Safe => {
                                format!("正在安全写回 {}（已生成独立备份）", source.display())
                            }
                            CommitMode::ForceLatest => format!(
                                "正在风险写入 {}（基于最新内容并生成独立备份）",
                                source.display()
                            ),
                        });
                        cx.notify();
                        Some(spawn_commit(prepared, backups, mode, cx))
                    });
                    match commit_task {
                        Some(commit_task) => match commit_task.await {
                            Ok(receipt) => {
                                let text = receipt.summary();
                                state_for_commit.update(window_cx, |state, _| {
                                    state.record_commit(generation, receipt);
                                });
                                text
                            }
                            Err(message) => {
                                state_for_commit.update(window_cx, |state, _| {
                                    state.save_in_flight = false;
                                    if state.reader.generation().0 == generation {
                                        state.status = StatusLine::error(message.clone());
                                    }
                                });
                                format!("写回失败：{message}")
                            }
                        },
                        None => "文档已切换，已取消旧文档的写回".to_string(),
                    }
                }
                Err(message) => {
                    state_for_commit.update(window_cx, |state, _| {
                        state.save_in_flight = false;
                        if state.reader.generation().0 == generation {
                            state.status = StatusLine::error(message.clone());
                        }
                    });
                    format!("准备写入失败：{message}")
                }
            };
            let _ = window_cx.update(|window, cx| {
                window.push_notification(Notification::new().message(message), cx)
            });
        })
        .detach();
    true
}

impl WorkspaceView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (acknowledged, registry_read_error) = match legal::is_acknowledged() {
            Ok(acknowledged) => (acknowledged, None),
            Err(error) => (false, Some(format!("无法读取当前用户的确认记录：{error}"))),
        };
        Self::new_with_legal_state(
            window,
            cx,
            acknowledged,
            legal::acknowledge,
            registry_read_error,
        )
    }

    /// Test seam for exercising the first-run modal without touching the user's registry.
    #[doc(hidden)]
    pub fn new_with_legal_acknowledgement(
        window: &mut Window,
        cx: &mut Context<Self>,
        acknowledged: bool,
        acknowledge: LegalAcknowledgementWriter,
    ) -> Self {
        Self::new_with_legal_state(window, cx, acknowledged, acknowledge, None)
    }

    fn new_with_legal_state(
        window: &mut Window,
        cx: &mut Context<Self>,
        acknowledged: bool,
        acknowledge: LegalAcknowledgementWriter,
        registry_read_error: Option<String>,
    ) -> Self {
        let state = cx.new(|_| AppState::new());
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索护甲名称或 ID"));
        let preset_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("预设名称，例如：双甲 · 蹂躏者"));

        let subscriptions = vec![
            cx.subscribe_in(
                &search_input,
                window,
                |view: &mut Self, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let query = view.search_input.read(cx).value().to_string();
                        view.state.update(cx, |state, cx| {
                            match state.target_slot {
                                TargetSlot::Head => state.head_query = query,
                                TargetSlot::Body => state.body_query = query,
                            }
                            cx.notify();
                        });
                    }
                },
            ),
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];

        if !acknowledged {
            cx.defer_in(window, move |_, window, cx| {
                let view = cx.entity();
                Self::open_legal_risk_dialog(
                    view,
                    acknowledge,
                    registry_read_error.clone(),
                    window,
                    cx,
                );
            });
        }

        WorkspaceView {
            state,
            search_input,
            preset_name_input,
            legal_acknowledgement_required: !acknowledged,
            pending_initial_path: None,
            _subscriptions: subscriptions,
        }
    }

    /// The shared application state.
    ///
    /// Exposed so integration tests can assert what the UI actually did after a
    /// dispatched click; the view itself still owns every mutation.
    pub fn state(&self) -> &Entity<AppState> {
        &self.state
    }

    fn start_update_check(&mut self, cx: &mut Context<Self>) {
        let should_start = self.state.update(cx, |state, cx| {
            if state.update_state.is_busy() {
                return false;
            }
            state.update_state = UpdateState::Checking;
            state.status = StatusLine::info("正在检查 GitHub 最新 Release…".to_string());
            cx.notify();
            true
        });
        if !should_start {
            return;
        }

        let task = cx.background_spawn(async move { update::check_for_update() });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.state.update(cx, |state, cx| {
                    match result {
                        Ok(UpdateCheck::UpToDate(release)) => {
                            state.status =
                                StatusLine::info(format!("当前已是最新版本：{}", release.tag));
                            state.update_state = UpdateState::UpToDate(release);
                        }
                        Ok(UpdateCheck::Available(release)) => {
                            state.status = StatusLine::info(format!("发现新版本：{}", release.tag));
                            state.update_state = UpdateState::Available(release);
                        }
                        Err(message) => {
                            state.status = StatusLine::error(format!("检查更新失败：{message}"));
                            state.update_state = UpdateState::CheckFailed(message);
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn start_update_download(
        &mut self,
        release: ReleaseInfo,
        destination: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let release_for_task = release.clone();
        self.state.update(cx, |state, cx| {
            state.status = StatusLine::info(format!("正在下载并校验 {}…", release.package.name));
            state.update_state = UpdateState::Downloading(release);
            cx.notify();
        });
        let task = cx.background_spawn(async move {
            update::download_release_to(&release_for_task, &destination)
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.state.update(cx, |state, cx| {
                    match result {
                        Ok(receipt) => {
                            state.status = StatusLine::info(format!(
                                "更新包已下载并通过 SHA-256 校验：{}",
                                receipt.path.display()
                            ));
                            state.update_state = UpdateState::Downloaded(receipt);
                        }
                        Err(message) => {
                            state.status = StatusLine::error(format!("下载更新失败：{message}"));
                            state.update_state = UpdateState::DownloadFailed(message);
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn open_releases_page(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = open::that_detached(RELEASES_PAGE_URL) {
            self.state.update(cx, |state, cx| {
                state.status = StatusLine::error(format!("无法打开 Releases 页面：{error}"));
                cx.notify();
            });
        }
    }

    // ---------------------------------------------------------------- opening

    /// Open a save from a chosen path.
    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let generation = self.state.update(cx, |state, cx| {
            if state.save_in_flight {
                state.status =
                    StatusLine::error("保存或恢复正在进行，完成前不能切换存档".to_string());
                cx.notify();
                return None;
            }
            let previous_generation = state.reader.generation().0;
            match state.reader.open(&path) {
                Ok(_) => {
                    let generation = state.reader.generation().0;
                    if generation != previous_generation && state.watching {
                        state.watching = false;
                        state.watch_task = None;
                    }
                    state.status = StatusLine::info(format!("正在读取 {}", path.display()));
                    cx.notify();
                    Some(generation)
                }
                Err(error) => {
                    state.status = StatusLine::error(format!("无法打开存档：{error}"));
                    cx.notify();
                    None
                }
            }
        });
        let Some(generation) = generation else {
            return;
        };
        let task = self
            .state
            .update(cx, |_, cx| spawn_open_save(path, generation, cx));
        cx.spawn(async move |view, cx| {
            let result = task.await;
            view.update(cx, |view, cx| view.apply_background(result, cx))
                .ok();
        })
        .detach();
    }

    /// Open a save passed on the command line, once the view exists.
    ///
    /// `open_path` needs a `Context<Self>`, which is only available after the
    /// entity is constructed; the window builder calls this immediately after.
    pub fn open_initial_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.legal_acknowledgement_required {
            self.pending_initial_path = Some(path);
            return;
        }
        self.open_path(path, cx);
    }

    /// Start the read-only watcher.
    fn start_watching(&mut self, cx: &mut Context<Self>) {
        let generation = self.state.update(cx, |state, cx| {
            state.watching = true;
            cx.notify();
            state.reader.generation().0
        });
        let weak = self.state.downgrade();
        let task = self
            .state
            .update(cx, |_, cx| spawn_watch(&weak, generation, cx));
        self.state
            .update(cx, |state, _| state.watch_task = Some(task));
    }

    // ------------------------------------------------------- background results

    /// Apply one background result, dropping anything from a stale generation.
    fn apply_background(&mut self, result: BackgroundResult, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            match result {
                BackgroundResult::Opened {
                    generation,
                    snapshot,
                    path,
                } => {
                    if state.reader.generation().0 != generation {
                        return;
                    }
                    state.reader.seed_accepted(snapshot.sha256.clone());
                    state.last_disk_revision = Some(snapshot.sha256.clone());
                    state.draft = Some(loadout_domain::Draft::new((*snapshot).clone()));
                    state.snapshot = Some(*snapshot);
                    state.status = StatusLine::info(format!("已打开 {}", path.display()));
                    state.refresh_diff();
                }
                BackgroundResult::OpenFailed {
                    generation,
                    message,
                } => {
                    if state.reader.generation().0 != generation {
                        return;
                    }
                    state.status = StatusLine::error(message);
                }
                BackgroundResult::ImportPreview(result) => match *result {
                    Ok(pending) => {
                        let count = pending.preview.items.len();
                        let issues = pending.preview.issues.len();
                        state.pending_import = Some(pending);
                        state.status = StatusLine::info(format!(
                            "目录预览：{count} 项可用，{issues} 条记录需注意"
                        ));
                    }
                    Err(message) => state.status = StatusLine::error(message),
                },
                BackgroundResult::Candidates(rows) => {
                    if rows.is_empty() {
                        state.status = StatusLine::info(
                            "未在 Steam userdata 目录发现存档；请手动选择 testament_new.sav"
                                .to_string(),
                        );
                    } else {
                        state.status = StatusLine::info(format!("发现 {} 个候选存档", rows.len()));
                    }
                    state.candidates = rows;
                }
            }
            cx.notify();
        });
    }

    // ------------------------------------------------------------- selection

    /// Select an item into the active slot.
    fn select_item(&mut self, item_key: &str, cx: &mut Context<Self>) {
        let key = item_key.to_string();
        self.state.update(cx, |state, cx| {
            let Some(item) = state.catalog.get(&key).cloned() else {
                return;
            };
            let slot = state.target_slot;
            if !player_usable(&item) {
                state.status = StatusLine::error(format!(
                    "“{}”缺少可用的装备类型，无法写入",
                    display_name(&item)
                ));
                cx.notify();
                return;
            }
            if !slot_accepts(slot, item.item_type) {
                state.status = StatusLine::error(slot_rejection(slot, item.item_type));
                cx.notify();
                return;
            }
            let item_ref = AppState::item_ref(&item);
            let label = format!("选择 {}", item_ref.label());
            let Some(draft) = state.draft.as_mut() else {
                state.status = StatusLine::error("请先打开一个存档".to_string());
                cx.notify();
                return;
            };
            match slot {
                TargetSlot::Head => draft.set_head(item_ref, label),
                TargetSlot::Body => {
                    if let Err(message) = draft.set_body(item_ref, label) {
                        state.status = StatusLine::error(message);
                        cx.notify();
                        return;
                    }
                }
            }
            state.status = StatusLine::info("选择已更新，查看下方改动预览".to_string());
            state.refresh_diff();
            cx.notify();
        });
    }

    /// Apply a preset onto the current snapshot.
    fn apply_preset(&mut self, preset_id: &str, cx: &mut Context<Self>) {
        let preset_id = preset_id.to_string();
        self.state.update(cx, |state, cx| {
            let Some(preset) = state.presets.get(&preset_id).cloned() else {
                return;
            };
            let Some(draft) = state.draft.as_mut() else {
                state.status = StatusLine::error("请先打开一个存档".to_string());
                cx.notify();
                return;
            };
            let resolution = loadout_domain::resolve_preset(&preset, &state.catalog);
            if !resolution.is_applicable() {
                state.status = StatusLine::error(format!(
                    "预设与当前目录冲突，未应用：{}",
                    resolution.warnings().join("；")
                ));
                cx.notify();
                return;
            }
            let warnings = resolution.warnings();
            draft.apply_intent(
                resolution.intent.clone(),
                format!("应用预设 {}", preset.name),
            );
            state.status = StatusLine::info(if warnings.is_empty() {
                format!("已应用预设“{}”", preset.name)
            } else {
                format!("已应用预设“{}”；{}", preset.name, warnings.join("；"))
            });
            state.refresh_diff();
            cx.notify();
        });
    }

    /// Apply the reviewed import.
    fn commit_pending_import(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let Some(pending) = state.pending_import.clone() else {
                return;
            };
            match apply_import(state, &pending) {
                Ok(added) => {
                    state.status = StatusLine::info(format!(
                        "已导入目录：新增 {added} 项，共 {} 项",
                        state.catalog.len()
                    ));
                    state.pending_import = None;
                }
                Err(message) => state.status = StatusLine::error(message),
            }
            cx.notify();
        });
    }

    fn open_legal_risk_dialog(
        view: Entity<Self>,
        acknowledge: LegalAcknowledgementWriter,
        registry_read_error: Option<String>,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.open_dialog(cx, move |dialog, _, _| {
            let acknowledged_view = view.clone();
            let mut dialog = dialog
                .title(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(DANGER_DIM))
                                .text_color(rgb(DANGER))
                                .text_xs()
                                .font_semibold()
                                .child("首次启动确认"),
                        )
                        .child(NOTICE_TITLE),
                )
                .width(px(760.0))
                .max_w(px(760.0))
                .margin_top(px(48.0))
                .close_button(false)
                .overlay_closable(false)
                .keyboard(false)
                .child(
                    div()
                        .px_4()
                        .py_3()
                        .rounded_md()
                        .bg(rgb(ACCENT_DIM))
                        .border_1()
                        .border_color(rgb(ACCENT))
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .child("继续前请完整阅读。只有下方确认按钮可以关闭此窗口。"),
                )
                .child(legal_notice_section("01", "非官方关系", NOTICE_INTRO, INFO))
                .child(legal_notice_section(
                    "02",
                    "修改存档的规则与数据风险",
                    NOTICE_SAVE_RISK,
                    ACCENT,
                ))
                .child(legal_notice_section(
                    "03",
                    "责任与法律意见声明",
                    NOTICE_RESPONSIBILITY,
                    DANGER,
                ));

            if let Some(error) = registry_read_error.as_ref() {
                dialog = dialog.child(
                    div()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(DANGER_DIM))
                        .text_sm()
                        .text_color(rgb(DANGER))
                        .child(format!(
                            "{error}。本次仍需确认；仅在确认记录成功写入后才能继续。"
                        )),
                );
            }

            dialog.footer(
                DialogFooter::new().child(
                    DialogAction::new().child(
                        Button::new("legal-risk-acknowledge")
                            .debug_selector(|| "legal-risk-acknowledge".into())
                            .label("我已查看并了解上述法律与数据风险")
                            .primary()
                            .on_click(move |_, window, cx| match acknowledge() {
                                Ok(()) => {
                                    acknowledged_view.update(cx, |view, cx| {
                                        view.legal_acknowledgement_required = false;
                                        if let Some(path) = view.pending_initial_path.take() {
                                            view.open_path(path, cx);
                                        }
                                        cx.notify();
                                    });
                                    window.close_dialog(cx);
                                }
                                Err(error) => {
                                    window.push_notification(
                                        Notification::new().message(format!(
                                            "无法保存确认记录，尚未允许继续：{error}"
                                        )),
                                        cx,
                                    );
                                }
                            }),
                    ),
                ),
            )
        });
    }

    // ---------------------------------------------------------------- dialogs

    /// Open a save and offer the discovered candidates.
    fn open_open_dialog(
        view: Entity<Self>,
        candidates: Vec<CandidateRow>,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            let candidates = candidates.clone();
            let mut dialog = dialog
                .title("打开存档")
                .width(px(620.0))
                .child("优先从 Steam userdata 目录中选择；也可以手动浏览任意 .sav 文件。");

            if candidates.is_empty() {
                dialog = dialog.child("尚未扫描到候选存档。");
            } else {
                dialog = dialog.children(candidates.iter().enumerate().map(|(index, row)| {
                    let path = row.path.clone();
                    let view = view.clone();
                    let description = row.description.clone();
                    let name = row
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default();
                    div()
                        .id(SharedString::from(format!("cand-{index}")))
                        .debug_selector(move || format!("candidate-{index}"))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(BG))
                        .child(div().text_sm().text_color(rgb(TEXT)).child(name))
                        .child(div().text_xs().text_color(rgb(MUTED)).child(description))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(format!("{}", path.display())),
                        )
                        .child(
                            Button::new(SharedString::from(format!("open-cand-{index}")))
                                .debug_selector(move || format!("open-cand-{index}"))
                                .label("打开这个存档")
                                .primary()
                                .on_click(move |_, window, cx| {
                                    // Route through the view: it keeps the task alive and
                                    // applies the snapshot, then closes the dialog.
                                    view.update(cx, |view, cx| view.open_path(path.clone(), cx));
                                    window.close_dialog(cx);
                                }),
                        )
                }));
            }

            dialog.footer(
                DialogFooter::new().child(
                    DialogAction::new().child(Button::new("browse").label("手动浏览…").on_click(
                        move |_, window, cx| {
                            let picked = rfd::FileDialog::new()
                                .set_title("选择 HELLDIVERS 2 存档")
                                .add_filter("存档文件", &["sav", "bin"])
                                .pick_file();
                            if let Some(path) = picked {
                                view.update(cx, |view, cx| view.open_path(path, cx));
                                window.close_dialog(cx);
                            }
                        },
                    )),
                ),
            )
        });
    }

    /// Open the save-as dialog.
    fn open_save_as_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(snapshot), Some(diff)) = ({
            let state = self.state.read(cx);
            (state.snapshot.clone(), state.diff.clone())
        }) else {
            self.state.update(cx, |state, cx| {
                state.status = StatusLine::error("请先打开一个存档".to_string());
                cx.notify();
            });
            return;
        };
        if diff.is_empty() {
            self.state.update(cx, |state, cx| {
                state.status = StatusLine::info("当前没有需要保存的改动".to_string());
                cx.notify();
            });
            return;
        }

        let patches = diff.patches.clone();
        let diff_for_prepare = diff.clone();
        let source = PathBuf::from(&snapshot.path);
        let default_name =
            local_io::default_export_name(None, &local_io::now_stamp().replace([':', '.'], ""));
        let default_dir = source
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let state_entity = self.state.clone();
        let generation = self.state.read(cx).reader.generation().0;

        window.open_dialog(cx, move |dialog, _, _| {
            let state_entity = state_entity.clone();
            let patches = patches.clone();
            let diff = diff_for_prepare.clone();
            let snapshot = snapshot.clone();
            let source = source.clone();
            let default_dir = default_dir.clone();
            let default_name = default_name.clone();
            dialog
                .title("另存为新配装")
                .width(px(540.0))
                .child("将当前改动写入一个新文件；原存档不会被修改。")
                .child(format!("默认文件名：{default_name}"))
                .child(format!("目录：{}", default_dir.display()))
                .footer(
                    DialogFooter::new().child(
                        DialogAction::new().child(Button::new("ok").label("选择位置并保存")),
                    ),
                )
                .on_ok(move |_, window, cx| {
                    let Some(target) = rfd::FileDialog::new()
                        .set_title("另存为新配装")
                        .set_directory(&default_dir)
                        .set_file_name(&default_name)
                        .add_filter("HELLDIVERS 2 存档", &["sav"])
                        .save_file()
                    else {
                        return false;
                    };
                    let task = state_entity.update(cx, |_, cx| {
                        spawn_prepare(
                            source.clone(),
                            snapshot.clone(),
                            patches.clone(),
                            diff.clone(),
                            generation,
                            CommitMode::Safe,
                            cx,
                        )
                    });
                    let state_for_copy = state_entity.clone();
                    window
                        .spawn(cx, async move |window_cx| {
                            let message = match task.await {
                                Ok(prepared) => {
                                    let copy_task = state_for_copy.update(window_cx, |_, cx| {
                                        spawn_copy(prepared, target.clone(), cx)
                                    });
                                    match copy_task.await {
                                        Ok(receipt) => {
                                            let text = format!(
                                                "已另存：{}",
                                                receipt.target_path.display()
                                            );
                                            state_for_copy.update(window_cx, |state, _| {
                                                state.record_copy(receipt);
                                            });
                                            text
                                        }
                                        Err(message) => format!("另存失败：{message}"),
                                    }
                                }
                                Err(message) => format!("准备写入失败：{message}"),
                            };
                            let _ = window_cx.update(|window, cx| {
                                window.push_notification(Notification::new().message(message), cx)
                            });
                        })
                        .detach();
                    true
                })
        });
    }

    /// Open the whole-file restore dialog.
    ///
    /// This replaces *everything*, not just the armor slots, so the dialog lists
    /// the concrete backup files and says so plainly; the head-only
    /// "恢复普通头盔" button on the card stays a separate, smaller action.
    fn open_restore_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        if state.save_in_flight {
            return;
        }
        let Some(snapshot) = state.snapshot.clone() else {
            return;
        };
        let backups = list_backups(state);
        let target = PathBuf::from(&snapshot.path);
        let backup_root = state.workspace.backups_dir();
        let state_entity = self.state.clone();
        let generation = state.reader.generation().0;

        window.open_dialog(cx, move |dialog, _, _| {
            let state_entity = state_entity.clone();
            let target = target.clone();
            let backup_root = backup_root.clone();
            let mut body = dialog
                .title("恢复整份备份")
                .width(px(680.0))
                .child(format!("将用备份整体替换：{}", target.display()))
                .child(
                    "这会替换整个存档，不只是头部/身体两个槽位。                     恢复前会自动保存一份当前文件的副本。",
                );

            if backups.is_empty() {
                body = body.child("还没有任何备份。先做一次“备份并写回存档”才会产生备份。");
            } else {
                body = body.child("选择要恢复的备份（最新在前）：");
                for entry in backups.iter().take(20) {
                    let path = entry.path.clone();
                    let state_for_row = state_entity.clone();
                    let target_for_row = target.clone();
                    let root_for_row = backup_root.clone();
                    let label = format!(
                        "{} · {}",
                        entry.file_name,
                        format_bytes(entry.size)
                    );
                    body = body.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .py_1()
                            .child(div().text_sm().child(label))
                            .child(
                                Button::new(SharedString::from(format!(
                                    "restore-{}",
                                    entry.file_name
                                )))
                                .debug_selector({
                                    let name = entry.file_name.clone();
                                    move || format!("restore-{name}")
                                })
                                .label("恢复这一份")
                                .on_click(move |_, window, cx| {
                                    let task = state_for_row.update(cx, |state, cx| {
                                        if state.save_in_flight
                                            || state.reader.generation().0 != generation
                                        {
                                            return None;
                                        }
                                        state.save_in_flight = true;
                                        state.status = StatusLine::info(format!(
                                            "正在恢复 {}",
                                            path.display()
                                        ));
                                        cx.notify();
                                        Some(spawn_restore(
                                            path.clone(),
                                            target_for_row.clone(),
                                            root_for_row.clone(),
                                            cx,
                                        ))
                                    });
                                    let Some(task) = task else {
                                        return;
                                    };
                                    let state_after = state_for_row.clone();
                                    window
                                        .spawn(cx, async move |window_cx| {
                                            let message = match task.await {
                                                Ok(receipt) => {
                                                    let text = receipt.summary();
                                                    state_after.update(window_cx, |state, _| {
                                                        state.record_restore(generation, receipt);
                                                    });
                                                    text
                                                }
                                                Err(message) => {
                                                    state_after.update(window_cx, |state, _| {
                                                        state.save_in_flight = false;
                                                        if state.reader.generation().0 == generation {
                                                            state.status =
                                                                StatusLine::error(message.clone());
                                                        }
                                                    });
                                                    format!("恢复失败：{message}")
                                                }
                                            };
                                            let _ = window_cx;
                                            message
                                        })
                                        .detach();
                                    window.close_dialog(cx);
                                }),
                            ),
                    );
                }
            }

            body.footer(
                DialogFooter::new()
                    .child(DialogAction::new().child(Button::new("cancel").label("关闭"))),
            )
        });
    }

    /// Open the write-back confirmation dialog.
    fn open_commit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        if state.save_in_flight {
            return;
        }
        let (Some(snapshot), Some(diff)) = (state.snapshot.clone(), state.diff.clone()) else {
            return;
        };
        let source = snapshot.path.clone();
        let summary = if diff.is_empty() {
            "当前明确选择与打开时的槽位值相同；风险模式仍会将该选择应用到最新文件。".to_string()
        } else {
            diff.summary()
        };
        let details = diff.technical_lines();
        let request = CommitRequest {
            snapshot,
            diff,
            generation: state.reader.generation().0,
        };
        let state_entity = self.state.clone();

        window.open_dialog(cx, move |dialog, _, _| {
            let safe_click_state = state_entity.clone();
            let safe_keyboard_state = state_entity.clone();
            let force_state = state_entity.clone();
            let safe_click_request = request.clone();
            let safe_keyboard_request = request.clone();
            let force_request = request.clone();
            let summary = summary.clone();
            let details = details.clone();
            let source = source.clone();
            dialog
                .title("写回当前存档")
                .width(px(640.0))
                .child(format!("目标：{source}"))
                .child(summary)
                .children(details.into_iter().map(|line| div().child(line)))
                .child("两种模式都会先创建独立备份，并在替换后回读校验。")
                .child(
                    div()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(DANGER_DIM))
                        .text_sm()
                        .text_color(rgb(DANGER))
                        .child(
                            "风险写入会忽略草稿基线是否过期：先读取当前磁盘文件，\
                             再把所选头部/身体护甲应用到最新内容。适合游戏内刚切换过装备的情况；\
                             游戏仍可能在写入后再次覆盖结果。",
                        ),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("force-latest")
                                .debug_selector(|| "action-force-latest".into())
                                .label("风险写入最新文件")
                                .ghost()
                                .tooltip("保留最新文件的其他内容，并覆盖所选护甲槽位")
                                .on_click(move |_, window, cx| {
                                    if begin_commit(
                                        force_state.clone(),
                                        force_request.clone(),
                                        CommitMode::ForceLatest,
                                        window,
                                        cx,
                                    ) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(
                            Button::new("safe-write")
                                .debug_selector(|| "action-safe-write".into())
                                .label("安全写回")
                                .primary()
                                .tooltip("源文件变化时拒绝写入")
                                .on_click(move |_, window, cx| {
                                    if begin_commit(
                                        safe_click_state.clone(),
                                        safe_click_request.clone(),
                                        CommitMode::Safe,
                                        window,
                                        cx,
                                    ) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    begin_commit(
                        safe_keyboard_state.clone(),
                        safe_keyboard_request.clone(),
                        CommitMode::Safe,
                        window,
                        cx,
                    )
                })
        });
    }

    /// Open the preset dialog.
    fn open_preset_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state_entity = self.state.clone();
        let name_input = self.preset_name_input.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let name_input = name_input.clone();
            let state_entity = state_entity.clone();
            dialog
                .title("保存为预设")
                .width(px(500.0))
                .child("预设只保存“头部/身体选择哪个护甲”，不会保存存档内容。")
                .child("应用到最新打开的存档时，武器与其他改动都会保留。")
                .child(Input::new(&name_input).aria_label("预设名称"))
                .footer(
                    DialogFooter::new()
                        .child(DialogAction::new().child(Button::new("ok").label("保存预设"))),
                )
                .on_ok(move |_, window, cx| {
                    let name = name_input.read(cx).value().to_string();
                    if name.trim().is_empty() {
                        window.push_notification(Notification::new().message("请填写预设名称"), cx);
                        return false;
                    }
                    let saved = state_entity.update(cx, |state, cx| {
                        let preset_id = format!("preset_{}", local_io::now_stamp());
                        let Some(preset) = preset_from_draft(state, name.trim(), preset_id) else {
                            state.status = StatusLine::error("当前没有可保存的选择".to_string());
                            cx.notify();
                            return false;
                        };
                        if let Err(error) = state.presets.upsert(preset.clone()) {
                            state.status = StatusLine::error(error);
                            cx.notify();
                            return false;
                        }
                        if let Err(error) = state.workspace.save_presets(&state.presets) {
                            state.status = StatusLine::error(error.to_string());
                            cx.notify();
                            return false;
                        }
                        state.status = StatusLine::info(format!("已保存预设“{}”", preset.name));
                        cx.notify();
                        true
                    });
                    window.push_notification(
                        Notification::new().message(if saved {
                            "预设已保存"
                        } else {
                            "预设未保存"
                        }),
                        cx,
                    );
                    true
                })
        });
    }

    /// Open the catalog import dialog.
    fn open_import_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state_entity = self.state.clone();
        let workspace = self.state.read(cx).workspace.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let state_entity = state_entity.clone();
            let workspace = workspace.clone();
            dialog
                .title("导入护甲目录")
                .width(px(560.0))
                .child("支持旧 Python 工具的 catalog.json、CSV 导出，以及本工具的 v2 JSON。")
                .child("导入不会修改你的原目录文件，也不会猜测物品 ID。")
                .footer(
                    DialogFooter::new()
                        .child(DialogAction::new().child(Button::new("ok").label("选择文件…"))),
                )
                .on_ok(move |_, _, cx| {
                    let Some(path) = rfd::FileDialog::new()
                        .set_title("选择护甲目录文件")
                        .add_filter("目录文件", &["json", "csv"])
                        .pick_file()
                    else {
                        return false;
                    };
                    let task = state_entity.update(cx, |_, cx| {
                        spawn_import_catalog(path, workspace.clone(), cx)
                    });
                    let state_for_apply = state_entity.clone();
                    cx.spawn(async move |cx| {
                        let result = task.await;
                        if let BackgroundResult::ImportPreview(result) = result {
                            match *result {
                                Ok(pending) => {
                                    let count = pending.preview.items.len();
                                    let issues = pending.preview.issues.len();
                                    state_for_apply.update(cx, |state, cx| {
                                        state.pending_import = Some(pending);
                                        state.status = StatusLine::info(format!(
                                            "目录预览：{count} 项可用，{issues} 条记录需注意"
                                        ));
                                        cx.notify();
                                    });
                                }
                                Err(message) => {
                                    state_for_apply.update(cx, |state, cx| {
                                        state.status = StatusLine::error(message);
                                        cx.notify();
                                    });
                                }
                            }
                        }
                    })
                    .detach();
                    true
                })
        });
    }

    // ----------------------------------------------------------------- render

    fn render_update_banner(&self, cx: &mut Context<Self>) -> Div {
        let update_state = self.state.read(cx).update_state.clone();
        let base = || {
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_5()
                .py_2()
                .bg(rgb(CARD_BG))
                .border_b_1()
                .border_color(rgb(CARD_BORDER))
        };
        let releases_button = |cx: &mut Context<Self>| {
            Button::new("open-releases-page")
                .debug_selector(|| "update-open-releases".into())
                .label("打开 Releases 页面")
                .on_click(cx.listener(|view, _, _, cx| view.open_releases_page(cx)))
        };
        let dismiss_button = |cx: &mut Context<Self>| {
            Button::new("dismiss-update")
                .label("关闭")
                .ghost()
                .on_click(cx.listener(|view, _, _, cx| {
                    view.state.update(cx, |state, cx| {
                        state.update_state = UpdateState::Idle;
                        cx.notify();
                    });
                }))
        };

        match update_state {
            UpdateState::Idle => div(),
            UpdateState::Checking => base()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(ACCENT))
                        .child("正在检查更新…"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(format!("当前版本：{CURRENT_RELEASE_TAG}")),
                ),
            UpdateState::UpToDate(release) => base()
                .child(div().text_sm().text_color(rgb(OK)).child("已是最新版本"))
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(release.tag),
                )
                .child(dismiss_button(cx)),
            UpdateState::Available(release) => {
                let package_name = release.package.name.clone();
                let release_for_download = release.clone();
                base()
                    .child(div().text_sm().text_color(rgb(ACCENT)).child("发现新版本"))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(rgb(TEXT))
                            .child(format!(
                                "{} · {}",
                                release.tag,
                                format_bytes(release.package.size)
                            )),
                    )
                    .child(
                        Button::new("download-update")
                            .debug_selector(|| "update-download".into())
                            .label("下载 ZIP")
                            .primary()
                            .on_click(cx.listener(move |view, _, _, cx| {
                                let Some(destination) = rfd::FileDialog::new()
                                    .set_title("保存最新 HD2 Armor Desk 发布包")
                                    .set_file_name(&package_name)
                                    .add_filter("ZIP 发布包", &["zip"])
                                    .save_file()
                                else {
                                    return;
                                };
                                view.start_update_download(
                                    release_for_download.clone(),
                                    destination,
                                    cx,
                                );
                            })),
                    )
                    .child(releases_button(cx))
                    .child(dismiss_button(cx))
            }
            UpdateState::Downloading(release) => base()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(ACCENT))
                        .child("正在下载并校验…"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(release.package.name),
                ),
            UpdateState::Downloaded(receipt) => base()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(OK))
                        .child("下载完成，SHA-256 校验通过"),
                )
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(receipt.path.display().to_string()),
                )
                .child(dismiss_button(cx)),
            UpdateState::CheckFailed(message) => base()
                .border_color(rgb(DANGER))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(DANGER))
                        .child("检查更新失败"),
                )
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(TEXT))
                        .child(message),
                )
                .child(releases_button(cx))
                .child(dismiss_button(cx)),
            UpdateState::DownloadFailed(message) => base()
                .border_color(rgb(DANGER))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(DANGER))
                        .child("下载更新失败"),
                )
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(TEXT))
                        .child(message),
                )
                .child(releases_button(cx))
                .child(dismiss_button(cx)),
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let state = self.state.read(cx);
        let (file_name, file_meta) = state
            .snapshot
            .as_ref()
            .map(|snapshot| {
                (
                    PathBuf::from(&snapshot.path)
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| "已载入存档".to_string()),
                    format!("{} · 本机文件", format_bytes(snapshot.raw.len() as u64)),
                )
            })
            .unwrap_or_else(|| {
                (
                    "等待载入存档".to_string(),
                    "打开 testament_new.sav 开始配置".to_string(),
                )
            });
        let writable = state.can_write();
        let has_snapshot = state.snapshot.is_some();
        let readonly_reason = state
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.readonly_reason.clone());
        let dirty = state.is_dirty();
        let watching = state.watching;

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .px_5()
            .py_3()
            .bg(rgb(BG))
            .border_b_2()
            .border_color(rgb(ACCENT))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(48.0))
                            .h(px(48.0))
                            .bg(rgb(ACCENT))
                            .text_color(rgb(BG))
                            .text_xl()
                            .font_semibold()
                            .child("II"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(ACCENT))
                                    .child("SUPER EARTH  /  EQUIPMENT TERMINAL"),
                            )
                            .child(
                                div()
                                    .text_xl()
                                    .font_semibold()
                                    .text_color(rgb(TEXT))
                                    .child("ARMOR DESK"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child("HELLDIVERS 2   ·   双甲配装控制台"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .gap_1()
                            .child(div().text_xs().text_color(rgb(SUBTLE)).child("LOCAL SAVE  /  本机存档"))
                            .child(div().text_sm().font_semibold().text_color(rgb(TEXT)).child(file_name))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(file_meta)),
                    )
                    .when(dirty, |this| {
                        this.child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(ACCENT_DIM))
                                .text_xs()
                                .text_color(rgb(ACCENT))
                                .child("有未保存改动"),
                        )
                    })
                    .when(watching, |this| {
                        this.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(OK_DIM))
                                .text_xs()
                                .text_color(rgb(OK))
                                .child("监视中"),
                        )
                    })
                    .when(has_snapshot && !writable, |this| {
                        this.child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(DANGER_DIM))
                                .text_xs()
                                .text_color(rgb(DANGER))
                                .child("只读"),
                        )
                    })
                    .when_some(readonly_reason, |this, reason| {
                        this.child(div().text_xs().text_color(rgb(MUTED)).child(reason))
                    }),
            )
    }

    fn render_slot_card(&self, slot: TargetSlot, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let is_head = slot == TargetSlot::Head;
        let (label, sub_label, item_key) = if is_head {
            (
                state.head_label(),
                if state.head_is_armor_in_helmet_slot() {
                    "身体护甲已部署到头部槽位"
                } else {
                    "头部装备"
                },
                state.head_item_key(),
            )
        } else {
            (
                state.body_label(),
                if state.body_locked() {
                    "身体槽位已锁定 · 保持原值"
                } else {
                    "身体槽位已解锁"
                },
                state.body_item_key(),
            )
        };
        let selected_here = state.target_slot == slot;
        let locked = !is_head && state.body_locked();
        let has_selection = item_key.is_some();
        let slot_title = if is_head {
            "头部槽位"
        } else {
            "身体槽位"
        };
        let slot_number = if is_head { "01" } else { "02" };
        let slot_mark = if is_head { "H" } else { "B" };

        div()
            .id(if is_head { "head-card" } else { "body-card" })
            .flex_1()
            .flex()
            .flex_col()
            .gap_4()
            .p_5()
            .rounded_sm()
            .bg(rgb(if selected_here { CARD_RAISED } else { CARD_BG }))
            .border_1()
            .border_color(rgb(if selected_here { ACCENT } else { CARD_BORDER }))
            .when(locked, |this| this.opacity(0.9))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(48.0))
                                    .rounded_full()
                                    .border_2()
                                    .border_color(rgb(if selected_here { ACCENT } else { CARD_BORDER }))
                                    .bg(rgb(BG))
                                    .text_color(rgb(if selected_here { ACCENT } else { MUTED }))
                                    .font_semibold()
                                    .child(slot_mark),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_xs().text_color(rgb(if selected_here { ACCENT } else { SUBTLE })).child(format!("LOADOUT  /  {slot_number}")))
                                    .child(div().text_base().font_semibold().text_color(rgb(TEXT)).child(slot_title))
                                    .child(div().text_xs().text_color(rgb(MUTED)).child(sub_label)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(locked, |this| {
                                this.child(div().text_xs().text_color(rgb(SUBTLE)).child("LOCK"))
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(if selected_here { ACCENT } else { SUBTLE }))
                                    .child(slot_number),
                            ),
                    ),
            )
            .child(
                div()
                    .min_h(px(52.0))
                    .px_3()
                    .py_2()
                    .bg(rgb(BG))
                    .border_l_2()
                    .border_color(rgb(if selected_here { ACCENT } else { CARD_BORDER }))
                    .text_lg()
                    .font_semibold()
                    .text_color(rgb(TEXT))
                    .child(label),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new(if is_head { "pick-head" } else { "pick-body" })
                            .debug_selector(move || {
                                if is_head {
                                    "card-head".into()
                                } else {
                                    "card-body".into()
                                }
                            })
                            .label(if selected_here {
                                "当前选择目标"
                            } else {
                                "选择此槽位"
                            })
                            .when(selected_here, |button| button.primary())
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.state.update(cx, |state, cx| {
                                    state.target_slot = slot;
                                    state.status = StatusLine::info(match slot {
                                        TargetSlot::Head => "正在为头部槽位选择护甲".to_string(),
                                        TargetSlot::Body => "正在为身体槽位选择护甲".to_string(),
                                    });
                                    cx.notify();
                                });
                            })),
                    )
                    .when(has_selection, |this| {
                        this.child(
                            Button::new(if is_head { "clear-head" } else { "clear-body" })
                                .label(if is_head {
                                    "恢复原头盔"
                                } else {
                                    "恢复原值"
                                })
                                .ghost()
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.state.update(cx, |state, cx| {
                                        let Some(draft) = state.draft.as_mut() else {
                                            return;
                                        };
                                        if is_head {
                                            draft.revert_head();
                                            state.status = StatusLine::info(
                                                "头部已恢复为存档中的原值".to_string(),
                                            );
                                        } else {
                                            draft.revert_body();
                                            state.status = StatusLine::info(
                                                "身体已恢复为存档中的原值".to_string(),
                                            );
                                        }
                                        state.refresh_diff();
                                        cx.notify();
                                    });
                                })),
                        )
                    })
                    .when(!is_head, |this| {
                        this.child(
                            Button::new("toggle-body-lock")
                                .debug_selector(|| "toggle-body-lock".into())
                                .label(if locked {
                                    "解锁身体"
                                } else {
                                    "锁定身体"
                                })
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.state.update(cx, |state, cx| {
                                        if let Some(draft) = state.draft.as_mut() {
                                            let next = !draft.body_locked();
                                            draft.set_body_locked(next);
                                            state.status = StatusLine::info(if next {
                                                "身体已锁定，将保持存档中的值".to_string()
                                            } else {
                                                "身体已解锁，可以单独更换".to_string()
                                            });
                                        }
                                        cx.notify();
                                    });
                                })),
                        )
                    }),
            )
    }

    fn render_browser(&self, cx: &mut Context<Self>) -> Div {
        let (rows, total, slot, filter, locked_target) = {
            let state = self.state.read(cx);
            let selected_key = match state.target_slot {
                TargetSlot::Head => state.head_item_key(),
                TargetSlot::Body => state.body_item_key(),
            };
            let rows: Vec<BrowserRow> = state
                .browser_items()
                .into_iter()
                .take(300)
                .map(|item| BrowserRow {
                    key: item.item_key.clone(),
                    name: display_name(item),
                    type_label: item.item_type.label().to_string(),
                    id_hex: loadout_domain::ItemId(item.id_u32).hex(),
                    passive_name: (!item.metadata.passive_tags.is_empty())
                        .then(|| item.metadata.passive_tags.join(" · ")),
                    passive_description: item
                        .metadata
                        .passive_description
                        .as_deref()
                        .filter(|description| !description.trim().is_empty())
                        .map(str::to_owned),
                    usable: player_usable(item),
                    selected: selected_key.as_deref() == Some(item.item_key.as_str()),
                })
                .collect();
            let locked_target = state.target_slot == TargetSlot::Body && state.body_locked();
            (
                rows,
                state.catalog.len(),
                state.target_slot,
                state.type_filter,
                locked_target,
            )
        };
        let shown = rows.len();
        let empty = rows.is_empty();
        let target_label = match slot {
            TargetSlot::Head => "头部槽位",
            TargetSlot::Body => "身体槽位",
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_sm()
            .bg(rgb(CARD_BG))
            .border_1()
            .border_color(rgb(CARD_BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(34.0))
                                    .bg(rgb(ACCENT))
                                    .text_color(rgb(BG))
                                    .font_semibold()
                                    .child("A"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_xs().text_color(rgb(ACCENT)).child("ARMORY  /  装备数据库"))
                                    .child(div().text_base().font_semibold().text_color(rgb(TEXT)).child("装备库"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(MUTED))
                                            .child(format!("当前目标：{target_label}")),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(BG))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!("{shown} / {total}")),
                    ),
            )
            .child(Input::new(&self.search_input).aria_label("搜索装备名称或十六进制 ID"))
            .when(locked_target, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(ACCENT_DIM))
                        .border_1()
                        .border_color(rgb(ACCENT))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .text_color(rgb(ACCENT))
                                .child("LOCK")
                                .child("身体槽位受保护；解锁后才能替换装备。"),
                        )
                        .child(
                            Button::new("unlock-body-browser")
                                .label("解锁身体")
                                .primary()
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.state.update(cx, |state, cx| {
                                        if let Some(draft) = state.draft.as_mut() {
                                            draft.set_body_locked(false);
                                            state.status = StatusLine::info(
                                                "身体已解锁，可以选择装备".to_string(),
                                            );
                                        }
                                        cx.notify();
                                    });
                                })),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div().flex().flex_wrap().gap_2().children(
                            [
                                ("全部装备", None),
                                ("身体护甲", Some(ItemType::Armor)),
                                ("头盔", Some(ItemType::Helmet)),
                            ]
                            .into_iter()
                            .map(|(label, value)| {
                                let active = filter == value;
                                Button::new(SharedString::from(format!("filter-{label}")))
                                    .label(label)
                                    .when(active, |button| button.primary())
                                    .when(!active, |button| button.ghost())
                                    .toggled(active)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.state.update(cx, |state, cx| {
                                            state.type_filter = value;
                                            cx.notify();
                                        });
                                    }))
                                    .into_any_element()
                            }),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(SUBTLE))
                            .child("最多显示 300 项"),
                    ),
            )
            .child(
                div()
                    .id("browser-list")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(px(360.0))
                    .overflow_y_scroll()
                    .when(empty, |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .h(px(150.0))
                                .rounded_md()
                                .bg(rgb(BG))
                                .text_color(rgb(MUTED))
                                .child("—")
                                .child(
                                    div()
                                        .text_sm()
                                        .child("没有匹配的装备，试试名称或十六进制 ID"),
                                ),
                        )
                    })
                    .children(rows.into_iter().map(|row| {
                        let BrowserRow {
                            key,
                            name,
                            type_label,
                            id_hex,
                            passive_name,
                            passive_description,
                            usable,
                            selected,
                        } = row;
                        let key_for_click = key.clone();
                        div()
                            .id(SharedString::from(format!("item-{key}")))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .px_3()
                            .py_2()
                            .rounded_sm()
                            .bg(rgb(if selected { ACCENT_DIM } else { BG }))
                            .border_1()
                            .border_color(rgb(if selected { ACCENT } else { CARD_BORDER }))
                            .when(!usable, |this| this.opacity(0.65))
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_w_0()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .size(px(30.0))
                                            .rounded_sm()
                                            .bg(rgb(if selected { ACCENT } else { CARD_RAISED }))
                                            .text_color(rgb(if selected { BG } else { MUTED }))
                                            .child(if type_label == "身体护甲" {
                                                "甲"
                                            } else {
                                                "盔"
                                            }),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_1()
                                            .min_w_0()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div().text_sm().text_color(rgb(TEXT)).child(name),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(MUTED))
                                                    .child(id_hex),
                                            )
                                            .when_some(passive_name, {
                                                let key = key.clone();
                                                move |this, passive_name| {
                                                    this.child(
                                                        div()
                                                            .debug_selector(move || {
                                                                format!("passive-name-{key}")
                                                            })
                                                            .text_xs()
                                                            .text_color(rgb(ACCENT))
                                                            .child(format!(
                                                                "被动 · {passive_name}"
                                                            )),
                                                    )
                                                }
                                            })
                                            .when_some(passive_description, {
                                                let key = key.clone();
                                                move |this, description| {
                                                    this.child(
                                                        div()
                                                            .debug_selector(move || {
                                                                format!("passive-description-{key}")
                                                            })
                                                            .text_xs()
                                                            .text_color(rgb(MUTED))
                                                            .child(description),
                                                    )
                                                }
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .bg(rgb(CARD_RAISED))
                                            .text_xs()
                                            .text_color(rgb(MUTED))
                                            .child(type_label),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!("pick-{key}")))
                                            .debug_selector({
                                                let key = key.clone();
                                                move || format!("pick-{key}")
                                            })
                                            .label(if locked_target {
                                                "先解锁"
                                            } else if selected {
                                                "已选择"
                                            } else if usable {
                                                "选择"
                                            } else {
                                                "不可用"
                                            })
                                            .when(selected, |button| button.success())
                                            .when(usable && !selected, |button| button.primary())
                                            .when(locked_target, |button| button.disabled(true))
                                            .tooltip(if locked_target {
                                                "先解除身体槽位保护".to_string()
                                            } else if usable {
                                                format!("应用到{target_label}")
                                            } else {
                                                "此条目缺少可用的装备类型，不能写入".to_string()
                                            })
                                            .on_click(cx.listener(move |view, _, _, cx| {
                                                view.select_item(&key_for_click, cx);
                                            })),
                                    ),
                            )
                            .into_any_element()
                    })),
            )
    }

    fn render_diff_bar(&self, cx: &mut Context<Self>) -> Div {
        let state = self.state.read(cx);
        let summary = state
            .diff
            .as_ref()
            .map(|diff| diff.summary())
            .unwrap_or_else(|| "打开存档后，改动会在这里集中预览".to_string());
        let details: Vec<String> = state
            .diff
            .as_ref()
            .map(|diff| diff.technical_lines().into_iter().take(2).collect())
            .unwrap_or_default();
        let dirty = state.is_dirty();
        let can_write = state.can_write();
        let busy = state.save_in_flight;
        let in_conflict = state.in_conflict();
        let can_undo = state.can_undo();
        let state_label = if busy {
            "正在写入"
        } else if in_conflict {
            "需要处理冲突"
        } else if dirty {
            "改动待保存"
        } else {
            "当前无改动"
        };
        let state_color = if in_conflict {
            DANGER
        } else if dirty || busy {
            ACCENT
        } else {
            OK
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_5()
            .py_3()
            .bg(rgb(CARD_BG))
            .border_t_2()
            .border_color(rgb(if in_conflict { DANGER } else if dirty { ACCENT } else { CARD_BORDER }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(36.0))
                                    .rounded_full()
                                    .border_2()
                                    .border_color(rgb(state_color))
                                    .bg(rgb(if dirty { ACCENT_DIM } else { OK_DIM }))
                                    .text_color(rgb(state_color))
                                    .child(if dirty { "Δ" } else { "✓" }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_semibold()
                                                    .text_color(rgb(state_color))
                                                    .child(format!("SAVE STATUS  /  {state_label}")),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(rgb(TEXT))
                                                    .child(summary),
                                            ),
                                    )
                                    .children(details.into_iter().map(|line| {
                                        div().text_xs().text_color(rgb(MUTED)).child(line)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("undo")
                                    .debug_selector(|| "action-undo".into())
                                    .label("撤销")
                                    .tooltip("撤销上一步配装操作")
                                    .when(!can_undo, |button| button.disabled(true))
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.state.update(cx, |state, cx| {
                                            if let Some(draft) = state.draft.as_mut() {
                                                draft.undo();
                                                state.refresh_diff();
                                            }
                                            cx.notify();
                                        });
                                    })),
                            )
                            .child(
                                Button::new("save-as")
                                    .debug_selector(|| "action-save-as".into())
                                    .label("另存副本")
                                    .tooltip("保留原存档，将配装写入新文件")
                                    .when(!dirty || busy, |button| button.disabled(true))
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.open_save_as_dialog(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("commit")
                                    .debug_selector(|| "action-commit".into())
                                    .label(if busy {
                                        "正在写入…"
                                    } else {
                                        "备份并写回"
                                    })
                                    .tooltip("可选择安全或风险模式；两者均自动备份并回读校验")
                                    .primary()
                                    .loading(busy)
                                    .when(!dirty || !can_write || busy, |button| {
                                        button.disabled(true)
                                    })
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.open_commit_dialog(window, cx);
                                    })),
                            ),
                    ),
            )
            .when(in_conflict, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(DANGER_DIM))
                        .text_sm()
                        .text_color(rgb(DANGER))
                        .child("!")
                        .child(
                            "磁盘存档已更新：草稿仍保留。可另存副本，或在写回确认中选择“风险写入最新文件”。",
                        ),
                )
            })
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let state = self.state.read(cx);
        let watching = state.watching;
        let has_snapshot = state.snapshot.is_some();
        let busy = state.save_in_flight;
        let diagnostics = state.show_diagnostics;
        let dirty = state.is_dirty();
        let update_busy = state.update_state.is_busy();

        div()
            .flex()
            .items_center()
            .gap_3()
            .px_5()
            .py_2()
            .bg(rgb(CARD_RAISED))
            .border_b_1()
            .border_color(rgb(CARD_BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .p_1()
                    .rounded_sm()
                    .bg(rgb(BG))
                    .child(
                        Button::new("open-save")
                            .label("打开存档")
                            .primary()
                            .tooltip("选择本机 HELLDIVERS 2 存档")
                            .when(busy, |button| button.disabled(true))
                            .on_click(cx.listener(|view, _, window, cx| {
                                let start = view
                                    .state
                                    .read(cx)
                                    .snapshot
                                    .as_ref()
                                    .map(|snapshot| PathBuf::from(&snapshot.path))
                                    .and_then(|path| {
                                        path.parent().map(|parent| parent.to_path_buf())
                                    });
                                let mut dialog = rfd::FileDialog::new()
                                    .set_title("选择 HELLDIVERS 2 存档")
                                    .add_filter("存档文件", &["sav", "bin"]);
                                if let Some(start) = start {
                                    dialog = dialog.set_directory(start);
                                }
                                if let Some(path) = dialog.pick_file() {
                                    view.open_path(path, cx);
                                }
                                let _ = window;
                            })),
                    )
                    .child(
                        Button::new("discover")
                            .debug_selector(|| "toolbar-discover".into())
                            .label("自动查找")
                            .tooltip("扫描 Steam 安装目录下的 userdata")
                            .on_click(cx.listener(|view, _, window, cx| {
                                let task = view.state.update(cx, |_, cx| spawn_discover(cx));
                                let view = cx.entity();
                                window
                                    .spawn(cx, async move |window_cx| {
                                        let result = task.await;
                                        let _ = window_cx.update(|window, cx| {
                                            view.update(cx, |view, cx| {
                                                view.apply_background(result, cx)
                                            });
                                            let candidates =
                                                view.read(cx).state.read(cx).candidates.clone();
                                            WorkspaceView::open_open_dialog(
                                                view.clone(),
                                                candidates,
                                                window,
                                                cx,
                                            );
                                        });
                                    })
                                    .detach();
                            })),
                    ),
            )
            .child(div().w(px(1.0)).h(px(24.0)).bg(rgb(CARD_BORDER)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("import")
                            .debug_selector(|| "toolbar-import".into())
                            .label("导入目录")
                            .ghost()
                            .tooltip("导入 JSON 或 CSV 护甲目录")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.open_import_dialog(window, cx);
                            })),
                    )
                    .child(
                        Button::new("preset")
                            .label("保存预设")
                            .ghost()
                            .tooltip("保存当前双甲选择，便于再次应用")
                            .when(!dirty, |button| button.disabled(true))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.open_preset_dialog(window, cx);
                            })),
                    )
                    .child(
                        Button::new("restore")
                            .debug_selector(|| "toolbar-restore".into())
                            .label("恢复备份")
                            .ghost()
                            .tooltip("用历史备份替换整份当前存档")
                            .when(!has_snapshot || busy, |button| button.disabled(true))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.open_restore_dialog(window, cx);
                            })),
                    ),
            )
            .child(div().flex_1())
            .child(
                Button::new("check-update")
                    .debug_selector(|| "toolbar-update".into())
                    .label(if update_busy {
                        "更新处理中…"
                    } else {
                        "检查更新"
                    })
                    .ghost()
                    .tooltip(format!("当前版本：{CURRENT_RELEASE_TAG}"))
                    .when(update_busy, |button| button.disabled(true))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.start_update_check(cx);
                    })),
            )
            .child(
                Button::new("watch")
                    .debug_selector(|| "toolbar-watch".into())
                    .label(if watching {
                        "停止监视"
                    } else {
                        "只读监视"
                    })
                    .when(watching, |button| button.success())
                    .when(!watching, |button| button.ghost())
                    .toggled(watching)
                    .tooltip("监视磁盘变化；监视本身绝不写盘")
                    .when(!has_snapshot, |button| button.disabled(true))
                    .on_click(cx.listener(|view, _, _, cx| {
                        let start = {
                            let state = view.state.read(cx);
                            !state.watching && state.snapshot.is_some()
                        };
                        if start {
                            view.start_watching(cx);
                        } else {
                            view.state.update(cx, |state, cx| {
                                state.watching = false;
                                state.status =
                                    StatusLine::info("已停止监视（监视期间从不写盘）".to_string());
                                cx.notify();
                            });
                        }
                    })),
            )
            .child(
                Button::new("diagnostics")
                    .label(if diagnostics {
                        "收起诊断"
                    } else {
                        "诊断"
                    })
                    .ghost()
                    .toggled(diagnostics)
                    .tooltip("显示文件校验值与工作区路径")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.state.update(cx, |state, cx| {
                            state.show_diagnostics = !state.show_diagnostics;
                            cx.notify();
                        });
                    })),
            )
    }

    fn render_guide(&self, cx: &mut Context<Self>) -> Div {
        let state = self.state.read(cx);
        let opened = state.snapshot.is_some();
        let dirty = state.is_dirty();
        let committed = state.last_commit.is_some() && !dirty;
        let steps = [
            ("01", "打开存档", "读取并校验本机文件", opened, !opened),
            (
                "02",
                "配置双甲",
                "选择头部与身体装备",
                dirty,
                opened && !dirty,
            ),
            (
                "03",
                "写回存档",
                "安全或风险模式均会备份回读",
                committed,
                dirty,
            ),
        ];

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_sm()
            .bg(rgb(CARD_BG))
            .border_1()
            .border_color(rgb(CARD_BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(ACCENT)).child("MISSION  /  03"))
                    .child(div().text_base().font_semibold().text_color(rgb(TEXT)).child("操作流程")),
            )
            .children(
                steps
                    .into_iter()
                    .map(|(number, label, description, complete, active)| {
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .p_2()
                            .rounded_md()
                            .bg(rgb(if active { ACCENT_DIM } else { BG }))
                            .border_1()
                            .border_color(rgb(if active { ACCENT } else { CARD_BORDER }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(28.0))
                                    .rounded_full()
                                    .bg(rgb(if complete { OK_DIM } else { CARD_RAISED }))
                                    .text_xs()
                                    .text_color(rgb(if complete {
                                        OK
                                    } else if active {
                                        ACCENT
                                    } else {
                                        SUBTLE
                                    }))
                                    .when(complete, |this| this.child("✓"))
                                    .when(!complete, |this| this.child(number)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(if active { ACCENT } else { TEXT }))
                                            .child(label),
                                    )
                                    .child(
                                        div().text_xs().text_color(rgb(MUTED)).child(description),
                                    ),
                            )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(OK_DIM))
                    .text_xs()
                    .text_color(rgb(OK))
                    .child("✓")
                    .child("写回前自动创建独立备份；完成后重新读取并校验文件。"),
            )
    }

    fn render_diagnostics(&self, cx: &mut Context<Self>) -> Div {
        let state = self.state.read(cx);
        let mut lines: Vec<String> = Vec::new();
        if let Some(snapshot) = state.snapshot.as_ref() {
            lines.push(format!("路径  {}", snapshot.path));
            lines.push(format!("SHA256  {}", snapshot.sha256));
            lines.push(format!("外层 CRC32  0x{:08X}", snapshot.outer_crc32));
            lines.push(format!("内层 Murmur64A  0x{:08X}", snapshot.inner_low32));
            lines.push(format!("正文长度  {} 字节", snapshot.payload.len()));
            lines.push(format!(
                "槽位  头部 {}  /  身体 {}",
                snapshot
                    .head_id()
                    .map(|id| format!("0x{id:08X}"))
                    .unwrap_or_else(|| "—".into()),
                snapshot
                    .body_id()
                    .map(|id| format!("0x{id:08X}"))
                    .unwrap_or_else(|| "—".into())
            ));
        }
        if let Some(note) = state.last_verify_note.as_ref() {
            lines.push(format!("最近校验  {note}"));
        }
        if let Some(receipt) = state.last_commit.as_ref() {
            lines.push(format!("最近写回  {}", receipt.summary()));
            lines.push(format!("写回详情  {}", receipt.detail));
        }
        if let Some(copy) = state.last_copy.as_ref() {
            lines.push(format!(
                "最近另存  {} · SHA256 {}",
                copy.target_path.display(),
                copy.output_sha256.chars().take(16).collect::<String>()
            ));
        }
        if let Some(pending) = state.pending_import.as_ref() {
            if let Some(archive) = pending.archive_path.as_ref() {
                lines.push(format!("导入副本  {}", archive.display()));
            }
        }
        lines.push(format!("工作区  {}", state.workspace.root().display()));
        lines.push(format!(
            "备份目录  {}",
            state.workspace.backups_dir().display()
        ));

        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .rounded_lg()
            .bg(rgb(CARD_BG))
            .border_1()
            .border_color(rgb(CARD_BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(INFO)).child("i"))
                    .child(div().text_base().text_color(rgb(TEXT)).child("诊断信息")),
            )
            .child(
                div()
                    .id("diagnostics-list")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(BG))
                    .max_h(px(220.0))
                    .overflow_y_scroll()
                    .children(
                        lines
                            .into_iter()
                            .map(|line| div().text_xs().text_color(rgb(MUTED)).child(line)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(OK))
                    .child("LOCAL")
                    .child("仅在本机处理文件 · 不联网 · 不上传诊断"),
            )
    }

    fn render_import_review(&self, cx: &mut Context<Self>) -> Div {
        let state = self.state.read(cx);
        let Some(pending) = state.pending_import.as_ref() else {
            return div();
        };
        let preview = &pending.preview;
        let issues: Vec<String> = preview
            .issues
            .iter()
            .take(8)
            .map(|issue| {
                format!(
                    "· 第 {} 行 · {} · {}",
                    issue.line,
                    match issue.severity {
                        loadout_domain::IssueSeverity::Error => "已拒绝",
                        loadout_domain::IssueSeverity::Warning => "注意",
                        loadout_domain::IssueSeverity::Info => "提示",
                    },
                    issue.message
                )
            })
            .collect();
        let ambiguous: Vec<(u32, String)> = preview
            .items
            .iter()
            .filter(|item| {
                matches!(
                    item.suggestion,
                    loadout_domain::SuggestedType::NeedsUserDecision
                )
            })
            .take(8)
            .map(|item| (item.id_u32, item.display_name.clone()))
            .collect();
        let header = format!(
            "目录导入预览 · {} · {} 项 · {} 条记录需注意",
            preview.source_file_name,
            preview.items.len(),
            preview.issues.len()
        );

        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .rounded_lg()
            .bg(rgb(CARD_BG))
            .border_2()
            .border_color(rgb(ACCENT))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(ACCENT)).child("↓"))
                    .child(div().text_base().text_color(rgb(TEXT)).child(header)),
            )
            .children(
                issues
                    .into_iter()
                    .map(|line| div().text_xs().text_color(rgb(DANGER)).child(line)),
            )
            .when(!ambiguous.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("以下条目类型未确认，导入后可见但普通模式不会写入："),
                )
                .children(ambiguous.into_iter().map(|(id, name)| {
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(format!("· {name} · 0x{id:08X}"))
                }))
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("import-apply")
                            .label("确认导入")
                            .primary()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.commit_pending_import(cx);
                            })),
                    )
                    .child(Button::new("import-cancel").ghost().label("取消").on_click(
                        cx.listener(|view, _, _, cx| {
                            view.state.update(cx, |state, cx| {
                                state.pending_import = None;
                                state.status = StatusLine::info("已取消本次目录导入".to_string());
                                cx.notify();
                            });
                        }),
                    )),
            )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> Div {
        let state = self.state.read(cx);
        let error = state.status.is_error;
        let color = if error { DANGER } else { OK };
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_5()
            .py_2()
            .bg(rgb(if error { DANGER_DIM } else { BG }))
            .border_t_1()
            .border_color(rgb(if error { DANGER } else { CARD_BORDER }))
            .child(div().w(px(7.0)).h(px(7.0)).rounded_full().bg(rgb(color)))
            .child(div().text_xs().font_semibold().text_color(rgb(color)).child(if error {
                "ALERT / 需要处理"
            } else {
                "SYSTEM / 就绪"
            }))
            .child(div().w(px(1.0)).h(px(12.0)).bg(rgb(CARD_BORDER)))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .child(state.status.text.clone()),
            )
    }

    fn render_presets(&self, cx: &mut Context<Self>) -> Div {
        let rows: Vec<(String, String, String)> = {
            let state = self.state.read(cx);
            state
                .presets
                .presets()
                .iter()
                .take(20)
                .map(|preset| {
                    (
                        preset.preset_id.clone(),
                        preset.name.clone(),
                        describe_preset_intent(preset),
                    )
                })
                .collect()
        };
        let count = rows.len();
        let empty = rows.is_empty();

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_sm()
            .bg(rgb(CARD_BG))
            .border_1()
            .border_color(rgb(CARD_BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_xs().text_color(rgb(ACCENT)).child("ARCHIVE  /  P"))
                            .child(div().text_base().font_semibold().text_color(rgb(TEXT)).child("快速预设")),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(BG))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(count.to_string()),
                    ),
            )
            .when(empty, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .p_4()
                        .rounded_md()
                        .bg(rgb(BG))
                        .text_color(rgb(MUTED))
                        .child("—")
                        .child(
                            div()
                                .text_xs()
                                .child("完成一次配装后，可从顶部保存为预设。"),
                        ),
                )
            })
            .child(
                div()
                    .id("preset-list")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(px(260.0))
                    .overflow_y_scroll()
                    .children(rows.into_iter().map(|(preset_id, name, summary)| {
                        let id_for_apply = preset_id.clone();
                        div()
                            .id(SharedString::from(format!("preset-{preset_id}")))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .p_2()
                            .rounded_md()
                            .bg(rgb(BG))
                            .border_1()
                            .border_color(rgb(CARD_BORDER))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_sm().text_color(rgb(TEXT)).child(name))
                                    .child(div().text_xs().text_color(rgb(MUTED)).child(summary)),
                            )
                            .child(
                                Button::new(SharedString::from(format!("apply-{preset_id}")))
                                    .tooltip("将此预设应用到当前存档")
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.apply_preset(&id_for_apply, cx);
                                    })),
                            )
                    })),
            )
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_diagnostics = self.state.read(cx).show_diagnostics;
        let dialogs = Root::render_dialog_layer(window, cx);
        let sheets = Root::render_sheet_layer(window, cx);
        let notifications = Root::render_notification_layer(window, cx);
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(self.render_header(cx))
            .child(self.render_toolbar(cx))
            .child(self.render_update_banner(cx))
            .child(
                div()
                    .id("main-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().w(px(28.0)).h(px(2.0)).bg(rgb(ACCENT)))
                            .child(div().text_xs().text_color(rgb(ACCENT)).child("DEPLOYMENT  /  双甲配装"))
                            .child(div().flex_1().h(px(1.0)).bg(rgb(CARD_BORDER))),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .items_stretch()
                            .child(self.render_slot_card(TargetSlot::Head, cx))
                            .child(self.render_slot_card(TargetSlot::Body, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_4()
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_4()
                                    .child(self.render_browser(cx))
                                    .child(self.render_import_review(cx)),
                            )
                            .child(
                                div()
                                    .w(px(310.0))
                                    .flex_shrink_0()
                                    .flex()
                                    .flex_col()
                                    .gap_4()
                                    .child(self.render_guide(cx))
                                    .child(self.render_presets(cx))
                                    .when(show_diagnostics, |this| {
                                        this.child(self.render_diagnostics(cx))
                                    }),
                            ),
                    ),
            )
            .child(self.render_diff_bar(cx))
            .child(self.render_status(cx))
            .children(dialogs)
            .children(sheets)
            .children(notifications)
    }
}

/// Display name with a fallback for unnamed entries.
fn display_name(item: &loadout_domain::Item) -> String {
    if item.display_name.trim().is_empty() {
        "（未命名护甲）".to_string()
    } else {
        item.display_name.clone()
    }
}

/// Describe a preset's stored intent for the list.
fn describe_preset_intent(preset: &loadout_domain::LoadoutPreset) -> String {
    let slot = |intent: &SlotIntent| match intent {
        SlotIntent::Keep => "保持原值".to_string(),
        SlotIntent::Set { item } => item.label(),
    };
    format!(
        "头部：{} · 身体：{}",
        slot(&preset.head),
        slot(&preset.body)
    )
}

/// Silence unused-import warnings for items used only in some build paths.
#[allow(dead_code)]
fn _unused(_: LoadoutIntent) {}
