//! Validation tests: which bytes a player-mode command may reach, and what the
//! preview says. These are acceptance criteria A04–A06, C06, C07, P01, P02, U02.

use loadout_domain::{
    encode_intent, preview_intent, resolve_intent, Catalog, Classification, Draft, Item, ItemRef,
    ItemType, SlotIntent, Snapshot, ValidationError, OFFSET_BODY, OFFSET_CAPE, OFFSET_HEAD,
};
use sav_codec::{fields, FieldPatch, PayloadOffset, SaveImage, INNER_CHECKSUM_OFFSET};

const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/valid_baseline.bin");

fn image() -> SaveImage {
    SaveImage::decode(FIXTURE.to_vec()).unwrap()
}

fn snapshot_from(image: &SaveImage) -> Snapshot {
    Snapshot {
        raw: image.raw().to_vec(),
        payload: image.payload().to_vec(),
        sha256: "b".repeat(64),
        captured_at: "2026-09-19T00:00:00Z".into(),
        path: "fixture.sav".into(),
        writable: image.is_writable(),
        readonly_reason: None,
        outer_crc32: image.outer_crc32(),
        inner_low32: image.inner_low32(),
    }
}

fn item_ref(id: u32, item_type: ItemType, name: &str) -> ItemRef {
    ItemRef {
        item_key: Item::make_key(item_type, id),
        id_u32: id,
        item_type,
        label_snapshot: name.into(),
    }
}

fn catalog_with_both() -> Catalog {
    let mut armor = Item::new(ItemType::Armor, 0xD346_1392, Classification::UserVerified);
    armor.display_name = "FS-55 蹂躏者".into();
    let mut helmet = Item::new(ItemType::Helmet, 0x261C_4A52, Classification::UserVerified);
    helmet.display_name = "B-01 战术".into();
    Catalog::from_items(vec![armor, helmet]).unwrap()
}

/// C03/P02: Keep produces no patch at all.
#[test]
fn keep_produces_no_patches() {
    let image = image();
    let snapshot = snapshot_from(&image);
    let intent = loadout_domain::LoadoutIntent::default();
    let patches = resolve_intent(&snapshot, &Catalog::default(), &intent).unwrap();
    assert!(patches.is_empty());
    assert_eq!(image.encode_patches(&patches).unwrap(), image.raw());
}

/// Setting a slot to the value it already holds is not a change.
#[test]
fn set_to_current_value_produces_no_patch() {
    let image = image();
    let snapshot = snapshot_from(&image);
    let current_head = snapshot.head_id().unwrap();
    let intent = loadout_domain::LoadoutIntent {
        head: SlotIntent::Set {
            item: item_ref(current_head, ItemType::Helmet, "当前头盔"),
        },
        body: SlotIntent::Keep,
    };
    let patches = resolve_intent(&snapshot, &Catalog::default(), &intent).unwrap();
    assert!(patches.is_empty());
}

/// C03/A04: the head slot accepts an Armor ID — the core product requirement.
#[test]
fn head_accepts_armor_id() {
    let image = image();
    let snapshot = snapshot_from(&image);
    let intent = loadout_domain::LoadoutIntent {
        head: SlotIntent::Set {
            item: item_ref(0xD346_1392, ItemType::Armor, "FS-55 身体护甲"),
        },
        body: SlotIntent::Keep,
    };
    let patches = resolve_intent(&snapshot, &catalog_with_both(), &intent).unwrap();
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].offset.0, OFFSET_HEAD);
    assert_eq!(patches[0].after, 0xD346_1392u32.to_le_bytes());

    let produced = encode_intent(&image, &patches).unwrap();
    let redecoded = SaveImage::decode(produced).unwrap();
    assert_eq!(redecoded.read_u32(fields::HEAD).unwrap(), 0xD346_1392);
    assert_eq!(
        redecoded.read_u32(fields::BODY).unwrap(),
        image.read_u32(fields::BODY).unwrap(),
        "body must not move when only the head is set"
    );
}

/// The body slot refuses a helmet.
#[test]
fn body_refuses_helmet() {
    let image = image();
    let snapshot = snapshot_from(&image);
    let intent = loadout_domain::LoadoutIntent {
        head: SlotIntent::Keep,
        body: SlotIntent::Set {
            item: item_ref(0x261C_4A52, ItemType::Helmet, "B-01 头盔"),
        },
    };
    let error = resolve_intent(&snapshot, &catalog_with_both(), &intent).unwrap_err();
    assert!(matches!(error, ValidationError::BodyTypeNotAllowed(_)));
}

/// The head slot refuses a cape.
#[test]
fn head_refuses_cape() {
    let image = image();
    let snapshot = snapshot_from(&image);
    let intent = loadout_domain::LoadoutIntent {
        head: SlotIntent::Set {
            item: item_ref(0x4657_CFB3, ItemType::Cape, "歼敌战士"),
        },
        body: SlotIntent::Keep,
    };
    let error = resolve_intent(&snapshot, &catalog_with_both(), &intent).unwrap_err();
    assert!(matches!(error, ValidationError::HeadTypeNotAllowed(_)));
}

/// Unknown types are refused in player mode for both slots.
#[test]
fn unknown_types_are_refused_in_player_mode() {
    let image = image();
    let snapshot = snapshot_from(&image);
    for slot in [true, false] {
        let intent = loadout_domain::LoadoutIntent {
            head: if slot {
                SlotIntent::Set {
                    item: item_ref(0x9999, ItemType::Unknown, "未知护甲"),
                }
            } else {
                SlotIntent::Keep
            },
            body: if slot {
                SlotIntent::Keep
            } else {
                SlotIntent::Set {
                    item: item_ref(0x9999, ItemType::Unknown, "未知护甲"),
                }
            },
        };
        let error = resolve_intent(&snapshot, &catalog_with_both(), &intent).unwrap_err();
        assert!(matches!(error, ValidationError::UnknownItemNotConfirmed(_)));
    }
}

/// A read-only layout refuses to produce patches.
#[test]
fn read_only_layout_refuses_patches() {
    let unknown = SaveImage::decode(
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/unknown_layout_valid.bin"
        ))
        .unwrap(),
    )
    .unwrap();
    assert!(!unknown.is_writable());
    let mut snapshot = snapshot_from(&unknown);
    snapshot.writable = false;
    let intent = loadout_domain::LoadoutIntent {
        head: SlotIntent::Set {
            item: item_ref(0xD346_1392, ItemType::Armor, "FS-55 身体护甲"),
        },
        body: SlotIntent::Keep,
    };
    let error = resolve_intent(&snapshot, &catalog_with_both(), &intent).unwrap_err();
    assert!(matches!(error, ValidationError::LayoutNotWritable));
}

/// A06: setting both slots changes exactly those two fields plus the inner hash.
#[test]
fn setting_both_slots_touches_only_whitelisted_ranges() {
    let image = image();
    let snapshot = snapshot_from(&image);
    let intent = loadout_domain::LoadoutIntent {
        head: SlotIntent::Set {
            item: item_ref(0xD346_1392, ItemType::Armor, "FS-55 身体护甲"),
        },
        body: SlotIntent::Set {
            item: item_ref(0x61B3_1723, ItemType::Armor, "B-01 身体护甲"),
        },
    };
    let patches = resolve_intent(&snapshot, &catalog_with_both(), &intent).unwrap();
    assert_eq!(patches.len(), 2);

    let produced = encode_intent(&image, &patches).unwrap();
    let redecoded = SaveImage::decode(produced).unwrap();

    let allowed = [
        (INNER_CHECKSUM_OFFSET, INNER_CHECKSUM_OFFSET + 4),
        (OFFSET_HEAD, OFFSET_HEAD + 4),
        (OFFSET_BODY, OFFSET_BODY + 4),
    ];
    let mut ranges = Vec::new();
    let mut start: Option<usize> = None;
    for index in 0..image.payload().len() {
        let differs = image.payload()[index] != redecoded.payload()[index];
        match (differs, start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                ranges.push((begin, index));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        ranges.push((begin, image.payload().len()));
    }
    for range in &ranges {
        assert!(
            allowed
                .iter()
                .any(|allow| range.0 >= allow.0 && range.1 <= allow.1),
            "change {range:?} outside whitelist"
        );
    }
    // The cape is never part of any patch.
    assert!(!patches.iter().any(|patch| patch.offset.0 == OFFSET_CAPE));
}

/// C06: the preview names the change, the type and the untouched slots.
#[test]
fn preview_describes_change_type_and_untouched_slots() {
    let image = image();
    let snapshot = snapshot_from(&image);
    let intent = loadout_domain::LoadoutIntent {
        head: SlotIntent::Set {
            item: item_ref(0xD346_1392, ItemType::Armor, "FS-55 蹂躏者"),
        },
        body: SlotIntent::Keep,
    };
    let diff = preview_intent(&snapshot, &catalog_with_both(), &intent).unwrap();
    assert!(diff.head.changes);
    assert!(!diff.body.changes);
    assert!(!diff.is_empty());

    let description = diff.describe();
    assert!(description.contains("头部"), "{description}");
    assert!(
        description.contains("身体护甲"),
        "type must be visible: {description}"
    );
    assert!(
        description.contains("保持当前"),
        "unchanged body must be stated"
    );
    assert!(
        description.contains("披风"),
        "cape must be mentioned as untouched"
    );
    assert!(description.contains("0x"), "raw IDs must be available");

    let summary = diff.summary();
    assert!(summary.contains("头部"));
    assert!(!summary.contains("身体"));

    // Technical detail carries the exact bytes for the advanced view.
    let technical = diff.technical_lines();
    assert_eq!(technical.len(), 1);
    assert!(technical[0].contains("0x0121"));
}

/// C07: a restore-helmet flow only ever records a real helmet.
#[test]
fn restore_helmet_uses_a_recorded_normal_helmet() {
    let image = image();
    let snapshot = snapshot_from(&image);
    let mut draft = Draft::new(snapshot.clone());

    // The file currently has an armor in the head slot; that must not become the
    // "normal helmet" to restore to.
    assert!(draft
        .record_helmet(item_ref(0xD346_1392, ItemType::Armor, "FS-55 护甲"))
        .is_err());

    // Record the helmet the user had before, then restore to it.
    draft
        .record_helmet(item_ref(0x261C_4A52, ItemType::Helmet, "B-01 战术（头盔）"))
        .unwrap();
    let recorded = draft.recorded_helmet().cloned().unwrap();
    draft.set_head(recorded, "恢复普通头盔");

    let diff = preview_intent(&snapshot, &catalog_with_both(), draft.intent()).unwrap();
    assert_eq!(diff.head.to_id, Some(0x261C_4A52));
    assert_eq!(diff.head.to.as_ref().unwrap().item_type, ItemType::Helmet);
}

/// P01: applying a preset onto a newer snapshot keeps the newer file's other data.
#[test]
fn preset_application_preserves_unrelated_newer_data() {
    // Build a "today" snapshot: same head/body, but a weapon field changed.
    let image = image();
    let mut payload = image.payload().to_vec();
    payload[0x0011..0x0015].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    let today = Snapshot {
        raw: image.raw().to_vec(),
        payload,
        sha256: "c".repeat(64),
        captured_at: "2026-09-19T00:00:00Z".into(),
        path: "today.sav".into(),
        writable: true,
        readonly_reason: None,
        outer_crc32: 0,
        inner_low32: 0,
    };

    // A preset saved "yesterday" only names the two armor slots.
    let preset = loadout_domain::LoadoutPreset::from_intent(
        "p1",
        "双甲",
        &loadout_domain::LoadoutIntent {
            head: SlotIntent::Set {
                item: item_ref(0xD346_1392, ItemType::Armor, "FS-55 身体护甲"),
            },
            body: SlotIntent::Keep,
        },
        "2026-09-18T00:00:00Z",
    );

    let patches = resolve_intent(&today, &catalog_with_both(), &preset.intent()).unwrap();
    // Apply onto *today's* payload: that is the snapshot the user confirmed.
    let mut rebuilt = today.payload.clone();
    for patch in &patches {
        rebuilt[patch.offset.0..patch.offset.0 + 4].copy_from_slice(&patch.after);
    }
    assert_eq!(
        &rebuilt[0x0011..0x0015],
        &0xDEAD_BEEFu32.to_le_bytes(),
        "the weapon field changed today must survive preset application"
    );
    assert_eq!(
        &rebuilt[OFFSET_HEAD..OFFSET_HEAD + 4],
        &0xD346_1392u32.to_le_bytes()
    );
    assert_eq!(
        &rebuilt[OFFSET_BODY..OFFSET_BODY + 4],
        &today.payload[OFFSET_BODY..OFFSET_BODY + 4],
        "a Keep body must stay at today's value"
    );
}

/// A patch whose `before` no longer matches the payload is refused by the codec.
#[test]
fn stale_patch_before_is_refused() {
    let image = image();
    let stale = FieldPatch {
        offset: PayloadOffset(OFFSET_HEAD),
        before: [0, 0, 0, 0],
        after: [1, 2, 3, 4],
    };
    assert!(image.encode_patches(&[stale]).is_err());
}

/// U02: a locked body is untouched even when a head selection is made.
#[test]
fn locked_body_survives_head_selection() {
    let image = image();
    let snapshot = snapshot_from(&image);
    let mut draft = Draft::new(snapshot.clone());
    draft.set_body_locked(true);
    assert!(draft.body_locked());
    draft.set_head(
        item_ref(0xD346_1392, ItemType::Armor, "FS-55 护甲"),
        "设置头部",
    );

    let patches = resolve_intent(&snapshot, &catalog_with_both(), draft.intent()).unwrap();
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].offset.0, OFFSET_HEAD);
    assert!(!patches.iter().any(|patch| patch.offset.0 == OFFSET_BODY));
}

/// `find_verified_helmets` never offers unverified or non-helmet entries.
#[test]
fn restore_helmet_list_only_contains_verified_helmets() {
    let mut unverified = Item::new(ItemType::Helmet, 0x1111, Classification::Unknown);
    unverified.display_name = "未确认头盔".into();
    let mut verified = Item::new(ItemType::Helmet, 0x2222, Classification::UserVerified);
    verified.display_name = "已确认头盔".into();
    let armor = Item::new(ItemType::Armor, 0x3333, Classification::UserVerified);
    let catalog = Catalog::from_items(vec![unverified, verified, armor]).unwrap();

    let helmets = loadout_domain::find_verified_helmets(&catalog);
    assert_eq!(helmets.len(), 1);
    assert_eq!(helmets[0].id_u32, 0x2222);
}
