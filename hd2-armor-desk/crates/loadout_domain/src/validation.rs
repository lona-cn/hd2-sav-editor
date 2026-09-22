//! Turning an intent into patches, with the type and whitelist rules enforced.
//!
//! This is the only place that decides which payload bytes a user command may
//! touch. The codec enforces equal length and protected headers; this layer
//! enforces *which* fields the player-facing UI may reach at all.

use sav_codec::{fields, FieldPatch, PayloadOffset, SaveImage};

use crate::catalog::{Catalog, Classification, Item};
use crate::draft::{LoadoutIntent, Snapshot, OFFSET_BODY, OFFSET_CAPE, OFFSET_HEAD};
use crate::types::{ItemRef, ItemType, SlotIntent};

/// A validation failure that must block the write.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("此存档布局未经核验，只允许查看")]
    LayoutNotWritable,

    #[error("头部槽需要明确的身体护甲或头盔，当前是{0}")]
    HeadTypeNotAllowed(&'static str),

    #[error("身体槽只接受身体护甲，当前是{0}")]
    BodyTypeNotAllowed(&'static str),

    #[error("普通模式不允许写入类型未知的物品：{0}")]
    UnknownItemNotConfirmed(String),

    #[error("正文长度不足，无法读取装备槽")]
    PayloadTooShort,

    #[error("编解码错误：{0}")]
    Codec(String),
}

/// One human-readable change.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffEntry {
    pub slot: &'static str,
    /// Item actually equipped on disk, if it could be identified.
    pub from: Option<ItemRef>,
    /// Raw ID on disk when no catalog entry matched.
    pub from_id: Option<u32>,
    /// Item that will be written.
    pub to: Option<ItemRef>,
    /// Raw ID that will be written.
    pub to_id: Option<u32>,
    /// Whether this slot changes.
    pub changes: bool,
}

impl DiffEntry {
    /// Player-facing line for this slot.
    pub fn describe(&self) -> String {
        if !self.changes {
            let current = self
                .from
                .as_ref()
                .map(|item| format!("{}（{}）", item.label(), item.item_type.label()))
                .or_else(|| self.from_id.map(|id| format!("0x{id:08X}")))
                .unwrap_or_else(|| "未识别".to_string());
            return format!("{}：保持当前（{current}）", self.slot, current = current);
        }
        let before = self
            .from
            .as_ref()
            .map(|item| format!("{}（{}）", item.label(), item.item_type.label()))
            .or_else(|| self.from_id.map(|id| format!("0x{id:08X}")))
            .unwrap_or_else(|| "未识别".to_string());
        let after = self
            .to
            .as_ref()
            .map(|item| format!("{}（{}）", item.label(), item.item_type.label()))
            .or_else(|| self.to_id.map(|id| format!("0x{id:08X}")))
            .unwrap_or_else(|| "未识别".to_string());
        format!("{}：{} → {}", self.slot, before, after)
    }
}

/// Everything the UI needs to show before saving.
#[derive(Debug, Clone, PartialEq)]
pub struct HumanReadableDiff {
    pub head: DiffEntry,
    pub body: DiffEntry,
    pub cape_note: String,
    pub other_note: String,
    pub checksum_note: String,
    /// Raw patches that will be applied.
    pub patches: Vec<FieldPatch>,
}

impl HumanReadableDiff {
    /// Whether anything would change.
    pub fn is_empty(&self) -> bool {
        self.patches.is_empty()
    }

    /// Summary line for the bottom bar.
    pub fn summary(&self) -> String {
        if self.patches.is_empty() {
            return "没有需要写入的改动".into();
        }
        let mut parts = Vec::new();
        if self.head.changes {
            parts.push("头部");
        }
        if self.body.changes {
            parts.push("身体");
        }
        format!("待修改：{}", parts.join(" + "))
    }

    /// Full multi-line description.
    pub fn describe(&self) -> String {
        format!(
            "{}\n{}\n{}\n{}\n{}",
            self.head.describe(),
            self.body.describe(),
            self.cape_note,
            self.other_note,
            self.checksum_note
        )
    }

    /// Technical detail lines (offset, before, after bytes) for the advanced view.
    pub fn technical_lines(&self) -> Vec<String> {
        self.patches
            .iter()
            .map(|patch| {
                format!(
                    "偏移 0x{:04X}：{:02X?} → {:02X?}",
                    patch.offset.0, patch.before, patch.after
                )
            })
            .collect()
    }
}

/// Resolve an intent into patches against a snapshot.
///
/// * `Keep` never produces a patch.
/// * `Set` to the value already on disk produces no patch (but stays visible).
/// * The cape and every other field are untouched by construction.
pub fn resolve_intent(
    snapshot: &Snapshot,
    catalog: &Catalog,
    intent: &LoadoutIntent,
) -> Result<Vec<FieldPatch>, ValidationError> {
    if !snapshot.writable {
        return Err(ValidationError::LayoutNotWritable);
    }
    if snapshot.payload.len() < OFFSET_BODY + 4 {
        return Err(ValidationError::PayloadTooShort);
    }

    let mut patches = Vec::new();

    if let SlotIntent::Set { item } = &intent.head {
        check_head_type(item)?;
        if let Some(patch) = build_patch(snapshot, OFFSET_HEAD, item.id_u32)? {
            patches.push(patch);
        }
    }

    if let SlotIntent::Set { item } = &intent.body {
        check_body_type(item)?;
        if let Some(patch) = build_patch(snapshot, OFFSET_BODY, item.id_u32)? {
            patches.push(patch);
        }
    }

    // The cape is deliberately never patched in P0.
    let _ = OFFSET_CAPE;
    let _ = catalog;
    Ok(patches)
}

/// Build the full human-readable diff for an intent.
pub fn preview_intent(
    snapshot: &Snapshot,
    catalog: &Catalog,
    intent: &LoadoutIntent,
) -> Result<HumanReadableDiff, ValidationError> {
    let patches = resolve_intent(snapshot, catalog, intent)?;

    let head_current = snapshot.head_id();
    let body_current = snapshot.body_id();

    let head_target = intent.head.target_id();
    let body_target = intent.body.target_id();

    let head_entry = DiffEntry {
        slot: "头部",
        from: lookup(catalog, head_current, None),
        from_id: head_current,
        to: lookup(catalog, head_target, intent.head_item_type()),
        to_id: head_target,
        changes: patches.iter().any(|patch| patch.offset.0 == OFFSET_HEAD),
    };
    let body_entry = DiffEntry {
        slot: "身体",
        from: lookup(catalog, body_current, None),
        from_id: body_current,
        to: lookup(catalog, body_target, intent.body_item_type()),
        to_id: body_target,
        changes: patches.iter().any(|patch| patch.offset.0 == OFFSET_BODY),
    };

    let cape = snapshot
        .cape_id()
        .map(|id| format!("0x{id:08X}"))
        .unwrap_or_else(|| "未识别".into());

    Ok(HumanReadableDiff {
        head: head_entry,
        body: body_entry,
        cape_note: format!("披风：保持不变（{cape}）"),
        other_note: "武器、角色与未知数据：不修改".into(),
        checksum_note: "必要的内层/外层校验：自动更新".into(),
        patches,
    })
}

/// Look up an ID in the catalog, preferring a specific type when known.
fn lookup(catalog: &Catalog, id: Option<u32>, preferred: Option<ItemType>) -> Option<ItemRef> {
    let id = id?;
    let candidates = catalog.by_id(id);
    let chosen = preferred
        .and_then(|item_type| {
            candidates
                .iter()
                .copied()
                .find(|item| item.item_type == item_type)
        })
        .or_else(|| candidates.first().copied());
    chosen.map(|item| ItemRef {
        item_key: item.item_key.clone(),
        id_u32: item.id_u32,
        item_type: item.item_type,
        label_snapshot: item.label(),
    })
}

fn check_head_type(item: &ItemRef) -> Result<(), ValidationError> {
    match item.item_type {
        // The core requirement: a body armor may occupy the head slot.
        ItemType::Armor | ItemType::Helmet => Ok(()),
        ItemType::PrimaryWeapon => Err(ValidationError::HeadTypeNotAllowed("主要武器")),
        ItemType::SecondaryWeapon => Err(ValidationError::HeadTypeNotAllowed("副武器")),
        ItemType::Cape => Err(ValidationError::HeadTypeNotAllowed("披风")),
        ItemType::Unknown => Err(ValidationError::UnknownItemNotConfirmed(item.label())),
    }
}

fn check_body_type(item: &ItemRef) -> Result<(), ValidationError> {
    match item.item_type {
        ItemType::Armor => Ok(()),
        ItemType::PrimaryWeapon => Err(ValidationError::BodyTypeNotAllowed("主要武器")),
        ItemType::SecondaryWeapon => Err(ValidationError::BodyTypeNotAllowed("副武器")),
        ItemType::Helmet => Err(ValidationError::BodyTypeNotAllowed("头盔")),
        ItemType::Cape => Err(ValidationError::BodyTypeNotAllowed("披风")),
        ItemType::Unknown => Err(ValidationError::UnknownItemNotConfirmed(item.label())),
    }
}

fn build_patch(
    snapshot: &Snapshot,
    offset: usize,
    value: u32,
) -> Result<Option<FieldPatch>, ValidationError> {
    let end = offset + 4;
    let current: [u8; 4] = snapshot.payload[offset..end]
        .try_into()
        .map_err(|_| ValidationError::PayloadTooShort)?;
    let after = value.to_le_bytes();
    if current == after {
        // Setting a slot to the value it already holds is not a change.
        return Ok(None);
    }
    Ok(Some(FieldPatch {
        offset: PayloadOffset(offset),
        before: current,
        after,
    }))
}

/// Whether the catalog contains a trustworthy helmet to offer in "restore helmet".
pub fn find_verified_helmets(catalog: &Catalog) -> Vec<&Item> {
    catalog
        .items()
        .iter()
        .filter(|item| item.item_type == ItemType::Helmet)
        .filter(|item| item.classification.is_authoritative())
        .collect()
}

/// Confirm that an item's type is trustworthy enough for player mode.
pub fn is_player_usable(item_type: ItemType, classification: Classification) -> bool {
    matches!(item_type, ItemType::Armor | ItemType::Helmet) && classification.is_authoritative()
}

/// Apply patches through the codec, returning the new container bytes.
pub fn encode_intent(
    image: &SaveImage,
    patches: &[FieldPatch],
) -> Result<Vec<u8>, ValidationError> {
    image
        .encode_patches(patches)
        .map_err(|error| ValidationError::Codec(error.to_string()))
}

/// Offset constants re-exported for the UI's diagnostics view.
pub mod offsets {
    pub use crate::draft::{OFFSET_BODY, OFFSET_CAPE, OFFSET_HEAD};
    pub use sav_codec::INNER_CHECKSUM_OFFSET;
}

/// The head field's codec offset, checked once at compile time against the codec.
const _: () = {
    assert!(sav_codec::fields::HEAD.0 == crate::draft::OFFSET_HEAD);
    assert!(sav_codec::fields::BODY.0 == crate::draft::OFFSET_BODY);
    assert!(sav_codec::fields::CAPE.0 == crate::draft::OFFSET_CAPE);
};

/// Convenience: the codec's head offset as a `usize`.
pub fn codec_head_offset() -> usize {
    fields::HEAD.0
}
