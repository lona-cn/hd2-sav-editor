#!/usr/bin/env python3
"""HD2 SAV Inspector: live local-file observation, ID catalog, checked editing.
Requires Python 3.10+, tkinter, lz4. Run: python sav_gui.py [path] [--workspace dir]
"""
from __future__ import annotations
import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import queue
import struct
import sys
import time
import traceback
import tkinter as tk
from tkinter import ttk, filedialog, messagebox, simpledialog

try:
    import hd2_core as core
except ImportError:
    root=tk.Tk();root.withdraw()
    messagebox.showerror('缺少依赖', '请运行 INSTALL.cmd，或：\npython -m pip install -r requirements.txt')
    root.destroy();raise SystemExit(1)

TITLE = 'HD2 SAV Inspector 1.0'
POLL_MS = 500
MAX_DIFF_ROWS = 2000
MAX_JOURNAL_ROWS = 600


def tree_frame(parent, columns, widths, labels, height=12):
    frame=ttk.Frame(parent)
    frame.rowconfigure(0,weight=1);frame.columnconfigure(0,weight=1)
    tree=ttk.Treeview(frame,columns=columns,show='headings',height=height,selectmode='browse')
    for name,width,label in zip(columns,widths,labels):
        tree.heading(name,text=label)
        tree.column(name,width=width,minwidth=60,anchor='w',stretch=True)
    y=ttk.Scrollbar(frame,orient='vertical',command=tree.yview)
    x=ttk.Scrollbar(frame,orient='horizontal',command=tree.xview)
    tree.configure(yscrollcommand=y.set,xscrollcommand=x.set)
    tree.grid(row=0,column=0,sticky='nsew');y.grid(row=0,column=1,sticky='ns');x.grid(row=1,column=0,sticky='ew')
    return frame,tree


def auto_paths():
    bases=[]
    if os.name=='nt':
        try:
            import winreg
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER,r'Software\Valve\Steam') as k:
                bases.append(Path(winreg.QueryValueEx(k,'SteamPath')[0]))
        except (OSError,ImportError):pass
        bases.extend([Path(os.environ.get('PROGRAMFILES(X86)',r'C:\Program Files (x86)'))/'Steam',
                      Path(os.environ.get('PROGRAMFILES',r'C:\Program Files'))/'Steam'])
    else:
        bases.extend([Path.home()/'.steam/steam',Path.home()/'.local/share/Steam'])
    paths=[]
    for base in bases:
        try: paths.extend((base/'userdata').glob('*/553850/remote/testament_new.sav'))
        except OSError:pass
    return sorted(set(str(p.resolve()) for p in paths if p.is_file()))


class App:
    def __init__(self, root: tk.Tk, workspace: Path, *, start_poll=True):
        self.root=root;self.workspace=workspace.resolve();self.workspace.mkdir(parents=True,exist_ok=True)
        self.settings_path=self.workspace/'settings.json'
        settings={}
        if self.settings_path.exists():
            try:settings=json.loads(self.settings_path.read_text(encoding='utf-8'))
            except (OSError,ValueError):pass
        self.fields=list(core.DEFAULT_FIELDS)
        if isinstance(settings.get('fields'),list):
            try:
                fs=[core.Field(str(f['name']),int(f['offset']),str(f.get('confidence','自定义'))) for f in settings['fields']]
                if fs and len(fs)<=256 and len({f.offset for f in fs})==len(fs) and all(16<=f.offset<=core.MAX_SIZE-4 for f in fs):
                    self.fields=fs
            except (ValueError,KeyError,TypeError):pass
        self.catalog=core.Catalog(self.workspace/'catalog.json')
        self.editor=None;self.latest=None;self.previous=None;self.baseline=None
        self.event_rows=[];self.last_error='';self.closed=False;self.busy=False
        self.active_path='';self.epoch=0;self.reader=core.StableReader();self.manual_pending=False
        self.pool=concurrent.futures.ThreadPoolExecutor(max_workers=1,thread_name_prefix='sav-reader')
        self.results=queue.Queue();self.after_id=None
        self.path_var=tk.StringVar(value=str(settings.get('last_path','')))
        self.monitor=tk.BooleanVar(value=True)
        self.poll_ms=tk.StringVar(value=str(settings.get('poll_ms',500)))
        self.file_status=tk.StringVar(value='打开 testament_new.sav 后即可自动采集。')
        self.edit_status=tk.StringVar(value='只读采集模式：不会写入游戏存档。')
        self.bottom_status=tk.StringVar(value=f'工作区：{self.workspace}')
        self.edit_enabled=tk.BooleanVar(value=False)
        self.topmost=tk.BooleanVar(value=False)
        self.only_changed=tk.BooleanVar(value=False)
        self.new_value=tk.StringVar()
        self.selected_text=tk.StringVar(value='选择一行，复制 ID 或双击命名。')
        self.catalog_filter=tk.StringVar()
        self.diff_mode=tk.StringVar(value='上一份有效快照')
        self.diff_start=tk.StringVar(value='0x10');self.diff_end=tk.StringVar(value='0x400')
        self.diff_status=tk.StringVar(value='等待第一次变化；默认只看正文 0x10～0x400，避免校验和遥测噪声。')
        self.hex_offset=tk.StringVar(value='0x100')
        self.hex_find=tk.StringVar();self.hex_search_kind=tk.StringVar(value='uint32 ID')
        self.root.title(TITLE);self.root.geometry('1240x840');self.root.minsize(1000,680)
        self._build_ui()
        self.root.protocol('WM_DELETE_WINDOW',self.close)
        self.root.bind('<Control-o>',lambda e:self.choose_file())
        self.root.bind('<Control-s>',lambda e:self.save_as())
        self.root.bind('<F5>',lambda e:self.reload_now())
        self.root.bind('<Control-f>',lambda e:self.focus_search())
        self.catalog_filter.trace_add('write',lambda *_:self.refresh_catalog())
        self.refresh_catalog()
        if start_poll:
            self.after_id=self.root.after(100,self.tick)
            if self.path_var.get() and Path(self.path_var.get()).is_file():
                self.open_path(self.path_var.get())

    def _build_ui(self):
        style=ttk.Style(self.root)
        if 'vista' in style.theme_names():style.theme_use('vista')
        elif 'clam' in style.theme_names():style.theme_use('clam')
        font=('Microsoft YaHei UI',10) if os.name=='nt' else ('Noto Sans CJK SC',10)
        self.root.option_add('*Font',font)
        style.configure('Treeview',rowheight=27,font=font)
        style.configure('Treeview.Heading',font=(font[0],10,'bold'))
        style.configure('Title.TLabel',font=(font[0],16,'bold'))
        outer=ttk.Frame(self.root,padding=12);outer.pack(fill='both',expand=True)
        top=ttk.Frame(outer);top.pack(fill='x')
        ttk.Label(top,text='HD2 · 存档观察器',style='Title.TLabel').pack(side='left')
        ttk.Checkbutton(top,text='窗口置顶',variable=self.topmost,command=lambda:self.root.attributes('-topmost',self.topmost.get())).pack(side='right')
        ttk.Label(outer,text='游戏内逐项装备 → 自动读取 → 收集 ID → 手动命名。所有偏移均为解压正文偏移。').pack(anchor='w',pady=(3,8))
        row=ttk.Frame(outer);row.pack(fill='x')
        entry=ttk.Entry(row,textvariable=self.path_var);entry.pack(side='left',fill='x',expand=True)
        entry.bind('<Return>',lambda e:self.open_path(self.path_var.get()))
        for label,cmd in [('浏览…',self.choose_file),('查找 Steam',self.find_steam),('读取 / F5',self.reload_now)]:
            ttk.Button(row,text=label,command=cmd).pack(side='left',padx=(5,0))
        row=ttk.Frame(outer);row.pack(fill='x',pady=7)
        ttk.Checkbutton(row,text='自动监视（只读）',variable=self.monitor).pack(side='left')
        ttk.Label(row,text='检查间隔 ms').pack(side='left',padx=(10,4))
        ttk.Combobox(row,textvariable=self.poll_ms,values=['200','500','1000','2000'],width=6,state='readonly').pack(side='left')
        ttk.Button(row,text='保存当前磁盘快照…',command=self.save_snapshot).pack(side='left',padx=8)
        ttk.Button(row,text='设置比较基线',command=self.pin_baseline).pack(side='left')
        ttk.Button(row,text='打开工作区',command=self.open_workspace).pack(side='right')
        ttk.Label(outer,textvariable=self.file_status,wraplength=1160).pack(anchor='w',fill='x')
        footer=ttk.Frame(outer)
        footer.pack(side='bottom',fill='x')
        ttk.Separator(footer).pack(fill='x')
        row=ttk.Frame(footer);row.pack(fill='x',pady=7)
        ttk.Checkbutton(row,text='开启编辑',variable=self.edit_enabled,command=self.toggle_edit).pack(side='left')
        ttk.Button(row,text='另存 SAV… / Ctrl+S',command=self.save_as).pack(side='right')
        ttk.Button(row,text='保存到源文件…',command=self.save_source).pack(side='right',padx=5)
        ttk.Button(row,text='放弃编辑 / 跟随磁盘',command=self.discard_edits).pack(side='right',padx=5)
        ttk.Label(footer,textvariable=self.edit_status,wraplength=1160).pack(anchor='w',fill='x')
        ttk.Label(footer,textvariable=self.bottom_status,wraplength=1160).pack(anchor='w',fill='x',pady=(3,0))
        self.tabs=ttk.Notebook(outer);self.tabs.pack(fill='both',expand=True,pady=8)
        self.field_tab=ttk.Frame(self.tabs,padding=8);self.diff_tab=ttk.Frame(self.tabs,padding=8)
        self.catalog_tab=ttk.Frame(self.tabs,padding=8);self.hex_tab=ttk.Frame(self.tabs,padding=8)
        for frame,label in [(self.field_tab,'装备 / 观察字段'),(self.diff_tab,'前后差异'),(self.catalog_tab,'已采集 ID'),(self.hex_tab,'原始正文 / HEX')]:
            self.tabs.add(frame,text=label)
        self._build_fields();self._build_diff();self._build_catalog();self._build_hex()

    def _build_fields(self):
        row=ttk.Frame(self.field_tab);row.pack(fill='x',pady=(0,7))
        ttk.Button(row,text='新增观察字段…',command=self.add_field).pack(side='left')
        ttk.Button(row,text='重命名字段…',command=self.rename_field).pack(side='left',padx=4)
        ttk.Button(row,text='移除字段',command=self.remove_field).pack(side='left')
        ttk.Checkbutton(row,text='只看有变化的字段',variable=self.only_changed,command=self.refresh_fields).pack(side='left',padx=12)
        fieldfooter=ttk.Frame(self.field_tab);fieldfooter.pack(side='bottom',fill='x')
        frame,self.fields_tree=tree_frame(self.field_tab,
            ['field','offset','hex','decimal','previous','name','status'],[170,90,115,115,115,245,160],
            ['字段 / 槽位','正文偏移','当前 uint32 HEX','当前十进制','上一份值','ID 名称','验证 / 状态'])
        self.fields_tree.tag_configure('changed',background='#FFF2CC')
        self.fields_tree.tag_configure('edited',background='#E1ECFA')
        self.fields_tree.bind('<<TreeviewSelect>>',self.field_selected)
        self.fields_tree.bind('<Double-1>',lambda e:self.label_selected_field())
        row=ttk.Frame(fieldfooter);row.pack(fill='x',pady=7)
        ttk.Label(row,textvariable=self.selected_text).pack(side='left')
        ttk.Button(row,text='复制 HEX',command=lambda:self.copy_field(False)).pack(side='right')
        ttk.Button(row,text='复制十进制',command=lambda:self.copy_field(True)).pack(side='right',padx=5)
        ttk.Button(row,text='命名此 ID…',command=self.label_selected_field).pack(side='right')
        row=ttk.Frame(fieldfooter);row.pack(fill='x')
        ttk.Label(row,text='新 uint32（0x 十六进制 / 十进制）').pack(side='left')
        self.value_entry=ttk.Entry(row,textvariable=self.new_value,width=20);self.value_entry.pack(side='left',padx=6)
        self.stage_button=ttk.Button(row,text='暂存到内存（不写盘）',command=self.stage_selected)
        self.stage_button.pack(side='left');self.stage_button.state(['disabled'])
        ttk.Label(fieldfooter,text='蓝色＝待保存修改；黄色＝磁盘读取变化。').pack(anchor='w',pady=(4,0))
        frame.pack(fill='both',expand=True)

    def _build_diff(self):
        row=ttk.Frame(self.diff_tab);row.pack(fill='x',pady=(0,6))
        cb=ttk.Combobox(row,textvariable=self.diff_mode,values=['上一份有效快照','固定比较基线'],width=19,state='readonly')
        cb.pack(side='left');cb.bind('<<ComboboxSelected>>',lambda e:self.refresh_diff())
        ttk.Label(row,text='正文范围').pack(side='left',padx=(10,3))
        ttk.Entry(row,textvariable=self.diff_start,width=10).pack(side='left')
        ttk.Label(row,text=' 至 ').pack(side='left')
        ttk.Entry(row,textvariable=self.diff_end,width=10).pack(side='left')
        ttk.Button(row,text='刷新范围',command=self.refresh_diff).pack(side='left',padx=5)
        ttk.Button(row,text='查看全文范围',command=self.full_diff).pack(side='left')
        ttk.Button(row,text='加入观察字段…',command=self.add_diff_field).pack(side='right')
        ttk.Label(self.diff_tab,textvariable=self.diff_status,wraplength=1120).pack(anchor='w',pady=4)
        panes=ttk.Panedwindow(self.diff_tab,orient='vertical');panes.pack(fill='both',expand=True)
        frame,self.diff_tree=tree_frame(panes,['start','length','before','after','u32'],[95,65,310,310,135],
            ['变化起点','字节数','之前 HEX（最多32字节）','之后 HEX（最多32字节）','起点 uint32 LE'],height=8)
        panes.add(frame,weight=3)
        eventframe=ttk.Frame(panes)
        ttk.Label(eventframe,text='采集事件（最新在前；只记录观察字段，不把改动中的校验值当成装备）').pack(anchor='w')
        f,self.events_tree=tree_frame(eventframe,['time','field','before','after','name'],[115,190,130,130,240],
            ['本地时间','字段','之前 ID','之后 ID','名称'],height=5)
        f.pack(fill='both',expand=True);panes.add(eventframe,weight=2)

    def _build_catalog(self):
        row=ttk.Frame(self.catalog_tab);row.pack(fill='x',pady=(0,7))
        ttk.Label(row,text='过滤名称 / ID / 字段').pack(side='left')
        self.catalog_entry=ttk.Entry(row,textvariable=self.catalog_filter,width=34);self.catalog_entry.pack(side='left',padx=6)
        ttk.Button(row,text='命名 / 备注…',command=self.label_catalog).pack(side='left')
        ttk.Button(row,text='复制 ID',command=self.copy_catalog).pack(side='left',padx=5)
        ttk.Button(row,text='导出 CSV…',command=self.export_csv).pack(side='right')
        ttk.Button(row,text='导出 JSON…',command=self.export_catalog).pack(side='right',padx=5)
        ttk.Label(self.catalog_tab,text='每次有效读取自动保存到 workspace/catalog.json。双击命名；字段不同或 ID 不同均分别记录。').pack(anchor='w',pady=(0,5))
        frame,self.catalog_tree=tree_frame(self.catalog_tab,
            ['field','offset','hex','decimal','name','note','last'],[150,85,115,115,220,210,170],
            ['来源字段','正文偏移','ID HEX','ID 十进制','名称（手动/已知）','备注','末次采集 UTC'])
        frame.pack(fill='both',expand=True)
        self.catalog_tree.bind('<Double-1>',lambda e:self.label_catalog())

    def _build_hex(self):
        row=ttk.Frame(self.hex_tab);row.pack(fill='x',pady=(0,7))
        ttk.Label(row,text='跳转正文偏移').pack(side='left')
        ttk.Entry(row,textvariable=self.hex_offset,width=12).pack(side='left',padx=4)
        ttk.Button(row,text='跳转',command=self.refresh_hex).pack(side='left')
        ttk.Button(row,text='上一页',command=lambda:self.hex_page(-1)).pack(side='left',padx=4)
        ttk.Button(row,text='下一页',command=lambda:self.hex_page(1)).pack(side='left')
        ttk.Button(row,text='此偏移编辑 uint32…',command=self.edit_hex_value).pack(side='left',padx=8)
        ttk.Button(row,text='导出解压正文…',command=self.export_payload).pack(side='right')
        row=ttk.Frame(self.hex_tab);row.pack(fill='x',pady=(0,6))
        ttk.Combobox(row,textvariable=self.hex_search_kind,values=['uint32 ID','HEX 字节','UTF-8 文本'],width=12,state='readonly').pack(side='left')
        ttk.Entry(row,textvariable=self.hex_find,width=37).pack(side='left',padx=6)
        ttk.Button(row,text='搜索（下一个）',command=self.find_hex).pack(side='left')
        ttk.Label(row,text='只读 HEX 视图；每页 512 字节；蓝色标记待保存修改。').pack(side='right')
        f=ttk.Frame(self.hex_tab);f.pack(fill='both',expand=True)
        self.hex_text=tk.Text(f,wrap='none',font=('Consolas',11) if os.name=='nt' else ('monospace',11),state='disabled')
        y=ttk.Scrollbar(f,orient='vertical',command=self.hex_text.yview)
        x=ttk.Scrollbar(f,orient='horizontal',command=self.hex_text.xview)
        self.hex_text.configure(yscrollcommand=y.set,xscrollcommand=x.set)
        f.rowconfigure(0,weight=1);f.columnconfigure(0,weight=1)
        self.hex_text.grid(row=0,column=0,sticky='nsew');y.grid(row=0,column=1,sticky='ns');x.grid(row=1,column=0,sticky='ew')
        self.hex_text.tag_configure('edited',background='#D7E8FA')
        ttk.Label(self.hex_tab,text='正文含其他账户/会话状态。这里只做本地显示，导出的完整 .bin / .sav 不宜公开。').pack(anchor='w',pady=6)

    def persist_settings(self):
        try:core.atomic_json(self.settings_path,{'last_path':self.active_path or self.path_var.get(),'poll_ms':self.poll_ms.get(),
                'fields':[{'name':f.name,'offset':f.offset,'confidence':f.confidence} for f in self.fields]})
        except OSError as exc:self.bottom_status.set(f'配置保存失败：{exc}')

    def choose_file(self):
        path=filedialog.askopenfilename(title='打开 HD2 存档',filetypes=[('SAV','*.sav'),('全部文件','*.*')])
        if path:self.open_path(path)

    def find_steam(self):
        paths=auto_paths()
        if not paths:
            messagebox.showinfo('未找到','未在常见 Steam 目录找到存档，请使用“浏览”选择。');return
        if len(paths)==1:self.open_path(paths[0]);return
        dlg=tk.Toplevel(self.root);dlg.title('选择账户存档');dlg.geometry('950x280');dlg.transient(self.root)
        box=tk.Listbox(dlg);box.pack(fill='both',expand=True,padx=8,pady=8)
        for p in paths:box.insert('end',p)
        def pick():
            sel=box.curselection()
            if sel:self.open_path(paths[sel[0]]);dlg.destroy()
        ttk.Button(dlg,text='打开所选',command=pick).pack(pady=5)
        box.bind('<Double-1>',lambda e:pick())

    def open_path(self,path):
        if not path:return
        if self.editor and self.editor.dirty:
            if not messagebox.askyesno('尚有编辑','打开文件将放弃未保存的修改，继续？'):return
        self.active_path=str(Path(path).expanduser().resolve());self.path_var.set(self.active_path)
        self.epoch+=1;self.reader=core.StableReader();self.manual_pending=True;self.editor=None;self.latest=None;self.previous=None;self.baseline=None
        self.event_rows=[];self.last_error='';self.edit_enabled.set(False)
        self.file_status.set('正在等待文件稳定并校验…');self.edit_status.set('只读采集模式')
        self.persist_settings();self.refresh_views();self.request_read()

    def reload_now(self):
        typed=self.path_var.get().strip()
        if typed and (not self.active_path or str(Path(typed).resolve())!=self.active_path):
            self.open_path(typed);return
        if self.editor and self.editor.dirty:
            if not messagebox.askyesno('重新读取','放弃待保存修改并读取最新磁盘数据？'):return
            self.editor=core.Editor(self.latest) if self.latest else None
        self.reader=core.StableReader(0);self.last_error='';self.manual_pending=True;self.request_read()

    def request_read(self):
        if self.closed or self.busy or not self.active_path:return
        self.busy=True;epoch=self.epoch;reader=self.reader;path=Path(self.active_path)
        future=self.pool.submit(reader.poll,path)
        def done(f):
            try:answer=f.result()
            except BaseException as exc:answer=('error',str(exc))
            self.results.put((epoch,answer))
        future.add_done_callback(done)

    def tick(self):
        if self.closed:return
        while True:
            try:epoch,(kind,data)=self.results.get_nowait()
            except queue.Empty:break
            self.busy=False
            if epoch!=self.epoch:continue
            if kind=='snapshot':
                self.manual_pending=False;self.last_error='';self.accept_snapshot(data)
            elif kind=='error':
                self.manual_pending=False;self.last_error=str(data)
                self.file_status.set(f'等待可读的完整存档：{data}（保留上一份有效快照，不采集错误值）')
            elif kind=='unchanged':
                self.manual_pending=False
                if self.last_error and self.latest:
                    self.last_error=''
                    self.file_status.set('文件已恢复，与上一份通过双层校验的有效快照相同。')
            elif kind=='pending':
                self.manual_pending=True
                self.bottom_status.set('检测到新内容，等待稳定后校验…')
        if self.monitor.get() or self.latest is None or self.manual_pending:self.request_read()
        try:interval=max(100,min(int(self.poll_ms.get()),5000))
        except ValueError:interval=POLL_MS
        self.after_id=self.root.after(interval,self.tick)

    def accept_snapshot(self,snapshot):
        if self.latest and self.latest.sha256==snapshot.sha256:return
        previous=self.latest
        self.previous=previous;self.latest=snapshot
        if self.baseline is None:self.baseline=snapshot
        frozen=bool(self.editor and self.editor.dirty)
        if not frozen:self.editor=core.Editor(snapshot)
        if snapshot.known_layout:
            fresh=self.catalog.observe(snapshot,self.fields)
            storage_error=''
            try:self.catalog.save()
            except OSError as exc:storage_error=f'ID表保存失败：{exc}'
            rows=[]
            for f in self.fields:
                old=f.value(previous.payload) if previous and previous.known_layout else None
                new=f.value(snapshot.payload)
                if old!=new and new is not None:
                    row={'time':time.strftime('%H:%M:%S'),'field':f.name,'offset':f.offset,
                        'before':old,'after':new,'name':self.catalog.name(f.offset,new),'captured_at':snapshot.captured_at,
                        'source_sha256':snapshot.sha256}
                    rows.append(row)
            self.event_rows=(rows+self.event_rows)[:MAX_JOURNAL_ROWS]
            if rows:
                try:
                    with (self.workspace/'observations.jsonl').open('a',encoding='utf-8') as h:
                        for r in rows:h.write(json.dumps(r,ensure_ascii=False)+'\n')
                except OSError as exc:storage_error+=f' 事件日志写入失败：{exc}'
            self.bottom_status.set(storage_error or f'采集完成 {time.strftime("%H:%M:%S")} · 新 ID {len(fresh)} · 总记录 {len(self.catalog.records)} · 未写入源存档')
        else:
            self.bottom_status.set('未知布局：显示原始字节与差异；禁用已知偏移编辑和自动 ID 分类。')
        self.file_status.set(f'{"已知布局" if snapshot.known_layout else "未知布局（只读）"} · '
            f'文件 {len(snapshot.raw):,} B / 正文 {len(snapshot.payload):,} B · 外层 CRC + 内层 Murmur 校验通过 · '
            f'SHA256 {snapshot.sha256[:16]}…')
        self.refresh_views()

    def refresh_views(self):
        self.refresh_fields();self.refresh_diff();self.refresh_catalog();self.refresh_hex();self.update_edit_status()

    def update_edit_status(self):
        if self.editor and self.editor.dirty:
            if self.latest and self.editor.conflicts(self.latest):
                self.edit_status.set(f'外部文件已更新！保留 {len(self.editor.patches)} 项旧快照编辑；禁止覆盖源文件。')
            else:self.edit_status.set(f'内存中有 {len(self.editor.patches)} 项修改，尚未写盘。')
        else:self.edit_status.set('允许暂存编辑；监视仍只读。' if self.edit_enabled.get() else '只读采集模式：不会写入游戏存档。')
        self.stage_button.state(['!disabled'] if self.edit_enabled.get() and self.editor and self.editor.original.known_layout else ['disabled'])

    def refresh_fields(self):
        selected=self.fields_tree.selection();self.fields_tree.delete(*self.fields_tree.get_children())
        if not self.editor:return
        p=self.editor.view();prev=self.editor.original.payload if self.editor.dirty else self.previous.payload if self.previous else None
        self.fields_tree.heading('previous',text='编辑基线值' if self.editor.dirty else '上一份值')
        known=self.editor.original.known_layout
        for f in self.fields:
            value=f.value(p);old=f.value(prev) if prev else None
            changed=old is not None and old!=value
            edited=f.offset in self.editor.patches
            if self.only_changed.get() and not changed and not edited:continue
            if value is None:continue
            name=self.catalog.name(f.offset,value) if known else ''
            tags=('edited',) if edited else ('changed',) if changed else ()
            self.fields_tree.insert('','end',iid=str(f.offset),values=(f.name if known else f'未解释 0x{f.offset:X}',
                f'0x{f.offset:04X}',f'0x{value:08X}',str(value),f'0x{old:08X}' if old is not None else '—',name,
                '待保存修改' if edited else f.confidence if known else '未知布局'),tags=tags)
        if selected and self.fields_tree.exists(selected[0]):self.fields_tree.selection_set(selected[0])

    def get_selected_field(self):
        sel=self.fields_tree.selection()
        if not sel:return None
        off=int(sel[0]);return next((f for f in self.fields if f.offset==off),None)

    def field_selected(self,event=None):
        f=self.get_selected_field()
        if f and self.editor:
            value=f.value(self.editor.view());self.selected_text.set(f'{f.name}  @ 0x{f.offset:04X}')
            self.new_value.set(f'0x{value:08X}')

    def copy_field(self,decimal=False):
        f=self.get_selected_field()
        if f and self.editor:
            n=f.value(self.editor.view());self.copy(str(n) if decimal else f'0x{n:08X}')

    def copy(self,text):
        self.root.clipboard_clear();self.root.clipboard_append(text);self.bottom_status.set('已复制：'+text)

    def toggle_edit(self):
        if self.edit_enabled.get() and (not self.editor or not self.editor.original.known_layout):
            self.edit_enabled.set(False);messagebox.showwarning('只读','请先打开已验证布局的有效存档。')
        self.update_edit_status()

    def stage_selected(self):
        f=self.get_selected_field()
        if not f:return
        self.stage_value(f.offset,self.new_value.get())

    def stage_value(self,off,text):
        if not self.edit_enabled.get() or not self.editor:
            messagebox.showinfo('只读模式','先勾选“开启编辑”。采集 ID 本身不需要开启编辑。');return
        try:self.editor.stage_u32(off,core.parse_u32(text))
        except ValueError as exc:messagebox.showerror('不能编辑',str(exc));return
        self.refresh_views()

    def discard_edits(self):
        if self.editor and self.editor.dirty:
            if not messagebox.askyesno('放弃编辑','放弃所有未保存的内存修改，跟随最新磁盘快照？'):return
        if self.latest:self.editor=core.Editor(self.latest)
        self.refresh_views()

    def add_field(self,initial='0x0011'):
        text=simpledialog.askstring('观察字段','输入解压正文 uint32 起始偏移（0x 十六进制或十进制）',initialvalue=initial,parent=self.root)
        if text is None:return
        try:off=core.parse_u32(text)
        except ValueError as exc:messagebox.showerror('偏移错误',str(exc));return
        if not 16<=off<=core.MAX_SIZE-4 or (self.latest and off>len(self.latest.payload)-4):
            messagebox.showerror('偏移错误','正文头部受保护或偏移超出文件。');return
        if any(f.offset==off for f in self.fields):
            messagebox.showinfo('已存在','这个偏移已经在观察列表中。');return
        name=simpledialog.askstring('字段名称','给字段命名；未知用途请保留“待确认”',initialvalue=f'待确认 0x{off:04X}',parent=self.root)
        if not name:return
        self.fields.append(core.Field(name,off,'用户自定义，语义待确认'))
        if self.latest:
            self.catalog.observe(self.latest,self.fields);self.catalog.save()
        self.persist_settings();self.refresh_views();self.tabs.select(self.field_tab)

    def rename_field(self):
        f=self.get_selected_field()
        if not f:return
        name=simpledialog.askstring('重命名字段','确认用途后填写名称（不修改 SAV）',initialvalue=f.name,parent=self.root)
        if not name:return
        self.fields=[core.Field(name,x.offset,'用户标注') if x.offset==f.offset else x for x in self.fields]
        for r in self.catalog.records.values():
            if r['offset']==f.offset:r['field']=name
        self.catalog.save();self.persist_settings();self.refresh_views()

    def remove_field(self):
        f=self.get_selected_field()
        if not f:return
        if not messagebox.askyesno('移除观察字段','仅移除观察字段；已采集 ID 和游戏存档均不删除。'):return
        self.fields=[x for x in self.fields if x.offset!=f.offset];self.persist_settings();self.refresh_views()

    def label_selected_field(self):
        f=self.get_selected_field()
        if not f or not self.editor:return
        value=f.value(self.editor.view());key=self.catalog.key(f.offset,value)
        if key not in self.catalog.records:
            messagebox.showinfo('尚未采集','此值尚未由有效磁盘快照采集；待保存的编辑值不会自动入库。');return
        self.label_record(key)

    def label_record(self,key):
        r=self.catalog.records[key]
        dlg=tk.Toplevel(self.root);dlg.title('标注已采集 ID');dlg.transient(self.root);dlg.grab_set();dlg.resizable(False,False)
        f=ttk.Frame(dlg,padding=14);f.pack(fill='both',expand=True)
        ttk.Label(f,text=f"{r['field']}  ·  0x{r['value']:08X}  /  {r['value']}").grid(row=0,column=0,columnspan=2,sticky='w',pady=(0,9))
        name=tk.StringVar(value=r.get('name',''));note=tk.StringVar(value=r.get('note',''))
        ttk.Label(f,text='装备名称').grid(row=1,column=0,sticky='w');entry=ttk.Entry(f,textvariable=name,width=48);entry.grid(row=1,column=1,pady=5)
        ttk.Label(f,text='备注 / 类型').grid(row=2,column=0,sticky='w');ttk.Entry(f,textvariable=note,width=48).grid(row=2,column=1,pady=5)
        def save():
            try:self.catalog.label(key,name.get(),note.get());self.catalog.save()
            except OSError as exc:messagebox.showerror('保存失败',str(exc),parent=dlg);return
            dlg.destroy();self.refresh_views()
        ttk.Button(f,text='保存名称（不修改 SAV）',command=save).grid(row=3,column=1,sticky='e',pady=7)
        entry.focus_set();entry.selection_range(0,'end');dlg.bind('<Return>',lambda e:save())

    def refresh_catalog(self):
        if not hasattr(self,'catalog_tree'):return
        sel=self.catalog_tree.selection();self.catalog_tree.delete(*self.catalog_tree.get_children())
        query=self.catalog_filter.get().strip().casefold()
        for key,r in reversed(list(self.catalog.records.items())):
            vals=(r['field'],f"0x{r['offset']:04X}",f"0x{r['value']:08X}",str(r['value']),r.get('name',''),r.get('note',''),r['last_seen'])
            if query and query not in ' '.join(vals).casefold():continue
            self.catalog_tree.insert('','end',iid=key,values=vals)
        if sel and self.catalog_tree.exists(sel[0]):self.catalog_tree.selection_set(sel[0])

    def label_catalog(self):
        sel=self.catalog_tree.selection()
        if sel:self.label_record(sel[0])

    def copy_catalog(self):
        sel=self.catalog_tree.selection()
        if sel:self.copy(f"0x{self.catalog.records[sel[0]]['value']:08X}")

    def ensure_not_source(self,path):
        if self.active_path and Path(path).resolve()==Path(self.active_path).resolve():
            raise ValueError('不能用导出数据覆盖正在观察的源存档。')

    def export_csv(self):
        p=filedialog.asksaveasfilename(defaultextension='.csv',initialfile='equipment_ids.csv',filetypes=[('CSV','*.csv')])
        if not p:return
        try:self.ensure_not_source(p);self.catalog.export_csv(Path(p));self.bottom_status.set('已导出 ID 表：'+p)
        except (OSError,ValueError) as exc:messagebox.showerror('导出失败',str(exc))

    def export_catalog(self):
        p=filedialog.asksaveasfilename(defaultextension='.json',initialfile='equipment_ids.json',filetypes=[('JSON','*.json')])
        if not p:return
        try:self.ensure_not_source(p);core.atomic_json(Path(p),{'schema':1,'records':list(self.catalog.records.values())});self.bottom_status.set('已导出：'+p)
        except (OSError,ValueError) as exc:messagebox.showerror('导出失败',str(exc))

    def pin_baseline(self):
        if self.latest:
            self.baseline=self.latest;self.diff_mode.set('固定比较基线');self.refresh_diff()
            self.bottom_status.set('基线已设置为最新有效磁盘快照（不包含待保存编辑）。')

    def full_diff(self):
        self.diff_start.set('0x0')
        if self.latest:self.diff_end.set(hex(len(self.latest.payload)))
        self.refresh_diff()

    def refresh_diff(self):
        self.diff_tree.delete(*self.diff_tree.get_children());self.events_tree.delete(*self.events_tree.get_children())
        for i,r in enumerate(self.event_rows):
            self.events_tree.insert('','end',iid=str(i),values=(r['time'],r['field'],
                f"0x{r['before']:08X}" if r['before'] is not None else '首次读取',f"0x{r['after']:08X}",r['name']))
        before=self.baseline if self.diff_mode.get()=='固定比较基线' else self.previous
        after=self.latest
        if not before or not after:return
        try:start=core.parse_u32(self.diff_start.get());end=core.parse_u32(self.diff_end.get())
        except ValueError:self.diff_status.set('范围格式错误。');return
        maxlen=max(len(before.payload),len(after.payload));end=min(end,maxlen)
        if not start<end:self.diff_status.set('范围需要起点小于终点。');return
        ranges=core.diff_ranges(before.payload[start:end],after.payload[start:end]);total=sum(b-a for a,b in ranges)
        for i,(a,b) in enumerate(ranges[:MAX_DIFF_ROWS]):
            a+=start;b+=start
            old=before.payload[a:b];new=after.payload[a:b]
            val=f'0x{core.u32(after.payload,a):08X}' if a+4<=len(after.payload) else '—'
            self.diff_tree.insert('','end',iid=str(a),values=(f'0x{a:06X}',b-a,old[:32].hex(' ').upper(),new[:32].hex(' ').upper(),val))
        suffix='（列表已截断）' if len(ranges)>MAX_DIFF_ROWS else ''
        self.diff_status.set(f'磁盘快照比较：范围内 {total:,} 字节变化 / {len(ranges)} 段{suffix}。连续变化段不一定是完整字段；新增观察时确认真实起点。')

    def add_diff_field(self):
        sel=self.diff_tree.selection()
        if sel:self.add_field(hex(int(sel[0])))

    def refresh_hex(self):
        if not hasattr(self,'hex_text'):return
        if not self.editor:
            text='尚未打开文件。';start=0
        else:
            p=self.editor.view()
            try:start=core.parse_u32(self.hex_offset.get())
            except ValueError:return
            start=min(start,max(0,len(p)-1));start-=start%16
            lines=['正文偏移   00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F   ASCII',
                   '--------------------------------------------------------------------------']
            for off in range(start,min(start+512,len(p)),16):
                chunk=p[off:off+16];hexpart=chunk.hex(' ').upper().ljust(47)
                asc=''.join(chr(b) if 32<=b<=126 else '.' for b in chunk)
                lines.append(f'{off:08X}   {hexpart}   {asc}')
            text='\n'.join(lines)
        self.hex_text.configure(state='normal');self.hex_text.delete('1.0','end');self.hex_text.insert('1.0',text)
        if self.editor:
            for off,patch in self.editor.patches.items():
                for pos in range(off,off+len(patch)):
                    if start<=pos<start+512:
                        line=3+(pos-start)//16;col=11+3*((pos-start)%16)
                        self.hex_text.tag_add('edited',f'{line}.{col}',f'{line}.{col+2}')
        self.hex_text.configure(state='disabled')

    def hex_page(self,direction):
        try:off=core.parse_u32(self.hex_offset.get())
        except ValueError:off=0
        limit=len(self.editor.view())-1 if self.editor else 0
        self.hex_offset.set(hex(max(0,min(limit,off+direction*512))));self.refresh_hex()

    def edit_hex_value(self):
        if not self.editor:return
        if not self.edit_enabled.get():messagebox.showinfo('只读','先勾选“开启编辑”。');return
        try:off=core.parse_u32(self.hex_offset.get());n=core.u32(self.editor.view(),off)
        except (ValueError,struct.error):messagebox.showerror('偏移错误','uint32 超出正文。');return
        text=simpledialog.askstring('高级 uint32 编辑',f'正文偏移 0x{off:04X}。未知字段可能导致配置无效。\n输入新值（只暂存，不写盘）：',initialvalue=f'0x{n:08X}',parent=self.root)
        if text is not None:self.stage_value(off,text)

    def find_hex(self):
        if not self.editor:return
        try:
            kind=self.hex_search_kind.get();query=self.hex_find.get()
            if kind=='uint32 ID':needle=struct.pack('<I',core.parse_u32(query))
            elif kind=='HEX 字节':needle=bytes.fromhex(query.replace('0x',''))
            else:needle=query.encode('utf-8')
            if not needle:raise ValueError('搜索内容为空')
            start=core.parse_u32(self.hex_offset.get())+1
        except ValueError as exc:messagebox.showerror('搜索格式',str(exc));return
        p=self.editor.view();pos=p.find(needle,start)
        if pos<0:pos=p.find(needle,0)
        if pos<0:self.bottom_status.set('未找到。');return
        self.hex_offset.set(hex(pos));self.refresh_hex()
        self.bottom_status.set(f'找到匹配：正文 0x{pos:06X}；再次搜索查找下一处。')

    def focus_search(self):self.tabs.select(self.catalog_tab);self.catalog_entry.focus_set()

    def export_payload(self):
        if not self.editor:return
        if self.editor.dirty:
            messagebox.showinfo('解压正文导出','将导出当前内存正文，包含待保存修改；内层校验会刷新。它不是可直接覆盖的 SAV。')
        path=filedialog.asksaveasfilename(defaultextension='.bin',initialfile='payload.bin',filetypes=[('Binary','*.bin')])
        if not path:return
        try:
            p=core.refresh_inner_checksum(self.editor.view())
            with Path(path).open('xb') as h:h.write(p)
            self.bottom_status.set('已导出正文；不要作为 SAV 覆盖游戏文件。')
        except (OSError,ValueError) as exc:messagebox.showerror('导出失败',str(exc))

    def save_snapshot(self):
        if not self.latest:return
        path=filedialog.asksaveasfilename(defaultextension='.sav',initialfile=f'snapshot_{time.strftime("%Y%m%d_%H%M%S")}.sav',filetypes=[('SAV','*.sav')])
        if not path:return
        try:core.save_new(Path(path),self.latest.raw);self.bottom_status.set('已保存磁盘快照，不包含未保存编辑：'+path)
        except (OSError,ValueError) as exc:messagebox.showerror('保存失败','不会覆盖已有文件，请使用新文件名。\n'+str(exc))

    def save_as(self):
        if not self.editor:return
        if self.latest and self.editor.conflicts(self.latest):
            if not messagebox.askyesno('旧快照编辑','游戏已产生更新的存档。此次另存仍使用编辑开始时的快照，可能包含旧配装。仅另存副本，继续？'):return
        path=filedialog.asksaveasfilename(defaultextension='.sav',initialfile='testament_edited.sav',filetypes=[('SAV','*.sav')])
        if not path:return
        try:
            data=self.editor.build();core.save_new(Path(path),data)
            self.bottom_status.set(f'副本已保存并校验：{path}；源存档未改变。')
            messagebox.showinfo('已另存','副本已通过解压和双层校验。\n源存档未改变；内存编辑保留，未切换到输出文件。\n文件合法不保证所有字段组合在游戏内有效。')
        except (OSError,ValueError) as exc:messagebox.showerror('另存失败','不会覆盖已有文件，请使用新文件名。\n'+str(exc))

    def save_source(self):
        if not self.editor or not self.editor.dirty:
            messagebox.showinfo('没有修改','没有待保存的内存编辑。');return
        if not self.edit_enabled.get():messagebox.showwarning('编辑关闭','开启编辑后才能覆盖源文件。');return
        if self.latest and self.editor.conflicts(self.latest):
            messagebox.showerror('拒绝覆盖','游戏已经更新源文件。请放弃旧编辑、重新读取后再操作，或另存副本。');return
        if not self.active_path:return
        if not messagebox.askyesno('回写源文件：请停止游戏写入',
            '只读采集可在游戏运行时进行，但回写不能与游戏并发。\n\n'
            '请先退出游戏或确认游戏不再写入该文件，并处理好 Steam 云同步。\n'
            '程序会校验源文件 SHA256、备份、临时写入后替换，但无法保证与游戏并发写入安全。\n\n'
            '已停止外部写入，仍要覆盖源文件吗？'):return
        try:
            data=self.editor.build()
            backup=core.save_checked(Path(self.active_path),data,self.editor.original.sha256,self.workspace/'backups')
            self.editor=core.Editor(core.Snapshot.from_bytes(data,self.active_path))
            self.accept_snapshot(self.editor.original);self.reader=core.StableReader();self.reader.accepted=self.editor.original.sha256
            self.bottom_status.set('源文件已保存；备份：'+str(backup))
            messagebox.showinfo('已保存','已回写并校验。\n备份：'+str(backup)+'\n\n游戏是否重新加载这些数据需要单独验证。')
        except (OSError,ValueError) as exc:messagebox.showerror('未完成保存',str(exc))

    def open_workspace(self):
        try:
            if os.name=='nt':os.startfile(str(self.workspace))
            else:
                import subprocess
                subprocess.Popen(['open' if sys.platform=='darwin' else 'xdg-open',str(self.workspace)])
        except OSError:self.bottom_status.set(str(self.workspace))

    def close(self):
        if self.editor and self.editor.dirty:
            if not messagebox.askyesno('尚有未保存编辑','退出将放弃内存修改。ID表已经单独保存。继续退出？'):return
        self.persist_settings();self.close_for_test()

    def close_for_test(self):
        if self.closed:return
        self.closed=True
        if self.after_id:
            try:self.root.after_cancel(self.after_id)
            except tk.TclError:pass
        self.pool.shutdown(wait=True,cancel_futures=True)
        try:self.root.destroy()
        except tk.TclError:pass


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('file',nargs='?',type=Path)
    parser.add_argument('--workspace',type=Path,default=Path(__file__).resolve().parent/'workspace')
    args=parser.parse_args()
    if os.name=='nt':
        try:
            import ctypes
            ctypes.windll.shcore.SetProcessDpiAwareness(1)
        except (AttributeError,OSError):pass
    root=tk.Tk()
    try:app=App(root,args.workspace)
    except (OSError,ValueError) as exc:
        root.withdraw();messagebox.showerror('启动失败',f'{exc}\n\n请解压到可写目录，或用 --workspace 指定工作区。损坏的 catalog.json 不会被自动覆盖。')
        root.destroy();return 1
    def report_error(exc,value,tb):
        details=''.join(traceback.format_exception(exc,value,tb))
        try:
            with (app.workspace/'error.log').open('a',encoding='utf-8') as h:h.write(details+'\n')
        except OSError:pass
        messagebox.showerror('工具错误',str(value)+'\n详情在工作区 error.log；程序没有自动修复或覆盖源文件。')
    root.report_callback_exception=report_error
    if args.file:app.open_path(str(args.file))
    root.mainloop();return 0

if __name__=='__main__':raise SystemExit(main())
