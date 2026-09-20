//! Cross-language oracle test.
//!
//! Writes Rust-produced containers into `target/oracle/` so the Python reference
//! implementation can re-read them (`scripts/oracle_crosscheck.py`), and consumes
//! Python-produced bytes when they are present. The Python tool is a development
//! oracle only — the shipped application never calls it.

use std::path::{Path, PathBuf};

use sav_codec::{fields, FieldPatch, PayloadOffset, SaveImage};
use serde_json::json;
use sha2::{Digest, Sha256};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
}

fn out_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("oracle")
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn patch(image: &SaveImage, offset: PayloadOffset, value: u32) -> FieldPatch {
    FieldPatch {
        offset,
        before: image.payload()[offset.0..offset.0 + 4].try_into().unwrap(),
        after: value.to_le_bytes(),
    }
}

/// Emit Rust-produced containers for the Python oracle to re-read.
#[test]
fn emit_rust_outputs_for_python_oracle() {
    let dir = out_dir();
    std::fs::create_dir_all(&dir).unwrap();

    let baseline_bytes = std::fs::read(fixtures_dir().join("valid_baseline.bin")).unwrap();
    let baseline = SaveImage::decode(baseline_bytes).unwrap();

    let mut outputs = Vec::new();
    let cases: [(&str, Vec<FieldPatch>); 3] = [
        (
            "rust_head_armor.bin",
            vec![patch(&baseline, fields::HEAD, 0xD3461392)],
        ),
        (
            "rust_body_b01.bin",
            vec![patch(&baseline, fields::BODY, 0x61B31723)],
        ),
        (
            "rust_head_and_body.bin",
            vec![
                patch(&baseline, fields::HEAD, 0x261C4A52),
                patch(&baseline, fields::BODY, 0x61B31723),
            ],
        ),
    ];

    for (name, patches) in cases {
        let produced = baseline.encode_patches(&patches).unwrap();
        std::fs::write(dir.join(name), &produced).unwrap();
        let image = SaveImage::decode(produced.clone()).unwrap();
        outputs.push(json!({
            "file": name,
            "payload_sha256": sha256_hex(image.payload()),
            "inner_low32": image.inner_low32(),
            "outer_crc32": image.outer_crc32(),
            "head_id": image.read_u32(fields::HEAD).unwrap(),
            "body_id": image.read_u32(fields::BODY).unwrap(),
            "cape_id": image.read_u32(fields::CAPE).unwrap(),
            "raw_sha256": sha256_hex(&produced),
        }));
    }

    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&json!({ "outputs": outputs })).unwrap(),
    )
    .unwrap();
}

/// Consume the Python-produced container when the oracle has been run.
#[test]
fn python_repacked_bytes_decode_identically() {
    let candidate = out_dir().join("python_repacked_head_b01.bin");
    if !candidate.is_file() {
        // The oracle script has not been run in this checkout; the reverse
        // direction is covered by the fixture set, which Python produced.
        eprintln!(
            "skip: {} not present (run scripts/oracle_crosscheck.py)",
            candidate.display()
        );
        return;
    }
    let bytes = std::fs::read(&candidate).unwrap();
    let image = SaveImage::decode(bytes).unwrap();
    assert_eq!(image.read_u32(fields::HEAD).unwrap(), 0x261C_4A52);
    assert_eq!(image.read_u32(fields::BODY).unwrap(), 0xD346_1392);

    // Re-encoding Python's own output with no patches must be byte identical.
    assert_eq!(image.encode_patches(&[]).unwrap(), image.raw());
}
