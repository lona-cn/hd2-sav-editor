//! Cross-language development oracle; never used by the shipped application.
//!
//! From hd2-armor-desk: run `cargo test -p sav_codec --test oracle
//! emit_rust_outputs_for_python_oracle`, then `python scripts/oracle_crosscheck.py
//! target/oracle`, then run `cargo test -p sav_codec --test oracle
//! python_repacked_bytes_decode_identically` with HD2_REQUIRE_PYTHON_ORACLE=1.
//! The last step fails if either layout's Python output is missing.

use std::path::{Path, PathBuf};

use sav_codec::{fields, FieldPatch, PayloadOffset, SaveImage};
use serde_json::json;
use sha2::{Digest, Sha256};

const BASELINES: [(&str, &str); 2] = [
    ("observed_0106", "valid_baseline.bin"),
    ("observed_0107", "valid_new_baseline.bin"),
];

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

/// Emit Rust-produced containers for both layouts for Python to re-read.
#[test]
fn emit_rust_outputs_for_python_oracle() {
    let dir = out_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut outputs = Vec::new();
    for (layout, fixture) in BASELINES {
        let baseline =
            SaveImage::decode(std::fs::read(fixtures_dir().join(fixture)).unwrap()).unwrap();
        let cases: [(&str, Vec<FieldPatch>); 3] = [
            (
                "head_armor",
                vec![patch(&baseline, fields::HEAD, 0xD3461392)],
            ),
            ("body_b01", vec![patch(&baseline, fields::BODY, 0x61B31723)]),
            (
                "head_and_body",
                vec![
                    patch(&baseline, fields::HEAD, 0x261C4A52),
                    patch(&baseline, fields::BODY, 0x61B31723),
                ],
            ),
        ];
        for (case, patches) in cases {
            let name = format!("rust_{layout}_{case}.bin");
            let produced = baseline.encode_patches(&patches).unwrap();
            std::fs::write(dir.join(&name), &produced).unwrap();
            let image = SaveImage::decode(produced.clone()).unwrap();
            let mut expected = baseline.payload().to_vec();
            for change in &patches {
                expected[change.offset.0..change.offset.0 + 4].copy_from_slice(&change.after);
            }
            expected[12..16].copy_from_slice(&image.payload()[12..16]);
            assert_eq!(
                image.payload(),
                expected,
                "{name}: unexpected payload change"
            );
            outputs.push(json!({
                "file": name,
                "layout": layout,
                "fixture": fixture,
                "payload_sha256": sha256_hex(image.payload()),
                "inner_low32": image.inner_low32(),
                "outer_crc32": image.outer_crc32(),
                "head_id": image.read_u32(fields::HEAD).unwrap(),
                "body_id": image.read_u32(fields::BODY).unwrap(),
                "cape_id": image.read_u32(fields::CAPE).unwrap(),
                "raw_sha256": sha256_hex(&produced),
            }));
        }
    }
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&json!({ "outputs": outputs })).unwrap(),
    )
    .unwrap();
}

/// Set HD2_REQUIRE_PYTHON_ORACLE=1 in CI: missing Python outputs must fail.
#[test]
fn python_repacked_bytes_decode_identically() {
    let required = std::env::var_os("HD2_REQUIRE_PYTHON_ORACLE").is_some();
    for (layout, fixture) in BASELINES {
        let candidate = out_dir().join(format!("python_{layout}_repacked_head_b01.bin"));
        if !candidate.is_file() {
            assert!(
                !required,
                "missing {}; run scripts/oracle_crosscheck.py target/oracle",
                candidate.display()
            );
            eprintln!(
                "skip: {} not present (run scripts/oracle_crosscheck.py target/oracle)",
                candidate.display()
            );
            continue;
        }
        let baseline =
            SaveImage::decode(std::fs::read(fixtures_dir().join(fixture)).unwrap()).unwrap();
        let image = SaveImage::decode(std::fs::read(&candidate).unwrap()).unwrap();
        let mut expected = baseline.payload().to_vec();
        expected[fields::HEAD.0..fields::HEAD.0 + 4]
            .copy_from_slice(&0x261C_4A52_u32.to_le_bytes());
        expected[12..16].copy_from_slice(&image.payload()[12..16]);
        assert_eq!(
            image.payload(),
            expected,
            "{layout}: unexpected Python payload change"
        );
        assert_eq!(image.layout_id(), baseline.layout_id());
        assert_eq!(image.encode_patches(&[]).unwrap(), image.raw());
        // Compare the two independent encoders' logical output, not compressed bytes.
        let rust = baseline
            .encode_patches(&[patch(&baseline, fields::HEAD, 0x261C_4A52)])
            .unwrap();
        assert_eq!(SaveImage::decode(rust).unwrap().payload(), image.payload());
    }
}
