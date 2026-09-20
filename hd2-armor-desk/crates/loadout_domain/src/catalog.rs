//! Armor catalog: items, provenance observations, legacy import and export.
//!
//! Two rules drive this module:
//! * An **observation** is evidence, not identity. The same armor ID seen at the
//!   head offset and the body offset is one item with two observations.
//! * A **display name** is never a key. Same-name helmet and armor stay separate.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::types::{ItemId, ItemType, SlotObservation, SourceFormat};

/// Maximum items accepted from an imported document.
pub const MAX_ITEMS: usize = 100_000;
/// Maximum observations per item.
pub const MAX_OBSERVATIONS: usize = 100_000;
/// Maximum length of a display name or alias.
pub const MAX_NAME_LEN: usize = 512;
/// Maximum length of a free-text note.
pub const MAX_NOTE_LEN: usize = 8_192;

/// Payload offset of the head slot, as observed.
pub const OFFSET_HEAD: u32 = 0x0121;
/// Payload offset of the cape slot, as observed.
pub const OFFSET_CAPE: u32 = 0x0125;
/// Payload offset of the body slot, as observed.
pub const OFFSET_BODY: u32 = 0x0129;

/// IDs the user verified in game. Used only as a seed, never as a full database.
pub const SEED_NAMES: &[(u32, &str)] = &[
    (0x0568_48E9, "FS-55 蹂躏者（头盔）"),
    (0x261C_4A52, "B-01 战术（头盔）"),
    (0xD346_1392, "FS-55 蹂躏者（身体护甲）"),
    (0x61B3_1723, "B-01 战术（身体护甲）"),
    (0x4657_CFB3, "歼敌战士（披风）"),
];

/// How an item's type was decided. Shown to the user; never invented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// The user confirmed this type in this tool.
    UserVerified,
    /// From the user's own in-game tests recorded in the handoff.
    SeedUserVerified,
    /// A body-slot observation, which normally means a body armor.
    InferredFromBodyObservation,
    /// Taken from the name or another non-authoritative field; needs confirmation.
    ImportedMetadata,
    /// No usable evidence.
    Unknown,
}

impl Classification {
    /// Player-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Classification::UserVerified => "已人工确认",
            Classification::SeedUserVerified => "用户实测确认",
            Classification::InferredFromBodyObservation => "由身体槽观察推断",
            Classification::ImportedMetadata => "由名称/字段推断，待确认",
            Classification::Unknown => "类型未知",
        }
    }

    /// Whether the type may be used without asking the user again.
    pub fn is_authoritative(self) -> bool {
        matches!(
            self,
            Classification::UserVerified | Classification::SeedUserVerified
        )
    }
}

/// Optional metadata. Absent fields stay absent — nothing is guessed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight_class: Option<WeightClass>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passive_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passive_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warbond: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_path: Option<String>,
}

/// Armor weight class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeightClass {
    Light,
    Medium,
    Heavy,
}

impl WeightClass {
    pub fn label(self) -> &'static str {
        match self {
            WeightClass::Light => "轻甲",
            WeightClass::Medium => "中甲",
            WeightClass::Heavy => "重甲",
        }
    }
}

/// One piece of evidence that an ID exists at a payload offset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub observed_offset: u32,
    pub observed_slot: SlotObservation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
    pub source_format: SourceFormat,
}

/// A catalog entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub item_key: String,
    pub id_u32: u32,
    pub item_type: ItemType,
    pub display_name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub classification: Classification,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default)]
    pub metadata: Metadata,
    #[serde(default)]
    pub observations: Vec<Observation>,
}

impl Item {
    /// Stable key for the given type and ID: `armor:0xD3461392`.
    pub fn make_key(item_type: ItemType, id: u32) -> String {
        format!("{}:0x{id:08X}", item_type.key_prefix())
    }

    /// Build a minimal item that is usable even without any metadata.
    pub fn new(item_type: ItemType, id: u32, classification: Classification) -> Item {
        Item {
            item_key: Item::make_key(item_type, id),
            id_u32: id,
            item_type,
            display_name: String::new(),
            aliases: Vec::new(),
            classification,
            note: String::new(),
            favorite: false,
            metadata: Metadata::default(),
            observations: Vec::new(),
        }
    }

    /// Name shown in the UI. Unnamed items stay visible and searchable.
    pub fn label(&self) -> String {
        if self.display_name.trim().is_empty() {
            format!("未命名护甲 · 0x{:08X}", self.id_u32)
        } else {
            self.display_name.clone()
        }
    }

    /// Type badge text: `身体护甲`, `头盔`, `披风`, `未知类型`.
    pub fn type_label(&self) -> &'static str {
        self.item_type.label()
    }

    /// Verify that `item_key`, `item_type` and `id_u32` agree.
    pub fn validate(&self) -> Result<(), String> {
        let expected = Item::make_key(self.item_type, self.id_u32);
        if self.item_key != expected {
            return Err(format!(
                "item_key 与类型/ID 不一致：{} ≠ {}",
                self.item_key, expected
            ));
        }
        if self.display_name.chars().count() > MAX_NAME_LEN {
            return Err("显示名称过长".into());
        }
        if self.note.chars().count() > MAX_NOTE_LEN {
            return Err("备注过长".into());
        }
        if self.aliases.len() > 64 {
            return Err("别名过多".into());
        }
        if self.observations.len() > MAX_OBSERVATIONS {
            return Err("观察记录过多".into());
        }
        for observation in &self.observations {
            if observation.observed_offset < 16 {
                return Err("观察偏移超出正文范围".into());
            }
        }
        Ok(())
    }

    /// The best-known name for this ID, falling back to the seed table.
    pub fn seed_label(id: u32) -> Option<&'static str> {
        SEED_NAMES
            .iter()
            .find(|(seed, _)| *seed == id)
            .map(|(_, name)| *name)
    }
}

/// The catalog document written to the workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogDocument {
    pub schema_version: u32,
    pub game: String,
    pub updated_at: String,
    pub items: Vec<Item>,
}

/// Current catalog plus lookup indexes.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    items: Vec<Item>,
    by_key: HashMap<String, usize>,
    by_id: HashMap<u32, Vec<usize>>,
}

impl Catalog {
    /// Build from a validated document.
    pub fn from_items(items: Vec<Item>) -> Result<Catalog, String> {
        if items.len() > MAX_ITEMS {
            return Err(format!("目录条目数 {} 超过上限 {MAX_ITEMS}", items.len()));
        }
        let mut catalog = Catalog::default();
        for item in items {
            item.validate()?;
            catalog.insert(item);
        }
        Ok(catalog)
    }

    /// Parse a v2 catalog document.
    pub fn from_document_json(text: &str) -> Result<Catalog, String> {
        let document: CatalogDocument =
            serde_json::from_str(text).map_err(|error| format!("目录 JSON 解析失败：{error}"))?;
        if document.schema_version != 2 {
            return Err(format!(
                "不支持的目录 schema_version {}（本工具只接受 2）",
                document.schema_version
            ));
        }
        if document.game != "helldivers2" {
            return Err(format!("目录 game 字段不是 helldivers2：{}", document.game));
        }
        Catalog::from_items(document.items)
    }

    /// Serialize back to a v2 document.
    pub fn to_document_json(&self, updated_at: &str) -> Result<String, String> {
        let document = CatalogDocument {
            schema_version: 2,
            game: "helldivers2".into(),
            updated_at: updated_at.to_string(),
            items: self.items.clone(),
        };
        serde_json::to_string_pretty(&document).map_err(|error| error.to_string())
    }

    /// All items, in insertion order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Number of items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the catalog is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Look up by stable key.
    pub fn get(&self, item_key: &str) -> Option<&Item> {
        self.by_key.get(item_key).map(|index| &self.items[*index])
    }

    /// Every item carrying this raw ID (a helmet and an armor can share one).
    pub fn by_id(&self, id: u32) -> Vec<&Item> {
        self.by_id
            .get(&id)
            .map(|indexes| indexes.iter().map(|index| &self.items[*index]).collect())
            .unwrap_or_default()
    }

    /// The item with this ID and type, if present.
    pub fn get_typed(&self, id: u32, item_type: ItemType) -> Option<&Item> {
        let key = Item::make_key(item_type, id);
        self.get(&key)
    }

    /// Insert or replace an item, keeping indexes consistent.
    pub fn insert(&mut self, item: Item) {
        let key = item.item_key.clone();
        let id = item.id_u32;
        if let Some(index) = self.by_key.get(&key).copied() {
            self.items[index] = item;
            return;
        }
        let index = self.items.len();
        self.by_key.insert(key, index);
        self.by_id.entry(id).or_default().push(index);
        self.items.push(item);
    }

    /// Fill absent passive metadata from a trusted catalog without replacing
    /// values the user already supplied.
    pub fn fill_missing_passive_metadata(&mut self, item_key: &str, source: &Metadata) -> bool {
        let Some(index) = self.by_key.get(item_key).copied() else {
            return false;
        };
        let target = &mut self.items[index].metadata;
        let mut changed = false;
        if target.passive_tags.is_empty() && !source.passive_tags.is_empty() {
            target.passive_tags.clone_from(&source.passive_tags);
            changed = true;
        }
        let target_description_missing = target
            .passive_description
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty();
        let source_description = source
            .passive_description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty());
        if target_description_missing {
            if let Some(description) = source_description {
                target.passive_description = Some(description.to_string());
                changed = true;
            }
        }
        changed
    }

    /// Merge an observation into an existing item, deduplicating by offset+slot.
    pub fn observe(&mut self, item_key: &str, observation: Observation) {
        let Some(index) = self.by_key.get(item_key).copied() else {
            return;
        };
        let target = &mut self.items[index];
        if let Some(existing) = target.observations.iter_mut().find(|candidate| {
            candidate.observed_offset == observation.observed_offset
                && candidate.observed_slot == observation.observed_slot
        }) {
            existing.count = Some(
                existing
                    .count
                    .unwrap_or(0)
                    .saturating_add(observation.count.unwrap_or(1)),
            );
            if observation.last_seen.is_some() {
                existing.last_seen = observation.last_seen;
            }
            if existing.first_seen.is_none() {
                existing.first_seen = observation.first_seen;
            }
        } else {
            target.observations.push(observation);
        }
    }

    /// Merge a batch of observations in linear expected time.
    pub fn observe_many(&mut self, item_key: &str, observations: &[Observation]) {
        let Some(index) = self.by_key.get(item_key).copied() else {
            return;
        };
        let target = &mut self.items[index];
        let mut by_observation: HashMap<(u32, SlotObservation), usize> = target
            .observations
            .iter()
            .enumerate()
            .map(|(index, observation)| {
                (
                    (observation.observed_offset, observation.observed_slot),
                    index,
                )
            })
            .collect();

        for observation in observations {
            let key = (observation.observed_offset, observation.observed_slot);
            if let Some(index) = by_observation.get(&key).copied() {
                let existing = &mut target.observations[index];
                existing.count = Some(
                    existing
                        .count
                        .unwrap_or(0)
                        .saturating_add(observation.count.unwrap_or(1)),
                );
                if observation.last_seen.is_some() {
                    existing.last_seen = observation.last_seen.clone();
                }
                if existing.first_seen.is_none() {
                    existing.first_seen = observation.first_seen.clone();
                }
            } else {
                by_observation.insert(key, target.observations.len());
                target.observations.push(observation.clone());
            }
        }
    }

    /// Update a display name, preserving the previous one as an alias.
    pub fn rename(&mut self, item_key: &str, new_name: &str) -> Result<(), String> {
        let index = self
            .by_key
            .get(item_key)
            .copied()
            .ok_or_else(|| format!("目录中没有 {item_key}"))?;
        let trimmed = new_name.trim();
        if trimmed.chars().count() > MAX_NAME_LEN {
            return Err("名称过长".into());
        }
        let item = &mut self.items[index];
        let previous = item.display_name.clone();
        if !previous.trim().is_empty() && previous != trimmed && !item.aliases.contains(&previous) {
            item.aliases.push(previous);
        }
        item.display_name = trimmed.to_string();
        Ok(())
    }

    /// Toggle favorite state.
    pub fn set_favorite(&mut self, item_key: &str, favorite: bool) {
        if let Some(index) = self.by_key.get(item_key).copied() {
            self.items[index].favorite = favorite;
        }
    }

    /// Set the user-confirmed type of an item, re-keying it.
    ///
    /// Returns the new key. Refuses when the target key is already taken by a
    /// different item, so a confirmation can never silently merge two records.
    pub fn confirm_type(&mut self, item_key: &str, item_type: ItemType) -> Result<String, String> {
        let index = self
            .by_key
            .get(item_key)
            .copied()
            .ok_or_else(|| format!("目录中没有 {item_key}"))?;
        let id = self.items[index].id_u32;
        let new_key = Item::make_key(item_type, id);
        if new_key == item_key {
            self.items[index].classification = Classification::UserVerified;
            return Ok(new_key);
        }
        if self.by_key.contains_key(&new_key) {
            return Err(format!("{new_key} 已存在，未自动合并两条记录"));
        }

        self.by_key.remove(item_key);
        let item = &mut self.items[index];
        item.item_key = new_key.clone();
        item.item_type = item_type;
        item.classification = Classification::UserVerified;
        self.by_key.insert(new_key.clone(), index);
        Ok(new_key)
    }

    /// Search by name, alias, model number, 8-digit hex or decimal ID.
    ///
    /// Case-insensitive; common whitespace and hyphen differences are ignored for
    /// text, but ID matching is exact.
    pub fn search(&self, query: &str, filter: &SearchFilter) -> Vec<&Item> {
        let trimmed = query.trim();
        let normalized_query = normalize_text(trimmed);
        let hex_query = parse_hex_id(trimmed);
        let dec_query = parse_decimal_id(trimmed);

        let mut matches: Vec<&Item> = self
            .items
            .iter()
            .filter(|item| filter.accepts(item))
            .filter(|item| {
                if trimmed.is_empty() {
                    return true;
                }
                if hex_query == Some(item.id_u32) || dec_query == Some(item.id_u32) {
                    return true;
                }
                if normalize_text(&item.display_name).contains(&normalized_query) {
                    return true;
                }
                if item
                    .aliases
                    .iter()
                    .any(|alias| normalize_text(alias).contains(&normalized_query))
                {
                    return true;
                }
                // Model number without separators, e.g. `FS55` matching `FS-55`.
                let compact = normalized_query.replace(['-', ' '], "");
                !compact.is_empty()
                    && normalize_text(&item.display_name)
                        .replace(['-', ' '], "")
                        .contains(&compact)
            })
            .collect();

        matches.sort_by(|a, b| {
            b.favorite
                .cmp(&a.favorite)
                .then_with(|| a.label().cmp(&b.label()))
        });
        matches
    }

    /// Export as CSV with the legacy column names, protecting against formula injection.
    pub fn export_csv(&self) -> String {
        let mut out = String::from("\u{feff}");
        out.push_str("字段,正文偏移,ID_十六进制,ID_十进制,名称,备注,类型,分类\n");
        for item in &self.items {
            let field = item
                .observations
                .first()
                .map(|observation| offset_field_name(observation.observed_offset))
                .unwrap_or("");
            out.push_str(&csv_row(&[
                field,
                &format!(
                    "0x{:04X}",
                    item.observations
                        .first()
                        .map(|o| o.observed_offset)
                        .unwrap_or(0)
                ),
                &format!("0x{:08X}", item.id_u32),
                &item.id_u32.to_string(),
                &item.display_name,
                &item.note,
                item.item_type.label(),
                item.classification.label(),
            ]));
        }
        out
    }

    /// A shareable export: IDs, types, names and notes only. No digests, no paths.
    pub fn export_shareable_json(&self, updated_at: &str) -> Result<String, String> {
        let shared: Vec<serde_json::Value> = self
            .items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "id_u32": item.id_u32,
                    "item_type": item.item_type,
                    "display_name": item.display_name,
                    "note": item.note,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 2,
            "game": "helldivers2",
            "updated_at": updated_at,
            "shareable": true,
            "items": shared,
        }))
        .map_err(|error| error.to_string())
    }
}

/// Which items a search should return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchFilter {
    /// Only items of this type.
    pub item_type: Option<ItemType>,
    /// Only favorites.
    pub favorites_only: bool,
}

impl SearchFilter {
    /// Whether an item passes the filter.
    pub fn accepts(&self, item: &Item) -> bool {
        if self.favorites_only && !item.favorite {
            return false;
        }
        match self.item_type {
            Some(item_type) => item.item_type == item_type,
            None => true,
        }
    }
}

/// Human-readable slot name for an observed offset.
pub fn offset_field_name(offset: u32) -> &'static str {
    match offset {
        OFFSET_HEAD => "头部槽",
        OFFSET_CAPE => "披风槽",
        OFFSET_BODY => "身体护甲槽",
        _ => "其他偏移",
    }
}

/// Map a payload offset to the slot it represents.
pub fn slot_for_offset(offset: u32) -> SlotObservation {
    match offset {
        OFFSET_HEAD => SlotObservation::Head,
        OFFSET_CAPE => SlotObservation::Cape,
        OFFSET_BODY => SlotObservation::Body,
        _ => SlotObservation::Unknown,
    }
}

/// Normalize text for searching: trim, lowercase, collapse internal whitespace.
pub fn normalize_text(text: &str) -> String {
    let lowered = text.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_space = false;
    for ch in lowered.chars() {
        if ch.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Parse `0x...` or a bare 8-digit hex ID.
fn parse_hex_id(text: &str) -> Option<u32> {
    let lowered = text.trim().to_lowercase();
    let body = if let Some(rest) = lowered.strip_prefix("0x") {
        rest
    } else if lowered.len() == 8 && lowered.chars().all(|c| c.is_ascii_hexdigit()) {
        // An 8-digit token that is not a plain decimal is treated as hex.
        if lowered.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        lowered.as_str()
    } else {
        return None;
    };
    u32::from_str_radix(body, 16).ok()
}

/// Parse a plain decimal ID.
fn parse_decimal_id(text: &str) -> Option<u32> {
    let trimmed = text.trim();
    if trimmed.is_empty() || !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    trimmed.parse::<u32>().ok()
}

/// Quote a CSV field and neutralize spreadsheet formula prefixes.
fn csv_row(fields: &[&str]) -> String {
    let mut out = String::new();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let guarded = if field.starts_with(['=', '+', '-', '@', '\t', '\r']) {
            format!("'{field}")
        } else {
            (*field).to_string()
        };
        if guarded.contains([',', '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&guarded.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(&guarded);
        }
    }
    out.push('\n');
    out
}

/// Group items by ID for conflict reporting.
pub fn group_by_id(catalog: &Catalog) -> BTreeMap<u32, Vec<&Item>> {
    let mut grouped: BTreeMap<u32, Vec<&Item>> = BTreeMap::new();
    for item in catalog.items() {
        grouped.entry(item.id_u32).or_default().push(item);
    }
    grouped
}

/// Convenience constructor for an [`ItemId`] from a raw u32.
pub fn item_id(value: u32) -> ItemId {
    ItemId(value)
}
