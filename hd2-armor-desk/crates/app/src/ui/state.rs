//! Application state: what the window shows, and the only place that talks to
//! `local_io`.
//!
//! Threading rules followed here:
//! * the UI thread owns `AppState`; every mutation happens inside `update`;
//! * file reads, imports, hashing and encoding run on the background executor
//!   and return plain data;
//! * results carry the generation they were produced for, and a stale result is
//!   dropped instead of being applied to the wrong document.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use gpui_kit::{AppContext as _, AsyncApp, Context, Task, WeakEntity};
use loadout_domain::{
    apply_catalog_delta, import_legacy_csv, import_legacy_json, import_v2_json, preview_intent,
    resolve_import, Catalog, Classification, Draft, HumanReadableDiff, ItemRef, ItemType,
    LoadoutPreset, PresetStore, SlotIntent, Snapshot, TypeResolution, MAX_IMPORT_BYTES,
};
use local_io::{
    commit_to_source, commit_to_source_force_latest, prepare_commit, read_bounded, restore_backup,
    save_copy, sha256_hex, BackupEntry, CommitReceipt, CopyReceipt, MonitorEvent, PreparedCommit,
    SaveCandidate, StableReader, Workspace,
};
use sav_codec::{SaveImage, MAX_INPUT};

const BUNDLED_CATALOG_JSON: &str = include_str!("../../assets/catalog.v2.json");

fn sync_bundled_armor_facts(catalog: &mut Catalog, bundled: &Catalog) -> (usize, usize) {
    let verified: Vec<(String, ItemType, loadout_domain::Metadata)> = bundled
        .items()
        .iter()
        .filter(|item| item.classification == Classification::UserVerified)
        .map(|item| (item.item_key.clone(), item.item_type, item.metadata.clone()))
        .collect();
    let mut promoted = 0;
    let mut enriched = 0;
    for (item_key, item_type, metadata) in verified {
        let needs_promotion = catalog
            .get(&item_key)
            .map(|item| item.classification != Classification::UserVerified)
            .unwrap_or(false);
        let resolved_key = if needs_promotion {
            match catalog.confirm_type(&item_key, item_type) {
                Ok(resolved_key) => {
                    promoted += 1;
                    resolved_key
                }
                Err(_) => item_key,
            }
        } else {
            item_key
        };
        if catalog.fill_missing_passive_metadata(&resolved_key, &metadata) {
            enriched += 1;
        }
    }
    (promoted, enriched)
}

/// A loaded import preview plus the decisions made about it.
#[derive(Clone)]
pub struct PendingImport {
    pub preview: loadout_domain::ImportPreview,
    /// Manual type overrides for ambiguous records, keyed by ID.
    pub resolutions: BTreeMap<u32, TypeResolution>,
    /// Where the untouched copy of the user's source file was archived.
    pub archive_path: Option<PathBuf>,
}

/// Which slot a selection is being made for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSlot {
    Head,
    Body,
}

/// How a source write handles a draft whose disk baseline has changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitMode {
    /// Require the source to match the draft baseline.
    Safe,
    /// Reapply the requested armor slots to the latest valid source revision.
    ForceLatest,
}

/// Bottom-bar status text and its severity.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusLine {
    pub text: String,
    pub is_error: bool,
}

impl StatusLine {
    pub fn info(text: impl Into<String>) -> StatusLine {
        StatusLine {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> StatusLine {
        StatusLine {
            text: text.into(),
            is_error: true,
        }
    }
}

/// A save candidate offered in the open dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateRow {
    pub path: PathBuf,
    pub description: String,
}

/// Everything the window renders from.
pub struct AppState {
    pub workspace: Workspace,
    pub catalog: Catalog,
    pub presets: PresetStore,
    pub draft: Option<Draft>,
    pub diff: Option<HumanReadableDiff>,
    pub status: StatusLine,
    pub snapshot: Option<Snapshot>,
    pub candidates: Vec<CandidateRow>,
    pub pending_import: Option<PendingImport>,

    /// Search text for the catalog browser.
    pub head_query: String,
    pub body_query: String,
    /// Which slot the browser is choosing for.
    pub target_slot: TargetSlot,
    pub type_filter: Option<ItemType>,

    /// Monitoring.
    pub reader: StableReader,
    pub watching: bool,
    pub last_disk_revision: Option<String>,

    /// One save job at a time.
    pub save_in_flight: bool,
    pub last_commit: Option<CommitReceipt>,
    pub last_copy: Option<CopyReceipt>,
    /// Result of the last read-back verification, shown in the diagnostics panel.
    pub last_verify_note: Option<String>,
    pub show_diagnostics: bool,

    /// Handle for the polling loop; kept so it is not dropped.
    pub watch_task: Option<Task<()>>,
}

impl AppState {
    /// Build the initial state, loading the workspace.
    #[allow(clippy::new_without_default)] // The default workspace depends on the environment.
    pub fn new() -> AppState {
        let workspace_root = Workspace::default_root();
        let workspace = Workspace::open(&workspace_root).unwrap_or_else(|error| {
            panic!(
                "无法在软件目录创建工作区 {}：{error}",
                workspace_root.display()
            )
        });
        let bundled_catalog = Catalog::from_document_json(BUNDLED_CATALOG_JSON)
            .expect("发布包内置的 catalog.v2.json 必须有效");
        let (catalog, catalog_status) = if workspace.catalog_path().is_file() {
            match workspace.load_catalog() {
                Ok(mut catalog) => {
                    let (promoted, enriched) =
                        sync_bundled_armor_facts(&mut catalog, &bundled_catalog);
                    let status = if promoted == 0 && enriched == 0 {
                        None
                    } else {
                        Some(match workspace.save_catalog(&catalog) {
                            Ok(()) => StatusLine::info(format!(
                                "已更新目录：确认 {promoted} 件护甲分类，补齐 {enriched} 件护甲被动"
                            )),
                            Err(error) => StatusLine::error(format!(
                                "目录已在本次运行中更新，但无法保存：{error}"
                            )),
                        })
                    };
                    (catalog, status)
                }
                Err(error) => (
                    Catalog::default(),
                    Some(StatusLine::error(format!(
                        "已有目录无法读取，未使用内置目录覆盖：{error}"
                    ))),
                ),
            }
        } else {
            let status = match workspace.save_catalog(&bundled_catalog) {
                Ok(()) => StatusLine::info(format!(
                    "已初始化内置目录：{} 件物品",
                    bundled_catalog.len()
                )),
                Err(error) => {
                    StatusLine::error(format!("已加载内置目录，但无法保存到软件目录：{error}"))
                }
            };
            (bundled_catalog, Some(status))
        };
        let presets = workspace.load_presets().unwrap_or_default();
        let status = catalog_status.unwrap_or_else(|| {
            StatusLine::info(if catalog.is_empty() {
                "目录为空：可点击“导入目录”选择 catalog.json、v2 JSON 或 CSV".to_string()
            } else {
                format!("已载入 {} 件物品", catalog.len())
            })
        });

        AppState {
            workspace,
            catalog,
            presets,
            draft: None,
            diff: None,
            status,
            snapshot: None,
            candidates: Vec::new(),
            pending_import: None,
            head_query: String::new(),
            body_query: String::new(),
            target_slot: TargetSlot::Head,
            type_filter: None,
            reader: StableReader::default(),
            watching: false,
            last_disk_revision: None,
            save_in_flight: false,
            last_commit: None,
            last_copy: None,
            last_verify_note: None,
            show_diagnostics: false,
            watch_task: None,
        }
    }

    /// Whether a draft exists and differs from its base.
    pub fn is_dirty(&self) -> bool {
        self.draft.as_ref().map(Draft::is_dirty).unwrap_or(false)
    }

    /// Find a catalog entry by ID, preferring the type the user is targeting.
    pub fn item_by_id(&self, id: u32, prefer: Option<ItemType>) -> Option<&loadout_domain::Item> {
        let mut fallback = None;
        for item in self.catalog.items() {
            if item.id_u32 != id {
                continue;
            }
            if Some(item.item_type) == prefer {
                return Some(item);
            }
            if fallback.is_none() {
                fallback = Some(item);
            }
        }
        fallback
    }

    /// Whether a snapshot is loaded and writable.
    pub fn can_write(&self) -> bool {
        self.snapshot
            .as_ref()
            .map(|snap| snap.writable)
            .unwrap_or(false)
    }

    /// Player-facing label for the current head selection.
    pub fn head_label(&self) -> String {
        self.slot_label(TargetSlot::Head)
    }

    /// Player-facing label for the current body selection.
    pub fn body_label(&self) -> String {
        self.slot_label(TargetSlot::Body)
    }

    fn slot_label(&self, slot: TargetSlot) -> String {
        let (intent, disk_id, prefer) = match slot {
            TargetSlot::Head => (
                self.draft.as_ref().map(|draft| draft.intent().head.clone()),
                self.snapshot.as_ref().and_then(Snapshot::head_id),
                Some(ItemType::Armor),
            ),
            TargetSlot::Body => (
                self.draft.as_ref().map(|draft| draft.intent().body.clone()),
                self.snapshot.as_ref().and_then(Snapshot::body_id),
                Some(ItemType::Armor),
            ),
        };

        match intent {
            Some(SlotIntent::Set { item }) => {
                let type_note = match item.item_type {
                    ItemType::Armor => "身体护甲",
                    ItemType::PrimaryWeapon => "主要武器",
                    ItemType::Helmet => "头盔",
                    ItemType::Cape => "披风",
                    ItemType::Unknown => "未知类型",
                };
                format!("{}（{type_note}）", item.label())
            }
            _ => match disk_id {
                Some(id) => match self.item_by_id(id, prefer) {
                    Some(item) => {
                        let name = if item.display_name.trim().is_empty() {
                            "未命名".to_string()
                        } else {
                            item.display_name.clone()
                        };
                        format!("{name}（当前存档值 · 0x{id:08X}）")
                    }
                    None => format!("未知物品 · 0x{id:08X}（不在目录中）"),
                },
                None => "未读取".to_string(),
            },
        }
    }

    /// Whether the body slot is currently locked against edits.
    pub fn body_locked(&self) -> bool {
        self.draft
            .as_ref()
            .map(|draft| draft.body_locked())
            .unwrap_or(false)
    }

    /// Whether the draft has an unresolved conflict with the disk.
    pub fn in_conflict(&self) -> bool {
        self.draft
            .as_ref()
            .map(|draft| draft.in_conflict())
            .unwrap_or(false)
    }

    /// Whether the draft has an undo step available.
    pub fn can_undo(&self) -> bool {
        self.draft
            .as_ref()
            .map(|draft| draft.can_undo())
            .unwrap_or(false)
    }

    /// Whether the current head selection is a body armor placed in the head slot.
    pub fn head_is_armor_in_helmet_slot(&self) -> bool {
        self.draft
            .as_ref()
            .and_then(|draft| draft.intent().head_item_type())
            .map(|item_type| item_type == ItemType::Armor)
            .unwrap_or(false)
    }

    /// Item key of the current head selection, when set.
    pub fn head_item_key(&self) -> Option<String> {
        match self.draft.as_ref().map(|draft| draft.intent().head.clone()) {
            Some(SlotIntent::Set { item }) => Some(item.item_key),
            _ => None,
        }
    }

    /// Item key of the current body selection, when set.
    pub fn body_item_key(&self) -> Option<String> {
        match self.draft.as_ref().map(|draft| draft.intent().body.clone()) {
            Some(SlotIntent::Set { item }) => Some(item.item_key),
            _ => None,
        }
    }

    /// Items matching the browser query for the active target slot.
    pub fn browser_items(&self) -> Vec<&loadout_domain::Item> {
        let query = match self.target_slot {
            TargetSlot::Head => &self.head_query,
            TargetSlot::Body => &self.body_query,
        };
        let filter = loadout_domain::SearchFilter {
            item_type: self.type_filter,
            favorites_only: false,
        };
        let mut items = self.catalog.search(query, &filter);
        // Prefer authoritative items first, then by name.
        items.sort_by(|a, b| {
            b.classification
                .is_authoritative()
                .cmp(&a.classification.is_authoritative())
                .then_with(|| a.display_name.cmp(&b.display_name))
        });
        items
    }

    /// Recompute the diff for the current intent.
    pub fn refresh_diff(&mut self) {
        let (Some(draft), catalog) = (self.draft.as_ref(), &self.catalog) else {
            self.diff = None;
            return;
        };
        let snapshot = draft.base().clone();
        let intent = draft.intent().clone();
        match preview_intent(&snapshot, catalog, &intent) {
            Ok(diff) => self.diff = Some(diff),
            Err(error) => {
                self.diff = None;
                self.status = StatusLine::error(format!("无法预览改动：{error}"));
            }
        }
    }

    /// Build an [`ItemRef`] for a catalog item.
    pub fn item_ref(item: &loadout_domain::Item) -> ItemRef {
        ItemRef {
            item_key: item.item_key.clone(),
            id_u32: item.id_u32,
            item_type: item.item_type,
            label_snapshot: if item.display_name.trim().is_empty() {
                format!("未命名护甲 · {}", loadout_domain::ItemId(item.id_u32).hex())
            } else {
                item.display_name.clone()
            },
        }
    }
}

/// Background task results, tagged with the generation they belong to.
pub enum BackgroundResult {
    Opened {
        generation: u64,
        snapshot: Box<Snapshot>,
        path: PathBuf,
    },
    OpenFailed {
        generation: u64,
        message: String,
    },
    ImportPreview(Box<Result<PendingImport, String>>),
    Candidates(Vec<CandidateRow>),
}

/// Spawn a background read of a save file.
pub fn spawn_open_save(
    path: PathBuf,
    generation: u64,
    cx: &mut Context<AppState>,
) -> Task<BackgroundResult> {
    cx.background_spawn(async move {
        let bytes = match read_bounded(&path, MAX_INPUT) {
            Ok(bytes) => bytes,
            Err(message) => {
                return BackgroundResult::OpenFailed {
                    generation,
                    message,
                }
            }
        };
        let digest = sha256_hex(&bytes);
        match SaveImage::decode(bytes.clone()) {
            Ok(image) => {
                let snapshot = Snapshot {
                    raw: image.raw().to_vec(),
                    payload: image.payload().to_vec(),
                    sha256: digest,
                    captured_at: local_io::now_stamp(),
                    path: path.display().to_string(),
                    writable: image.is_writable(),
                    readonly_reason: image.support().detail().map(str::to_string),
                    outer_crc32: image.outer_crc32(),
                    inner_low32: image.inner_low32(),
                };
                BackgroundResult::Opened {
                    generation,
                    snapshot: Box::new(snapshot),
                    path,
                }
            }
            Err(error) => BackgroundResult::OpenFailed {
                generation,
                message: local_io::describe_decode_error(&error),
            },
        }
    })
}

/// Spawn a background import of a catalog file.
pub fn spawn_import_catalog(
    path: PathBuf,
    workspace: Workspace,
    cx: &mut Context<AppState>,
) -> Task<BackgroundResult> {
    cx.background_spawn(async move {
        let result = (|| -> Result<PendingImport, String> {
            let bytes = read_bounded(&path, MAX_IMPORT_BYTES)?;
            let file_name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "catalog".into());
            let is_csv = path
                .extension()
                .map(|extension| extension.eq_ignore_ascii_case("csv"))
                .unwrap_or(false);
            let is_json = path
                .extension()
                .map(|extension| extension.eq_ignore_ascii_case("json"))
                .unwrap_or(false);

            let preview = if is_json {
                match import_v2_json(&bytes, &file_name) {
                    Ok(preview) => preview,
                    Err(v2_error) => import_legacy_json(&bytes, &file_name).map_err(|legacy_error| {
                        format!(
                            "既不是有效的 v2 目录（{v2_error}），也不是旧工具 JSON（{legacy_error}）"
                        )
                    })?,
                }
            } else if is_csv {
                import_legacy_csv(&bytes, &file_name)?
            } else {
                return Err("只支持 .json 或 .csv 目录文件".to_string());
            };

            // Keep the user's original file, untouched, in the workspace.
            let archive = workspace
                .archive_import(&preview.source_file_name, &bytes, &local_io::now_stamp())
                .ok();

            Ok(PendingImport {
                preview,
                resolutions: BTreeMap::new(),
                archive_path: archive,
            })
        })();
        BackgroundResult::ImportPreview(Box::new(result))
    })
}

/// Discover Steam save candidates with a bounded scan of known roots.
pub fn discover_candidates() -> Vec<CandidateRow> {
    local_io::discover_all()
        .into_iter()
        .map(|candidate: SaveCandidate| CandidateRow {
            description: candidate.describe(),
            path: candidate.path,
        })
        .collect()
}

/// Spawn Steam candidate discovery.
pub fn spawn_discover(cx: &mut Context<AppState>) -> Task<BackgroundResult> {
    cx.background_spawn(async move { BackgroundResult::Candidates(discover_candidates()) })
}

/// Apply an import preview into the catalog.
pub fn apply_import(state: &mut AppState, pending: &PendingImport) -> Result<usize, String> {
    let delta = resolve_import(&pending.preview, &pending.resolutions, &state.catalog);
    let before = state.catalog.len();
    let catalog = apply_catalog_delta(&state.catalog, &delta);
    let added = catalog.len().saturating_sub(before);
    state
        .workspace
        .save_catalog(&catalog)
        .map_err(|error| error.to_string())?;
    state.catalog = catalog;
    Ok(added)
}

/// Run the polling loop for the watched file.
pub fn spawn_watch(
    state: &WeakEntity<AppState>,
    generation: u64,
    cx: &mut Context<AppState>,
) -> Task<()> {
    let weak = state.clone();
    cx.spawn(
        async move |_view: WeakEntity<AppState>, cx: &mut AsyncApp| {
            loop {
                cx.background_executor()
                    .timer(local_io::POLL_INTERVAL)
                    .await;
                let Some(entity) = weak.upgrade() else {
                    return;
                };
                // Poll on the UI thread: the read itself is small and bounded, and
                // doing it here keeps the "no writes while monitoring" rule obvious.
                let event = entity.update(cx, |state, _| {
                    if !state.watching || state.reader.generation().0 != generation {
                        return None;
                    }
                    state.reader.poll(Instant::now())
                });
                let Some(event) = event else {
                    continue;
                };
                let _ = weak.update(cx, |state, cx| handle_monitor_event(state, event, cx));
            }
        },
    )
}

/// React to one monitoring event.
pub fn handle_monitor_event(state: &mut AppState, event: MonitorEvent, cx: &mut Context<AppState>) {
    match event {
        MonitorEvent::Snapshot {
            snapshot, changed, ..
        } => {
            state.last_disk_revision = Some(snapshot.sha256.clone());
            if !changed {
                return;
            }
            match state.draft.as_mut() {
                Some(draft) if draft.is_dirty() => {
                    // A dirty draft must never be silently rebased.
                    draft.mark_conflict(snapshot.as_ref(), local_io::now_stamp());
                    state.status = StatusLine::error(
                        "检测到存档已被外部更新；草稿保留，请选择“以磁盘为准”或“保留草稿”"
                            .to_string(),
                    );
                }
                Some(draft) => {
                    draft.abandon_to((*snapshot).clone());
                    state.snapshot = Some((*snapshot).clone());
                    state.refresh_diff();
                    state.status = StatusLine::info("存档已更新，草稿已跟随最新内容".to_string());
                }
                None => {
                    state.snapshot = Some((*snapshot).clone());
                }
            }
            cx.notify();
        }
        MonitorEvent::Pending { reason, .. } => {
            state.last_verify_note = Some(format!("等待写入完成：{reason}"));
            cx.notify();
        }
        MonitorEvent::ReadError { message, .. } => {
            state.status = StatusLine::error(format!("监视中断：{message}"));
            cx.notify();
        }
    }
}

/// Run a write-back on the background executor.
pub fn spawn_commit(
    prepared: PreparedCommit,
    backup_root: PathBuf,
    mode: CommitMode,
    cx: &mut Context<AppState>,
) -> Task<Result<CommitReceipt, String>> {
    cx.background_spawn(async move {
        match mode {
            CommitMode::Safe => commit_to_source(&prepared, &backup_root),
            CommitMode::ForceLatest => commit_to_source_force_latest(&prepared, &backup_root),
        }
        .map_err(|error| error.to_string())
    })
}

/// Restore a whole backup over the target on the background executor.
///
/// `restore_backup` always writes a safety copy of what it replaces, so this is
/// not a destructive one-way door even though it replaces the entire file.
pub fn spawn_restore(
    backup_path: PathBuf,
    target: PathBuf,
    backup_root: PathBuf,
    cx: &mut Context<AppState>,
) -> Task<Result<CommitReceipt, String>> {
    cx.background_spawn(async move {
        restore_backup(&backup_path, &target, &backup_root).map_err(|error| error.to_string())
    })
}

/// List the backups available for restoring, newest first.
pub fn list_backups(state: &AppState) -> Vec<BackupEntry> {
    state.workspace.list_backups()
}

/// Run a save-as on the background executor.
pub fn spawn_copy(
    prepared: PreparedCommit,
    target: PathBuf,
    cx: &mut Context<AppState>,
) -> Task<Result<CopyReceipt, String>> {
    cx.background_spawn(
        async move { save_copy(&prepared, &target).map_err(|error| error.to_string()) },
    )
}

/// Prepare a commit (patch, re-encode, re-verify) on the background executor.
pub fn spawn_prepare(
    path: PathBuf,
    snapshot: Snapshot,
    patches: Vec<sav_codec::FieldPatch>,
    diff: HumanReadableDiff,
    generation: u64,
    mode: CommitMode,
    cx: &mut Context<AppState>,
) -> Task<Result<PreparedCommit, String>> {
    cx.background_spawn(async move {
        // Risk mode needs the original draft bytes only to freeze the requested
        // target values. The transaction itself will decode and patch the latest
        // disk revision. Safe mode keeps its early source-change check.
        let raw = match mode {
            CommitMode::Safe => read_bounded(&path, MAX_INPUT)?,
            CommitMode::ForceLatest => snapshot.raw.clone(),
        };
        let image = SaveImage::decode(raw).map_err(|error| error.to_string())?;
        prepare_commit(&path, &snapshot, &image, &patches, &diff, generation)
            .map_err(|error| error.to_string())
    })
}

fn verified_receipt_image(receipt: &CommitReceipt) -> Result<SaveImage, String> {
    let raw = read_bounded(&receipt.source_path, MAX_INPUT)
        .map_err(|error| format!("回读失败：{error}"))?;
    let actual_sha256 = sha256_hex(&raw);
    if actual_sha256 != receipt.output_sha256 {
        return Err(format!(
            "回读摘要不一致：预期 {}，实际 {}",
            receipt.output_sha256, actual_sha256
        ));
    }
    SaveImage::decode(raw).map_err(|error| format!("回读内容无法解码：{error}"))
}

impl AppState {
    /// Record a verified write-back for the document generation that started it.
    pub fn record_commit(&mut self, generation: u64, receipt: CommitReceipt) {
        self.save_in_flight = false;
        self.last_verify_note = Some(receipt.detail.clone());
        self.last_commit = Some(receipt.clone());

        if self.reader.generation().0 != generation {
            return;
        }

        if receipt.is_verified() {
            let Some(current_path) = self.snapshot.as_ref().map(|snapshot| &snapshot.path) else {
                self.status = StatusLine::error("写回完成，但当前文档已关闭".to_string());
                return;
            };
            if std::path::Path::new(current_path) != receipt.source_path.as_path() {
                self.status = StatusLine::error("写回完成，但当前已打开另一份存档".to_string());
                return;
            }
            match verified_receipt_image(&receipt) {
                Ok(image) => {
                    if let (Some(draft), Some(snapshot)) =
                        (self.draft.as_mut(), self.snapshot.as_mut())
                    {
                        let committed = Snapshot {
                            raw: image.raw().to_vec(),
                            payload: image.payload().to_vec(),
                            sha256: receipt.output_sha256.clone(),
                            captured_at: receipt.committed_at.clone(),
                            path: receipt.source_path.display().to_string(),
                            writable: image.is_writable(),
                            readonly_reason: image.support().detail().map(str::to_string),
                            outer_crc32: image.outer_crc32(),
                            inner_low32: image.inner_low32(),
                        };
                        *snapshot = committed.clone();
                        draft.accept_committed(committed);
                        self.reader.seed_accepted(receipt.output_sha256.clone());
                        self.last_disk_revision = Some(receipt.output_sha256.clone());
                        self.status = StatusLine::info(format!(
                            "{}；备份：{}",
                            receipt.outcome.label(),
                            receipt
                                .backup_path
                                .as_ref()
                                .map(|path| path.display().to_string())
                                .unwrap_or_else(|| "（无）".into())
                        ));
                    }
                }
                Err(message) => {
                    self.status = StatusLine::error(format!("写回后未接受新基线：{message}"));
                }
            }
        } else {
            self.status =
                StatusLine::error(format!("{}：{}", receipt.outcome.label(), receipt.detail));
        }

        self.refresh_diff();
    }

    /// Record a completed restore for the document generation that started it.
    pub fn record_restore(&mut self, generation: u64, receipt: CommitReceipt) {
        self.save_in_flight = false;
        self.last_verify_note = Some(receipt.detail.clone());
        self.last_commit = Some(receipt.clone());

        if self.reader.generation().0 != generation {
            return;
        }

        if receipt.is_verified() {
            let Some(current_path) = self.snapshot.as_ref().map(|snapshot| &snapshot.path) else {
                self.status = StatusLine::error("恢复完成，但当前文档已关闭".to_string());
                return;
            };
            if std::path::Path::new(current_path) != receipt.source_path.as_path() {
                self.status = StatusLine::error("恢复完成，但当前已打开另一份存档".to_string());
                return;
            }
            match verified_receipt_image(&receipt) {
                Ok(image) => {
                    let snapshot = Snapshot {
                        raw: image.raw().to_vec(),
                        payload: image.payload().to_vec(),
                        sha256: receipt.output_sha256.clone(),
                        captured_at: receipt.committed_at.clone(),
                        path: receipt.source_path.display().to_string(),
                        writable: image.is_writable(),
                        readonly_reason: image.support().detail().map(str::to_string),
                        outer_crc32: image.outer_crc32(),
                        inner_low32: image.inner_low32(),
                    };
                    self.reader.seed_accepted(snapshot.sha256.clone());
                    self.last_disk_revision = Some(snapshot.sha256.clone());
                    self.draft = Some(Draft::new(snapshot.clone()));
                    self.snapshot = Some(snapshot);
                    self.status = StatusLine::info(format!(
                        "{}；恢复前副本：{}",
                        receipt.outcome.label(),
                        receipt
                            .backup_path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "（无）".into())
                    ));
                }
                Err(message) => {
                    self.status = StatusLine::error(format!("恢复后未接受新基线：{message}"));
                }
            }
        } else {
            self.status =
                StatusLine::error(format!("{}：{}", receipt.outcome.label(), receipt.detail));
        }
        self.refresh_diff();
    }

    /// Record a save-as receipt. The source is untouched, so nothing else changes.
    pub fn record_copy(&mut self, receipt: CopyReceipt) {
        self.status = StatusLine::info(format!(
            "已另存到 {}（{}）；源文件未改动",
            receipt.target_path.display(),
            format_bytes(receipt.bytes_written)
        ));
        self.last_copy = Some(receipt);
    }
}

/// Build a preset from the current draft intent.
pub fn preset_from_draft(
    state: &AppState,
    name: impl Into<String>,
    preset_id: impl Into<String>,
) -> Option<LoadoutPreset> {
    let draft = state.draft.as_ref()?;
    if !draft.is_dirty() {
        return None;
    }
    Some(LoadoutPreset::from_intent(
        preset_id,
        name,
        draft.intent(),
        local_io::now_stamp(),
    ))
}

/// Whether the given ID may be placed in the given slot.
pub fn slot_accepts(slot: TargetSlot, item_type: ItemType) -> bool {
    match slot {
        // The head slot accepts body armor: that is the whole point of this tool.
        TargetSlot::Head => matches!(item_type, ItemType::Armor | ItemType::Helmet),
        TargetSlot::Body => matches!(item_type, ItemType::Armor),
    }
}

/// Human-readable description of why a type cannot go in a slot.
pub fn slot_rejection(slot: TargetSlot, item_type: ItemType) -> String {
    match (slot, item_type) {
        (TargetSlot::Head, ItemType::Cape) => "披风不能放入头部槽位".into(),
        (TargetSlot::Head, ItemType::PrimaryWeapon) => "主要武器不能放入头部槽位".into(),
        (TargetSlot::Head, ItemType::Unknown) => "目录未提供可用装备类型，不能放入头部槽位".into(),
        (TargetSlot::Body, ItemType::Helmet) => "头盔不能放入身体槽位".into(),
        (TargetSlot::Body, ItemType::PrimaryWeapon) => "主要武器不能放入身体槽位".into(),
        (TargetSlot::Body, ItemType::Cape) => "披风不能放入身体槽位".into(),
        (TargetSlot::Body, ItemType::Unknown) => "目录未提供可用装备类型，不能放入身体槽位".into(),
        _ => "该类型不能放入此槽位".into(),
    }
}

/// Whether an item is safe to apply in player mode.
pub fn player_usable(item: &loadout_domain::Item) -> bool {
    loadout_domain::is_player_usable(item.item_type, item.classification)
}

/// The generation to use for a newly opened document.
pub fn next_generation(state: &mut AppState) -> u64 {
    state.reader.generation().0 + 1
}

/// Format a byte count for the UI.
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
