//! Golden tests over the bundled synthetic fixtures plus the MurmurHash64A vector set.
//!
//! Every fixture is synthetic (`SYNTHETIC_DO_NOT_IMPORT_TO_GAME`). The manifest is
//! the source of truth for expected accept/reject outcomes.

use std::path::{Path, PathBuf};

use sav_codec::{
    crc32, fields, inner_checksum, murmur64a, DecodeError, EncodeError, FieldPatch, LayoutId,
    LayoutSupport, PayloadOffset, SaveImage, BLOCK_SIZE, INNER_CHECKSUM_OFFSET,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
}

fn manifest() -> Value {
    let text = std::fs::read_to_string(fixtures_dir().join("manifest.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixtures_dir().join(name)).unwrap()
}

fn case<'a>(manifest: &'a Value, file: &str) -> &'a Value {
    manifest["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["file"] == file)
        .unwrap_or_else(|| panic!("fixture {file} missing from manifest"))
}

/// A02: every vector in `murmur_vectors.json` must match bit for bit.
#[test]
fn murmur64a_matches_all_reference_vectors() {
    let text = std::fs::read_to_string(fixtures_dir().join("murmur_vectors.json")).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();
    assert_eq!(vectors.len(), 228, "vector count changed");
    assert_eq!(doc["count"].as_u64().unwrap() as usize, vectors.len());

    for vector in vectors {
        let input = hex_to_bytes(vector["input_hex"].as_str().unwrap());
        let seed = vector["seed"].as_u64().unwrap();
        let expected_full = parse_hex_u64(vector["murmur64a_hex"].as_str().unwrap());
        let expected_low = parse_hex_u64(vector["low32_hex"].as_str().unwrap()) as u32;

        let actual = murmur64a(&input, seed);
        assert_eq!(
            actual,
            expected_full,
            "length {} seed {} full hash mismatch",
            input.len(),
            seed
        );
        assert_eq!(
            actual as u32,
            expected_low,
            "length {} seed {} low32 mismatch",
            input.len(),
            seed
        );
    }
}

/// A01/A09/A12: fixtures are accepted or rejected exactly as the manifest states.
#[test]
fn fixtures_match_manifest_expectations() {
    let manifest = manifest();
    let cases = manifest["cases"].as_array().unwrap();

    for case in cases {
        let name = case["file"].as_str().unwrap();
        let bytes = fixture(name);
        assert_eq!(
            bytes.len() as u64,
            case["raw_size"].as_u64().unwrap(),
            "{name}: raw size"
        );
        assert_eq!(
            sha256_hex(&bytes),
            case["raw_sha256"].as_str().unwrap(),
            "{name}: raw sha256"
        );

        let expected = case["expected"].as_str().unwrap();
        match expected {
            "valid" | "unknown_layout_readonly" => {
                let image = SaveImage::decode(bytes)
                    .unwrap_or_else(|error| panic!("{name}: expected accept, got {error}"));
                assert_eq!(
                    image.logical_size() as u64,
                    case["logical_size"].as_u64().unwrap(),
                    "{name}: logical size"
                );
                assert_eq!(
                    sha256_hex(image.payload()),
                    case["payload_sha256"].as_str().unwrap(),
                    "{name}: payload sha256"
                );
                assert_eq!(
                    format!("0x{:08X}", image.inner_low32()),
                    case["inner_low32"].as_str().unwrap(),
                    "{name}: inner low32"
                );
                assert_eq!(
                    format!("0x{:08X}", image.outer_crc32()),
                    case["outer_crc32"].as_str().unwrap(),
                    "{name}: outer crc"
                );
                assert_eq!(
                    image.read_u32(fields::HEAD).unwrap(),
                    parse_hex_u64(case["head_id"].as_str().unwrap()) as u32,
                    "{name}: head id"
                );
                assert_eq!(
                    image.read_u32(fields::BODY).unwrap(),
                    parse_hex_u64(case["body_id"].as_str().unwrap()) as u32,
                    "{name}: body id"
                );
                assert_eq!(
                    image.read_u32(fields::CAPE).unwrap(),
                    parse_hex_u64(case["cape_id"].as_str().unwrap()) as u32,
                    "{name}: cape id"
                );

                let writable = case["known_writable_layout"].as_bool().unwrap();
                assert_eq!(image.is_writable(), writable, "{name}: layout verdict");
                if !writable {
                    assert!(matches!(
                        image.support(),
                        LayoutSupport::DecodedReadOnly { .. }
                    ));
                }

                let lengths: Vec<usize> = image.compressed_block_lengths();
                let expected_lengths: Vec<usize> = case["compressed_block_lengths"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_u64().unwrap() as usize)
                    .collect();
                assert_eq!(lengths, expected_lengths, "{name}: block lengths");
            }
            "invalid" => {
                let error = SaveImage::decode(bytes)
                    .err()
                    .unwrap_or_else(|| panic!("{name}: expected rejection, got accept"));
                // The manifest records the reference tool's English reason; assert the
                // variant matches that reason rather than the localized message text.
                let expected_reason = case["reference_error"].as_str().unwrap();
                let variant_matches = matches!(
                    (expected_reason, &error),
                    ("Outer CRC32 mismatch", DecodeError::BadOuterCrc { .. })
                        | (
                            "Inner MurmurHash64A-low32 checksum mismatch",
                            DecodeError::BadInnerHash { .. }
                        )
                        | ("Nonzero terminal padding", DecodeError::NonZeroPadding)
                        | ("Unparsed trailing bytes", DecodeError::TrailingBytes { .. })
                        | (
                            "Logical length copy mismatch",
                            DecodeError::LogicalCopyMismatch { .. }
                        )
                        | (
                            "Compressed block out of bounds",
                            DecodeError::BlockOutOfBounds { .. }
                        )
                );
                assert!(
                    variant_matches,
                    "{name}: expected {expected_reason}, got {error}"
                );
            }
            other => panic!("{name}: unknown expectation {other}"),
        }
    }
}

/// A03: repacking with no patches returns the original container byte for byte.
#[test]
fn no_patch_repack_is_byte_identical() {
    let manifest = manifest();
    for case in manifest["cases"].as_array().unwrap() {
        if case["expected"].as_str() != Some("valid") {
            continue;
        }
        let name = case["file"].as_str().unwrap();
        let bytes = fixture(name);
        let image = SaveImage::decode(bytes.clone()).unwrap();
        assert_eq!(
            image.encode_patches(&[]).unwrap(),
            bytes,
            "{name}: no-op repack must be byte identical"
        );
    }
}

/// A04/A05/A06: patched repacks change only the intended field plus the inner hash,
/// and re-decoding reproduces the intended payload.
#[test]
fn patched_repack_changes_only_allowed_ranges() {
    let manifest = manifest();
    let baseline_bytes = fixture("valid_baseline.bin");
    let baseline = SaveImage::decode(baseline_bytes.clone()).unwrap();
    let baseline_payload = baseline.payload().to_vec();

    struct Scenario {
        name: &'static str,
        head: Option<u32>,
        body: Option<u32>,
    }

    let scenarios = [
        Scenario {
            name: "head only",
            head: Some(0x261C4A52),
            body: None,
        },
        Scenario {
            name: "body only",
            head: None,
            body: Some(0x61B31723),
        },
        Scenario {
            name: "head and body",
            head: Some(0x261C4A52),
            body: Some(0x61B31723),
        },
    ];

    for scenario in scenarios {
        let mut patches = Vec::new();
        if let Some(value) = scenario.head {
            patches.push(patch(&baseline, fields::HEAD, value));
        }
        if let Some(value) = scenario.body {
            patches.push(patch(&baseline, fields::BODY, value));
        }

        let produced = baseline.encode_patches(&patches).unwrap();
        let redecoded = SaveImage::decode(produced).unwrap();

        // Intended field values landed.
        if let Some(value) = scenario.head {
            assert_eq!(redecoded.read_u32(fields::HEAD).unwrap(), value);
        } else {
            assert_eq!(
                redecoded.read_u32(fields::HEAD).unwrap(),
                baseline.read_u32(fields::HEAD).unwrap()
            );
        }
        if let Some(value) = scenario.body {
            assert_eq!(redecoded.read_u32(fields::BODY).unwrap(), value);
        } else {
            assert_eq!(
                redecoded.read_u32(fields::BODY).unwrap(),
                baseline.read_u32(fields::BODY).unwrap()
            );
        }
        assert_eq!(
            redecoded.read_u32(fields::CAPE).unwrap(),
            baseline.read_u32(fields::CAPE).unwrap(),
            "{}: cape must not move",
            scenario.name
        );

        // Every differing payload byte lies in the allowed whitelist.
        let mut allowed: Vec<(usize, usize)> =
            vec![(INNER_CHECKSUM_OFFSET, INNER_CHECKSUM_OFFSET + 4)];
        if scenario.head.is_some() {
            allowed.push((fields::HEAD.0, fields::HEAD.0 + 4));
        }
        if scenario.body.is_some() {
            allowed.push((fields::BODY.0, fields::BODY.0 + 4));
        }
        for range in diff_ranges(&baseline_payload, redecoded.payload()) {
            assert!(
                allowed
                    .iter()
                    .any(|allow| range.0 >= allow.0 && range.1 <= allow.1),
                "{}: change {range:?} outside whitelist {allowed:?}",
                scenario.name
            );
        }
    }

    // The manifest records the same whitelist for the pre-built head+body fixture.
    let expected: Vec<(usize, usize)> = case(&manifest, "valid_head_and_body.bin")
        ["changed_payload_ranges_vs_baseline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|range| {
            (
                range["start"].as_u64().unwrap() as usize,
                range["end_exclusive"].as_u64().unwrap() as usize,
            )
        })
        .collect();
    assert_eq!(
        expected,
        vec![(12, 16), (289, 293), (297, 301)],
        "manifest whitelist changed"
    );
    let head_and_body = SaveImage::decode(fixture("valid_head_and_body.bin")).unwrap();
    assert_eq!(
        diff_ranges(&baseline_payload, head_and_body.payload()),
        expected
    );
}

/// A08: unchanged compressed blocks are reused; a later-block edit also refreshes block 0.
#[test]
fn block_reuse_and_cross_block_hash_refresh() {
    let baseline_bytes = fixture("valid_baseline.bin");
    let baseline = SaveImage::decode(baseline_bytes.clone()).unwrap();
    let baseline_lengths = baseline.compressed_block_lengths();

    // Head edit: block 0 changes (field + inner hash), blocks 1.. keep their bytes.
    let head_patch = patch(&baseline, fields::HEAD, 0x261C4A52);
    let head_out = baseline.encode_patches(&[head_patch]).unwrap();
    let head_image = SaveImage::decode(head_out.clone()).unwrap();
    // Only block 0 may change length; a different LZ4 implementation is allowed to
    // produce different bytes for a changed block, but not to touch untouched ones.
    assert_eq!(
        head_image.compressed_block_lengths()[1..],
        baseline_lengths[1..]
    );
    for index in 1..baseline_lengths.len() {
        assert_eq!(
            compressed_block(&head_out, index),
            compressed_block(&baseline_bytes, index),
            "block {index} must be reused verbatim"
        );
    }
    // The edited block still decodes to the same full 65536 bytes as the reference.
    assert_eq!(
        head_image.payload(),
        SaveImage::decode(fixture("valid_head_b01.bin"))
            .unwrap()
            .payload()
    );

    // A later-block edit must leave untouched blocks alone but recompress block 0.
    let manifest = manifest();
    let later_case = case(&manifest, "valid_later_block_edit.bin");
    let later_bytes = fixture("valid_later_block_edit.bin");
    let later = SaveImage::decode(later_bytes.clone()).unwrap();
    assert_ne!(
        later.payload(),
        baseline.payload(),
        "later-block fixture differs from baseline"
    );
    let later_ranges: Vec<(usize, usize)> = later_case["changed_payload_ranges_vs_baseline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|range| {
            (
                range["start"].as_u64().unwrap() as usize,
                range["end_exclusive"].as_u64().unwrap() as usize,
            )
        })
        .collect();
    assert_eq!(later_ranges, vec![(12, 16), (70000, 70004)]);

    // Reproduce that edit through the encoder and confirm block 0 was rewritten.
    let target = 70000usize;
    let before = u32::from_le_bytes(later.payload()[target..target + 4].try_into().unwrap());
    let restored = baseline.encode_patches(&[FieldPatch {
        offset: PayloadOffset(target),
        before: baseline.payload()[target..target + 4].try_into().unwrap(),
        after: before.to_le_bytes(),
    }]);
    let restored = restored.unwrap();
    let restored_image = SaveImage::decode(restored.clone()).unwrap();
    assert_eq!(restored_image.payload(), later.payload());
    assert_ne!(
        compressed_block(&restored, 0),
        compressed_block(&baseline_bytes, 0),
        "block 0 carries the inner hash and must be rewritten"
    );
    // Block 1 was rewritten too (it holds the edited byte), so its compressed bytes
    // may differ from Python's. The manifest states only semantics must match:
    // compare the decompressed content instead of the compressor's output.
    let restored_block1 = decompress_block(&restored, 1);
    let later_block1 = decompress_block(&later_bytes, 1);
    assert_eq!(
        restored_block1, later_block1,
        "block 1 must decode to the same 65536 bytes as the reference fixture"
    );
    assert_eq!(restored_block1.len(), BLOCK_SIZE);
}

/// A11: unaligned offsets decode identically regardless of optimisation level.
#[test]
fn unaligned_reads_are_safe() {
    let bytes = fixture("valid_baseline.bin");
    let image = SaveImage::decode(bytes).unwrap();
    assert_eq!(image.read_u32(PayloadOffset(0x121)).unwrap(), 0x0568_48E9);
    assert_eq!(image.read_u32(PayloadOffset(0x125)).unwrap(), 0x4657_CFB3);
    assert_eq!(image.read_u32(PayloadOffset(0x129)).unwrap(), 0xD346_1392);
    assert_eq!(
        image.read_u32(PayloadOffset(0x11D)).unwrap(),
        image.read_u32(PayloadOffset(0x11D)).unwrap()
    );

    let out_of_range = image.read_u32(PayloadOffset(image.logical_size() - 3));
    assert!(matches!(out_of_range, Err(DecodeError::OutOfBounds)));
}

/// Rejections that the encoder itself must perform.
#[test]
fn encoder_rejects_unsafe_requests() {
    let bytes = fixture("valid_baseline.bin");
    let image = SaveImage::decode(bytes.clone()).unwrap();

    // Wrong `before` bytes.
    let mismatch = image.encode_patches(&[FieldPatch {
        offset: fields::HEAD,
        before: [0, 0, 0, 0],
        after: [1, 2, 3, 4],
    }]);
    assert!(matches!(
        mismatch,
        Err(EncodeError::PatchBeforeMismatch { .. })
    ));

    // Overlapping patches.
    let overlap = image.encode_patches(&[
        FieldPatch {
            offset: PayloadOffset(0x121),
            before: image.payload()[0x121..0x125].try_into().unwrap(),
            after: [0xAA; 4],
        },
        FieldPatch {
            offset: PayloadOffset(0x123),
            before: image.payload()[0x123..0x127].try_into().unwrap(),
            after: [0xBB; 4],
        },
    ]);
    assert!(matches!(
        overlap,
        Err(EncodeError::OverlappingPatches { .. })
    ));

    // Out-of-range patch.
    let oversized = image.encode_patches(&[FieldPatch {
        offset: PayloadOffset(image.logical_size() - 2),
        before: [0; 4],
        after: [0; 4],
    }]);
    assert!(matches!(
        oversized,
        Err(EncodeError::PatchOutOfBounds { .. })
    ));

    // A hostile public offset must be rejected without overflowing or panicking.
    let maximal = image.encode_patches(&[FieldPatch {
        offset: PayloadOffset(usize::MAX),
        before: [0; 4],
        after: [0; 4],
    }]);
    assert!(matches!(maximal, Err(EncodeError::PatchOutOfBounds { .. })));

    // Protected header region (inner header and checksum) must not be patchable
    // through the known-field API: the encoder maintains those bytes itself.
    let protected = image.encode_patches(&[FieldPatch {
        offset: PayloadOffset(INNER_CHECKSUM_OFFSET),
        before: image.payload()[INNER_CHECKSUM_OFFSET..INNER_CHECKSUM_OFFSET + 4]
            .try_into()
            .unwrap(),
        after: [0xFF; 4],
    }]);
    assert!(matches!(
        protected,
        Err(EncodeError::ProtectedRegion { .. })
    ));

    // Unknown layout refuses writes even with a syntactically valid patch.
    let unknown = SaveImage::decode(fixture("unknown_layout_valid.bin")).unwrap();
    let refused = unknown.encode_patches(&[FieldPatch {
        offset: fields::HEAD,
        before: unknown.payload()[0x121..0x125].try_into().unwrap(),
        after: [1, 2, 3, 4],
    }]);
    assert!(matches!(refused, Err(EncodeError::LayoutNotWritable)));
}

/// A09/A10: a fixture whose outer CRC is repaired but inner hash is stale must be
/// rejected, and the inner hash must cover the whole logical body.
#[test]
fn inner_hash_is_mandatory_and_covers_whole_payload() {
    let bytes = fixture("invalid_inner_hash.bin");
    let error = SaveImage::decode(bytes.clone()).unwrap_err();
    assert!(matches!(error, DecodeError::BadInnerHash { .. }));
    // A stale inner hash is a hard failure, not a transient "still being written"
    // state: the watcher must not treat it as pending and keep retrying silently.
    assert!(!error.is_transient());

    // The stale fixture keeps a valid outer CRC: the rejection must come from the
    // inner layer, which is exactly the regression the old `02` build shipped.
    assert_eq!(
        crc32(&bytes[0x10..]),
        u32::from_le_bytes(bytes[0x0C..0x10].try_into().unwrap())
    );

    // Editing a head byte changes the whole-body hash, so the stored value can
    // never be reused.
    let baseline = SaveImage::decode(fixture("valid_baseline.bin")).unwrap();
    let mut body = baseline.payload().to_vec();
    body[0x121..0x125].copy_from_slice(&0x261C_4A52u32.to_le_bytes());
    assert_ne!(inner_checksum(&body), baseline.inner_low32());
    assert_ne!(inner_checksum(&body), inner_checksum(baseline.payload()));

    // A far-away byte counts too: the hash is over the entire logical payload.
    let mut distant = baseline.payload().to_vec();
    distant[500_000] ^= 0x01;
    assert_ne!(inner_checksum(&distant), baseline.inner_low32());
}

/// A12: hostile inputs are rejected deterministically, without panic or huge allocation.
#[test]
fn hostile_inputs_are_rejected() {
    let baseline = fixture("valid_baseline.bin");

    // Oversized container claim.
    let mut oversized = baseline.clone();
    oversized[0x14..0x18].copy_from_slice(&(u32::MAX).to_le_bytes());
    assert!(SaveImage::decode(oversized).is_err());

    // Zero-length block. The stored-size field is repaired first so the check
    // under test is the block-length rule, not the size mismatch.
    let mut zero_block = baseline.clone();
    zero_block[0x24..0x28].copy_from_slice(&0u32.to_le_bytes());
    let zero_block_crc = crc32(&zero_block[0x10..]);
    zero_block[0x0C..0x10].copy_from_slice(&zero_block_crc.to_le_bytes());
    assert!(matches!(
        SaveImage::decode(zero_block),
        Err(DecodeError::BlockOutOfBounds { .. })
    ));

    // Every truncation length must return an error rather than panic.
    for cut in 0..baseline.len() {
        let mut truncated = baseline.clone();
        truncated.truncate(cut);
        assert!(
            SaveImage::decode(truncated).is_err(),
            "truncation at {cut} must be rejected"
        );
    }

    // Single-byte mutations of the header must never panic.
    for index in 0..0x24usize {
        for mask in [0x01u8, 0x80, 0xFF] {
            let mut mutated = baseline.clone();
            mutated[index] ^= mask;
            let _ = SaveImage::decode(mutated);
        }
    }

    // Outer CRC must be checked before any decompression is attempted.
    let mut bad_crc = baseline.clone();
    bad_crc[0x0C] ^= 0xFF;
    assert!(matches!(
        SaveImage::decode(bad_crc),
        Err(DecodeError::BadOuterCrc { .. })
    ));
}

/// `crc32` must agree with the reference `zlib.crc32` values recorded in the manifest.
#[test]
fn outer_crc_matches_reference_values() {
    let manifest = manifest();
    for case in manifest["cases"].as_array().unwrap() {
        if case["expected"].as_str() == Some("invalid") {
            continue;
        }
        let bytes = fixture(case["file"].as_str().unwrap());
        let expected = parse_hex_u64(case["outer_crc32"].as_str().unwrap()) as u32;
        assert_eq!(crc32(&bytes[0x10..]), expected, "{}", case["file"]);
    }
}

#[test]
fn layout_recognition_requires_exact_header_and_length_pairs() {
    let old = SaveImage::decode(fixture("valid_baseline.bin")).unwrap();
    let new = SaveImage::decode(fixture("valid_new_baseline.bin")).unwrap();
    assert_eq!(old.layout_id(), Some(LayoutId::Observed0106));
    assert_eq!(new.layout_id(), Some(LayoutId::Observed0107));
    assert_eq!(LayoutId::recognize(&[]), None);

    for (image, other) in [(&old, &new), (&new, &old)] {
        // Keep a consistent inner length, but pair it with the other version tag.
        let mut payload = image.payload().to_vec();
        payload[..8].copy_from_slice(&other.payload()[..8]);
        let raw = container_for_payload(image, payload);
        let mismatch = SaveImage::decode(raw.clone()).unwrap();
        assert_eq!(mismatch.layout_id(), None);
        assert!(!mismatch.is_writable());
        assert_eq!(mismatch.encode_patches(&[]).unwrap(), raw);
        assert!(matches!(
            mismatch.encode_patches(&[patch(&mismatch, fields::HEAD, 0x261C_4A52)]),
            Err(EncodeError::LayoutNotWritable)
        ));

        // An exact header is insufficient if the slice has the other length.
        let mut wrong_length = image.payload().to_vec();
        wrong_length.resize(other.logical_size(), 0);
        assert_eq!(LayoutId::recognize(&wrong_length), None);
    }
}

#[test]
fn new_layout_edit_preserves_header_tail_and_untouched_blocks() {
    let raw = fixture("valid_new_baseline.bin");
    let image = SaveImage::decode(raw.clone()).unwrap();
    assert_eq!(image.encode_patches(&[]).unwrap(), raw);
    assert_eq!(image.read_u32(fields::HEAD).unwrap(), 0x9F73_133E);
    assert_eq!(image.read_u32(fields::BODY).unwrap(), 0x5D0D_8002);
    assert_eq!(image.read_u32(fields::CAPE).unwrap(), 0x6E72_F493);
    assert_eq!(image.read_u32(PayloadOffset(0x11)).unwrap(), 0xAB1B_4972);
    assert_eq!(image.read_u32(PayloadOffset(0x15)).unwrap(), 0x335B_8A1A);
    assert_ne!(&image.payload()[572_088..], &[0; 4]);

    let output = image
        .encode_patches(&[
            patch(&image, fields::HEAD, 0x261C_4A52),
            patch(&image, fields::BODY, 0xD346_1392),
        ])
        .unwrap();
    let edited = SaveImage::decode(output.clone()).unwrap();
    assert_eq!(edited.layout_id(), Some(LayoutId::Observed0107));
    let mut expected = image.payload().to_vec();
    expected[fields::HEAD.0..fields::HEAD.0 + 4].copy_from_slice(&0x261C_4A52u32.to_le_bytes());
    expected[fields::BODY.0..fields::BODY.0 + 4].copy_from_slice(&0xD346_1392u32.to_le_bytes());
    let checksum = inner_checksum(&expected);
    expected[12..16].copy_from_slice(&checksum.to_le_bytes());
    assert_eq!(edited.payload(), expected);
    for index in 1..image.compressed_block_lengths().len() {
        assert_eq!(
            compressed_block(&output, index),
            compressed_block(&raw, index)
        );
    }
}

fn container_for_payload(image: &SaveImage, mut payload: Vec<u8>) -> Vec<u8> {
    let logical_size = payload.len();
    payload[8..12].copy_from_slice(&(logical_size as u32).to_le_bytes());
    let checksum = inner_checksum(&payload);
    payload[12..16].copy_from_slice(&checksum.to_le_bytes());
    payload.resize(logical_size.div_ceil(BLOCK_SIZE) * BLOCK_SIZE, 0);
    let mut raw = image.raw()[..0x24].to_vec();
    raw[0x14..0x18].copy_from_slice(&(logical_size as u32).to_le_bytes());
    raw[0x1C..0x24].copy_from_slice(&(logical_size as u64).to_le_bytes());
    for block in payload.chunks_exact(BLOCK_SIZE) {
        let compressed = lz4_flex::block::compress(block);
        raw.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        raw.extend_from_slice(&compressed);
    }
    let stored_size = (raw.len() - 0x1C) as u32;
    raw[0x10..0x14].copy_from_slice(&stored_size.to_le_bytes());
    let checksum = crc32(&raw[0x10..]);
    raw[0x0C..0x10].copy_from_slice(&checksum.to_le_bytes());
    raw
}

fn patch(image: &SaveImage, offset: PayloadOffset, value: u32) -> FieldPatch {
    FieldPatch {
        offset,
        before: image.payload()[offset.0..offset.0 + 4].try_into().unwrap(),
        after: value.to_le_bytes(),
    }
}

fn compressed_block(raw: &[u8], index: usize) -> Vec<u8> {
    let mut pos = 0x24usize;
    for _ in 0..index {
        let length = u32::from_le_bytes(raw[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4 + length;
    }
    let length = u32::from_le_bytes(raw[pos..pos + 4].try_into().unwrap()) as usize;
    raw[pos + 4..pos + 4 + length].to_vec()
}

fn decompress_block(raw: &[u8], index: usize) -> Vec<u8> {
    let mut out = vec![0u8; BLOCK_SIZE];
    let written =
        lz4_flex::block::decompress_into(&compressed_block(raw, index), &mut out).unwrap();
    out.truncate(written);
    out
}

fn diff_ranges(before: &[u8], after: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start: Option<usize> = None;
    for index in 0..before.len().max(after.len()) {
        let differs = before.get(index) != after.get(index);
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
        ranges.push((begin, before.len().max(after.len())));
    }
    ranges
}

fn hex_to_bytes(text: &str) -> Vec<u8> {
    let cleaned: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    cleaned
        .as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn parse_hex_u64(text: &str) -> u64 {
    u64::from_str_radix(text.trim_start_matches("0x").trim_start_matches("0X"), 16).unwrap()
}

/// The manifest's own checksum column must stay consistent with the codec's report.
#[test]
fn checksum_report_mentions_both_layers() {
    let image = SaveImage::decode(fixture("valid_baseline.bin")).unwrap();
    let report = image.checksum_report();
    assert!(report.contains("CRC32"));
    assert!(report.contains("Murmur64A"));
    assert!(report.contains(&format!("0x{:08X}", image.outer_crc32())));
    assert!(report.contains(&format!("0x{:08X}", image.inner_low32())));
}
