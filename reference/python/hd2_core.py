"""HD2 known-format SAV codec and local inspector model. No network/process access.

The payload offsets are experimentally established only for the known layout.
A valid checksum is not a guarantee of game-level semantics or compatibility.
"""
from __future__ import annotations
import csv
import hashlib
import json
import os
import struct
import tempfile
import time
import zlib
from dataclasses import dataclass
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
import lz4.block

BLOCK_SIZE = 65536
MAX_SIZE = 16 * 1024 * 1024
MAGIC = bytes.fromhex('d4fd1c6b5f7b23cc')

MASK64 = (1 << 64) - 1
MURMUR64_MULTIPLIER = 0xC6A4A7935BD1E995
INNER_CHECKSUM_OFFSET = 0x0C


def murmur64a(data: bytes, seed: int = 0) -> int:
    """Little-endian MurmurHash64A, matching the author's reference algorithm."""
    m = MURMUR64_MULTIPLIER
    h = (seed ^ (len(data) * m)) & MASK64
    end = len(data) - len(data) % 8
    for offset in range(0, end, 8):
        k = int.from_bytes(data[offset:offset + 8], 'little')
        k = (k * m) & MASK64
        k ^= k >> 47
        k = (k * m) & MASK64
        h ^= k
        h = (h * m) & MASK64
    if end != len(data):
        h ^= int.from_bytes(data[end:], 'little')
        h = (h * m) & MASK64
    h ^= h >> 47
    h = (h * m) & MASK64
    h ^= h >> 47
    return h


def inner_checksum(payload: bytes) -> int:
    """Observed rule: zero payload[12:16], hash full logical bytes, take low32."""
    if len(payload) < 16:
        raise ValueError('Inner header is truncated')
    temp = payload[:12] + bytes(4) + payload[16:]
    return murmur64a(temp, seed=0) & 0xFFFFFFFF


def refresh_inner_checksum(payload: bytes) -> bytes:
    result = bytearray(payload)
    struct.pack_into('<I', result, INNER_CHECKSUM_OFFSET, inner_checksum(payload))
    return bytes(result)


@dataclass(frozen=True)
class Save:
    payload: bytes
    compressed_blocks: tuple[bytes, ...]
    full_blocks: tuple[bytes, ...]


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def u32(data: bytes | bytearray, offset: int) -> int:
    return struct.unpack_from('<I', data, offset)[0]


def read_file(path: Path) -> bytes:
    if not path.is_file():
        raise ValueError(f'Not a file: {path}')
    if path.stat().st_size > MAX_SIZE:
        raise ValueError('Input exceeds this research tool size limit')
    return path.read_bytes()


def decode(data: bytes, *, verify_inner: bool = True) -> Save:
    """Strict decoder for the observed layout, not for every HD2 save version."""
    if len(data) < 40 or len(data) > MAX_SIZE:
        raise ValueError('Truncated or oversized container')
    if data[:8] != MAGIC:
        raise ValueError('Unrecognized outer magic')
    if u32(data, 8) != 1 or u32(data, 0x18) != 0xF0000001:
        raise ValueError('Unsupported flags/layout')
    if u32(data, 0x0C) != (zlib.crc32(data[0x10:]) & 0xFFFFFFFF):
        raise ValueError('Outer CRC32 mismatch')
    if u32(data, 0x10) != len(data) - 0x1C:
        raise ValueError('Stored compressed size mismatch')
    logical_size = u32(data, 0x14)
    if not 16 <= logical_size <= MAX_SIZE:
        raise ValueError('Invalid logical size')
    if struct.unpack_from('<Q', data, 0x1C)[0] != logical_size:
        raise ValueError('Logical length copy mismatch')
    block_count = (logical_size + BLOCK_SIZE - 1) // BLOCK_SIZE
    pos, compressed, full = 0x24, [], []
    for _ in range(block_count):
        if pos + 4 > len(data):
            raise ValueError('Missing compressed block length')
        length = u32(data, pos)
        pos += 4
        if length == 0 or pos + length > len(data):
            raise ValueError('Compressed block out of bounds')
        chunk = data[pos:pos + length]
        try:
            plain = lz4.block.decompress(chunk, uncompressed_size=BLOCK_SIZE)
        except lz4.block.LZ4BlockError as exc:
            raise ValueError('Invalid raw LZ4 block') from exc
        if len(plain) != BLOCK_SIZE:
            raise ValueError('Unexpected decompressed block length')
        compressed.append(chunk)
        full.append(plain)
        pos += length
    if pos != len(data):
        raise ValueError('Unparsed trailing bytes')
    padded = b''.join(full)
    if any(padded[logical_size:]):
        raise ValueError('Nonzero terminal padding')
    payload = padded[:logical_size]
    if u32(payload, 8) != logical_size:
        raise ValueError('Inner length does not match this sample layout')
    if verify_inner and u32(payload, INNER_CHECKSUM_OFFSET) != inner_checksum(payload):
        raise ValueError('Inner MurmurHash64A-low32 checksum mismatch')
    return Save(payload, tuple(compressed), tuple(full))


class LayoutId(Enum):
    OBSERVED_0106 = (bytes.fromhex('060100003eeacea6b8ba0800'), 572088)
    OBSERVED_0107 = (bytes.fromhex('07010000b687e357bcba0800'), 572092)

    @classmethod
    def recognize(cls, payload: bytes) -> LayoutId | None:
        for layout in cls:
            header, length = layout.value
            if len(payload) == length and payload[:12] == header:
                return layout
        return None


def repack(original: bytes, payload: bytes) -> bytes:
    """Equal-length edit; refresh both checks; keep each untouched block byte-identical."""
    old = decode(original)
    if len(payload) != len(old.payload):
        raise ValueError('不允许更改正文长度')
    payload = refresh_inner_checksum(payload)
    if payload == old.payload:
        return original
    padded = payload + b''.join(old.full_blocks)[len(payload):]
    blocks = []
    for i, before in enumerate(old.full_blocks):
        after = padded[i * BLOCK_SIZE:(i + 1) * BLOCK_SIZE]
        blocks.append(old.compressed_blocks[i] if before == after else
                      lz4.block.compress(after, mode='default', store_size=False))
    body = b''.join(struct.pack('<I', len(x)) + x for x in blocks)
    result = bytearray(original[:0x24]) + body
    struct.pack_into('<I', result, 0x10, len(result) - 0x1C)
    struct.pack_into('<I', result, 0x0C, zlib.crc32(result[0x10:]) & 0xFFFFFFFF)
    output = bytes(result)
    if decode(output).payload != payload:
        raise ValueError('重新封装后独立解压检查不一致')
    return output


def parse_u32(text: str) -> int:
    text = text.strip()
    n = int(text, 16 if text.lower().startswith('0x') else 10)
    if not 0 <= n <= 0xFFFFFFFF:
        raise ValueError('数值必须在 0 至 0xFFFFFFFF 之间')
    return n


def stamp() -> str:
    return datetime.now(timezone.utc).isoformat(timespec='milliseconds')


@dataclass(frozen=True)
class Snapshot:
    raw: bytes
    payload: bytes
    sha256: str
    captured_at: str
    path: str = ''

    @classmethod
    def from_bytes(cls, raw: bytes, path: str = '') -> Snapshot:
        return cls(raw, decode(raw).payload, sha256(raw), stamp(), path)

    @property
    def layout_id(self) -> LayoutId | None:
        return LayoutId.recognize(self.payload)

    @property
    def known_layout(self) -> bool:
        return self.layout_id is not None


@dataclass(frozen=True)
class Field:
    name: str
    offset: int
    confidence: str = 'custom'

    def value(self, payload: bytes) -> int | None:
        return u32(payload, self.offset) if 0 <= self.offset <= len(payload) - 4 else None


DEFAULT_FIELDS = [
    Field('头部槽', 0x121, '用户换装验证'),
    Field('身体护甲槽', 0x129, '样本及用户观察'),
    Field('披风槽', 0x125, '样本及用户观察'),
    Field('主武器槽', 0x11, '新样本装备 ID 匹配'),
    Field('副武器槽', 0x15, '新样本装备 ID 匹配'),
    Field('未知字段 0x0019', 0x19, '待逐项点击验证'),
    Field('战略配备记录 1', 0x1D, '历史数据匹配，当前用途待确认'),
    Field('战略配备记录 2', 0x25, '历史记录布局，当前用途待确认'),
    Field('战略配备记录 3', 0x2D, '历史数据匹配，当前用途待确认'),
    Field('战略配备记录 4', 0x35, '历史数据匹配，当前用途待确认'),
] + [Field(f'未知字段 0x{x:04X}', x, '待逐项点击验证') for x in
     [0x11D, 0x12D, 0x131, 0x135, 0x139, 0x141]]

# Only IDs that the user actually identified in the preceding game tests.
SEED_NAMES = {
    0x056848E9: 'FS-55 蹂躏者（头盔）',
    0x261C4A52: 'B-01 战术（头盔）',
    0xD3461392: 'FS-55 蹂躏者（身体护甲）',
    0x61B31723: 'B-01 战术（身体护甲）',
}


class Editor:
    def __init__(self, snapshot: Snapshot):
        self.original = snapshot
        self.patches: dict[int, bytes] = {}

    @property
    def dirty(self) -> bool:
        return bool(self.patches)

    def stage_u32(self, offset: int, value: int) -> None:
        if not self.original.known_layout:
            raise ValueError('未知布局：仅允许查看，不能按已知字段写入')
        if not 16 <= offset <= len(self.original.payload) - 4:
            raise ValueError('偏移越界，或试图编辑受保护的正文头部')
        if not 0 <= value <= 0xFFFFFFFF:
            raise ValueError('需要 uint32 数值')
        for off in self.patches:
            if off != offset and max(off, offset) < min(off + 4, offset + 4):
                raise ValueError('修改区域与另一待保存的修改重叠')
        patch = struct.pack('<I', value)
        if self.original.payload[offset:offset + 4] == patch:
            self.patches.pop(offset, None)
        else:
            self.patches[offset] = patch

    def view(self) -> bytes:
        result = bytearray(self.original.payload)
        for off, data in self.patches.items():
            result[off:off + 4] = data
        return bytes(result)

    def conflicts(self, disk: Snapshot) -> bool:
        return disk.sha256 != self.original.sha256

    def build(self) -> bytes:
        if not self.dirty:
            return self.original.raw
        return repack(self.original.raw, self.view())


def diff_ranges(before: bytes, after: bytes) -> list[tuple[int, int]]:
    """Half-open changed byte ranges, including differing tails."""
    ranges, start = [], None
    for i in range(max(len(before), len(after))):
        different = i >= len(before) or i >= len(after) or before[i] != after[i]
        if different and start is None:
            start = i
        elif not different and start is not None:
            ranges.append((start, i)); start = None
    if start is not None:
        ranges.append((start, max(len(before), len(after))))
    return ranges


class ConflictError(ValueError):
    pass


def stable_read(path: Path) -> bytes:
    """Check metadata around a bounded read; decoding validates its two checksums."""
    a = path.stat()
    if a.st_size > MAX_SIZE:
        raise ValueError('文件超过 16 MiB 上限')
    with path.open('rb') as handle:
        data = handle.read(MAX_SIZE + 1)
    b = path.stat()
    if len(data) > MAX_SIZE:
        raise ValueError('文件读取期间超过大小上限')
    if (a.st_size, a.st_mtime_ns, a.st_ino) != (b.st_size, b.st_mtime_ns, b.st_ino) or len(data) != b.st_size:
        raise ConflictError('文件正在变化，稍后重试')
    return data


class StabilityGate:
    def __init__(self, delay: float = 0.25):
        self.delay = delay
        self.signature = None
        self.since = 0.0

    def ready(self, signature: str, now: float) -> bool:
        if signature != self.signature:
            self.signature = signature; self.since = now
            return False
        return now - self.since >= self.delay


class StableReader:
    """Run poll on a worker thread. No Tk calls, network, or held game-file handle."""
    def __init__(self, delay: float = 0.25):
        self.gate = StabilityGate(delay)
        self.accepted = ''
        self.path = ''

    def poll(self, path: Path) -> tuple[str, object]:
        try:
            target = str(path.resolve())
            if target != self.path:
                self.path = target; self.accepted = ''; self.gate = StabilityGate(self.gate.delay)
            raw = stable_read(path)
            digest = sha256(raw)
            if digest == self.accepted:
                return 'unchanged', digest
            if not self.gate.ready(digest, time.monotonic()):
                return 'pending', '等待文件写入稳定'
            snapshot = Snapshot.from_bytes(raw, target)
            self.accepted = digest
            return 'snapshot', snapshot
        except (OSError, ValueError) as exc:
            return 'error', str(exc)


def save_new(path: Path, data: bytes) -> None:
    decode(data)
    # x-mode refuses any existing file: never silently replaces another save.
    with path.open('xb') as h:
        try:
            h.write(data); h.flush(); os.fsync(h.fileno())
        except BaseException:
            h.close(); path.unlink(missing_ok=True); raise


def save_checked(path: Path, data: bytes, expected_sha: str, backup_dir: Path) -> Path:
    """Best-effort disk conflict guard, NOT a lock coordinated with the game.

    Caller must have stopped external writers before source overwrite. Even two
    checks and os.replace cannot implement cross-process compare-and-swap.
    """
    decode(data)
    path = path.resolve()
    before = stable_read(path)
    if sha256(before) != expected_sha:
        raise ConflictError('源文件已被游戏或其他程序更新；拒绝覆盖。重新读取后再编辑。')
    backup_dir.mkdir(parents=True, exist_ok=True)
    backup = backup_dir / f'{path.stem}_{time.time_ns()}_{expected_sha[:10]}.sav'
    with backup.open('xb') as h:
        h.write(before); h.flush(); os.fsync(h.fileno())
    fd, name = tempfile.mkstemp(prefix=f'.{path.name}.hd2_', suffix='.tmp', dir=str(path.parent))
    temp = Path(name)
    try:
        with os.fdopen(fd, 'wb') as h:
            h.write(data); h.flush(); os.fsync(h.fileno())
        if sha256(stable_read(path)) != expected_sha:
            raise ConflictError('保存准备期间源文件再次变化；未覆盖，备份已保留。')
        try:
            os.chmod(temp, path.stat().st_mode & 0o777)
        except OSError:
            pass
        os.replace(temp, path)
        if sha256(stable_read(path)) != sha256(data):
            raise ConflictError('保存后又检测到外部更新，请检查备份；不要反复回写。')
        return backup
    finally:
        temp.unlink(missing_ok=True)


def atomic_json(path: Path, obj: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix='.' + path.name, suffix='.tmp', dir=str(path.parent))
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as h:
            json.dump(obj, h, ensure_ascii=False, indent=2)
            h.write('\n'); h.flush(); os.fsync(h.fileno())
        os.replace(name, path)
    finally:
        if os.path.exists(name):os.unlink(name)


class Catalog:
    """Auto-collected field/value observations; names are user annotations, not a DB."""
    def __init__(self, path: Path):
        self.path = path
        self.records: dict[str, dict] = {}
        if path.exists():
            try:
                obj = json.loads(path.read_text(encoding='utf-8'))
                if obj.get('schema') != 1 or not isinstance(obj['records'], list):
                    raise ValueError('ID表格式不正确')
                for r in obj['records']:
                    if not isinstance(r['offset'], int) or not isinstance(r['value'], int):
                        raise ValueError('ID表偏移或数值不是整数')
                    if not 16 <= r['offset'] <= MAX_SIZE-4 or not 0 <= r['value'] <= 0xFFFFFFFF:
                        raise ValueError('ID表偏移或数值越界')
                    if not all(isinstance(r.get(k,''),str) for k in ['name','note','field','first_seen','last_seen']):
                        raise ValueError('ID表文字字段损坏')
                    self.records[self.key(r['offset'], r['value'])] = r
            except (ValueError, KeyError, TypeError, AttributeError) as exc:
                raise ValueError(f'ID表无法读取；原文件未改动：{path}') from exc

    @staticmethod
    def key(offset: int, value: int) -> str:
        return f'{offset:08X}:{value:08X}'

    def observe(self, snapshot: Snapshot, fields: list[Field]) -> list[str]:
        if not snapshot.known_layout:
            return []
        new = []
        for field in fields:
            value = field.value(snapshot.payload)
            if value in (None, 0, 0xFFFFFFFF):
                continue
            key = self.key(field.offset, value)
            if key not in self.records:
                self.records[key] = {'offset': field.offset, 'value': value,
                    'field': field.name, 'name': SEED_NAMES.get(value, ''), 'note': '',
                    'first_seen': snapshot.captured_at, 'last_seen': snapshot.captured_at,
                    'count': 1, 'source_sha256': snapshot.sha256}
                new.append(key)
            else:
                r=self.records[key];r['last_seen']=snapshot.captured_at;r['count']=r.get('count',0)+1
                r['field']=field.name
        return new

    def label(self, key: str, name: str, note: str = '') -> None:
        self.records[key]['name'] = name.strip()
        self.records[key]['note'] = note.strip()

    def name(self, offset: int, value: int) -> str:
        r=self.records.get(self.key(offset,value))
        if r and r.get('name'):return r['name']
        for r in self.records.values():
            if r['value']==value and r.get('name'):return r['name']
        return SEED_NAMES.get(value,'')

    def save(self) -> None:
        atomic_json(self.path, {'schema': 1, 'records': list(self.records.values())})

    def export_csv(self, path: Path) -> None:
        def safe(text):
            s=str(text)
            return "'"+s if s.startswith(('=','+','-','@','\t','\r')) else s
        with path.open('w', encoding='utf-8-sig', newline='') as h:
            w=csv.writer(h);w.writerow(['字段','正文偏移','ID_十六进制','ID_十进制','名称','备注','首次采集_UTC','末次采集_UTC'])
            for r in self.records.values():
                w.writerow([safe(r['field']),f"0x{r['offset']:04X}",f"0x{r['value']:08X}",r['value'],
                            safe(r.get('name','')),safe(r.get('note','')),r['first_seen'],r['last_seen']])
