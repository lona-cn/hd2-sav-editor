//! Presets: stored *intent*, never a copy of a save file.
//!
//! A preset records "head = X, body = Y" and nothing else. Applying it expands
//! the intent onto whatever snapshot the user has most recently confirmed, so a
//! weapon or character change made since the preset was created is preserved.

use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::draft::LoadoutIntent;
use crate::types::{ItemRef, ItemType, SlotIntent};

/// Current preset document schema.
pub const PRESET_SCHEMA_VERSION: u32 = 1;
/// Maximum presets in one document.
pub const MAX_PRESETS: usize = 10_000;

/// A saved loadout intention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadoutPreset {
    pub schema_version: u32,
    pub preset_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub head: SlotIntent,
    pub body: SlotIntent,
    pub created_at: String,
    pub updated_at: String,
    /// Optional free-form note about observed effects. Never a guarantee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_notes: Option<String>,
}

impl LoadoutPreset {
    /// Build a preset from the current intent.
    pub fn from_intent(
        preset_id: impl Into<String>,
        name: impl Into<String>,
        intent: &LoadoutIntent,
        created_at: impl Into<String>,
    ) -> LoadoutPreset {
        let stamp = created_at.into();
        LoadoutPreset {
            schema_version: PRESET_SCHEMA_VERSION,
            preset_id: preset_id.into(),
            name: name.into(),
            note: String::new(),
            tags: Vec::new(),
            head: intent.head.clone(),
            body: intent.body.clone(),
            created_at: stamp.clone(),
            updated_at: stamp,
            effect_notes: None,
        }
    }

    /// The intent this preset expands to.
    pub fn intent(&self) -> LoadoutIntent {
        LoadoutIntent {
            head: self.head.clone(),
            body: self.body.clone(),
        }
    }

    /// Whether the preset would write anything at all.
    pub fn is_noop(&self) -> bool {
        !self.head.is_set() && !self.body.is_set()
    }

    /// Validate structure and the player-mode type rules.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != PRESET_SCHEMA_VERSION {
            return Err(format!(
                "预设 schema_version {} 不受支持",
                self.schema_version
            ));
        }
        if self.preset_id.trim().is_empty() {
            return Err("预设缺少 preset_id".into());
        }
        if self.name.trim().is_empty() {
            return Err("预设缺少名称".into());
        }
        // Head: Armor or Helmet. Body: Armor only. Unknown never auto-applies.
        if let SlotIntent::Set { item } = &self.head {
            if !matches!(item.item_type, ItemType::Armor | ItemType::Helmet) {
                return Err(format!(
                    "头部槽只接受身体护甲或头盔，预设里是 {}",
                    item.item_type.label()
                ));
            }
        }
        if let SlotIntent::Set { item } = &self.body {
            if item.item_type != ItemType::Armor {
                return Err(format!(
                    "身体槽只接受身体护甲，预设里是 {}",
                    item.item_type.label()
                ));
            }
        }
        Ok(())
    }

    /// Both slots reference the same armor. Allowed, but never claimed to stack.
    pub fn repeats_same_item(&self) -> bool {
        match (self.head.target_id(), self.body.target_id()) {
            (Some(head), Some(body)) => head == body,
            _ => false,
        }
    }
}

/// The preset collection stored in the workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetDocument {
    pub schema_version: u32,
    pub presets: Vec<LoadoutPreset>,
}

/// Presets plus lookup by id.
#[derive(Debug, Clone, Default)]
pub struct PresetStore {
    presets: Vec<LoadoutPreset>,
}

impl PresetStore {
    /// Load from a document, validating every entry.
    pub fn from_document(document: PresetDocument) -> Result<PresetStore, String> {
        if document.schema_version != PRESET_SCHEMA_VERSION {
            return Err(format!(
                "预设文件 schema_version {} 不受支持（本工具为 {PRESET_SCHEMA_VERSION}）",
                document.schema_version
            ));
        }
        if document.presets.len() > MAX_PRESETS {
            return Err(format!("预设数量超过上限 {MAX_PRESETS}"));
        }
        for preset in &document.presets {
            preset.validate()?;
        }
        Ok(PresetStore {
            presets: document.presets,
        })
    }

    /// Parse JSON into a store.
    pub fn from_json(text: &str) -> Result<PresetStore, String> {
        let document: PresetDocument =
            serde_json::from_str(text).map_err(|error| format!("预设 JSON 解析失败：{error}"))?;
        PresetStore::from_document(document)
    }

    /// Serialize to JSON.
    pub fn to_json(&self) -> Result<String, String> {
        let document = PresetDocument {
            schema_version: PRESET_SCHEMA_VERSION,
            presets: self.presets.clone(),
        };
        serde_json::to_string_pretty(&document).map_err(|error| error.to_string())
    }

    /// All presets.
    pub fn presets(&self) -> &[LoadoutPreset] {
        &self.presets
    }

    /// Look up by id.
    pub fn get(&self, preset_id: &str) -> Option<&LoadoutPreset> {
        self.presets
            .iter()
            .find(|preset| preset.preset_id == preset_id)
    }

    /// Insert or replace by id.
    pub fn upsert(&mut self, preset: LoadoutPreset) -> Result<(), String> {
        preset.validate()?;
        match self
            .presets
            .iter_mut()
            .find(|present| present.preset_id == preset.preset_id)
        {
            Some(present) => *present = preset,
            None => {
                if self.presets.len() >= MAX_PRESETS {
                    return Err(format!("预设数量超过上限 {MAX_PRESETS}"));
                }
                self.presets.push(preset);
            }
        }
        Ok(())
    }

    /// Remove a preset by id.
    pub fn remove(&mut self, preset_id: &str) -> bool {
        let before = self.presets.len();
        self.presets.retain(|preset| preset.preset_id != preset_id);
        self.presets.len() != before
    }

    /// Whether a preset contains a raw save, an account ID or a path.
    ///
    /// Used as a regression guard: presets must never carry such payloads.
    pub fn audit_no_private_payload(json: &str) -> Result<(), String> {
        let lowered = json.to_lowercase();
        for forbidden in [
            "\"raw\"",
            "\"payload\"",
            "\"sha256\"",
            "\"source_path\"",
            "\"account\"",
            "\"steamid\"",
            "testament_new.sav",
        ] {
            if lowered.contains(forbidden) {
                return Err(format!("预设内容包含不允许的字段：{forbidden}"));
            }
        }
        Ok(())
    }
}

/// Result of checking a preset against the current catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetResolution {
    pub intent: LoadoutIntent,
    /// Items the preset references that the catalog does not know.
    pub missing: Vec<ItemRef>,
    /// Items whose catalog type differs from the preset's snapshot type.
    pub retyped: Vec<(ItemRef, ItemType)>,
    /// Whether applying this preset would write anything.
    pub is_noop: bool,
    /// The preset repeats one armor in both slots.
    pub repeats_same_item: bool,
}

impl PresetResolution {
    /// Whether the preset can be applied in player mode.
    pub fn is_applicable(&self) -> bool {
        self.retyped.is_empty()
            && self.missing.iter().all(|item| {
                // A missing catalog entry is acceptable only when the reference's own
                // type is already trustworthy (it came from a verified snapshot).
                matches!(item.item_type, ItemType::Armor | ItemType::Helmet)
            })
    }

    /// Player-facing warning lines.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.missing.is_empty() {
            out.push(format!(
                "{} 个预设引用的护甲不在当前目录中，仍保留其 ID 与名称快照",
                self.missing.len()
            ));
        }
        if !self.retyped.is_empty() {
            out.push(format!(
                "{} 个护甲在当前目录中的类型与预设不同，请重新映射后再应用",
                self.retyped.len()
            ));
        }
        if self.repeats_same_item {
            out.push("同一护甲同时用于头部与身体；被动是否重复叠加未知".into());
        }
        out
    }
}

/// Expand a preset against the current catalog.
///
/// Nothing is substituted by name: a reference whose ID is absent keeps its ID
/// and is reported, never silently replaced with a similarly named item.
pub fn resolve_preset(preset: &LoadoutPreset, catalog: &Catalog) -> PresetResolution {
    let mut missing = Vec::new();
    let mut retyped = Vec::new();

    for intent in [&preset.head, &preset.body] {
        let SlotIntent::Set { item } = intent else {
            continue;
        };
        match catalog.by_id(item.id_u32).first() {
            Some(present) => {
                if present.item_type != item.item_type {
                    retyped.push((item.clone(), present.item_type));
                }
            }
            None => missing.push(item.clone()),
        }
    }

    PresetResolution {
        intent: preset.intent(),
        missing,
        retyped,
        is_noop: preset.is_noop(),
        repeats_same_item: preset.repeats_same_item(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Catalog, Classification, Item};
    use crate::types::ItemRef;

    fn item_ref(id: u32, item_type: ItemType, name: &str) -> ItemRef {
        ItemRef {
            item_key: Item::make_key(item_type, id),
            id_u32: id,
            item_type,
            label_snapshot: name.into(),
        }
    }

    fn preset_with(head: SlotIntent, body: SlotIntent) -> LoadoutPreset {
        LoadoutPreset {
            schema_version: PRESET_SCHEMA_VERSION,
            preset_id: "p1".into(),
            name: "双甲".into(),
            note: String::new(),
            tags: Vec::new(),
            head,
            body,
            created_at: "2026-09-18T00:00:00Z".into(),
            updated_at: "2026-09-18T00:00:00Z".into(),
            effect_notes: None,
        }
    }

    #[test]
    fn keep_and_set_zero_are_different() {
        let keep = preset_with(SlotIntent::Keep, SlotIntent::Keep);
        assert!(keep.is_noop());
        assert_eq!(keep.head.target_id(), None);

        // Set(0) is a real intent carrying ID 0, not an empty slot.
        let zero = preset_with(
            SlotIntent::Set {
                item: item_ref(0, ItemType::Armor, "ID 0"),
            },
            SlotIntent::Keep,
        );
        assert!(!zero.is_noop());
        assert_eq!(zero.head.target_id(), Some(0));
    }

    #[test]
    fn preset_json_carries_no_save_or_account_data() {
        let preset = preset_with(
            SlotIntent::Set {
                item: item_ref(0xD346_1392, ItemType::Armor, "FS-55 护甲"),
            },
            SlotIntent::Keep,
        );
        let json = serde_json::to_string(&preset).unwrap();
        PresetStore::audit_no_private_payload(&json).unwrap();
        assert!(!json.contains("raw"));
        assert!(!json.contains("sha256"));
    }

    #[test]
    fn unknown_schema_version_is_rejected() {
        let json = r#"{"schema_version":99,"presets":[]}"#;
        assert!(PresetStore::from_json(json).is_err());
    }

    #[test]
    fn body_set_must_be_armor_and_head_accepts_armor() {
        let bad_body = preset_with(
            SlotIntent::Keep,
            SlotIntent::Set {
                item: item_ref(0x0568_48E9, ItemType::Helmet, "FS-55 头盔"),
            },
        );
        assert!(bad_body.validate().is_err());

        let good_head = preset_with(
            SlotIntent::Set {
                item: item_ref(0xD346_1392, ItemType::Armor, "FS-55 护甲"),
            },
            SlotIntent::Keep,
        );
        good_head.validate().unwrap();
    }

    #[test]
    fn missing_catalog_entry_keeps_its_id_instead_of_substituting_by_name() {
        let catalog = Catalog::from_items(vec![Item::new(
            ItemType::Armor,
            0x1111_1111,
            Classification::UserVerified,
        )])
        .unwrap();
        let preset = preset_with(
            SlotIntent::Set {
                item: item_ref(0x2222_2222, ItemType::Armor, "不在目录里的护甲"),
            },
            SlotIntent::Keep,
        );
        let resolution = resolve_preset(&preset, &catalog);
        assert_eq!(resolution.missing.len(), 1);
        assert_eq!(resolution.missing[0].id_u32, 0x2222_2222);
        assert_eq!(resolution.intent.head.target_id(), Some(0x2222_2222));
    }

    #[test]
    fn repeated_armor_warns_without_promising_stacking() {
        let catalog = Catalog::default();
        let preset = preset_with(
            SlotIntent::Set {
                item: item_ref(0xD346_1392, ItemType::Armor, "FS-55 护甲"),
            },
            SlotIntent::Set {
                item: item_ref(0xD346_1392, ItemType::Armor, "FS-55 护甲"),
            },
        );
        let resolution = resolve_preset(&preset, &catalog);
        assert!(resolution.repeats_same_item);
        let warnings = resolution.warnings().join(" ");
        assert!(warnings.contains("未知"));
        assert!(!warnings.contains("2 倍"));
        assert!(!warnings.contains("叠加生效"));
    }
}
