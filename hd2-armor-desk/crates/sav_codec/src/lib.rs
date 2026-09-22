//! Strict codec for the observed HELLDIVERS 2 `testament_new.sav` container.
//!
//! Scope and limits:
//! * Only layouts identified by [`LayoutId::recognize`] are writable;
//!   other checksum-valid layouts decode read-only.
//! * A passing checksum proves internal consistency, not game-level semantics.
//! * Every read is bounded; hostile lengths never allocate.

#![forbid(unsafe_code)]

pub mod checksum;
pub mod container;
pub mod error;
pub mod layout;

pub use checksum::{crc32, inner_checksum, murmur64a, INNER_CHECKSUM_OFFSET};
pub use container::{FieldPatch, FileOffset, PayloadOffset, SaveImage, BLOCK_SIZE, MAX_INPUT};
pub use error::{DecodeError, EncodeError};
pub use layout::{fields, LayoutId, LayoutSupport};
