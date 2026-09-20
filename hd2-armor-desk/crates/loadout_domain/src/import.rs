//! Import of the original Python tool's `catalog.json` and CSV exports.
//!
//! Everything here treats the input as untrusted: sizes, entry counts, string
//! lengths and value ranges are bounded, nothing is executed, and a conflicting
//! record is reported rather than silently overwritten.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;

use crate::catalog::{
    offset_field_name, slot_for_offset, Catalog, Classification, Item, Observation, MAX_ITEMS,
    MAX_NAME_LEN, MAX_NOTE_LEN, OFFSET_BODY, OFFSET_CAPE, OFFSET_HEAD,
};
use crate::types::{ItemType, SlotObservation, SourceFormat};

/// Maximum accepted import payload size.
pub const MAX_IMPORT_BYTES: usize = 32 * 1024 * 1024;
/// Maximum diagnostics retained from one hostile or malformed import.
const MAX_IMPORT_ISSUES: usize = 1_000;

/// One raw record from either legacy format, before interpretation.
#[derive(Debug, Clone, PartialEq)]
pub struct RawRecord {
    /// 1-based line number for CSV, or the index within `records` for JSON.
    pub line: usize,
    pub offset: u32,
    pub value: u32,
    pub field: String,
    pub name: String,
    pub note: String,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub count: Option<u64>,
    pub source_digest: Option<String>,
    pub source_format: SourceFormat,
}

/// A problem that blocks or annotates one record.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportIssue {
    pub line: usize,
    pub severity: IssueSeverity,
    pub message: String,
}

/// How bad an issue is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueSeverity {
    /// The record was rejected; other records still import.
    Error,
    /// The record imported but the user should look at it.
    Warning,
    /// Informational, e.g. an offset that is not one of the three known slots.
    Info,
}

/// What the user must confirm before the import is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestedType {
    /// A body-slot observation: likely a body armor, needs one-time confirmation.
    ArmorFromBodyObservation,
    /// A head-slot observation with no other evidence: slot only, type unknown.
    NeedsUserDecision,
    /// A cape-slot observation.
    CapeFromCapeObservation,
    /// Type comes from the tool's own verified mapping.
    AlreadyKnown,
}

/// A proposed item, pending user confirmation of ambiguous types.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposedItem {
    pub item_type: ItemType,
    pub classification: Classification,
    pub id_u32: u32,
    pub display_name: String,
    pub note: String,
    pub suggestion: SuggestedType,
    pub observations: Vec<Observation>,
    /// Raw record lines that produced this proposal.
    pub source_lines: Vec<usize>,
    /// True when the item already exists in the target catalog.
    pub already_present: bool,
}

/// The result of parsing, before any catalog is modified.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportPreview {
    pub source_format: SourceFormat,
    /// Records read from the file, before deduplication.
    pub raw_record_count: usize,
    /// Rejected records.
    pub issues: Vec<ImportIssue>,
    /// Deduplicated proposals.
    pub items: Vec<ProposedItem>,
    /// Raw records kept for auditing; never shown as the primary list.
    pub raw_records: Vec<RawRecord>,
    /// The original file's bytes, to be copied into the workspace untouched.
    pub source_bytes: Vec<u8>,
    pub source_file_name: String,
}

impl ImportPreview {
    /// Records that could be turned into items.
    pub fn accepted_count(&self) -> usize {
        self.raw_record_count
            - self
                .issues
                .iter()
                .filter(|issue| issue.severity == IssueSeverity::Error)
                .count()
    }

    /// Proposed body armors awaiting confirmation.
    pub fn armor_suggestions(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.suggestion == SuggestedType::ArmorFromBodyObservation)
            .count()
    }

    /// Proposed normal helmets (only from already-verified mappings).
    pub fn helmet_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.item_type == ItemType::Helmet)
            .count()
    }

    /// Items whose type is still unknown.
    pub fn unknown_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.item_type == ItemType::Unknown)
            .count()
    }

    /// Items that collide with an existing catalog entry of another type.
    pub fn conflict_count(&self, existing: &Catalog) -> usize {
        self.items
            .iter()
            .filter(|item| {
                existing
                    .by_id(item.id_u32)
                    .iter()
                    .any(|present| present.item_type != item.item_type)
            })
            .count()
    }

    /// One-line summary for the import dialog.
    pub fn summary(&self) -> String {
        format!(
            "共读取 {} 条记录，去重后 {} 个物品；建议身体护甲 {}，普通头盔 {}，未知 {}，被拒 {} 条",
            self.raw_record_count,
            self.items.len(),
            self.armor_suggestions(),
            self.helmet_count(),
            self.unknown_count(),
            self.issues
                .iter()
                .filter(|issue| issue.severity == IssueSeverity::Error)
                .count()
        )
    }
}

/// User's decision for one ambiguous proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeResolution {
    /// Import with the proposed type.
    AcceptSuggestion,
    /// Import as a body armor.
    AsArmor,
    /// Import as a normal helmet.
    AsHelmet,
    /// Import as a cape.
    AsCape,
    /// Keep the type unknown.
    KeepUnknown,
    /// Skip this item entirely.
    Skip,
}

/// A confirmed import ready to be applied to a catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogDelta {
    pub items: Vec<Item>,
    pub skipped: usize,
}

// ---------------------------------------------------------------- legacy JSON

#[derive(Debug, Deserialize)]
struct LegacyDocument {
    schema: u32,
    records: Vec<LegacyRecord>,
}

#[derive(Debug, Deserialize)]
struct LegacyRecord {
    offset: i64,
    value: i64,
    #[serde(default)]
    field: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    first_seen: Option<String>,
    #[serde(default)]
    last_seen: Option<String>,
    #[serde(default)]
    count: Option<i64>,
    #[serde(default)]
    source_sha256: Option<String>,
}

/// Parse the original tool's `catalog.json`.
pub fn import_legacy_json(bytes: &[u8], file_name: &str) -> Result<ImportPreview, String> {
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(format!("文件超过 {MAX_IMPORT_BYTES} 字节上限"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "目录 JSON 不是有效的 UTF-8".to_string())?;
    let document: LegacyDocument =
        serde_json::from_str(text).map_err(|error| format!("目录 JSON 解析失败：{error}"))?;
    if document.schema != 1 {
        return Err(format!(
            "这不是旧工具的 schema 1 目录（读到 schema={}）",
            document.schema
        ));
    }
    if document.records.len() > MAX_ITEMS {
        return Err(format!(
            "记录数 {} 超过上限 {MAX_ITEMS}",
            document.records.len()
        ));
    }

    let mut issues = Vec::new();
    let mut records = Vec::new();

    for (index, record) in document.records.iter().enumerate() {
        if issues.len() >= MAX_IMPORT_ISSUES {
            return Err(format!(
                "导入问题超过 {MAX_IMPORT_ISSUES} 条，已停止解析以避免耗尽内存"
            ));
        }
        let line = index + 1;
        // JSON integers are validated as exact u32; no truncation, no i32 wrap.
        let offset = match u32::try_from(record.offset) {
            Ok(value) if (16..=16 * 1024 * 1024 - 4).contains(&value) => value,
            _ => {
                issues.push(ImportIssue {
                    line,
                    severity: IssueSeverity::Error,
                    message: format!("偏移 {} 不是有效的正文偏移，已跳过该行", record.offset),
                });
                continue;
            }
        };
        let value = match u32::try_from(record.value) {
            Ok(value) => value,
            Err(_) => {
                issues.push(ImportIssue {
                    line,
                    severity: IssueSeverity::Error,
                    message: format!("ID {} 超出 u32 范围，已跳过该行", record.value),
                });
                continue;
            }
        };
        if record.name.chars().count() > MAX_NAME_LEN {
            issues.push(ImportIssue {
                line,
                severity: IssueSeverity::Error,
                message: "名称过长，已跳过该行".into(),
            });
            continue;
        }
        if record.note.chars().count() > MAX_NOTE_LEN {
            issues.push(ImportIssue {
                line,
                severity: IssueSeverity::Error,
                message: "备注过长，已跳过该行".into(),
            });
            continue;
        }
        if !is_known_offset(offset) {
            issues.push(ImportIssue {
                line,
                severity: IssueSeverity::Info,
                message: format!("偏移 0x{offset:04X} 不是已核验的三个槽位，只作为来源记录保留"),
            });
        }

        records.push(RawRecord {
            line,
            offset,
            value,
            field: record.field.clone(),
            name: record.name.clone(),
            note: record.note.clone(),
            first_seen: record.first_seen.clone(),
            last_seen: record.last_seen.clone(),
            count: record.count.and_then(|value| u64::try_from(value).ok()),
            source_digest: record.source_sha256.clone(),
            source_format: SourceFormat::LegacyJson,
        });
    }

    Ok(build_preview(
        records,
        issues,
        SourceFormat::LegacyJson,
        bytes,
        file_name,
        &Catalog::default(),
    ))
}

/// Parse this tool's v2 catalog into the same preview used for legacy imports.
///
/// The v2 schema already carries authoritative item types and observations, so
/// it must not be reparsed through the incompatible legacy schema.
pub fn import_v2_json(bytes: &[u8], file_name: &str) -> Result<ImportPreview, String> {
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(format!("文件超过 {MAX_IMPORT_BYTES} 字节上限"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "目录 JSON 不是有效的 UTF-8".to_string())?;
    let catalog = Catalog::from_document_json(text)?;
    let items = catalog
        .items()
        .iter()
        .map(|item| ProposedItem {
            item_type: item.item_type,
            classification: item.classification,
            id_u32: item.id_u32,
            display_name: item.display_name.clone(),
            note: item.note.clone(),
            suggestion: SuggestedType::AlreadyKnown,
            observations: item.observations.clone(),
            source_lines: Vec::new(),
            already_present: false,
        })
        .collect();

    Ok(ImportPreview {
        source_format: SourceFormat::CatalogV2,
        raw_record_count: catalog.len(),
        issues: Vec::new(),
        items,
        raw_records: Vec::new(),
        source_bytes: bytes.to_vec(),
        source_file_name: file_name.to_string(),
    })
}

// ----------------------------------------------------------------- legacy CSV

/// Column header of the original tool's CSV export.
const CSV_HEADER: [&str; 8] = [
    "字段",
    "正文偏移",
    "ID_十六进制",
    "ID_十进制",
    "名称",
    "备注",
    "首次采集_UTC",
    "末次采集_UTC",
];

/// Parse the original tool's CSV export (UTF-8 with BOM, standard quoting).
pub fn import_legacy_csv(bytes: &[u8], file_name: &str) -> Result<ImportPreview, String> {
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(format!("文件超过 {MAX_IMPORT_BYTES} 字节上限"));
    }
    let text = decode_csv_text(bytes)?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());

    let mut issues = Vec::new();
    let mut records = Vec::new();
    let mut header_checked = false;

    for (index, result) in reader.records().enumerate() {
        if index > MAX_ITEMS {
            return Err(format!("CSV 行数超过上限 {MAX_ITEMS}"));
        }
        if issues.len() >= MAX_IMPORT_ISSUES {
            return Err(format!(
                "导入问题超过 {MAX_IMPORT_ISSUES} 条，已停止解析以避免耗尽内存"
            ));
        }
        let line = index + 1;
        let row = match result {
            Ok(row) => row,
            Err(error) => {
                issues.push(ImportIssue {
                    line,
                    severity: IssueSeverity::Error,
                    message: format!("CSV 行解析失败：{error}"),
                });
                continue;
            }
        };

        if !header_checked {
            header_checked = true;
            let matches_header = row.iter().take(8).eq(CSV_HEADER.iter().copied());
            if !matches_header {
                // Header text is informational; column positions are what matter.
                issues.push(ImportIssue {
                    line,
                    severity: IssueSeverity::Info,
                    message: "表头与旧工具导出不完全一致，按列位置解析".into(),
                });
            }
            continue;
        }

        if row.iter().all(|field| field.trim().is_empty()) {
            continue;
        }
        if records.len() >= MAX_ITEMS {
            return Err(format!("记录数超过上限 {MAX_ITEMS}"));
        }

        let field = row.get(0).unwrap_or("").trim().to_string();
        let offset_text = row.get(1).unwrap_or("").trim();
        let hex_text = row.get(2).unwrap_or("").trim();
        let dec_text = row.get(3).unwrap_or("").trim();
        let name = row.get(4).unwrap_or("").to_string();
        let note = row.get(5).unwrap_or("").to_string();
        let first_seen = row.get(6).map(str::trim).filter(|s| !s.is_empty());
        let last_seen = row.get(7).map(str::trim).filter(|s| !s.is_empty());

        let offset = match parse_u32_text(offset_text) {
            Ok(value) if (16..=16 * 1024 * 1024 - 4).contains(&value) => value,
            _ => {
                issues.push(ImportIssue {
                    line,
                    severity: IssueSeverity::Error,
                    message: format!("偏移 {offset_text:?} 无法解析为有效正文偏移"),
                });
                continue;
            }
        };

        let hex_value = parse_u32_text(hex_text).ok();
        let dec_value = parse_u32_text(dec_text).ok();

        // Both columns present and contradictory: report the line, do not pick one.
        let value = match (hex_value, dec_value) {
            (Some(hex), Some(dec)) if hex != dec => {
                issues.push(ImportIssue {
                    line,
                    severity: IssueSeverity::Error,
                    message: format!(
                        "十六进制 ID {hex_text} 与十进制 ID {dec_text} 不一致（0x{hex:08X} ≠ {dec}）"
                    ),
                });
                continue;
            }
            (Some(value), _) | (None, Some(value)) => value,
            (None, None) => {
                issues.push(ImportIssue {
                    line,
                    severity: IssueSeverity::Error,
                    message: format!("ID 列无法解析：hex={hex_text:?} dec={dec_text:?}"),
                });
                continue;
            }
        };

        if name.chars().count() > MAX_NAME_LEN || note.chars().count() > MAX_NOTE_LEN {
            issues.push(ImportIssue {
                line,
                severity: IssueSeverity::Error,
                message: "名称或备注过长，已跳过该行".into(),
            });
            continue;
        }
        if !is_known_offset(offset) {
            issues.push(ImportIssue {
                line,
                severity: IssueSeverity::Info,
                message: format!("偏移 0x{offset:04X} 不是已核验的三个槽位，只作为来源记录保留"),
            });
        }

        records.push(RawRecord {
            line,
            offset,
            value,
            field,
            // Leading apostrophes are formula-injection guards added by the legacy
            // exporter; they are preserved as-is rather than silently stripped.
            name,
            note,
            first_seen: first_seen.map(str::to_string),
            last_seen: last_seen.map(str::to_string),
            count: None,
            source_digest: None,
            source_format: SourceFormat::LegacyCsv,
        });
    }

    if !header_checked {
        return Err("CSV 文件为空".into());
    }

    Ok(build_preview(
        records,
        issues,
        SourceFormat::LegacyCsv,
        bytes,
        file_name,
        &Catalog::default(),
    ))
}

/// Decode CSV bytes, tolerating a UTF-8 BOM.
fn decode_csv_text(bytes: &[u8]) -> Result<String, String> {
    let stripped = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8(stripped.to_vec()).map_err(|_| "CSV 不是有效的 UTF-8（含 BOM）文本".into())
}

// ------------------------------------------------------------ normalization

/// Deduplicate raw records into proposals, keyed by `(type, id)` — never by name.
fn build_preview(
    records: Vec<RawRecord>,
    mut issues: Vec<ImportIssue>,
    source_format: SourceFormat,
    source_bytes: &[u8],
    source_file_name: &str,
    existing: &Catalog,
) -> ImportPreview {
    let raw_record_count = records.len();
    // Group by raw ID first: the same ID observed at two offsets is one item.
    let mut by_id: BTreeMap<u32, Vec<&RawRecord>> = BTreeMap::new();
    for record in &records {
        by_id.entry(record.value).or_default().push(record);
    }

    let mut items = Vec::new();
    for (id, group) in by_id {
        let mut observations: Vec<Observation> = Vec::new();
        let mut observed_keys = HashSet::new();
        let mut source_lines = Vec::new();
        let mut best_name = String::new();
        let mut best_note = String::new();
        let mut digest: Option<String> = None;

        for record in &group {
            source_lines.push(record.line);
            let slot = slot_for_offset(record.offset);
            let observation = Observation {
                observed_offset: record.offset,
                observed_slot: slot,
                first_seen: record.first_seen.clone(),
                last_seen: record.last_seen.clone(),
                count: record.count,
                source_digest: record.source_digest.clone(),
                source_format: record.source_format,
            };
            if observed_keys.insert((observation.observed_offset, observation.observed_slot)) {
                observations.push(observation);
            }
            if best_name.trim().is_empty() && !record.name.trim().is_empty() {
                best_name = record.name.trim().to_string();
            }
            if best_note.trim().is_empty() && !record.note.trim().is_empty() {
                best_note = record.note.trim().to_string();
            }
            if digest.is_none() {
                digest = record.source_digest.clone();
            }
        }

        // Classification priority: verified mapping first, then body observation,
        // then cape, then the name only as a weak hint.
        let has_body = observations
            .iter()
            .any(|observation| observation.observed_slot == SlotObservation::Body);
        let has_head = observations
            .iter()
            .any(|observation| observation.observed_slot == SlotObservation::Head);
        let has_cape = observations
            .iter()
            .any(|observation| observation.observed_slot == SlotObservation::Cape);

        let (item_type, classification, suggestion) = match Item::seed_label(id) {
            Some(seed_name) if seed_name.contains("身体护甲") => (
                ItemType::Armor,
                Classification::SeedUserVerified,
                SuggestedType::AlreadyKnown,
            ),
            Some(seed_name) if seed_name.contains("头盔") => (
                ItemType::Helmet,
                Classification::SeedUserVerified,
                SuggestedType::AlreadyKnown,
            ),
            Some(_) => (
                ItemType::Cape,
                Classification::SeedUserVerified,
                SuggestedType::CapeFromCapeObservation,
            ),
            None => {
                if let Some(existing_item) = existing
                    .by_id(id)
                    .into_iter()
                    .find(|item| item.classification.is_authoritative())
                {
                    (
                        existing_item.item_type,
                        existing_item.classification,
                        SuggestedType::AlreadyKnown,
                    )
                } else if has_body {
                    (
                        ItemType::Armor,
                        Classification::InferredFromBodyObservation,
                        SuggestedType::ArmorFromBodyObservation,
                    )
                } else if has_cape {
                    (
                        ItemType::Cape,
                        Classification::InferredFromBodyObservation,
                        SuggestedType::CapeFromCapeObservation,
                    )
                } else if has_head {
                    // A head observation alone says nothing about type: the whole
                    // point of this tool is that armor can sit in the head slot.
                    (
                        ItemType::Unknown,
                        Classification::Unknown,
                        SuggestedType::NeedsUserDecision,
                    )
                } else {
                    (
                        ItemType::Unknown,
                        Classification::Unknown,
                        SuggestedType::NeedsUserDecision,
                    )
                }
            }
        };

        let already_present = existing.get_typed(id, item_type).is_some();
        items.push(ProposedItem {
            item_type,
            classification,
            id_u32: id,
            display_name: if best_name.is_empty() {
                Item::seed_label(id).unwrap_or_default().to_string()
            } else {
                best_name
            },
            note: best_note,
            suggestion,
            observations,
            source_lines,
            already_present,
        });
    }

    // Duplicate-ID type conflicts inside the same import are surfaced explicitly.
    let mut conflicts: BTreeMap<u32, Vec<ItemType>> = BTreeMap::new();
    for item in &items {
        conflicts
            .entry(item.id_u32)
            .or_default()
            .push(item.item_type);
    }
    for (id, types) in conflicts {
        if types.len() > 1 {
            issues.push(ImportIssue {
                line: 0,
                severity: IssueSeverity::Warning,
                message: format!("ID 0x{id:08X} 在导入中出现多种类型，请确认后再提交"),
            });
        }
    }
    if let Some(digest) = items
        .iter()
        .flat_map(|item| item.observations.iter())
        .find_map(|observation| observation.source_digest.clone())
    {
        let _ = digest;
    }

    ImportPreview {
        source_format,
        raw_record_count,
        issues,
        items,
        raw_records: records,
        source_bytes: source_bytes.to_vec(),
        source_file_name: source_file_name.to_string(),
    }
}

/// Resolve the preview into a concrete delta using the user's decisions.
///
/// Items already present with an authoritative type are skipped unless the user
/// chose an explicit type; nothing is merged by name.
pub fn resolve_import(
    preview: &ImportPreview,
    resolutions: &BTreeMap<u32, TypeResolution>,
    existing: &Catalog,
) -> CatalogDelta {
    let mut items = Vec::new();
    let mut skipped = 0usize;

    for proposal in &preview.items {
        let resolution =
            resolutions
                .get(&proposal.id_u32)
                .copied()
                .unwrap_or(match proposal.suggestion {
                    SuggestedType::AlreadyKnown => TypeResolution::AcceptSuggestion,
                    SuggestedType::ArmorFromBodyObservation => TypeResolution::AcceptSuggestion,
                    SuggestedType::CapeFromCapeObservation => TypeResolution::AcceptSuggestion,
                    SuggestedType::NeedsUserDecision => TypeResolution::KeepUnknown,
                });

        if resolution == TypeResolution::Skip {
            skipped += 1;
            continue;
        }

        let item_type = match resolution {
            TypeResolution::AsArmor => ItemType::Armor,
            TypeResolution::AsHelmet => ItemType::Helmet,
            TypeResolution::AsCape => ItemType::Cape,
            TypeResolution::KeepUnknown => ItemType::Unknown,
            TypeResolution::AcceptSuggestion | TypeResolution::Skip => proposal.item_type,
        };

        // Preserve user-authored names and notes when the item already exists.
        let (display_name, note, aliases, favorite, metadata) = match existing
            .get_typed(proposal.id_u32, item_type)
            .or_else(|| existing.by_id(proposal.id_u32).into_iter().next())
        {
            Some(present) => (
                if present.display_name.trim().is_empty() {
                    proposal.display_name.clone()
                } else {
                    present.display_name.clone()
                },
                if present.note.trim().is_empty() {
                    proposal.note.clone()
                } else {
                    present.note.clone()
                },
                present.aliases.clone(),
                present.favorite,
                present.metadata.clone(),
            ),
            None => (
                proposal.display_name.clone(),
                proposal.note.clone(),
                Vec::new(),
                false,
                Default::default(),
            ),
        };

        let mut item = Item::new(item_type, proposal.id_u32, proposal.classification);
        item.display_name = display_name;
        item.note = note;
        item.aliases = aliases;
        item.favorite = favorite;
        item.metadata = metadata;
        item.observations = proposal.observations.clone();
        items.push(item);
    }

    CatalogDelta { items, skipped }
}

/// Apply a delta to a catalog, merging observations into existing entries.
pub fn apply_catalog_delta(current: &Catalog, delta: &CatalogDelta) -> Catalog {
    let mut next = current.clone();
    for item in &delta.items {
        match next.get_typed(item.id_u32, item.item_type) {
            Some(present) => {
                // Idempotent: keep the existing entry, merge provenance only.
                let key = present.item_key.clone();
                next.observe_many(&key, &item.observations);
            }
            None => next.insert(item.clone()),
        }
    }
    next
}

fn is_known_offset(offset: u32) -> bool {
    matches!(offset, OFFSET_HEAD | OFFSET_BODY | OFFSET_CAPE)
}

/// Parse `0x...` hex or plain decimal into a u32 without truncation.
fn parse_u32_text(text: &str) -> Result<u32, String> {
    let trimmed = text.trim().trim_matches('\'');
    if trimmed.is_empty() {
        return Err("空值".into());
    }
    if let Some(rest) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u32::from_str_radix(rest, 16).map_err(|error| error.to_string())
    } else {
        trimmed.parse::<u32>().map_err(|error| error.to_string())
    }
}

/// Describe an offset for the import preview list.
pub fn describe_offset(offset: u32) -> String {
    format!("{}（0x{offset:04X}）", offset_field_name(offset))
}
