//! Layout recognition. A valid checksum is not permission to write.

/// A payload layout whose equipment offsets have been verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutId {
    Observed0106,
    Observed0107,
}

impl LayoutId {
    /// Recognize an exact header and logical-length pair, never either alone.
    pub fn recognize(payload: &[u8]) -> Option<Self> {
        match (payload.len(), payload.get(..12)) {
            (
                572_088,
                Some([0x06, 0x01, 0x00, 0x00, 0x3E, 0xEA, 0xCE, 0xA6, 0xB8, 0xBA, 0x08, 0x00]),
            ) => Some(Self::Observed0106),
            (
                572_092,
                Some([0x07, 0x01, 0x00, 0x00, 0xB6, 0x87, 0xE3, 0x57, 0xBC, 0xBA, 0x08, 0x00]),
            ) => Some(Self::Observed0107),
            _ => None,
        }
    }
}

/// Payload offsets established by controlled in-game tests.
pub mod fields {
    use crate::container::PayloadOffset;

    /// Equipped head slot. Accepts both Helmet and Armor IDs — that is the point.
    pub const HEAD: PayloadOffset = PayloadOffset(0x0121);
    /// Equipped cape slot. Displayed read-only in P0.
    pub const CAPE: PayloadOffset = PayloadOffset(0x0125);
    /// Equipped body armor slot.
    pub const BODY: PayloadOffset = PayloadOffset(0x0129);
}

/// Whether the decoded image may be written by the known-field editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutSupport {
    /// Length, header and both checksums match a verified layout.
    KnownWritable,
    /// Decoded and checksum-valid, but field offsets are not verified here.
    DecodedReadOnly { reason: String },
}

impl LayoutSupport {
    /// Player-facing status word. Never claims more than the checks show.
    pub fn status_label(&self) -> &'static str {
        match self {
            LayoutSupport::KnownWritable => "文件校验通过（可写入）",
            LayoutSupport::DecodedReadOnly { .. } => "已读取，但布局未验证（只读）",
        }
    }

    /// Explanation shown in diagnostics.
    pub fn detail(&self) -> Option<&str> {
        match self {
            LayoutSupport::KnownWritable => None,
            LayoutSupport::DecodedReadOnly { reason } => Some(reason),
        }
    }
}
