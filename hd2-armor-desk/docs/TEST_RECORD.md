# 测试记录（Rust / Windows）

本文件只记录**实际运行过**的结果。未运行的项目明确写为 NOT RUN，不用推断代替。

## CI 缓存与冷编译优化

Windows 测试和发布构建在独立的 `RUSTUP_HOME` 中按项目工具链文件安装唯一工具链，
避免 runner 预装的其他 Rust 版本参与缓存指纹。测试改用独立 `ci` 配置；
本地默认开发配置、正式发布优化、全部测试和双向校验保持不变。

### 本机冷构建对照

同一 Windows 主机、Rust `1.96.0`、相同源代码，顺序执行两个配置，分别使用全新的空构建目录和 `-j 1`。
两者均禁用全局 `rustc-wrapper` 和增量编译，复用已下载的依赖包；基线设置 `profile.test.debug=0`，
与原 CI 的无调试符号配置一致。下表构建时间取 Cargo 实际输出，包含编译和链接。

| 项目 | 原 `test` 配置 | 新 `ci` 配置 |
|---|---:|---:|
| 测试构建 | 23 分 18 秒 | 5 分 29 秒 |
| Rust 工作区测试 | 142 passed，0 failed | 142 passed，0 failed |
| 31 项 GPUI 交互测试执行 | 0.53 秒 | 2.60 秒 |

低优化测试产物执行稍慢，但冷构建明显缩短；正式程序仍使用原 `release` 配置。
这不是 GitHub 托管 runner 的全流程耗时对照，不能直接推算发布总耗时的同比变化。

### 其余实际验证

- 直接执行工作流中的隔离安装步骤：成功安装固定的 `1.96.0` 及要求的组件。
- 在后续独立进程读取导出的 UTF-8 环境文件：缓存枚举仅看到该工具链，编译器哈希与项目固定版本一致。
- `actionlint 1.7.12`：工作流校验通过。
- Python 参考实现与 Tk 界面：**48 passed，0 failed**。
- 新 `ci` 配置的 Rust 输出经 Python 校验：两种布局共六份正文、保留压缩块、哈希及双层校验一致。
- 设置 `HD2_REQUIRE_PYTHON_ORACLE=1`，使用 `ci` 配置验证 Python 输出：**2 passed，0 failed**。

本轮未触发 GitHub 工作流，托管 CI 总耗时为 **NOT RUN**；也未创建新 Release。
GitHub 超过 7 天未访问的缓存仍可能被清理，长期空闲后的冷启动不能仅靠缓存配置消除。

## 0.1.1 发布前复核

Rust 工作区及五个 crate 的内部包版本从 `0.1.0` 升为 `0.1.1`，锁文件同步更新。
GitHub 正式发布仍沿用 UTC+8 构建时间戳，不改为 SemVer 标签。

| 验证 | 实际结果 |
|---|---|
| `0.1.1` Rust 工作区测试 | **142 passed，0 failed**，包含 31 项 GPUI 交互测试 |
| Python 参考实现与 Tk 界面测试 | **48 passed，0 failed** |
| Rust → Python | 两种布局各三种修改，共六份完整正文、未修改压缩块、哈希及双层校验一致 |
| Python → Rust | 设置 `HD2_REQUIRE_PYTHON_ORACLE=1` 后，oracle 套件 **2 passed** |
| `cargo fmt --all -- --check` | 通过 |

本机重新编译时遇到缓存依赖缺失和编译器内存分配失败。通过调用参数
`--config "build.rustc-wrapper=''"` 禁用全局编译缓存，并使用独立构建目录和 `-j 1` 后完整验证通过；
未修改仓库编译配置，也未降低 CI 测试门槛。以下界面重构记录为先前阶段的独立证据。


## 界面精简与卡顿修复

### 本轮验证

| 验证 | 实际结果 |
|---|---|
| Rust 工作区测试 | **142 passed，0 failed**；包含 31 项真实 GPUI 事件交互测试 |
| Clippy：工作区、所有 target、`-D warnings` | 通过，零警告 |
| `cargo fmt --all -- --check` | 通过 |
| Windows release 构建 | 主程序与更新器均构建成功 |
| 原生桌面 | 打开合成存档、搜索、选择头甲、切换部位、恢复对应搜索词、监视开关、预设入口、文件信息和双模式写回确认均已操作 |
| 窗口尺寸 | 默认 1200 × 680 与最小 960 × 640 逻辑像素均已检查；文件信息独立占用工作区，可直接返回装备库 |
| 存档安全 | 桌面未执行最终写入；合成源文件 SHA-256 仍为 `9685e4608c00c50c06e6ba584b4e90b398835796e1e2339109627e5003886257` |

共享构建缓存所在盘空间不足，本轮 Cargo 构建、测试和 Clippy 额外使用了有剩余空间的临时 `--target-dir`，
没有更改项目工具链或编译优化配置。最终主程序与更新器已复制到项目的 release 输出目录。
界面改动阶段未重跑 Python 双向 oracle，后续发布前复核见上。本轮未执行游戏内验收、云同步并发或真实账号存档写回。

### 帧耗时对照

同一 Windows 环境、GPUI 无头测试窗口 1200 × 680、内置 93 件装备；预热后逐次通知界面更新并绘制完整帧，
每个搜索条件采样 40 次。以下是 CPU 侧界面帧耗时，不是显示器帧率或 GPU 呈现时间。

| 场景 / 指标 | 修改前 | 修改后 |
|---|---:|---:|
| 全部装备：中位数 | 96.670 ms | 2.011 ms |
| 全部装备：P95 | 104.742 ms | 2.669 ms |
| 搜索 `FS`：中位数 | 6.486 ms | 1.912 ms |
| 搜索 `FS`：P95 | 7.660 ms | 2.314 ms |

临时测量代码和调试输出已删除，不把性能阈值硬编码为永久测试。

### 原因与修复

- 旧装备列表每次刷新都构建全部行，并提前复制名称、ID 和被动描述；现改为只构建可见行，
  保留完整被动说明，同时移除前 300 项截断。
- 固定版本的 GPUI 在窗口宽度变化时清空未显示行的高度估计；界面在后续帧补回估计，
  避免长列表滚动范围缩小，不以全量测量换取滚动正确性。
- 旧监视器每 500 ms 在界面线程读盘、计算 SHA-256 并解码；现将这些工作移到后台，
  并拒绝重新打开存档或写入完成之前启动的过期结果。重复等待/错误不会反复触发相同刷新。
- 每次选择装备时不再为改动预览复制整份约 572 KB 的正文。
- 移除操作流程侧栏、英文分区标签、装饰编号和重复的类型/状态文字；文件字节、校验值保留在文件信息中。

新增回归覆盖：400 件目录的末尾装备选择与筛选复位、最小窗口下从辅助视图返回后继续选择、
滚动时部位和保存控件固定、后台读取的稳定等待窗口及过期读取拒绝。
旧的逐字匹配预览文案测试已移除；槽位写入范围、安全写回、风险写回、备份与校验仍由行为测试覆盖。


## 下载更新实时进度条

| 验证 | 实际结果 |
|---|---|
| `cargo test -p hd2-armor-desk --test ui_flow update_download_banner_shows_live_progress --locked` | 1 passed，0 failed；界面测试确认 37% 时进度条有非空的部分填充，并显示百分比 |
| `cargo test -p hd2-armor-desk --lib update::tests::downloads_the_package_only_after_its_sha256_matches --locked` | 1 passed，0 failed；下载回调报告已接收字节并最终到达包声明的完整大小 |
| `cargo fmt -- --check` | 通过 |

下载线程在每次写入数据后记录已接收字节数，界面每 100 ms 刷新一次；进度显示文件大小、百分比与使用应用强调色的进度条。

## 2026-09-25：装备库滚轮隔离

| 验证 | 实际结果 |
|---|---|
| `cargo test -p hd2-armor-desk --test ui_flow scrolling_inside_browser_does_not_scroll_the_main_page --locked` | 1 passed，0 failed；`ui_flow` 的 28 个测试中运行 1 个，过滤 27 个 |
| `cargo test --workspace --locked` | 138 passed，20 个测试套件，0 failed |
| `cargo fmt -- --check` | 通过 |

该测试先连续滚动主内容区，确认页面锚点移动，再在装备库列表的可见区域派发滚轮事件，验证条目移动而页面锚点保持不变；最后在列表外滚动确认主页面仍可继续移动。列表区域滚轮由内层消费；列表到边界后需将鼠标移出列表才能滚主页面。

## CI #8 失败修复：暂停监视后的手动刷新

远端运行 `35738473632` 的 Python 步骤在 `test_manual_refresh_when_monitor_paused` 中
访问 `app.latest.sha256` 时收到 `None`；同次 release 构建成功，发布因测试失败跳过。
测试直接注入未经 `open_path()` 规范化的路径和快照，刷新可进入重新打开流程并清空 `latest`。
本地用含 `..` 的路径别名验证了该状态转换。

修复仅调整测试初始化和等待方式：通过真实 `open_path()` 加载首份快照，再在暂停监视状态下手动刷新；
等待以目标文件 SHA 为条件，超时提供界面错误状态。测试继续断言监视保持暂停、源文件不被工具改写，
不跳过 GUI、不忽略异常，也不改动生产读取逻辑。修复后本机 Python 全部 48 项通过。

## 2026-09-22：Observed0107 兼容验证

本轮在 Windows 本机执行；以下结果独立于后面的历史记录：

| 验证 | 实际结果 |
|---|---|
| `cargo test --workspace --locked` | 128 passed，0 failed；覆盖新旧布局、混配头部/长度只读、跨布局冲突与 force-latest、目录迁移、武器槽位拒绝和 GPUI 加载 |
| Python `unittest discover -s tests -v`（reference/python） | 48 passed，0 failed，含 Tk 界面和旧配置迁移 |
| Rust → Python oracle | 两种布局各 3 种编辑，共 6 份输出的完整正文、未修改块和双层校验一致 |
| Python → Rust oracle | 设置 `HD2_REQUIRE_PYTHON_ORACLE=1` 运行已编译 oracle 测试程序，2 项通过，两种布局输出均被检查 |
| 缺输出故障实验 | 暂移新布局 Python 输出，严格反向检查退出 101；输出已恢复 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过，无警告 |
| 配套数据 | 两套夹具共同的 17 个文件逐字节一致；两套 schema 一致；内置目录通过 JSON Schema 校验 |
| 真实新样本：Python 与 Rust | 支持 0107；无修改原样返回；内存编辑及重解码通过；非目标正文保留；源文件未改 |
| 真实桌面 | Tk 检查器显示已知布局、头部/身体/披风/主副武器字段；原生 GPUI 应用用隔离副本显示 TR-117 与 TG-8，副本未写入 |

真实样本逻辑长度 572092，外层 CRC32 `B2E4E0B3`、内层校验 `B44BA676`。
真实样本不进入公开夹具。临时 Rust 冒烟 example 和隔离运行目录已在验证后清理。

验证环境差异：直接从 Python 子进程调用 Cargo 时命中了用户 home 下启用不稳定 codegen-backend 的配置，
因此该次 Cargo 启动失败；正常终端的隔离 Cargo 环境已完成全部 Rust 测试和 Clippy。
严格反向检查随后直接运行本轮编译的测试程序通过，不把失败调用计为通过。

发布工作流已加入 Python 测试及双向 oracle 必需检查；本轮未触发远端 GitHub Actions、未构建 release 包，
也未进行游戏内回读、云同步并发或被动效果验证。以下是之前的历史测试与资源测量，不代表本轮重新测量。

## 环境

| 项目 | 值 |
|---|---|
| 机器 | Intel(R) Core(TM) Ultra 5 250K Plus |
| 系统 | Windows 11 Pro，10.0.26200 |
| 架构 | x64 |
| 工具链 | `stable`（由 `rust-toolchain.toml` 固定，MSVC target） |
| 依赖 | `Cargo.lock` 锁定；UI 层 `gpui-kit 0.6.4` |
| 采样方式 | `GetProcessMemoryInfo` / `GetProcessTimes`，启动时间由 spawn 到窗口可见的墙钟时间测得 |

## 1. 自动测试

命令：

```powershell
cargo test --workspace --locked
```

结果：**111 passed, 0 failed**（stable 工具链）。

| 套件 | 通过 | 覆盖内容 |
|---|---|---|
| `sav_codec` golden | 11 | 14 个 fixture 的接受/拒绝、无修改字节保真、差异白名单范围 |
| `sav_codec` oracle | 2 | 228 组 MurmurHash64A 向量；Rust 输出与 Python 参考实现互读 |
| `sav_codec` unit | 2 | 编解码内部不变量 |
| `loadout_domain` unit | 17 | 草稿、默认解锁、撤销栈、意图、类型判定 |
| `loadout_domain` import | 21 | 旧 JSON / CSV / v2 JSON 导入、BOM、中文引号、冲突行号 |
| `loadout_domain` validation | 14 | schema 与运行时语义校验、非法 u32 |
| `local_io` unit + monitor | 1 + 9 | 工作区、稳定窗口、半写入、generation、监视不写盘 |
| `local_io` transactions | 15 | 另存、安全写回、风险写入最新文件、备份、恢复 |
| `app` ui_flow | 19 | 真实 GPUI 窗口内派发鼠标事件、装备库被动详情与隔离文件写回 |

### ui_flow 说明（UI 点击不是静态断言）

`crates/app/tests/ui_flow.rs` 在 GPUI 的 headless 测试应用里构建**真实的**
`WorkspaceView`，通过 `window.dispatch_event` 派发真实的
`MouseDownEvent`/`MouseUpEvent`/`MouseMoveEvent`，再断言状态变化。因此下列结论
来自实际点击，而非阅读代码推断：

| 测试 | 断言 |
|---|---|
| `opening_a_save_populates_both_slot_cards` | 打开 `valid_baseline.bin` 后 `head_id==0x056848E9`、`body_id==0xD3461392`（与 manifest 一致）；两卡片与写回/撤销按钮均已布局 |
| `browser_renders_armor_passive_details` | 搜索 FS-55 后，被动名称与中文效果说明均进入实际 GPUI 布局 |
| `body_starts_unlocked_and_can_be_locked_without_writing` | 开档即建立干净草稿、身体默认解锁；点「锁定身体」后 `body_locked==true` 且不产生写回 |
| `risk_write_rebases_the_head_choice_onto_the_games_new_body_value` | 真实点击选择头甲、打开确认框并执行风险写入；用户头甲落盘，游戏刚切换的身体甲保留 |
| `selecting_the_body_slot_then_the_head_slot_moves_the_target` | 点身体卡片 → `target_slot==Body`；点头部卡片 → 回到 `Head` |
| `a_clean_draft_keeps_write_back_disabled` | 无改动时点「备份并写回存档」不启动任务、不产生写回记录 |
| `an_unknown_layout_offers_no_write_back` | `unknown_layout_valid.bin` 打开为只读且带理由；点写回不启动任务 |
| `the_watcher_toggle_flips_and_never_touches_the_file` | 点「只读监视」→ `watching==true`，再点 → `false`；被监视文件字节完全不变 |
| `an_empty_catalog_never_claims_a_type_for_the_head_slot` | 目录为空时，头部为身体护甲 ID 也不断言其类型，只显示未知 |
| `app_state_defaults_are_safe` | 默认不监视、无写回任务、无存档、无草稿、诊断收起 |
| `restore_is_reachable_and_whole_file_scoped` | 工具栏「恢复整份备份」存在且可点；无备份时点它不启动任务、不改动源文件 |
| `recording_a_restore_rebuilds_the_document_from_disk` | `restore_backup` 后源文件与备份逐字节相同、留下 `before_restore_*` 副本；`record_restore` 把快照与草稿按磁盘重建（头部 ID 变为备份中的值、SHA 更新、草稿干净、不再 in-flight） |

### 过程中发现并修正的三处

1. **测试断言写反了**：最初断言「解锁身体后 `is_dirty()` 为真」。实际运行（插入探针打印
   `locked=false dirty=false head_set=false body_set=false`）证明解锁只改变锁状态、
   不改变任何槽位值，因此草稿仍是干净的——这是正确行为，断言已改正。
2. **元素定位需要已绘制帧**：`debug_bounds` 读取的是上一次**已绘制**的帧，仅改变状态
   不会填充它。测试改为先 `window.render_frame(cx)` 再断言/点击。
3. **测试互相污染**：多个测试共享同一 scratch 工作区，导致备份文件在测试间泄漏
   （`restore_is_reachable...` 断言"不应有备份"时实际读到 3 个）。改为每个测试独立
   目录 + 进程级互斥锁（`HD2_ARMOR_DESK_WORKSPACE` 是进程全局的，必须串行化）。

## 2. 构建与静态检查

| 命令 | 结果 |
|---|---|
| `cargo fmt --check` | 通过（无 diff） |
| `cargo clippy --workspace --all-targets --locked` | **0 warning，0 error** |
| `cargo build --release --locked` | 成功（stable 工具链） |

## 3. Windows 手工冒烟（release 程序）

使用 `target\release\hd2-armor-desk.exe`，命令行参数传入
`tests/fixtures/valid_baseline.bin` 的副本。

| 项目 | 实际观察 |
|---|---|
| 启动 | 窗口正常出现，标题「Armor Desk · HELLDIVERS 2 双甲配置器」 |
| 打开存档 | 头部与身体卡片读出 manifest 对应 ID；身体槽位显示「已解锁」 |
| 装备库 | 行内显示名称、十六进制 ID 与装备类型，不显示内部证据分类 |
| 选择装备 | 头部卡片、列表选中态、差异栏和写回按钮同步更新 |
| 写回确认 | 实际桌面确认同时显示「风险写入最新文件」与「安全写回」及风险说明 |
| 源文件安全 | 未点击最终写入，仓库 fixture 内容未改动 |
| 截图 | 本轮使用临时截图检查，验证完成后删除 |
| 本次被动详情冒烟 | release 进程成功启动并生成含 67 件护甲被动的便携目录；工作站处于锁屏界面，无法取得可信桌面像素截图，行内布局由 `browser_renders_armor_passive_details` 验证 |

资源占用（同机采样，采样方法见上表）：

| 指标 | 值 |
|---|---|
| 启动到窗口可见 | 0.27 s |
| 空闲 RSS | 63.9 MiB |
| 空闲 CPU | 0.00%（单核占比，5 秒采样） |
| EXE 大小 | 14.3 MiB |

## 4. 未验证项（NOT RUN / SKIPPED）

以下项目**没有**通过，也未执行，明确记录以免误读为已验证：

| 项目 | 状态 | 原因 |
|---|---|---|
| 真实桌面上的最终写盘 | **PARTIAL** | 已实际完成「打开 → 选择护甲 → 打开双模式确认框」；为避免修改仓库 fixture，未点最终写入。隔离 scratch 文件的完整点击写回由 `ui_flow::risk_write_rebases_*` 覆盖。 |
| 与真实 HELLDIVERS 2 存档的读写 | **NOT RUN** | 需用户本地私有样本；包内不含真实 `.sav`。 |
| 游戏内验收 G01–G06 | **NOT RUN** | 需用户账号实测，见 `docs/ACCEPTANCE.md` 与 README 第 8 节。 |
| 被动效果倍率 / 客户端 build 兼容性 | **NOT RUN** | 未知事项，不作保证。 |
| 真实中文输入法（IME）候选窗交互 | **NOT RUN** | 受同一前台焦点限制，未能驱动真实 IME 输入。 |

## 5. 结论

数据正确性、目录/预设与风险写入中可自动验证的部分已由 111 个测试覆盖并通过；
玩家体验中「卡片选择、身体默认解锁、无改动禁用写回、只读版本拒绝写入、
风险写入最新文件」已由真实事件派发的 UI 测试覆盖。**游戏内验收（G 类）仍需用户在本地完成**，
不应把本工具视为已通过完整验收。
