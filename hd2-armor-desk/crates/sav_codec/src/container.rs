//! Strict, bounded decoding of the observed save container, plus equal-length repacking.
//!
//! Three address spaces are kept apart on purpose:
//! * `file` — the compressed container on disk;
//! * `padded` — every decompressed 65536-byte block concatenated;
//! * `payload` — `padded[..logical_size]`, the body that carries the fields.

use crate::checksum::{crc32, inner_checksum, INNER_CHECKSUM_OFFSET};
use crate::error::{DecodeError, EncodeError};
use crate::layout::{LayoutSupport, KNOWN_HEADER, KNOWN_LENGTH};

/// Bytes per decompressed block.
pub const BLOCK_SIZE: usize = 65536;
/// Application-level input ceiling; not a statement about the game format.
pub const MAX_INPUT: usize = 16 * 1024 * 1024;
/// Container signature used by the observed layout.
pub const MAGIC: [u8; 8] = [0xD4, 0xFD, 0x1C, 0x6B, 0x5F, 0x7B, 0x23, 0xCC];
/// Offset of the first compressed block length.
const FIRST_BLOCK_OFFSET: usize = 0x24;
/// Offset of the byte the outer CRC32 starts covering.
const CRC_REGION_START: usize = 0x10;
/// Payload header (magic, inner length, inner checksum) is never patchable:
/// the checksum is maintained by the encoder itself.
pub const PROTECTED_HEADER_LEN: usize = 0x10;

/// A byte offset into the logical payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PayloadOffset(pub usize);

/// A byte offset into the raw container file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileOffset(pub usize);

/// An equal-length 4-byte edit at a payload offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldPatch {
    pub offset: PayloadOffset,
    pub before: [u8; 4],
    pub after: [u8; 4],
}

/// A decoded save image that retains every byte needed to repack it faithfully.
#[derive(Debug, Clone)]
pub struct SaveImage {
    raw: Vec<u8>,
    payload: Vec<u8>,
    compressed_blocks: Vec<Vec<u8>>,
    full_blocks: Vec<Vec<u8>>,
    support: LayoutSupport,
    outer_crc32: u32,
    inner_low32: u32,
}

impl SaveImage {
    /// Decode a container. Never panics, never allocates from unvalidated lengths.
    pub fn decode(raw: Vec<u8>) -> Result<SaveImage, DecodeError> {
        if raw.len() < 40 || raw.len() > MAX_INPUT {
            return Err(DecodeError::TruncatedOrOversized { size: raw.len() });
        }
        if raw[..8] != MAGIC {
            return Err(DecodeError::UnrecognizedMagic);
        }
        if read_u32(&raw, 8)? != 1 || read_u32(&raw, 0x18)? != 0xF000_0001 {
            return Err(DecodeError::UnsupportedFlags);
        }

        let stored_crc = read_u32(&raw, 0x0C)?;
        let calculated_crc = crc32(&raw[CRC_REGION_START..]);
        if stored_crc != calculated_crc {
            return Err(DecodeError::BadOuterCrc {
                stored: stored_crc,
                calculated: calculated_crc,
            });
        }

        let stored_size = read_u32(&raw, 0x10)? as usize;
        let actual_size = raw.len() - 0x1C;
        if stored_size != actual_size {
            return Err(DecodeError::StoredSizeMismatch {
                stored: stored_size,
                actual: actual_size,
            });
        }

        let logical_size = read_u32(&raw, 0x14)? as usize;
        if !(16..=MAX_INPUT).contains(&logical_size) {
            return Err(DecodeError::InvalidLogicalSize { size: logical_size });
        }

        let logical_copy = read_u64(&raw, 0x1C)?;
        if logical_copy != logical_size as u64 {
            return Err(DecodeError::LogicalCopyMismatch {
                copy: logical_copy,
                size: logical_size,
            });
        }

        let block_count = logical_size.div_ceil(BLOCK_SIZE);
        let mut pos = FIRST_BLOCK_OFFSET;
        let mut compressed_blocks = Vec::with_capacity(block_count);
        let mut full_blocks = Vec::with_capacity(block_count);

        for _ in 0..block_count {
            if pos + 4 > raw.len() {
                return Err(DecodeError::MissingBlockLength { at: pos });
            }
            let length = read_u32(&raw, pos)? as usize;
            pos += 4;
            if length == 0 || length > raw.len() || pos + length > raw.len() {
                return Err(DecodeError::BlockOutOfBounds { at: pos, length });
            }
            let chunk = &raw[pos..pos + length];
            let mut plain = vec![0u8; BLOCK_SIZE];
            let written = lz4_flex::block::decompress_into(chunk, &mut plain)
                .map_err(|_| DecodeError::InvalidLz4Block { at: pos })?;
            if written != BLOCK_SIZE {
                return Err(DecodeError::UnexpectedBlockLength {
                    at: pos,
                    length: written,
                });
            }
            compressed_blocks.push(chunk.to_vec());
            full_blocks.push(plain);
            pos += length;
        }

        if pos != raw.len() {
            return Err(DecodeError::TrailingBytes {
                consumed: pos,
                total: raw.len(),
            });
        }

        let mut padded = Vec::with_capacity(block_count * BLOCK_SIZE);
        for block in &full_blocks {
            padded.extend_from_slice(block);
        }
        if padded[logical_size..].iter().any(|byte| *byte != 0) {
            return Err(DecodeError::NonZeroPadding);
        }

        let payload = padded[..logical_size].to_vec();
        let stored_inner = read_u32(&payload, 8)? as usize;
        if stored_inner != logical_size {
            return Err(DecodeError::InnerLengthMismatch {
                stored: stored_inner,
                logical: logical_size,
            });
        }

        let stored_hash = read_u32(&payload, INNER_CHECKSUM_OFFSET)?;
        let calculated_hash = inner_checksum(&payload);
        if stored_hash != calculated_hash {
            return Err(DecodeError::BadInnerHash {
                stored: stored_hash,
                calculated: calculated_hash,
            });
        }

        // Checksums are valid; only now does layout matching decide write access.
        let support = if payload.len() == KNOWN_LENGTH && payload[..12] == KNOWN_HEADER {
            LayoutSupport::KnownWritable
        } else {
            LayoutSupport::DecodedReadOnly {
                reason: format!(
                    "正文长度 {} 或头部 {} 与已核验布局不一致",
                    payload.len(),
                    hex_prefix(&payload, 12)
                ),
            }
        };

        Ok(SaveImage {
            raw,
            payload,
            compressed_blocks,
            full_blocks,
            support,
            outer_crc32: stored_crc,
            inner_low32: stored_hash,
        })
    }

    /// The compressed container exactly as decoded.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// The logical payload.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Whether this image may be written by the known-field editor.
    pub fn support(&self) -> &LayoutSupport {
        &self.support
    }

    /// True when the layout matches the verified field offsets.
    pub fn is_writable(&self) -> bool {
        matches!(self.support, LayoutSupport::KnownWritable)
    }

    /// Outer CRC32 as stored in the container.
    pub fn outer_crc32(&self) -> u32 {
        self.outer_crc32
    }

    /// Inner MurmurHash64A low 32 bits as stored in the payload.
    pub fn inner_low32(&self) -> u32 {
        self.inner_low32
    }

    /// Logical payload length.
    pub fn logical_size(&self) -> usize {
        self.payload.len()
    }

    /// Per-block compressed lengths, in file order.
    pub fn compressed_block_lengths(&self) -> Vec<usize> {
        self.compressed_blocks.iter().map(Vec::len).collect()
    }

    /// Bounds-checked little-endian read from the logical payload.
    pub fn read_u32(&self, offset: PayloadOffset) -> Result<u32, DecodeError> {
        read_u32(&self.payload, offset.0)
    }

    /// Repack with equal-length patches.
    ///
    /// * No effective change returns the original bytes verbatim.
    /// * Unchanged blocks reuse their original compressed bytes.
    /// * The inner hash and the outer CRC32 are always refreshed.
    pub fn encode_patches(&self, patches: &[FieldPatch]) -> Result<Vec<u8>, EncodeError> {
        if !patches.is_empty() && !self.is_writable() {
            return Err(EncodeError::LayoutNotWritable);
        }

        let mut next = self.payload.clone();
        let mut touched = Vec::with_capacity(patches.len());
        for patch in patches {
            let start = patch.offset.0;
            let end = start
                .checked_add(4)
                .ok_or(EncodeError::PatchOutOfBounds { offset: start })?;
            if end > self.payload.len() {
                return Err(EncodeError::PatchOutOfBounds { offset: start });
            }
            if start < PROTECTED_HEADER_LEN {
                return Err(EncodeError::ProtectedRegion { offset: start });
            }
            if self.payload[start..end] != patch.before {
                return Err(EncodeError::PatchBeforeMismatch {
                    offset: start,
                    expected: patch.before,
                    found: self.payload[start..end].try_into().unwrap(),
                });
            }
            for other in &touched {
                if ranges_overlap(start, end, *other) {
                    return Err(EncodeError::OverlappingPatches { offset: start });
                }
            }
            touched.push((start, end));
            next[start..end].copy_from_slice(&patch.after);
        }

        let new_hash = inner_checksum(&next);
        next[INNER_CHECKSUM_OFFSET..INNER_CHECKSUM_OFFSET + 4]
            .copy_from_slice(&new_hash.to_le_bytes());

        if next == self.payload {
            return Ok(self.raw.clone());
        }

        // Padded body: the edited payload followed by the original terminal padding.
        // Every block, including the last, is compressed at the full 65536 bytes.
        let mut padded = Vec::with_capacity(self.full_blocks.len() * BLOCK_SIZE);
        for block in &self.full_blocks {
            padded.extend_from_slice(block);
        }
        padded[..next.len()].copy_from_slice(&next);

        let mut blocks: Vec<Vec<u8>> = Vec::with_capacity(self.full_blocks.len());
        for (index, before) in self.full_blocks.iter().enumerate() {
            let start = index * BLOCK_SIZE;
            let after = &padded[start..start + BLOCK_SIZE];
            if before.as_slice() == after {
                blocks.push(self.compressed_blocks[index].clone());
            } else {
                // Raw LZ4 block: no frame, no uncompressed-size prefix.
                blocks.push(lz4_flex::block::compress(after));
            }
        }

        let mut out = Vec::with_capacity(self.raw.len());
        out.extend_from_slice(&self.raw[..FIRST_BLOCK_OFFSET]);
        for block in &blocks {
            let length = u32::try_from(block.len()).map_err(|_| EncodeError::BlockTooLarge)?;
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(block);
        }

        let body_len = u32::try_from(out.len() - 0x1C).map_err(|_| EncodeError::BlockTooLarge)?;
        out[0x10..0x14].copy_from_slice(&body_len.to_le_bytes());
        let crc = crc32(&out[CRC_REGION_START..]);
        out[0x0C..0x10].copy_from_slice(&crc.to_le_bytes());

        // Independent verification of the produced container.
        let check = SaveImage::decode(out.clone())
            .map_err(|error| EncodeError::VerificationFailed(error.to_string()))?;
        if check.payload() != next.as_slice() {
            return Err(EncodeError::VerificationMismatch);
        }

        Ok(out)
    }

    /// Human-readable summary of the two checksums and the layout verdict.
    pub fn checksum_report(&self) -> String {
        format!(
            "外层 CRC32 0x{:08X} · 内层 Murmur64A-low32 0x{:08X} · 逻辑长度 {}",
            self.outer_crc32,
            self.inner_low32,
            self.payload.len()
        )
    }
}

fn ranges_overlap(a: usize, b: usize, other: (usize, usize)) -> bool {
    a.max(other.0) < b.min(other.1)
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, DecodeError> {
    let end = offset.checked_add(4).ok_or(DecodeError::OutOfBounds)?;
    let slice = data.get(offset..end).ok_or(DecodeError::OutOfBounds)?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn read_u64(data: &[u8], offset: usize) -> Result<u64, DecodeError> {
    let end = offset.checked_add(8).ok_or(DecodeError::OutOfBounds)?;
    let slice = data.get(offset..end).ok_or(DecodeError::OutOfBounds)?;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

fn hex_prefix(data: &[u8], count: usize) -> String {
    data.iter()
        .take(count)
        .map(|byte| format!("{byte:02X}"))
        .collect()
}
