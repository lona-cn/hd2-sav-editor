"""Regenerate only the synthetic 0107 baseline and its two manifest entries.

Run from any directory: python hd2-armor-desk/scripts/generate_new_fixture.py
No player save is read. Existing fixture bytes and manifest cases are preserved.
"""
from __future__ import annotations

import json
import struct
import sys
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO.parent / "reference" / "python"))

import hd2_core as core


def main() -> None:
    payload = bytearray(572092)
    payload[:12] = bytes.fromhex("07010000b687e357bcba0800")
    for offset, value in ((0x121, 0x9F73133E), (0x129, 0x5D0D8002),
                          (0x125, 0x6E72F493), (0x11, 0xAB1B4972),
                          (0x15, 0x335B8A1A)):
        struct.pack_into("<I", payload, offset, value)
    # Public, deterministic sentinels: later-block bytes, old boundary, extra four.
    payload[70000:70016] = bytes.fromhex("102132435465768798a9bacbdcedfe0f")
    payload[-36:] = bytes(range(0xA1, 0xC5))
    payload = core.refresh_inner_checksum(bytes(payload))
    padded = payload + bytes((-len(payload)) % core.BLOCK_SIZE)
    blocks = [core.lz4.block.compress(padded[i:i + core.BLOCK_SIZE],
                                     mode="default", store_size=False)
              for i in range(0, len(padded), core.BLOCK_SIZE)]
    raw = bytearray(core.MAGIC + struct.pack("<IIIIIQ", 1, 0, 0, len(payload),
                                           0xF0000001, len(payload)))
    raw.extend(b"".join(struct.pack("<I", len(block)) + block for block in blocks))
    struct.pack_into("<I", raw, 0x10, len(raw) - 0x1C)
    struct.pack_into("<I", raw, 0x0C, zlib.crc32(raw[0x10:]) & 0xFFFFFFFF)
    raw = bytes(raw)
    assert core.decode(raw).payload == payload
    entry = {
        "file": "valid_new_baseline.bin",
        "synthetic": True,
        "do_not_import_to_game": True,
        "purpose": "Synthetic 0107 baseline with later-block and nonzero tail sentinels; no account/session data",
        "expected": "valid",
        "raw_size": len(raw),
        "raw_sha256": core.sha256(raw),
        "payload_sha256": core.sha256(payload),
        "logical_size": len(payload),
        "inner_low32": f"0x{core.u32(payload, 12):08X}",
        "outer_crc32": f"0x{core.u32(raw, 12):08X}",
        "known_writable_layout": True,
        "head_id": "0x9F73133E",
        "body_id": "0x5D0D8002",
        "cape_id": "0x6E72F493",
        "primary_id": "0xAB1B4972",
        "secondary_id": "0x335B8A1A",
        "compressed_block_lengths": [len(block) for block in blocks],
        "changed_payload_ranges_vs_baseline": [],
    }
    for directory in (REPO.parent / "fixtures", REPO / "tests" / "fixtures"):
        (directory / entry["file"]).write_bytes(raw)
        manifest_path = directory / "manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["cases"] = [case for case in manifest["cases"] if case["file"] != entry["file"]]
        manifest["cases"].append(entry)
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(entry, indent=2))


if __name__ == "__main__":
    main()
