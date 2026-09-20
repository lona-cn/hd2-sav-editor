//! Domain layer: catalog, legacy import, draft editing, presets and validation.
//!
//! This crate never depends on GPUI or on the file system. Everything here is
//! testable without a window, which is what makes the byte-level guarantees in
//! `sav_codec` reachable from automated tests.

#![forbid(unsafe_code)]

pub mod catalog;
pub mod draft;
pub mod import;
pub mod preset;
pub mod types;
pub mod validation;

pub use catalog::{
    Catalog, CatalogDocument, Classification, Item, Metadata, Observation, SearchFilter,
    WeightClass,
};
pub use draft::{
    Command, ConflictInfo, Draft, LoadoutIntent, Snapshot, OFFSET_BODY, OFFSET_CAPE, OFFSET_HEAD,
};
pub use import::{
    apply_catalog_delta, import_legacy_csv, import_legacy_json, import_v2_json, resolve_import,
    CatalogDelta, ImportIssue, ImportPreview, IssueSeverity, ProposedItem, RawRecord,
    SuggestedType, TypeResolution, MAX_IMPORT_BYTES,
};
pub use preset::{
    resolve_preset, LoadoutPreset, PresetDocument, PresetResolution, PresetStore,
    PRESET_SCHEMA_VERSION,
};
pub use types::{
    EquipmentSlot, ItemId, ItemRef, ItemType, SlotIntent, SlotObservation, SourceFormat,
};
pub use validation::{
    encode_intent, find_verified_helmets, is_player_usable, preview_intent, resolve_intent,
    DiffEntry, HumanReadableDiff, ValidationError,
};
