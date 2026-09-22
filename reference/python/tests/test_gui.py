"""Event-loop tests: run with a display, e.g. xvfb-run -a python -m unittest discover -s tests."""
import json,os,struct,tempfile,time,unittest
from pathlib import Path
import tkinter as tk
from test_core import fixture
import hd2_core as c
import sav_gui

@unittest.skipUnless(os.environ.get('DISPLAY') or os.name=='nt','GUI requires display')
class GuiTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=tk.Tk();self.app=sav_gui.App(self.root,Path(self.temp.name),start_poll=False)
        self.addCleanup(self.app.close_for_test)
        self.raw=fixture();self.a=c.Snapshot.from_bytes(self.raw,'synthetic.sav')
        self.app.accept_snapshot(self.a)
        self.root.update()
    def test_window_has_four_tabs(self):self.assertEqual(len(self.app.tabs.tabs()),4)
    def test_save_buttons_visible_at_default_size(self):
        def walk(w):
            yield w
            for x in w.winfo_children():yield from walk(x)
        targets=[w for w in walk(self.root) if w.winfo_class()=='TButton' and '另存 SAV' in str(w.cget('text'))]
        self.assertEqual(len(targets),1)
        b=targets[0]
        self.assertTrue(b.winfo_ismapped())
        self.assertLessEqual(b.winfo_rooty()+b.winfo_height(),self.root.winfo_rooty()+self.root.winfo_height())

    def test_helmet_row(self):
        row=self.app.fields_tree.item('289','values')
        self.assertIn('0x056848E9',row)
    def test_non_sav_export_cannot_target_source(self):
        self.app.active_path=str(Path(self.temp.name)/'source.sav')
        with self.assertRaises(ValueError):self.app.ensure_not_source(self.app.active_path)
    def test_edit_row_visible_at_default_size(self):
        b=self.app.stage_button
        self.assertTrue(b.winfo_ismapped())
        self.assertLessEqual(b.winfo_rooty()+b.winfo_height(),self.app.field_tab.winfo_rooty()+self.app.field_tab.winfo_height())
    def test_catalog_records_real_read_not_staged(self):
        count=len(self.app.catalog.records)
        self.app.editor.stage_u32(0x121,0xAABBCCDD);self.app.refresh_views()
        self.assertEqual(len(self.app.catalog.records),count)
        self.assertFalse(any(r['value']==0xAABBCCDD for r in self.app.catalog.records.values()))
    def test_pending_edits_are_not_rebased(self):
        self.app.editor.stage_u32(0x121,2)
        p=bytearray(self.a.payload);struct.pack_into('<I',p,0x129,5)
        b=c.Snapshot.from_bytes(c.repack(self.raw,bytes(p)))
        self.app.accept_snapshot(b);self.root.update()
        self.assertEqual(self.app.editor.original.sha256,self.a.sha256)
        self.assertEqual(self.app.latest.sha256,b.sha256)
        self.assertIn('外部',self.app.edit_status.get())
    def test_clean_live_read_updates_display(self):
        p=bytearray(self.a.payload);struct.pack_into('<I',p,0x129,5)
        b=c.Snapshot.from_bytes(c.repack(self.raw,bytes(p)))
        self.app.accept_snapshot(b);self.root.update()
        self.assertEqual(self.app.editor.original.sha256,b.sha256)
        self.assertEqual(c.u32(self.app.editor.view(),0x129),5)
    def test_auto_save_id_table(self):
        self.assertTrue((Path(self.temp.name)/'catalog.json').is_file())
    def test_unknown_layout_no_autocapture(self):
        n=len(self.app.catalog.records)
        p=bytearray(self.a.payload);p[0]=8;struct.pack_into('<I',p,0x121,999)
        s=c.Snapshot.from_bytes(c.repack(self.raw,bytes(p)))
        self.app.accept_snapshot(s);self.root.update()
        self.assertEqual(len(self.app.catalog.records),n)
        self.assertIn('未知布局',self.app.file_status.get())
        self.app.edit_enabled.set(True);self.app.update_edit_status()
        self.assertTrue(self.app.stage_button.instate(['disabled']))
    def test_no_user_save_written_on_observation(self):
        self.assertEqual(list(Path(self.temp.name).glob('**/*.sav')),[])
    def test_new_layout_can_observe_and_stage_without_writing_source(self):
        header,size=c.LayoutId.OBSERVED_0107.value
        raw=fixture(size,header);source=Path(self.temp.name)/'new.sav';source.write_bytes(raw)
        snapshot=c.Snapshot.from_bytes(raw,str(source))
        self.app.accept_snapshot(snapshot)
        self.app.edit_enabled.set(True);self.app.toggle_edit();self.root.update()
        self.assertTrue(self.app.stage_button.instate(['!disabled']))
        self.assertEqual(self.app.fields_tree.item(str(0x121),'values')[0],'头部槽')
        self.assertIn(self.app.catalog.key(0x121,0x056848E9),self.app.catalog.records)
        self.app.editor.stage_u32(0x121,0x9F73133E);self.app.refresh_views()
        self.assertEqual(c.u32(c.decode(self.app.editor.build()).payload,0x121),0x9F73133E)
        self.assertEqual(source.read_bytes(),raw)


@unittest.skipUnless(os.environ.get('DISPLAY') or os.name=='nt','GUI requires display')
class SettingsTests(unittest.TestCase):
    def test_exact_legacy_defaults_migrate_without_rewriting_settings_on_load(self):
        fields=[
            {'name':'头盔槽','offset':0x121,'confidence':'用户换装验证'},
            {'name':'未知字段 0x0011','offset':0x11,'confidence':'待逐项点击验证'},
            {'name':'未知字段 0x0015','offset':0x15,'confidence':'待逐项点击验证'},
            {'name':'未知字段 0x0019','offset':0x19,'confidence':'待逐项点击验证'},
            {'name':'我的观察','offset':0x140,'confidence':'自定义证据'},
        ]
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'settings.json'
            settings={'last_path':'my-save.sav','poll_ms':'875','fields':fields,
                      'custom_option':{'keep':['unchanged']}}
            original=json.dumps(settings,ensure_ascii=False).encode('utf-8');path.write_bytes(original)
            root=tk.Tk();app=sav_gui.App(root,Path(d),start_poll=False)
            try:
                self.assertEqual([f.name for f in app.fields],
                                 ['头部槽','主武器槽','副武器槽','未知字段 0x0019','我的观察'])
                self.assertEqual(app.fields[1].confidence,'新样本装备 ID 匹配')
                self.assertEqual(app.fields[2].confidence,'新样本装备 ID 匹配')
                self.assertEqual(path.read_bytes(),original)
                app.persist_settings()
                saved=json.loads(path.read_text(encoding='utf-8'))
                for key in ['last_path','poll_ms','custom_option']:
                    self.assertEqual(saved[key],settings[key])
                self.assertEqual(saved['fields'][3:],fields[3:])
            finally:app.close_for_test()
    def test_custom_names_confidence_and_offsets_are_not_migrated(self):
        fields=[
            {'name':'我的头盔','offset':0x121,'confidence':'用户换装验证'},
            {'name':'未知字段 0x0011','offset':0x11,'confidence':'我的证据'},
            {'name':'未知字段 0x0015','offset':0x15,'confidence':'用户标注'},
            {'name':'头盔槽','offset':0x140,'confidence':'用户换装验证'},
        ]
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'settings.json';path.write_text(json.dumps({'fields':fields}),encoding='utf-8')
            root=tk.Tk();app=sav_gui.App(root,Path(d),start_poll=False)
            try:
                self.assertEqual(app.fields,[c.Field(**f) for f in fields])
                app.persist_settings()
                self.assertEqual(json.loads(path.read_text(encoding='utf-8'))['fields'],fields)
            finally:app.close_for_test()

if __name__=='__main__':unittest.main()

@unittest.skipUnless(os.environ.get('DISPLAY') or os.name=='nt','GUI requires display')
class PollTests(unittest.TestCase):
    def test_manual_refresh_when_monitor_paused(self):
        with tempfile.TemporaryDirectory() as d:
            root=tk.Tk();app=sav_gui.App(root,Path(d)/'ws',start_poll=False)
            try:
                f=Path(d)/'x.sav';raw=fixture();f.write_bytes(raw)
                app.active_path=str(f);app.path_var.set(str(f));app.accept_snapshot(c.Snapshot.from_bytes(raw,str(f)))
                app.monitor.set(False);app.poll_ms.set('100')
                p=bytearray(c.decode(raw).payload);struct.pack_into('<I',p,0x121,17)
                changed=c.repack(raw,bytes(p));f.write_bytes(changed)
                app.reload_now();app.tick()
                deadline=time.monotonic()+2
                while time.monotonic()<deadline and app.latest.sha256!=c.sha256(changed):
                    root.update();time.sleep(.01)
                self.assertEqual(app.latest.sha256,c.sha256(changed))
            finally:app.close_for_test()
    def test_monitor_reads_new_valid_file_after_partial_write(self):
        with tempfile.TemporaryDirectory() as d:
            root=tk.Tk();app=sav_gui.App(root,Path(d)/'ws',start_poll=False)
            try:
                f=Path(d)/'x.sav';raw=fixture();f.write_bytes(raw)
                app.poll_ms.set('100');app.open_path(str(f));app.tick()
                deadline=time.monotonic()+3
                while time.monotonic()<deadline and not app.latest:root.update();time.sleep(.01)
                self.assertIsNotNone(app.latest)
                f.write_bytes(raw[:50])
                for _ in range(50):root.update();time.sleep(.01)
                self.assertEqual(app.latest.sha256,c.sha256(raw))
                p=bytearray(c.decode(raw).payload);struct.pack_into('<I',p,0x121,18)
                changed=c.repack(raw,bytes(p));f.write_bytes(changed)
                deadline=time.monotonic()+3
                while time.monotonic()<deadline and app.latest.sha256!=c.sha256(changed):root.update();time.sleep(.01)
                self.assertEqual(app.latest.sha256,c.sha256(changed))
                self.assertIn(app.catalog.key(0x121,18),app.catalog.records)
            finally:app.close_for_test()
