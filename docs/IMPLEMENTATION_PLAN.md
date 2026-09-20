# HD2 Armor Desk — Rust/GPUI 实施计划

**目标：** 将现有经用户验证的 SAV 修改能力实现为 Windows 玩家友好的双甲配置器。  
**规格：** `../HANDOFF.md`，任何实现细节不得削弱其字节保真、冲突与隐私约束。  
**架构：** `sav_codec`（纯字节）→ `loadout_domain`（意图/目录）→ `local_io`（监听/事务）→ `app`（GPUI）。  
**技术：** Rust + GPUI；组件层可选兼容的 GPUI Kit；具体兼容版本经 Windows 最小切片验证后锁定。  
**执行方式：** 按里程碑进行测试先行与审查。支持子代理的环境可并行处理独立模块；不支持则依序完成，不声称进行了不存在的独立审查。

## 全局约束

- 日常运行不依赖 Python；Python 只作开发时的格式 oracle。
- 16 MiB 输入和逻辑正文防护上限；unknown-layout 只读。
- 自动监视默认 500ms / 稳定窗口 250ms；监视为只读。
- 默认只修改 head，body=Keep、cape不写；必要内层/外层校验自动更新。
- 非保存动作绝不写游戏源文件。
- 预设不含原始存档；应用到确认的最新有效基线。
- 当前包没有用户全量目录；必须兼容导入，不能猜ID、不能因此停止核心开发。
- `.bin` fixtures 全部合成，不能导入游戏；真实存档仅用户本地选择。
- 不能把最终目录定义、GPUI版本、效果倍率等未测内容当作事实。

## Review Focus：最容易遗漏的五类失败

1. 文件写回后游戏再次覆盖：M6测试必须区分成功、被覆盖、结果不确定，不得重试抢写。
2. 头部观察到身体护甲：M2保留类型与观察槽位独立，不能以offset分错物品。
3. watcher旧任务在用户切换账号后返回：M4/M5用generation丢弃，不能跨文件更新。
4. UI另存成功后误标记源已应用：M3/M6使用不同receipt和状态，不清空真实源脏标记。
5. 导入CSV包含同名、冲突ID、中文引号/换行：M2使用标准解析与预览，不静默覆盖原目录。

---

## M0 — Windows/GPUI 最小可运行切片

**负责文件：** workspace Cargo配置、`crates/app/src/main.rs`、最小窗口、Windows构建说明。  
**产物：** 真实 Windows 窗口，可输入中文、显示卡片、打开系统文件选择器、弹出确认框、后台任务返回状态。

- [ ] 阅读所选 GPUI/Kit 官方当前版本示例，选择一个相互兼容的依赖组合。
- [ ] 在 Windows 上创建最小窗口；不必连接真实存档，显示合成卡片即可。
- [ ] 输入 `FS-55 蹂躏者`，检查中文IME组合输入；改变窗口尺寸与DPI。
- [ ] 后台模拟读取返回结果，前台更新，不阻塞光标。
- [ ] 关窗口时取消或忽略后续异步结果。
- [ ] 锁定 Cargo.lock 和 rust-toolchain；记录 crate版本/rev、MSVC与SDK。
- [ ] 执行 `cargo check --locked`、`cargo tree -d`；确认无两份不兼容GPUI。
- [ ] 提交最小可运行切片及截图；不要把未运行的代码称为Windows成功。

**通过标准：** 基础交互真实可用，版本锁定，不靠mock图片假装GUI实现。失败时先解决平台依赖，不在大量业务代码完成后才发现框架版本混乱。

## M1 — 严格 Rust codec 与双层校验

**负责文件：** `crates/sav_codec/src/{lib,container,checksum,layout}.rs`、`tests/golden.rs`。  
**输入：** `fixtures/`、Python参考源码。  
**接口：** SaveImage::decode/read_u32/encode_patches。

- [ ] 先建立读取fixture和manifest的测试框架；不使用原始用户存档。
- [ ] 为MurmurHash64A加入228个向量；先确认占位实现失败，再按wrapping-u64实现。
- [ ] 为有效baseline、截断、CRC错误、内层错误等建立独立用例，预期失败原因明确。
- [ ] 实现有界raw块解析、完整65536字节解压、终端零填充和长度副本检查。
- [ ] 区分容器拒绝、解码成功但layout未知、known writable。
- [ ] 测试无patch encode完全返回原raw；阶段尚未实现时应看到对应测试失败。
- [ ] 实现等长patch、内层hash更新、变化块重压、外层长度/CRC更新。
- [ ] 对头、身体、两槽位和后续块分别修改，检查允许范围与原压缩块复用。
- [ ] 拒绝受保护头部、重叠patch、before不匹配、逻辑长度变化和未知布局写入。
- [ ] Rust生成结果由Python decode回读；Python fixtures由Rust decode读取。不要要求不同压缩器输出相同修改后raw。
- [ ] 执行所有codec测试、clippy；保存真实输出。

**关键断言（与HANDOFF接口约定一致）：**

```rust
// 示例断言；实现时补齐实际测试函数/import，不能当整项目已实现。
let image = SaveImage::decode(fixture_bytes)?;
assert_eq!(image.encode_patches(&[])?, image.raw());
assert_eq!(image.read_u32(PayloadOffset(0x121))?, 0x056848E9);
```

**通过标准：** 14个fixture各自按manifest预期接受/拒绝，228向量一致。错误内层即使外层正确也拒绝；无任何panic/超量分配。

## M2 — 用户目录导入与类型规范化

**负责文件：** `loadout_domain/catalog.rs`、`local_io/catalog_store.rs`、JSON schema与迁移测试。  
**输入：** 原catalog schema1/CSV示例与v2 schema。  
**输出：** ImportPreview → 用户分类解决 → CatalogDelta → 独立workspace目录。

- [ ] 先写“同一Armor在head/body出现仍是一物品”的失败测试。
- [ ] 写“同名不同ID Helmet/Armor不合并”“大于i32max的ID不截断”测试。
- [ ] 实现JSON整数、u32范围、schema检测；不把bool/float当合法ID。
- [ ] 实现带BOM的CSV：逗号、引号、换行、中文；十进制与hex不一致须报告行。
- [ ] 建立保留观察来源的规范化模型；body observation只给建议分类；head observation不能直接归Helmet。
- [ ] 实现未知/冲突导入预览和用户批量确认；名称/类型冲突保留，不以最后一条静默覆盖。
- [ ] 重复导入幂等，保留手工名称与备注；测试源目录文件bytes未改变。
- [ ] 校验新schema示例；schema通过后仍验证item_key与id/type一致。
- [ ] 原子保存新目录；损坏已有目录时拒绝覆盖为空表。
- [ ] 实现JSON与CSV导出，防CSV公式注入，不把私人source摘要作为默认分享字段。

**通过标准：** 本包5条legacy记录规范化为4个物品，FS-55 Armor有head/body两条observation；用户真实目录到手后无需改代码格式即可导入。

## M3 — 草稿、预设与领域验证

**负责文件：** `loadout_domain/{draft,preset,validation}.rs`。  
**输入：** 不可变Snapshot、Catalog、LoadoutIntent。  
**输出：** PatchSet与HumanReadableDiff。

- [ ] 测Keep不产生patch、Set等于当前值不dirty、Set(0)不能混成Keep。
- [ ] 测head接受明确Armor或Helmet；body只接受Armor；Unknown普通模式拒绝。
- [ ] 实现body默认锁定；锁定状态下SetHead不改变body。
- [ ] 实现undo/redo，每个“应用整对预设”是一项命令。
- [ ] 测预设只存意图，不含snapshot/path/account；反序列化未知schema拒绝。
- [ ] 测昨天保存preset后今天模拟修改0x0011等非目标字段，再套用preset必须保留今天字段。
- [ ] 定义CopyReceipt与CommitReceipt分离的状态转换，不因另存清空源文件状态。
- [ ] 实现普通头盔恢复逻辑：只可记录真实Helmet，不能把打开时已有的Armor头部当普通头盔。
- [ ] 创建可读差异：名称、类型、ID、头/身体范围；未知名称保留ID。

**通过标准：** 编解码层不需要知道护甲文案，领域层不需要GPUI。全部业务变化可以在无窗口测试中验证。

## M4 — 稳定文件监听与revision冲突

**负责文件：** `local_io/monitor.rs`、`discovery.rs`、相关I/O测试。  
**输出：** 带generation/path_identity的Snapshot事件。

- [ ] 先写半截文件→完整文件的测试，last-good保持，最终只发布有效版本。
- [ ] 实现读前后metadata、有界读取、SHA去重、settle窗口、双层校验。
- [ ] 测游戏采用rename-replace而不是原地写时，目标路径仍可跟随。
- [ ] 测dirty草稿时源变化产生Conflict，不更改基线、不丢用户选择。
- [ ] 测切换到B路径后迟到A事件被丢弃。
- [ ] 实现受限Steam路径发现与多账号选择，不递归扫盘，不猜当前账号。
- [ ] 错误限频：短时pending不刷一屏弹窗；持续错误有可见状态。
- [ ] 关闭窗口时结束任务，监视暂停可手动F5，F5仍遵守稳定/校验。

**通过标准：** 停止监视不等于停止游戏写入的提示存在；旧任务绝不跨文档应用。

## M5 — 玩家主界面与完整选择闭环

**负责文件：** `app/workspace.rs`、slot/armor卡片、catalog/preset视图、actions。  
**依赖：** M0、M2、M3；M4事件接入。

- [ ] 先写/定义关键UI场景：body锁定、head选Armor、同名类型、未命名项、没有目录。
- [ ] 双槽卡片，明确`头部槽 · 身体护甲`；不可只写同名标题。
- [ ] 搜索/筛选、详情、收藏、可选本地图片；缺metadata仍可选。
- [ ] 卡片选中只更新草稿，检查源文件SHA保持不变。
- [ ] 显示最新磁盘状态与草稿差异；外部Conflict时不会重建InputState导致丢焦点。
- [ ] 使用所选GPUI版本的Root/overlay初始化；测试所有确认框真能显示。
- [ ] 900×620与1180×780、多个DPI下检查关键按钮；中文IME、键盘导航、undo/redo。
- [ ] P0高级页只读文件头/校验/少量HEX，不加入任意写入入口。

**通过标准：** 用户不用懂偏移，可在同屏选择head护甲且保持body，清晰看到即将改变什么。不能只完成编解码CLI就称GUI交付。

## M6 — 安全另存、备份回写与恢复

**负责文件：** `local_io/save_transaction.rs`、`platform_windows.rs`、backups视图。  
**依赖：** M1、M3、M4。

- [ ] 测另存源bytes不变、已存在路径拒绝、非SAV导出也不可指向源身份。
- [ ] 测基线SHA变化前置拒绝、备份失败不触碰源、临时写失败不删原件。
- [ ] 实现PreparedCommit冻结，memory校验与patch白名单检查。
- [ ] 实现独立备份+SHA验证、同目录临时文件+sync、源再次检查、Windows替换、回读。
- [ ] 阅读所选Win32 API失败语义；测试占用/ACL/中文长路径，不能依赖Linux rename行为推断Windows。
- [ ] 测写回后外部再写返回Superseded；无自动反复覆盖。
- [ ] 测复杂替换失败返回Indeterminate，保留现场/备份，文案不能说源必定未变。
- [ ] UI的“已应用”只能来自回读成功receipt；另存只显示副本已写。
- [ ] 实现普通头盔恢复与全快照恢复的不同确认范围。
- [ ] 并发保存按钮去重/禁用，一个文档只允许一个提交任务。

**通过标准：** 所有故障注入路径没有静默数据丢失；所有成功状态都有磁盘回读证据。游戏运行中不提供默认自动回写。

## M7 — 包装、质量与用户验收

**负责文件：** Windows脚本、README、测试记录、schema迁移说明、release清单。

- [ ] 运行 `cargo fmt --check`、`cargo clippy --workspace --all-targets --locked`、`cargo test --workspace --locked`。
- [ ] 对不支持的GUI/headless环境明确skip；不能把skip记成通过。
- [ ] Windows release构建、首次启动、退出、再启动、导入目录、另存、备份回写实测。
- [ ] 记录启动时间、RSS/CPU/GPU空闲占用、EXE大小；指标注明机器和采样方法。
- [ ] 使用合成数据录制/截图默认主界面、搜索、Conflict、unknown-layout和保存结果。
- [ ] 查包内无真实sav、账号目录、用户catalog、系统字体与不可分发素材。
- [ ] 提供无Rust/Python环境的运行说明，记录必要系统运行时；不承诺未验证的单文件零依赖。
- [ ] 按ACCEPTANCE清单进行用户游戏验证；先少量普通头盔对照，不重复整套旧逆向。
- [ ] 更新IMPLEMENTATION_STATUS：已做、未做、平台验证、已知问题、下一动作。

**最终交付：** Windows可执行程序/便携包、全部Rust源码、Cargo.lock、中文README、用户目录导入步骤、备份恢复路径、真实测试结果。不得仅交付设计文档或漂亮截图。
