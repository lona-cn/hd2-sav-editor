from __future__ import annotations
import json, os, struct, tempfile, unittest, zlib
from pathlib import Path
import lz4.block
import hd2_core as c

# Intentionally synthetic: contains no user, account or game-session data.
def fixture(size=572088, header=None):
    p=bytearray(size)
    p[:8]=bytes.fromhex('060100003eeacea6')
    struct.pack_into('<I',p,8,size)
    if header is not None:p[:12]=header
    for off, value in [(0x11,0x11223344),(0x121,0x056848E9),(0x125,0x4657CFB3),(0x129,0xD3461392)]:
        struct.pack_into('<I',p,off,value)
    p[70000:70016]=bytes.fromhex('102132435465768798a9bacbdcedfe0f')
    p[-36:]=bytes(range(0xA1,0xC5))
    p=c.refresh_inner_checksum(bytes(p))
    chunks=[]
    for i in range(0,len(p),65536):
        b=lz4.block.compress(p[i:i+65536].ljust(65536,b'\0'),store_size=False)
        chunks.append(struct.pack('<I',len(b))+b)
    h=bytearray(36);h[:8]=c.MAGIC
    struct.pack_into('<I',h,8,1);struct.pack_into('<I',h,20,size)
    struct.pack_into('<I',h,24,0xF0000001);struct.pack_into('<Q',h,28,size)
    raw=h+b''.join(chunks)
    struct.pack_into('<I',raw,16,len(raw)-28)
    struct.pack_into('<I',raw,12,zlib.crc32(raw[16:])&0xffffffff)
    return bytes(raw)

class CodecTests(unittest.TestCase):
    def setUp(self):self.raw=fixture()
    def test_noop_is_byte_identical(self):
        self.assertEqual(c.repack(self.raw,c.decode(self.raw).payload),self.raw)
    def test_dual_checksum_roundtrip(self):
        p=bytearray(c.decode(self.raw).payload);struct.pack_into('<I',p,0x121,0xABCDEF01)
        raw=c.repack(self.raw,bytes(p)); q=c.decode(raw).payload
        self.assertEqual(c.u32(q,0x121),0xABCDEF01)
        self.assertEqual(c.u32(q,12),c.inner_checksum(q))
        self.assertEqual(c.u32(raw,12),zlib.crc32(raw[16:])&0xffffffff)
        self.assertEqual(c.decode(raw).compressed_blocks[1:],c.decode(self.raw).compressed_blocks[1:])
    def test_later_block_edit_preserves_untouched_blocks(self):
        p=bytearray(c.decode(self.raw).payload);p[70000:70004]=b'ABCD'
        raw=c.repack(self.raw,bytes(p));a,b=c.decode(self.raw),c.decode(raw)
        self.assertEqual(b.payload[70000:70004],b'ABCD')
        self.assertEqual(a.compressed_blocks[2:],b.compressed_blocks[2:])
    def test_bad_outer_rejected(self):
        b=bytearray(self.raw);b[-1]^=1
        with self.assertRaises(ValueError):c.decode(bytes(b))
    def test_bad_inner_rejected(self):
        b=bytearray(c.decode(self.raw).payload);b[12]^=1
        # Rebuild literal blocks without auto refreshing the hash.
        blocks=[]
        for i in range(0,len(b),65536):
            x=lz4.block.compress(bytes(b[i:i+65536]).ljust(65536,b'\0'),store_size=False)
            blocks.append(struct.pack('<I',len(x))+x)
        raw=bytearray(self.raw[:36])+b''.join(blocks)
        struct.pack_into('<I',raw,16,len(raw)-28);struct.pack_into('<I',raw,12,zlib.crc32(raw[16:])&0xffffffff)
        with self.assertRaisesRegex(ValueError,'Murmur'):c.decode(bytes(raw))
    def test_truncated(self):
        for n in [0,10,36,len(self.raw)-1]:
            with self.assertRaises(ValueError):c.decode(self.raw[:n])
    def test_changed_length_rejected(self):
        with self.assertRaises(ValueError):c.repack(self.raw,c.decode(self.raw).payload+b'x')
    def test_parse_integer(self):
        self.assertEqual(c.parse_u32('0xD3461392'),0xD3461392)
        self.assertEqual(c.parse_u32('3544585106'),3544585106)
        for s in ['-1','0x100000000','bad']:
            with self.assertRaises(ValueError):c.parse_u32(s)
    def test_header_readonly(self):
        e=c.Editor(c.Snapshot.from_bytes(self.raw))
        with self.assertRaises(ValueError):e.stage_u32(12,2)
    def test_editor_same_value_not_dirty(self):
        e=c.Editor(c.Snapshot.from_bytes(self.raw));e.stage_u32(0x121,0x056848E9)
        self.assertFalse(e.dirty)
    def test_editor_revert(self):
        e=c.Editor(c.Snapshot.from_bytes(self.raw));e.stage_u32(0x121,2);e.stage_u32(0x121,0x056848E9)
        self.assertFalse(e.dirty)
    def test_editor_overlap_rejected(self):
        e=c.Editor(c.Snapshot.from_bytes(self.raw));e.stage_u32(0x121,2)
        with self.assertRaises(ValueError):e.stage_u32(0x122,3)
    def test_editor_external_conflict_not_silently_merged(self):
        a=c.Snapshot.from_bytes(self.raw);e=c.Editor(a);e.stage_u32(0x121,12)
        p=bytearray(a.payload);p[7000]=2;b=c.Snapshot.from_bytes(c.repack(a.raw,bytes(p)))
        self.assertTrue(e.conflicts(b));self.assertEqual(e.original.sha256,a.sha256)
    def test_unknown_layout_readonly(self):
        p=bytearray(c.decode(self.raw).payload);p[0]=9
        a=c.Snapshot.from_bytes(c.repack(self.raw,bytes(p)))
        self.assertFalse(a.known_layout)
        with self.assertRaises(ValueError):c.Editor(a).stage_u32(0x121,3)
    def test_both_layouts_preserve_source_and_opaque_bytes_on_edit(self):
        for layout in c.LayoutId:
            with self.subTest(layout=layout):
                header,size=layout.value
                raw=fixture(size,header);before=c.decode(raw)
                snapshot=c.Snapshot.from_bytes(raw)
                self.assertEqual(snapshot.layout_id,layout)
                self.assertTrue(snapshot.known_layout)
                editor=c.Editor(snapshot)
                self.assertEqual(editor.build(),raw)
                expected=bytearray(snapshot.payload)
                for offset,value in [(0x121,0x9F73133E),(0x125,0x6E72F493),
                                     (0x129,0x5D0D8002),(0x11,0xAB1B4972),(0x15,0x335B8A1A)]:
                    editor.stage_u32(offset,value)
                    struct.pack_into('<I',expected,offset,value)
                result=c.decode(editor.build())
                self.assertEqual(result.payload,c.refresh_inner_checksum(bytes(expected)))
                self.assertEqual(result.compressed_blocks[1:],before.compressed_blocks[1:])
                self.assertEqual(result.full_blocks[-1],before.full_blocks[-1])
                self.assertEqual(snapshot.raw,raw)
                self.assertEqual(snapshot.payload,before.payload)
                self.assertEqual(c.Snapshot.from_bytes(editor.build()).layout_id,layout)
    def test_mismatched_header_and_length_stays_readonly(self):
        cases=[(572092,c.LayoutId.OBSERVED_0106.value[0]),
               (572088,c.LayoutId.OBSERVED_0107.value[0]),
               (572092,bytes.fromhex('08010000b687e357bcba0800'))]
        with tempfile.TemporaryDirectory() as d:
            catalog=c.Catalog(Path(d)/'catalog.json')
            for size,header in cases:
                with self.subTest(size=size,header=header):
                    self.assertIsNone(c.LayoutId.recognize(header+bytes(size-12)))
                    header=header[:8]+struct.pack('<I',size)
                    snapshot=c.Snapshot.from_bytes(fixture(size,header))
                    self.assertIsNone(snapshot.layout_id)
                    self.assertFalse(snapshot.known_layout)
                    editor=c.Editor(snapshot)
                    with self.assertRaisesRegex(ValueError,'未知布局'):
                        editor.stage_u32(0x121,3)
                    self.assertEqual(editor.build(),snapshot.raw)
                    self.assertEqual(catalog.observe(snapshot,c.DEFAULT_FIELDS),[])
            self.assertEqual(catalog.records,{})
    def test_catalog_observes_both_supported_layouts(self):
        with tempfile.TemporaryDirectory() as d:
            catalog=c.Catalog(Path(d)/'catalog.json')
            for layout in c.LayoutId:
                header,size=layout.value
                snapshot=c.Snapshot.from_bytes(fixture(size,header))
                catalog.observe(snapshot,[c.Field('head',0x121)])
            record=catalog.records[catalog.key(0x121,0x056848E9)]
            self.assertEqual(record['count'],2)
    def test_byte_diff_full_offsets(self):
        a=b'012345';b=b'01XX45'
        self.assertEqual(c.diff_ranges(a,b),[(2,4)])
        self.assertEqual(c.diff_ranges(a,a),[])
    def test_diff_size_mismatch(self):
        self.assertEqual(c.diff_ranges(b'ab',b'abc'),[(2,3)])

class MonitorTests(unittest.TestCase):
    def test_stability_gate(self):
        g=c.StabilityGate(0.25)
        self.assertFalse(g.ready('A',0));self.assertFalse(g.ready('A',0.1));self.assertTrue(g.ready('A',0.3))
        self.assertFalse(g.ready('B',0.4));self.assertTrue(g.ready('B',0.7))
    def test_new_data_restarts_gate(self):
        g=c.StabilityGate(0.25)
        self.assertFalse(g.ready('A',0));self.assertFalse(g.ready('B',0.2));self.assertFalse(g.ready('B',0.3))
        self.assertTrue(g.ready('B',0.5))
    def test_worker_ignores_partial_file_and_eventually_loads(self):
        import time
        with tempfile.TemporaryDirectory() as d:
            f=Path(d)/'x.sav';f.write_bytes(fixture()[:100]);m=c.StableReader(0)
            m.poll(f);bad=m.poll(f);self.assertEqual(bad[0],'error')
            f.write_bytes(fixture());m.poll(f);good=m.poll(f);self.assertEqual(good[0],'snapshot')
            self.assertEqual(m.poll(f)[0],'unchanged')
    def test_missing_file(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(c.StableReader().poll(Path(d)/'none')[0],'error')

class FileTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.d=Path(self.tmp.name)
        self.f=self.d/'a.sav';self.raw=fixture();self.f.write_bytes(self.raw)
    def test_save_as_refuses_existing(self):
        with self.assertRaises(FileExistsError):c.save_new(self.f,self.raw)
    def test_save_as(self):
        c.save_new(self.d/'new.sav',self.raw);self.assertEqual((self.d/'new.sav').read_bytes(),self.raw)
    def test_overwrite_with_backup(self):
        e=c.Editor(c.Snapshot.from_bytes(self.raw));e.stage_u32(0x121,4)
        out=e.build();backup=c.save_checked(self.f,out,c.sha256(self.raw),self.d/'backups')
        self.assertEqual(self.f.read_bytes(),out);self.assertEqual(backup.read_bytes(),self.raw)
    def test_save_conflict_refuses_and_preserves_disk(self):
        self.f.write_bytes(b'new data')
        with self.assertRaises(c.ConflictError):c.save_checked(self.f,self.raw,c.sha256(self.raw),self.d/'backups')
        self.assertEqual(self.f.read_bytes(),b'new data')
    def test_invalid_output_not_saved(self):
        with self.assertRaises(ValueError):c.save_new(self.d/'bad.sav',b'bad')
        self.assertFalse((self.d/'bad.sav').exists())
    def test_export_catalog_and_deduplicate(self):
        store=c.Catalog(self.d/'catalog.json')
        fields=[c.Field('身体护甲',0x129,'observed')];s=c.Snapshot.from_bytes(self.raw)
        store.observe(s,fields);store.observe(s,fields)
        self.assertEqual(len(store.records),1)
        key=next(iter(store.records));store.label(key,'测试甲','重甲')
        store.save();again=c.Catalog(self.d/'catalog.json')
        self.assertEqual(again.records[key]['name'],'测试甲')
        again.export_csv(self.d/'x.csv');self.assertIn('测试甲',(self.d/'x.csv').read_text(encoding='utf-8-sig'))
    def test_catalog_id_zero_and_all_ones_not_autocollected(self):
        st=c.Catalog(self.d/'cat.json');s=c.Snapshot.from_bytes(self.raw)
        st.observe(s,[c.Field('zero',0x100,'unknown')]);self.assertEqual(len(st.records),0)
    def test_catalog_name_separate_from_same_display_name(self):
        st=c.Catalog(self.d/'cat.json');s=c.Snapshot.from_bytes(self.raw)
        st.observe(s,[c.Field('头盔',0x121,'observed'),c.Field('护甲',0x129,'observed')]);self.assertEqual(len(st.records),2)
    def test_invalid_catalog_does_not_overwrite(self):
        f=self.d/'cat.json';f.write_text('{oops')
        with self.assertRaises(ValueError):c.Catalog(f)
        self.assertEqual(f.read_text(),'{oops')

if __name__=='__main__':unittest.main()
