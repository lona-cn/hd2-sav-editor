//! Core value types shared by the catalog, presets, draft and validation layers.
//!
//! None of these types depend on GPUI: the UI converts to `SharedString` at its
//! own boundary.

use serde::{Deserialize, Serialize};

/// A raw u32 item identifier as stored in the save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub u32);

impl ItemId {
    /// Uppercase `0x`-prefixed hex form used in keys and diagnostics.
    pub fn hex(self) -> String {
        format!("0x{:08X}", self.0)
    }
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "0x{:08X}", self.0)
    }
}

/// What kind of thing an ID denotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemType {
    /// Body armor. This is the type the head slot is deliberately allowed to hold.
    Armor,
    /// A primary weapon. It is catalogued for accuracy but cannot occupy armor slots.
    #[serde(rename = "primary_weapon")]
    PrimaryWeapon,
    /// A secondary weapon, catalogued but never allowed in armor slots.
    #[serde(rename = "secondary_weapon")]
    SecondaryWeapon,
    /// A normal helmet.
    Helmet,
    /// A cape.
    Cape,
    /// Not established yet.
    Unknown,
}

impl ItemType {
    /// Key namespace prefix.
    pub fn key_prefix(self) -> &'static str {
        match self {
            ItemType::Armor => "armor",
            ItemType::PrimaryWeapon => "primary_weapon",
            ItemType::SecondaryWeapon => "secondary_weapon",
            ItemType::Helmet => "helmet",
            ItemType::Cape => "cape",
            ItemType::Unknown => "unknown",
        }
    }

    /// Player-facing label.
    pub fn label(self) -> &'static str {
        match self {
            ItemType::Armor => "身体护甲",
            ItemType::PrimaryWeapon => "主要武器",
            ItemType::SecondaryWeapon => "副武器",
            ItemType::Helmet => "头盔",
            ItemType::Cape => "披风",
            ItemType::Unknown => "未知类型",
        }
    }
}

/// An equipment slot in the save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EquipmentSlot {
    /// Head slot (`0x0121`). Accepts Helmet **and** Armor.
    Head,
    /// Body slot (`0x0129`). Accepts Armor.
    Body,
    /// Cape slot (`0x0125`). Read-only in P0.
    Cape,
}

impl EquipmentSlot {
    /// Player-facing label, including the "armor may be placed here" nuance.
    pub fn label(self) -> &'static str {
        match self {
            EquipmentSlot::Head => "头部槽",
            EquipmentSlot::Body => "身体槽",
            EquipmentSlot::Cape => "披风槽",
        }
    }
}

/// Which slot an observation came from. Provenance only — never a type claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlotObservation {
    Head,
    Body,
    Cape,
    Unknown,
}

impl SlotObservation {
    /// Player-facing label.
    pub fn label(self) -> &'static str {
        match self {
            SlotObservation::Head => "头部槽",
            SlotObservation::Body => "身体护甲槽",
            SlotObservation::Cape => "披风槽",
            SlotObservation::Unknown => "未知偏移",
        }
    }
}

/// Where a catalog record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    /// The original Python tool's `catalog.json`.
    LegacyJson,
    /// The original tool's CSV export.
    LegacyCsv,
    /// A live read-only capture by this tool.
    LiveCapture,
    /// Entered or confirmed by the user by hand.
    UserManual,
    /// This tool's own v2 catalog.
    CatalogV2,
}

impl SourceFormat {
    /// Player-facing label for the provenance list.
    pub fn label(self) -> &'static str {
        match self {
            SourceFormat::LegacyJson => "旧工具 JSON",
            SourceFormat::LegacyCsv => "旧工具 CSV",
            SourceFormat::LiveCapture => "只读监听采集",
            SourceFormat::UserManual => "手动录入",
            SourceFormat::CatalogV2 => "本工具目录",
        }
    }
}

/// An item reference that survives catalog renames: the ID carries the meaning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemRef {
    pub item_key: String,
    pub id_u32: u32,
    pub item_type: ItemType,
    /// Name at the time the reference was made. Display only.
    pub label_snapshot: String,
}

impl ItemRef {
    /// Player-facing label, falling back to the raw ID.
    pub fn label(&self) -> String {
        if self.label_snapshot.trim().is_empty() {
            format!("未命名护甲 · {}", ItemId(self.id_u32).hex())
        } else {
            self.label_snapshot.clone()
        }
    }
}

/// What the user wants done to one slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "lowercase")]
pub enum SlotIntent {
    /// Leave the slot exactly as the disk snapshot has it. Not `Set(0)`.
    Keep,
    /// Write this item's ID into the slot.
    Set { item: ItemRef },
}

impl SlotIntent {
    /// The ID this intent would write, if any.
    pub fn target_id(&self) -> Option<u32> {
        match self {
            SlotIntent::Keep => None,
            SlotIntent::Set { item } => Some(item.id_u32),
        }
    }

    /// Whether this intent writes the slot.
    pub fn is_set(&self) -> bool {
        matches!(self, SlotIntent::Set { .. })
    }
}
