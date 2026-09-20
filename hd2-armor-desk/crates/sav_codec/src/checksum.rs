//! MurmurHash64A (little-endian) and CRC32 helpers.
//!
//! The save container uses two unrelated checksums:
//! * inner: `low32(MurmurHash64A_LE(payload, seed=0))` with `payload[0x0C..0x10]` zeroed;
//! * outer: IEEE CRC32 over `file[0x10..EOF]` (same polynomial as `zlib.crc32`).
//!
//! Every 64-bit operation is explicitly wrapping so debug and release builds agree.

/// MurmurHash64A multiplier.
const M: u64 = 0xC6A4_A793_5BD1_E995;
/// MurmurHash64A shift distance.
const R: u32 = 47;

/// MurmurHash64A over arbitrary bytes with the given 64-bit seed.
pub fn murmur64a(data: &[u8], seed: u64) -> u64 {
    let mut h: u64 = seed ^ (data.len() as u64).wrapping_mul(M);
    let complete = data.len() - data.len() % 8;

    let mut chunks = data[..complete].chunks_exact(8);
    for chunk in &mut chunks {
        let mut k = u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h ^= k;
        h = h.wrapping_mul(M);
    }

    let tail = &data[complete..];
    if !tail.is_empty() {
        let mut buf = [0u8; 8];
        buf[..tail.len()].copy_from_slice(tail);
        h ^= u64::from_le_bytes(buf);
        h = h.wrapping_mul(M);
    }

    h ^= h >> R;
    h = h.wrapping_mul(M);
    h ^= h >> R;
    h
}

/// Offset of the stored inner checksum inside the logical payload.
pub const INNER_CHECKSUM_OFFSET: usize = 0x0C;

/// Inner checksum of a logical payload: hash the whole body with the stored
/// checksum field zeroed, then keep the low 32 bits.
///
/// The zeroed field (`0x0C..0x10`) lies inside the second 8-byte word, so the
/// hash runs at full speed over the remaining aligned chunks instead of copying
/// or masking the 500 KiB+ payload byte by byte.
pub fn inner_checksum(payload: &[u8]) -> u32 {
    if payload.len() < INNER_CHECKSUM_OFFSET + 4 {
        return murmur64a(payload, 0) as u32;
    }
    let len = payload.len();
    let mut h: u64 = (len as u64).wrapping_mul(M);

    // Word 0: payload[0..8], untouched.
    let mut k = u64::from_le_bytes(payload[0..8].try_into().unwrap());
    k = k.wrapping_mul(M);
    k ^= k >> R;
    k = k.wrapping_mul(M);
    h ^= k;
    h = h.wrapping_mul(M);

    // Word 1: payload[8..12] then four zeroed checksum bytes.
    let mut word1 = [0u8; 8];
    word1[..4].copy_from_slice(&payload[8..12]);
    let mut k = u64::from_le_bytes(word1);
    k = k.wrapping_mul(M);
    k ^= k >> R;
    k = k.wrapping_mul(M);
    h ^= k;
    h = h.wrapping_mul(M);

    // Remaining words start at the next 8-byte boundary after the field.
    let start = INNER_CHECKSUM_OFFSET + 4;
    let complete = len - len % 8;
    for chunk in payload[start..complete].chunks_exact(8) {
        let mut k = u64::from_le_bytes(chunk.try_into().unwrap());
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h ^= k;
        h = h.wrapping_mul(M);
    }

    if complete != len {
        let mut buf = [0u8; 8];
        buf[..len - complete].copy_from_slice(&payload[complete..]);
        h ^= u64::from_le_bytes(buf);
        h = h.wrapping_mul(M);
    }

    h ^= h >> R;
    h = h.wrapping_mul(M);
    h ^= h >> R;
    h as u32
}

/// IEEE CRC32, matching `zlib.crc32`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_matches_reference_seed_rule() {
        assert_eq!(murmur64a(&[], 0), 0);
        assert_eq!(murmur64a(&[], 1), 0xC6A4_A793_5BD0_64DC);
    }

    #[test]
    fn streaming_inner_matches_whole_buffer_hash() {
        // Build a synthetic payload whose checksum field is zeroed, then check
        // the streaming implementation equals the one-shot hash.
        let mut payload = vec![0u8; 572_088];
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        payload[INNER_CHECKSUM_OFFSET..INNER_CHECKSUM_OFFSET + 4].fill(0);
        assert_eq!(inner_checksum(&payload), murmur64a(&payload, 0) as u32);
    }
}
