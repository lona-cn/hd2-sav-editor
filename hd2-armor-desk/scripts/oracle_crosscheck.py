"""Development-only oracle: check that Rust codec output is readable by the Python
reference implementation, and vice versa.

The Python tool is NOT a runtime dependency of the shipped application; this script
only exists to prove cross-language agreement during development. Run:

    python scripts/oracle_crosscheck.py <rust-output-dir>

It reads the .bin files the Rust cross-check test writes into
`target/oracle/`, decodes each with `hd2_core`, and compares payload bytes and
both checksums against the values the Rust side recorded in `oracle/manifest.json`.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO.parent / "reference" / "python"))

import hd2_core as core  # noqa: E402


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__)
        return 2
    out_dir = Path(argv[1])
    manifest_path = out_dir / "manifest.json"
    if not manifest_path.is_file():
        print(f"missing {manifest_path}; run `cargo test -p sav_codec --test oracle` first")
        return 2

    record = json.loads(manifest_path.read_text(encoding="utf-8"))
    failures = 0
    for entry in record["outputs"]:
        name = entry["file"]
        data = (out_dir / name).read_bytes()
        try:
            save = core.decode(data)
        except Exception as exc:  # noqa: BLE001 - report any oracle rejection verbatim
            print(f"FAIL {name}: python decode rejected: {exc}")
            failures += 1
            continue

        problems = []
        if core.sha256(save.payload) != entry["payload_sha256"]:
            problems.append("payload sha256")
        if core.u32(save.payload, core.INNER_CHECKSUM_OFFSET) != entry["inner_low32"]:
            problems.append("inner low32")
        if core.u32(data, 0x0C) != entry["outer_crc32"]:
            problems.append("outer crc32")
        if core.u32(save.payload, 0x121) != entry["head_id"]:
            problems.append("head id")
        if core.u32(save.payload, 0x129) != entry["body_id"]:
            problems.append("body id")
        if core.u32(save.payload, 0x125) != entry["cape_id"]:
            problems.append("cape id")

        if problems:
            print(f"FAIL {name}: {', '.join(problems)}")
            failures += 1
        else:
            print(f"ok   {name}: python re-read agrees on payload and both checksums")

    # Reverse direction: Python-repacked bytes must be accepted by the Rust decoder.
    # The Rust test already asserts its own fixtures; here we emit a Python edit for
    # the Rust side to consume on its next run.
    baseline = core.decode((REPO / "tests" / "fixtures" / "valid_baseline.bin").read_bytes())
    body = bytearray(baseline.payload)
    body[0x121:0x125] = (0x261C4A52).to_bytes(4, "little")
    repacked = core.repack((REPO / "tests" / "fixtures" / "valid_baseline.bin").read_bytes(), bytes(body))
    (out_dir / "python_repacked_head_b01.bin").write_bytes(repacked)
    print("wrote python_repacked_head_b01.bin for the Rust decoder to consume")

    print(f"\n{'FAILURES: ' + str(failures) if failures else 'all Rust outputs agree with the Python oracle'}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
