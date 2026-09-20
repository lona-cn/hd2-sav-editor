//! Import tests against the bundled legacy examples and the user's own
//! `workspace/catalog.json` when it is present in the handoff checkout.
//!
//! These are the acceptance criteria B01–B09, D01–D05 in automated form.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use loadout_domain::{
    apply_catalog_delta, import_legacy_csv, import_legacy_json, import_v2_json, resolve_import,
    Catalog, Classification, Item, ItemType, TypeResolution,
};

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("examples")
}

/// The real legacy catalog from the handoff package, if this checkout has it.
fn workspace_catalog() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("workspace")
        .join("catalog.json");
    path.is_file().then_some(path)
}

#[test]
fn legacy_json_example_imports_without_column_juggling() {
    let bytes = std::fs::read(examples_dir().join("legacy_catalog.schema1.example.json")).unwrap();
    let preview = import_legacy_json(&bytes, "legacy_catalog.schema1.example.json").unwrap();
    assert!(preview.raw_record_count > 0);
    assert!(preview.items.iter().any(|item| item.id_u32 == 0xD346_1392));
    assert!(
        preview
            .issues
            .iter()
            .all(|issue| issue.severity != loadout_domain::IssueSeverity::Error),
        "example must import cleanly: {:?}",
        preview.issues
    );
}

#[test]
fn v2_catalog_import_builds_a_preview_without_legacy_reparsing() {
    let bytes = std::fs::read(examples_dir().join("equipment_catalog.v2.example.json")).unwrap();
    let preview = import_v2_json(&bytes, "equipment_catalog.v2.example.json").unwrap();

    assert!(!preview.items.is_empty());
    assert_eq!(
        preview.source_format,
        loadout_domain::SourceFormat::CatalogV2
    );
    assert_eq!(preview.raw_record_count, preview.items.len());
    assert!(preview
        .items
        .iter()
        .all(|item| item.suggestion == loadout_domain::SuggestedType::AlreadyKnown));
}

/// B05/D02: one armor observed at head *and* body is one item with two observations.
#[test]
fn same_armor_at_head_and_body_is_one_item_with_two_observations() {
    let bytes = std::fs::read(examples_dir().join("legacy_catalog.schema1.example.json")).unwrap();
    let preview = import_legacy_json(&bytes, "legacy.json").unwrap();

    let fs55: Vec<_> = preview
        .items
        .iter()
        .filter(|item| item.id_u32 == 0xD346_1392)
        .collect();
    assert_eq!(fs55.len(), 1, "must not split into a helmet and an armor");
    let observations = &fs55[0].observations;
    assert_eq!(observations.len(), 2);
    assert!(observations
        .iter()
        .any(|observation| observation.observed_slot == loadout_domain::SlotObservation::Head));
    assert!(observations
        .iter()
        .any(|observation| observation.observed_slot == loadout_domain::SlotObservation::Body));
    assert_eq!(fs55[0].item_type, ItemType::Armor);
}

/// B04/D01: same name, different type, two IDs — never merged by name.
#[test]
fn same_named_helmet_and_armor_stay_separate_items() {
    let mut helmet = Item::new(ItemType::Helmet, 0x0568_48E9, Classification::UserVerified);
    helmet.display_name = "FS-55 蹂躏者".into();
    let mut armor = Item::new(ItemType::Armor, 0xD346_1392, Classification::UserVerified);
    armor.display_name = "FS-55 蹂躏者".into();
    let catalog = Catalog::from_items(vec![helmet, armor]).unwrap();

    let matches = catalog.search("FS-55", &Default::default());
    assert_eq!(matches.len(), 2, "both FS-55 entries must be visible");
    let types: Vec<ItemType> = matches.iter().map(|item| item.item_type).collect();
    assert!(types.contains(&ItemType::Helmet));
    assert!(types.contains(&ItemType::Armor));

    // The two entries are distinct items even though the names collide.
    assert_ne!(
        catalog.get("helmet:0x056848E9").unwrap().item_key,
        catalog.get("armor:0xD3461392").unwrap().item_key
    );
}

#[test]
fn confirming_an_item_type_rekeys_without_duplicating_the_item() {
    let item = Item::new(ItemType::Unknown, 0x1234_5678, Classification::Unknown);
    let mut catalog = Catalog::from_items(vec![item]).unwrap();

    let old_key = Item::make_key(ItemType::Unknown, 0x1234_5678);
    let new_key = catalog.confirm_type(&old_key, ItemType::Armor).unwrap();

    assert_eq!(
        catalog.len(),
        1,
        "type confirmation must not append a duplicate"
    );
    assert!(catalog.get(&old_key).is_none());
    assert_eq!(catalog.get(&new_key).unwrap().item_type, ItemType::Armor);
    assert_eq!(catalog.search("0x12345678", &Default::default()).len(), 1);
}

/// D03: a large u32 ID survives import as a positive value.
#[test]
fn large_ids_are_not_truncated_or_made_negative() {
    let json = r#"{"schema":1,"records":[{"offset":297,"value":3544585106,"field":"body","name":"FS-55","note":"","first_seen":"","last_seen":"","count":1,"source_sha256":null}]}"#.as_bytes();
    let preview = import_legacy_json(json, "inline.json").unwrap();
    assert_eq!(preview.items[0].id_u32, 3_544_585_106);
    assert!(
        preview.items[0].id_u32 > i32::MAX as u32,
        "must not be reinterpreted as a negative i32"
    );
    // And it must survive the export round trip unchanged.
    let catalog = apply_catalog_delta(
        &Catalog::default(),
        &resolve_import(&preview, &BTreeMap::new(), &Catalog::default()),
    );
    assert!(catalog.export_csv().contains("3544585106"));
}

/// Negative or out-of-range JSON integers are rejected, not wrapped.
#[test]
fn negative_and_oversized_json_values_are_rejected() {
    let json = r#"{"schema":1,"records":[
        {"offset":297,"value":-1,"field":"","name":"","note":"","first_seen":"","last_seen":"","count":1},
        {"offset":297,"value":4294967296,"field":"","name":"","note":"","first_seen":"","last_seen":"","count":1},
        {"offset":297,"value":123,"field":"","name":"ok","note":"","first_seen":"","last_seen":"","count":1}
    ]}"#.as_bytes();
    let preview = import_legacy_json(json, "inline.json").unwrap();
    assert_eq!(preview.items.len(), 1, "only the valid record survives");
    assert_eq!(preview.items[0].id_u32, 123);
    assert_eq!(
        preview
            .issues
            .iter()
            .filter(|issue| issue.severity == loadout_domain::IssueSeverity::Error)
            .count(),
        2
    );
}

/// D04: contradictory hex/decimal columns report the line instead of guessing.
#[test]
fn contradictory_hex_and_decimal_columns_are_reported() {
    let csv = "\u{feff}字段,正文偏移,ID_十六进制,ID_十进制,名称,备注,首次采集_UTC,末次采集_UTC\n\
        身体护甲槽,0x0129,0xD3461392,999,FS-55,,\n\
        身体护甲槽,0x0129,0x61B31723,1639126819,B-01,,\n";
    let preview = import_legacy_csv(csv.as_bytes(), "inline.csv").unwrap();
    assert_eq!(preview.items.len(), 1);
    assert_eq!(preview.items[0].id_u32, 0x61B3_1723);
    let error = preview
        .issues
        .iter()
        .find(|issue| issue.severity == loadout_domain::IssueSeverity::Error)
        .expect("contradiction must be reported");
    assert_eq!(error.line, 2, "line number must point at the bad row");
    assert!(error.message.contains("不一致"));
}

/// B02: BOM, Chinese text, embedded commas, quotes and newlines all parse.
#[test]
fn csv_with_bom_quotes_commas_and_newlines_parses() {
    let csv = "\u{feff}字段,正文偏移,ID_十六进制,ID_十进制,名称,备注,首次采集_UTC,末次采集_UTC\n\
        身体护甲槽,0x0129,0x000004D2,1234,\"护甲，带逗号\",\"备注\"\"带引号\"\"\",,\n\
        身体护甲槽,0x0129,0x000004D3,1235,\"多行\n名称\",备注,,\n";
    let preview = import_legacy_csv(csv.as_bytes(), "inline.csv").unwrap();
    assert_eq!(preview.items.len(), 2);
    let with_comma = preview
        .items
        .iter()
        .find(|item| item.id_u32 == 1234)
        .unwrap();
    assert_eq!(with_comma.display_name, "护甲，带逗号");
    assert_eq!(with_comma.note, "备注\"带引号\"");
    let multiline = preview
        .items
        .iter()
        .find(|item| item.id_u32 == 1235)
        .unwrap();
    assert!(multiline.display_name.contains('\n'));
}

/// The legacy exporter's formula-injection guard is preserved, not stripped.
#[test]
fn leading_apostrophe_from_legacy_export_is_preserved() {
    let csv = "\u{feff}字段,正文偏移,ID_十六进制,ID_十进制,名称,备注,首次采集_UTC,末次采集_UTC\n\
        身体护甲槽,0x0129,0x000004D2,1234,'=SUM(A1),,\n";
    let preview = import_legacy_csv(csv.as_bytes(), "inline.csv").unwrap();
    assert_eq!(preview.items[0].display_name, "'=SUM(A1)");
}

/// B07: re-importing the same source is idempotent and keeps hand-edited names.
#[test]
fn reimport_is_idempotent_and_preserves_manual_names() {
    let bytes = std::fs::read(examples_dir().join("legacy_catalog.schema1.example.json")).unwrap();
    let preview = import_legacy_json(&bytes, "legacy.json").unwrap();
    let resolutions = BTreeMap::new();

    let first = resolve_import(&preview, &resolutions, &Catalog::default());
    let catalog = apply_catalog_delta(&Catalog::default(), &first);
    let count_after_first = catalog.len();

    // The user renames an item by hand.
    let key = Item::make_key(ItemType::Armor, 0xD346_1392);
    let mut catalog = catalog;
    catalog.rename(&key, "我的 FS-55 护甲").unwrap();
    catalog.set_favorite(&key, true);

    // Import the same file again.
    let preview2 = import_legacy_json(&bytes, "legacy.json").unwrap();
    let second = resolve_import(&preview2, &resolutions, &catalog);
    let catalog2 = apply_catalog_delta(&catalog, &second);

    assert_eq!(
        catalog2.len(),
        count_after_first,
        "import must be idempotent"
    );
    let item = catalog2.get(&key).unwrap();
    assert_eq!(
        item.display_name, "我的 FS-55 护甲",
        "manual name preserved"
    );
    assert!(item.favorite, "favorite flag preserved");
}

/// The import never modifies the source bytes it was given.
#[test]
fn import_does_not_modify_source_bytes() {
    let path = examples_dir().join("legacy_catalog.schema1.example.json");
    let before = std::fs::read(&path).unwrap();
    let preview = import_legacy_json(&before, "legacy.json").unwrap();
    let _ = resolve_import(&preview, &BTreeMap::new(), &Catalog::default());
    let after = std::fs::read(&path).unwrap();
    assert_eq!(before, after, "source catalog file must be untouched");
}

/// B06: head-only observations do not become helmets, and unknown items stay visible.
#[test]
fn head_observation_alone_does_not_imply_helmet() {
    let json = r#"{"schema":1,"records":[{"offset":289,"value":222,"field":"头盔槽","name":"","note":"","first_seen":"","last_seen":"","count":1}]}"#.as_bytes();
    let preview = import_legacy_json(json, "inline.json").unwrap();
    assert_eq!(preview.items.len(), 1);
    assert_eq!(
        preview.items[0].item_type,
        ItemType::Unknown,
        "a head observation is provenance, not a helmet claim"
    );
    assert_eq!(
        preview.items[0].suggestion,
        loadout_domain::SuggestedType::NeedsUserDecision
    );
}

/// B03: an illegal u32 in CSV is reported with its line, never truncated.
#[test]
fn csv_out_of_range_id_is_reported_with_line_number() {
    let csv = "\u{feff}字段,正文偏移,ID_十六进制,ID_十进制,名称,备注,首次采集_UTC,末次采集_UTC\n\
        身体护甲槽,0x0129,0x1D3461392,7831105938,过大ID,,\n";
    let preview = import_legacy_csv(csv.as_bytes(), "inline.csv").unwrap();
    assert!(preview.items.is_empty());
    let error = preview
        .issues
        .iter()
        .find(|issue| issue.severity == loadout_domain::IssueSeverity::Error)
        .expect("must report the bad line");
    assert_eq!(error.line, 2);
}

/// B08: a v2 document whose key disagrees with its type/ID is rejected.
#[test]
fn v2_document_with_inconsistent_key_is_rejected() {
    let json = r#"{
        "schema_version": 2,
        "game": "helldivers2",
        "updated_at": "2026-09-19T00:00:00Z",
        "items": [{
            "item_key": "helmet:0xD3461392",
            "id_u32": 3544585106,
            "item_type": "armor",
            "display_name": "错位条目",
            "classification": "user_verified",
            "observations": []
        }]
    }"#;
    let error = Catalog::from_document_json(json).unwrap_err();
    assert!(error.contains("item_key"), "unexpected error: {error}");
}

/// The bundled v2 example validates and round-trips.
#[test]
fn v2_example_validates_and_round_trips() {
    let path = examples_dir().join("equipment_catalog.v2.example.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let catalog = Catalog::from_document_json(&text).unwrap();
    assert!(catalog.len() >= 4);

    let again = catalog.to_document_json("2026-09-19T00:00:00Z").unwrap();
    let reparsed = Catalog::from_document_json(&again).unwrap();
    assert_eq!(reparsed.len(), catalog.len());
    for item in catalog.items() {
        let other = reparsed.get(&item.item_key).unwrap();
        assert_eq!(other.id_u32, item.id_u32);
        assert_eq!(other.item_type, item.item_type);
        assert_eq!(other.observations.len(), item.observations.len());
    }
}

/// B09: an item with no metadata is still searchable by ID and shown by name.
#[test]
fn items_without_metadata_remain_usable() {
    let catalog = Catalog::from_items(vec![Item::new(
        ItemType::Unknown,
        0x1234_5678,
        Classification::Unknown,
    )])
    .unwrap();
    let item = &catalog.items()[0];
    assert!(item.label().contains("未命名护甲"));
    assert!(item.label().contains("12345678"));
    assert!(item.metadata.passive_tags.is_empty());

    assert_eq!(catalog.search("0x12345678", &Default::default()).len(), 1);
    assert_eq!(
        catalog.search("305419896", &Default::default()).len(),
        1,
        "decimal ID search"
    );
}

/// CSV export guards against spreadsheet formula injection.
#[test]
fn csv_export_guards_formula_prefixes() {
    let mut catalog = Catalog::from_items(vec![Item::new(
        ItemType::Armor,
        7,
        Classification::UserVerified,
    )])
    .unwrap();
    catalog
        .rename("armor:0x00000007", "=cmd|' /C calc'!A0")
        .unwrap();
    let csv = catalog.export_csv();
    assert!(csv.contains("'=cmd"), "formula prefix must be neutralized");
    assert!(csv.starts_with('\u{feff}'), "UTF-8 BOM for Excel");
}

/// The user's real catalog imports when present in the checkout.
#[test]
fn real_workspace_catalog_imports_when_present() {
    let Some(path) = workspace_catalog() else {
        eprintln!("skip: {} not present in this checkout", path_display());
        return;
    };
    let bytes = std::fs::read(&path).unwrap();
    let preview = import_legacy_json(&bytes, "catalog.json").unwrap();
    assert!(preview.raw_record_count > 0);

    let delta = resolve_import(&preview, &BTreeMap::new(), &Catalog::default());
    let catalog = apply_catalog_delta(&Catalog::default(), &delta);
    assert!(!catalog.is_empty());

    // Every imported item must be searchable by its own hex ID.
    for item in catalog.items() {
        let query = format!("0x{:08X}", item.id_u32);
        assert!(
            !catalog.search(&query, &Default::default()).is_empty(),
            "{query} must be findable"
        );
    }
}

fn path_display() -> String {
    workspace_catalog()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<absent>".into())
}

/// Applying an explicit type resolution re-keys the item without merging.
#[test]
fn confirming_type_creates_a_distinct_entry() {
    let mut catalog = Catalog::from_items(vec![Item::new(
        ItemType::Unknown,
        0x1234,
        Classification::Unknown,
    )])
    .unwrap();
    let new_key = catalog
        .confirm_type("unknown:0x00001234", ItemType::Armor)
        .unwrap();
    assert_eq!(new_key, "armor:0x00001234");
    assert!(catalog.get("unknown:0x00001234").is_none());
    assert_eq!(catalog.get(&new_key).unwrap().item_type, ItemType::Armor);

    // Confirming into an occupied key must refuse rather than merge.
    let mut catalog = Catalog::from_items(vec![
        Item::new(ItemType::Unknown, 0x1234, Classification::Unknown),
        Item::new(ItemType::Armor, 0x1234, Classification::UserVerified),
    ])
    .unwrap();
    let result = catalog.confirm_type("unknown:0x00001234", ItemType::Armor);
    assert!(result.is_err());
}

/// Resolutions let the user mark imported records as body armor in one pass.
#[test]
fn user_resolution_batch_confirms_body_armors() {
    let json = r#"{"schema":1,"records":[
        {"offset":297,"value":1001,"field":"","name":"护甲甲","note":"","first_seen":"","last_seen":"","count":1},
        {"offset":297,"value":1002,"field":"","name":"护甲乙","note":"","first_seen":"","last_seen":"","count":1},
        {"offset":289,"value":1003,"field":"","name":"","note":"","first_seen":"","last_seen":"","count":1}
    ]}"#.as_bytes();
    let preview = import_legacy_json(json, "inline.json").unwrap();
    assert_eq!(preview.armor_suggestions(), 2);
    assert_eq!(preview.unknown_count(), 1);

    let mut resolutions = BTreeMap::new();
    resolutions.insert(1003u32, TypeResolution::AsHelmet);
    let delta = resolve_import(&preview, &resolutions, &Catalog::default());
    assert_eq!(delta.items.len(), 3);

    let catalog = apply_catalog_delta(&Catalog::default(), &delta);
    assert_eq!(
        catalog.get("armor:0x000003E9").unwrap().item_type,
        ItemType::Armor
    );
    assert_eq!(
        catalog.get("helmet:0x000003EB").unwrap().item_type,
        ItemType::Helmet
    );
}
