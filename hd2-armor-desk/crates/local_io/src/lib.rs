//! Local I/O: monitoring, workspace stores, save transactions and discovery.
//!
//! This crate is the only place that touches the file system. It never writes to
//! a game save except through an explicit, user-confirmed
//! [`save_transaction::commit_to_source`] call.
//!
//! `unsafe` is used only in two narrow Win32 FFI sites (file identity and the
//! documented replace call); every other module is safe Rust.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod catalog_store;
pub mod discovery;
pub mod monitor;
pub mod save_transaction;

pub use catalog_store::{
    atomic_write, default_export_name, sanitize_file_name, BackupEntry, StoreError, Workspace,
};
pub use discovery::{
    discover_all, find_candidates, format_epoch, looks_like_save, SaveCandidate, APP_ID, SAVE_NAME,
};
pub use monitor::{
    describe_decode_error, iso_stamp, now_stamp, read_bounded, DocumentGeneration, MonitorEvent,
    PathIdentity, StabilityGate, StableReader, POLL_INTERVAL, SETTLE_WINDOW,
};
pub use save_transaction::{
    commit_to_source, commit_to_source_force_latest, prepare_commit, restore_backup, save_copy,
    sha256_hex, CommitError, CommitOutcome, CommitReceipt, CopyReceipt, PreparedCommit,
};
