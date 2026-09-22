"""Development-only bidirectional oracle for both observed save layouts.

From hd2-armor-desk, CI must run these steps in order:
    cargo test -p sav_codec --test oracle emit_rust_outputs_for_python_oracle
    python scripts/oracle_crosscheck.py target/oracle
    HD2_REQUIRE_PYTHON_ORACLE=1 cargo test -p sav_codec --test oracle python_repacked_bytes_decode_identically

The last line uses POSIX environment syntax. In PowerShell first set
$env:HD2_REQUIRE_PYTHON_ORACLE = '1', then run the cargo command. Presence of this
environment variable makes missing reverse-direction output a Rust test failure.
The Python reference is not a shipped runtime dependency.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO.parent / "reference" / "python"))

import hd2_core as core  # noqa: E402

BASELINES = {
    "observed_0106": "valid_baseline.bin",
    "observed_0107": "valid_new_baseline.bin",
}
CASES = {
    "head_armor": ((0x121, 0xD3461392),),
    "body_b01": ((0x129, 0x61B31723),),
    "head_and_body": ((0x121, 0x261C4A52), (0x129, 0x61B31723)),
}


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__)
        return 2
    out_dir = Path(argv[1])
    manifest_path = out_dir / "manifest.json"
    if not manifest_path.is_file():
        print(f"missing {manifest_path}; run the Rust emission step documented above")
        return 2
    record = json.loads(manifest_path.read_text(encoding="utf-8"))
    expected_entries = {
        f"rust_{layout}_{case}.bin": (layout, fixture, patches)
        for layout, fixture in BASELINES.items()
        for case, patches in CASES.items()
    }
    names = [entry["file"] for entry in record["outputs"]]
    if len(names) != len(expected_entries) or set(names) != set(expected_entries):
        print("FAIL manifest must contain exactly the three Rust edits for each layout")
        return 1
    failures = 0
    for entry in record["outputs"]:
        name = entry["file"]
        layout, fixture, patches = expected_entries[name]
        try:
            data = (out_dir / name).read_bytes()
            save = core.decode(data)
            baseline = core.decode((REPO / "tests" / "fixtures" / fixture).read_bytes())
            expected = bytearray(baseline.payload)
            for offset, value in patches:
                expected[offset:offset + 4] = value.to_bytes(4, "little")
            expected = core.refresh_inner_checksum(bytes(expected))
            problems = []
            if entry["layout"] != layout or entry["fixture"] != fixture:
                problems.append("layout/fixture metadata")
            if save.payload != expected:
                problems.append("independent expected payload (including preserved tail)")
            if core.sha256(save.payload) != entry["payload_sha256"]:
                problems.append("payload sha256")
            if core.sha256(data) != entry["raw_sha256"]:
                problems.append("raw sha256")
            if core.u32(save.payload, core.INNER_CHECKSUM_OFFSET) != entry["inner_low32"]:
                problems.append("inner low32")
            if core.u32(data, 0x0C) != entry["outer_crc32"]:
                problems.append("outer crc32")
            for field, offset in (("head", 0x121), ("body", 0x129), ("cape", 0x125)):
                if core.u32(save.payload, offset) != entry[f"{field}_id"]:
                    problems.append(f"{field} id")
            if save.compressed_blocks[1:] != baseline.compressed_blocks[1:]:
                problems.append("untouched compressed blocks")
            if problems:
                print(f"FAIL {name}: {', '.join(problems)}")
                failures += 1
            else:
                print(f"ok   {name}: payload, preserved blocks, hashes and checksums agree")
        except Exception as exc:  # noqa: BLE001 - report oracle rejection verbatim
            print(f"FAIL {name}: {exc}")
            failures += 1

    if failures:
        return 1
    for layout, fixture in BASELINES.items():
        original = (REPO / "tests" / "fixtures" / fixture).read_bytes()
        baseline = core.decode(original)
        payload = bytearray(baseline.payload)
        payload[0x121:0x125] = (0x261C4A52).to_bytes(4, "little")
        repacked = core.repack(original, bytes(payload))
        name = f"python_{layout}_repacked_head_b01.bin"
        (out_dir / name).write_bytes(repacked)
        print(f"wrote {name} for the required Rust reverse-direction step")
    print("all six Rust outputs agree with Python; run the required Rust reverse step next")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
