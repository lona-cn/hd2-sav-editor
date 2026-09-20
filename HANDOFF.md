# HD2 双甲配置器：Rust + GPUI 开发交接

**交接版本：1.0 · 编制日期：2026-09-19**  
**目标平台：Windows x64，Steam 版 HELLDIVERS 2**  
**交付对象：接手实现的 AI agent / 开发者**  
**当前交付物性质：工程规格与参考实现包，不是已经实现的 Rust 应用。**

> 核心任务：把已经打通的本地 `.sav` 解析、双层校验与头部装备替换能力，做成一个玩家不需要查看偏移、不需要输入十六进制，就能直观选择“头部护甲 A + 身体护甲 B”的小工具。用户已经自行采集所有身体护甲 ID；不要再把主要精力花在网上寻找 `.sav` 或重新发现头盔字段。

## 阅读顺序

1. 先阅读本文件的 **1—4 节**，理解已验证事实、失败原因和数据边界。
2. 完整阅读 `reference/python/hd2_core.py`，特别是 `decode`、`repack`、`inner_checksum`、`Catalog`、`save_checked`。
3. 阅读 **5—10 节**：实际文件结构、ID 迁移、玩家交互、预设、监听与保存事务。
4. 阅读 **11—15 节**：Rust/GPUI 架构、接口、性能、测试及错误处理。
5. 按 `docs/IMPLEMENTATION_PLAN.md` 分阶段实施；用 `docs/ACCEPTANCE.md` 验收。
6. 用户实测结论、离线参考复测结果见 `evidence/`；公开技术来源见 `docs/SOURCES.md`。

文中 **已验证** 表示本次对话中的样本复算或用户游戏测试支持；**设计要求** 表示新工具需要实现，不表示已经存在；**未知** 不能用猜测填成事实。

---

## 1. 用户与产品目标

### 1.1 场景

用户在 Windows / Steam 上玩 HELLDIVERS 2。当前研究的是把**身体护甲的物品 ID 写入头盔装备槽**，使人物的两个装备槽分别引用护甲。用户已确认这类异常装备状态可被其客户端读取。

此前已经提供过 Python/Tkinter 的通用 SAV Inspector。用户发现游戏中点击装备后，游戏会写入 `.sav`，因而能在不退出游戏的情况下，以只读监听逐项收集装备 ID。用户现在报告：**已成功获取所有护甲 ID**。

新需求不是再做一个给逆向工程师使用的十六进制表格。主要体验应当是：

```text
打开当前存档 → 导入我的护甲目录 → 点选头部护甲 → 身体护甲可选/保持不变
→ 看清即将修改的内容 → 另存或确认写回 → 可以恢复普通头盔
```

### 1.2 必须实现的结果

- Rust + GPUI 桌面程序；中文优先；不以 Electron、WebView 或浏览器服务替代。
- 显示实际文件中的头盔、身体护甲、披风及文件状态。
- 直接导入原 Python 工具生成的 ID 表，保留用户的名称、备注和采集依据。
- 按护甲名称、编号、可选被动标签搜索，点击卡片即可设为头部或身体。
- 默认只改头部，身体和披风保持现状；提供明确的“锁定身体”状态。
- 支持保存双甲预设、应用预设、另存 `.sav`、有备份与冲突检查的写回。
- 支持运行游戏时的只读监视，但**不宣称并发改档或游戏运行中热加载已验证**。
- 未识别的数据原样保留；未知版本默认只读。
- 日常使用不需要 Python。Python 参考代码只在开发/测试时使用。

### 1.3 明确不做

不修改进度、货币、解锁状态、账号标识；不扫描或注入游戏内存；不绕过反作弊；不自动关闭游戏；不把存档上传到服务端；不依赖 Discord、第三方存档下载器或在线数据库；不把普通 UI 伪装成“全被动必定叠加”的承诺。

不要把此项目扩展成全能游戏修改器、模组管理器、账号管理工具或配装社区。复杂皮肤系统、3D 人物预览、在线排名、自动更新器均不属于第一版。

---

## 2. 当前材料与缺失输入

### 2.1 本交接包实际包含的材料

| 路径 | 用途 | 注意 |
|---|---|---|
| `reference/python/hd2_core.py` | 原工具的完整编解码、哈希、编辑、监听、目录和写回逻辑 | 语义参考，不是新的 Rust 实现 |
| `reference/python/sav_gui.py` | 原 Tkinter 前端 | 参考监视、脏编辑和导出行为；不要照搬其面向逆向的主界面 |
| `reference/python/tests/` | 42 项原有核心/GUI 测试 | 全部使用合成样本 |
| `reference/legacy/INSPECTOR_README_zh.md` | 原版使用说明 | 保留历史说明，不等于本次新产品设计 |
| `reference/legacy/inspector_synthetic_preview.png` | 原版 GUI 的合成数据截图 | 仅历史参考，不是新 UI 的设计稿 |
| `fixtures/*.bin` | 本次新建的跨语言编解码对照样本 | **合成数据，禁止导入游戏** |
| `fixtures/manifest.json` | 对照样本哈希、预期结果及字段值 | 以其具体值为准 |
| `fixtures/murmur_vectors.json` | 不同长度/seed 的哈希测试向量 | 用于发现 Rust 移植的溢出/尾字节错误 |
| `examples/`、`schemas/` | 旧目录示例、新目录/预设的结构契约 | 演示数据，不是用户全量护甲目录 |
| `evidence/` | 参考工具复测、实测结论摘要 | 明确区分用户观察与当前离线执行 |

### 2.2 没有包含的材料

**用户自己采集的全量 `catalog.json` / CSV，本轮没有上传。**不要把本包示例当成全量数据库，也不要声称已经检验用户目录完整性。

用户真实的原始 `.sav` 和默认化后 `.sav` 也**没有放入交接包**，因为其中含账户/会话信息。实现时允许用户通过文件选择器提供当前存档。用户实测已经足以指导本轮 UI/编解码开发，不需要再次要求其寻找第三方存档。

程序首次启动应允许导入：

```text
<旧 Python 工具目录>\workspace\catalog.json
或旧工具导出的 CSV / JSON
```

目录缺失时仍可用合成目录开发、运行编解码测试和完善 UI；到用户验收时再导入其真实目录。**不要因缺少未交付的全量目录而停在“请补充数据”阶段。**

### 2.3 用户的实际存档位置

路径结构已知：

```text
<Steam 安装目录>\userdata\<账户数字目录>\553850\remote\testament_new.sav
```

`553850` 是这个工作流使用的应用目录。不要把示例中的账户数字硬编码到程序，也不要把它自动当成 SteamID64。首次默认候选可以从 Steam 安装路径和 `userdata` 的目录结构发现；多账号时让用户选择。

**不会自动切换到修改时间最新的账号。**可列出路径、大小、最近修改时间，让用户确认当前目标。

---

## 3. 已验证事实：不要重复踩坑

### 3.1 调查与实测时间线

| 阶段 | 发生了什么 | 接手者应保留的结论 |
|---|---|---|
| 原始解析 | 成功解开自定义容器和 raw LZ4 分块，匹配外层 CRC32 | 不是加密文件，也不是标准 `.lz4` frame |
| `01` | 只重新封装，不改正文；用户报告正常配装恢复 | 当前封装路线可被其客户端接受 |
| 旧 `02` | 改候选头盔字段，但只更新外层 CRC | 游戏回到默认武器/护甲/角色等配置 |
| `after_02` | 用户上传游戏写回文件；正文大量状态被初始化 | “整体默认化”不是只换了另一个头盔 |
| 修复 | 找到正文 MurmurHash64A-low32 校验 | 必须先改正文校验，再压缩，再改外层 CRC |
| `04` | 与旧 `02` 相比，只修正正文校验 | 头盔变为 FS-55 蹂躏者**头盔**，其他不变 |
| `05` | 正常头盔对照 | 头盔变为 B-01 战术**头盔**，其他不变 |
| `06` | 把身体护甲 ID 放入头部并正确更新校验 | 头部变为 FS-55 蹂躏者**身体护甲**，其他不变 |
| Inspector | 玩家用游戏点击装备触发写盘，收集 ID | 只读文件监听已用于采集；不证明并发回写安全 |
| 当前 | 用户报告已采集所有身体护甲 ID | 重点转为易用的可视化配置器 |

### 3.2 字段结论与置信度

以下全部是**解压后逻辑正文 `payload` 的偏移**：

| 字段 | 偏移 | 宽度 | 编码 | 依据 |
|---|---:|---:|---|---|
| 已装备头部槽 | `0x0121` / 289 | 4 字节 | uint32 little-endian | 两个普通头盔和一个身体护甲的受控游戏测试 |
| 已装备身体护甲槽 | `0x0129` / 297 | 4 字节 | uint32 little-endian | 身体装备记录、默认样本及此前采集工作流支持 |
| 已装备披风槽 | `0x0125` / 293 | 4 字节 | uint32 little-endian | 物品类型匹配和默认配装观察支持；新工具 P0 默认不写 |

重要：`0x0121` 不是 4 字节对齐地址。Rust 不能把任意 `&[u8]` 直接转成 `&u32`；应使用范围检查、切片和 `u32::from_le_bytes`。

公开 ID 表或存档中其他位置可能出现相同 ID。**不可全局查找替换**。其他副本可能是历史、解锁记录、选择记录或遥测，不需要为了当前槽位修改而同步改写。

### 3.3 已经被用户辨认的少量 ID

| 物品类型 | 名称 | 十六进制 ID | 十进制 ID |
|---|---|---|---:|
| Helmet | FS-55 蹂躏者（头盔） | `0x056848E9` | 90720489 |
| Helmet | B-01 战术（头盔） | `0x261C4A52` | 639388242 |
| Armor | FS-55 蹂躏者（身体护甲） | `0xD3461392` | 3544585106 |
| Armor | B-01 战术（身体护甲） | `0x61B31723` | 1639126819 |
| Cape | 默认化后观察到的披风（用户称“歼敌战士”） | `0x4657CFB3` | 1180159923 |

这些值可以作为种子/测试案例。不要把 TG-8、TG-122 或其他后来采集的护甲 ID 猜出来。真实全量目录由用户导入。

### 3.4 必须保留的未知边界

- **头部槽接受身体护甲 ID** 已由用户实测；所有护甲都能这样使用，未逐件证明。
- **第二份被动实际生效**、同被动叠加、减伤公式、换弹速度倍率，均不能由装备 UI 自动推出。
- 原用户存档的当前客户端 build 没有可靠固定记录；不要写成某个未经核对的最新版本。
- 精确匹配布局头部是写入前置条件，不是未来字段语义不变的保证。
- “ID 曾被观察到”不等同于服务器授权、付费解锁或全局永久拥有。
- 只读采集时游戏运行正常，不等同于游戏运行时回写会热加载。
- 不保证账号风险为零，也不把速通社区规则当作发行方授权。

产品界面的正确状态词是“文件校验通过”“已保存到磁盘”“头部引用身体护甲”“用户已验证加载”；不要显示“被动必定叠加”“绝对安全”“永久不失效”。

---

## 4. 设计决策摘要：先锁定这十件事

1. **默认玩家模式，非 HEX 主界面。**高级二进制查看放到折叠诊断页。
2. **物品类型与目标槽位分离。**`ItemType::Armor` 能作为 `EquipmentSlot::Head` 的目标，这是核心需求，不能被常规头盔过滤器拦掉。
3. **默认保留身体。**只选头部护甲就能导出双甲存档；身体修改必须显式解除锁定。
4. **预设保存意图，不保存整份历史 `.sav`。**加载预设时把目标 ID 应用到用户确认的最新有效快照。
5. **不变更未知字节。**P0 修改范围只有头部、可选身体和必要校验。
6. **读写分离。**可以边玩边只读监听；写回源文件必须单独确认并先备份。
7. **磁盘快照、草稿、已提交结果是三个对象。**不要用同一个 mutable payload 到处共享。
8. **文件变了不暗中合并。**脏草稿遇到源文件更新进入冲突态；用户选择重新应用意图并再次审阅。
9. **离线为默认。**不下载整套武器/装备数据库，不上传存档；图片为可选本地素材。
10. **按顺序验证。**Rust codec 对齐 Python → 目录迁移 → 玩家 UI → 安全另存 → 备份回写 → Windows 实测。

---

## 5. `.sav` 格式完整说明

### 5.1 三个地址空间

定义并在代码中显式区分：

- `file`：磁盘上的压缩容器。
- `padded`：所有完整 LZ4 块解压后拼接的数据。
- `payload`：`padded[..logical_size]`，真正用于字段读取和内层校验的逻辑正文。

两处 `0x0C` 分别处在 `file` 与 `payload` 中，绝对不是同一项。可以引入 `FileOffset` 和 `PayloadOffset` newtype，避免整数混用。

### 5.2 外层头部与分块

| 文件偏移 | 类型/长度 | 本研究布局中的值或规则 |
|---:|---|---|
| `0x00` | 8 bytes | `D4 FD 1C 6B 5F 7B 23 CC`：被当前解析器作为格式签名；内部语义未知 |
| `0x08` | LE u32 | `1`：目前接受的标志，内部语义未确定 |
| `0x0C` | LE u32 | `CRC32(file[0x10..EOF])` |
| `0x10` | LE u32 | `file.len() - 0x1C`，不是“所有 LZ4 字节之和” |
| `0x14` | LE u32 | 逻辑正文长度 |
| `0x18` | LE u32 | `0xF0000001`：目前接受的标志 |
| `0x1C` | 8 bytes，按 LE u64 检查 | 与逻辑正文长度相同；也可能是低 32 位长度+保留零，语义尚未独立区分 |
| `0x24` | LE u32 | 第一个压缩块长度 |
| `0x28` | raw LZ4 bytes | 第一个压缩块 |
| 后续 | LE u32 长度 + raw LZ4 bytes | 直到规定块数解析完毕 |

这里的 LE u64 读法在已知样本中匹配，不允许因此改写未知的高位含义。保存时保留原头字段，只更新已经确认的长度/校验。

参数：

```text
BLOCK_SIZE       = 65536
KNOWN_LENGTH     = 572088      // 0x8BAB8
KNOWN_BLOCKS     = 9
PADDED_LENGTH    = 589824
TERMINAL_PADDING = 17736       // 这些字节在两个真实样本中都为 0
MAX_INPUT        = 16 MiB      // 应用防护上限，不是游戏规范
MAX_LOGICAL      = 16 MiB      // 同上
```

块数按 `ceil(logical_size / 65536)` 推导，不写死为 9；但是写入权限仍由已知布局匹配限制。每个块包括最后一块，都应解出 **65536 字节**。

### 5.3 逻辑正文头部

当前布局前 12 字节：

```text
06 01 00 00 3E EA CE A6 B8 BA 08 00
```

| 正文偏移 | 字段 | 规则 |
|---:|---|---|
| `0x00` | u32 `0x106` | 不擅自命名成游戏版本；保留 |
| `0x04` | u32 `0xA6CEEA3E` | 内部语义未知；保留 |
| `0x08` | u32 `572088` | 与逻辑正文长度一致 |
| `0x0C` | u32 | MurmurHash64A 低32位内层校验 |

写入布局识别等价于：

```text
payload.len() == 572088
AND payload[0..12] == hex("060100003eeacea6b8ba0800")
AND 容器结构及两层校验通过
```

不要因为哈希匹配就允许对其他头部套用 `0x0121`。`LayoutId` 可以命名为 `Hd2Observed0601A6ceea3e`，不要取一个没有证据的客户端版本号。

### 5.4 严格解码流程

按如下顺序实现，错误不应 panic：

1. 有界读取整个文件；总长度在 40 字节与 16 MiB 之间。
2. 检查签名、标志；未知容器直接拒绝语义解码，可以只展示原始文件 HEX。
3. 检查外层 CRC 和 `file.len()-0x1C`。
4. 验证逻辑长度范围和位于 `0x1C` 的副本。
5. 用 checked arithmetic 计算块数和游标，不分配文件宣称的无限空间。
6. 从 `0x24` 读每个 `u32 block_len`；拒绝零长、越界、截断、整数溢出。
7. 将 raw block 解压到固定 65536 字节缓冲区；返回长度必须正好为 65536。
8. 规定块数读完，游标必须等于 EOF；不接受额外隐藏尾部。
9. 拼接或按块访问；终端填充必须为零。
10. 切出逻辑正文，检查 `payload.u32(8)==logical_size`。
11. 计算并检查内层哈希。
12. 最后匹配已知布局，决定 `KnownWritable` / `DecodedReadOnly`。

`Corrupt` 与 `UnknownLayout` 是不同状态。错误 checksum 不得被默认“修好然后继续”。可以有诊断读取，但不能通过诊断模式获得写入许可。

### 5.5 内层哈希：最容易再次做错的部分

实际计算对象是**完整逻辑正文**，不是装备区、第一块或 padded 全长。计算前只把自身四字节清零。

```text
work = copy(payload)
work[0x0C..0x10] = 00 00 00 00
full_hash = MurmurHash64A_LE(work, seed=0)
stored = low32(full_hash)
write_u32_le(payload, 0x0C, stored)
```

MurmurHash64A 的逐步规则（非 MurmurHash3、非 xxHash）：

```text
m = 0xC6A4A7935BD1E995
r = 47
h = seed XOR (len(data) * m)                  // 全部乘法 wrapping u64
for each complete 8-byte little-endian chunk:
    k = read_u64_le(chunk)
    k = k * m
    k = k XOR (k >> r)
    k = k * m
    h = h XOR k
    h = h * m
if remaining bytes exist:
    tail = little-endian integer of those 1..7 bytes
    h = h XOR tail
    h = h * m
h = h XOR (h >> r)
h = h * m
h = h XOR (h >> r)
return h
```

Rust 要用 `wrapping_mul`，移位为无符号逻辑右移。不要依赖 release 模式溢出而在 debug 测试里 panic。算法参考与格式推导是两类来源：MurmurHash64A 是公开算法，本存档的清零区间、覆盖范围和截取规则来自样本实验。[W5]

### 5.6 外层校验

```text
outer = CRC32_IEEE(file[0x10..EOF])
file[0x0C..0x10] = outer.to_le_bytes()
```

采用与 Python `zlib.crc32` 一致的 CRC32 IEEE；不是 CRC32C。`crc32fast` 的 API 文档明确其算法为 IEEE，可作为候选实现；用金标准向量复核，不靠名称相似判断。[W7]

### 5.7 重打包与字节保真

输入：原始有效 `SaveImage` + 小范围 `PatchSet`。不可用 JSON 重建完整人物记录。

1. 在原始 `payload` 的副本上应用已确认字段的 4 字节修改。
2. 更新内层哈希。
3. 若刷新校验后的正文与原正文相同，直接返回原始 `raw`；**无修改另存应逐字节相同**。
4. 保留原末尾填充；对每个完整解压块做字节比较。
5. 未变块复用原始压缩块。变化块使用 raw LZ4 压缩，不加 frame、不加未压缩长度前缀。
6. 输出原 `file[..0x24]` 加新的长度+块序列。
7. 更新 `0x10` 总长度字段，再更新外层 CRC。
8. 对结果重新严格解码，核对目标正文逐字节一致。
9. 核对业务修改白名单：P0 双甲 only `[0x121,0x125)`、可选 `[0x129,0x12D)`，以及哈希 `[0x0C,0x10)`。

**每个允许的四字节范围不一定四字节全部发生差异。**测试应断言“所有实际差异均包含于允许范围”，不要硬断言必须恰好 8 或 12 个字节不同。

正文任意块改动都会更新位于第 0 块的内层哈希；因此即使修改第 1 块，第 0 块也可能要重新压缩。

Rust 的 `lz4_flex::block::compress` / `decompress_into` 属于合适的 raw-block API 方向；不要使用文档首页示例的 `compress_prepend_size` / `decompress_size_prepended`，它们会添加本格式没有的长度前缀。固定输出缓冲区并验证解压长度。[W6]

不同 LZ4 实现可以生成不同压缩字节，这不代表错误。修改后验收比较解压语义、校验和未改压缩块；无修改时才要求整个容器 byte-identical。

### 5.8 两份真实历史样本的对照值

本次重新运行 Python 参考实现复核过下表；原始二进制未随包分发。

| 项目 | 正常原始样本 | 默认化后 `after_02.sav` |
|---|---|---|
| 文件长度 | 7040 | 2767 |
| 正文长度 | 572088 | 572088 |
| 内层低32位 | `0x657DC575` | `0x71F87E8C` |
| 外层 CRC32 | `0x7BB49FA2` | `0x974F3669` |
| 头部 ID | `0xC1611AC9`（名称未知） | `0x261C4A52` |
| 身体 ID | `0xD3461392` | `0x61B31723` |
| 披风 ID | `0x53B64BF8` | `0x4657CFB3` |

这是历史验证数据，不可拿来构造其他账号存档。详细可用的公开跨语言 fixture 另见 `fixtures/manifest.json`。

---

## 6. 护甲目录：兼容用户已经采集的数据

### 6.1 旧 JSON 的真实结构

这个结构来自已经交付的 `Catalog.save()`，不是设想。数字是 JSON 整数，offset 是**十进制**：

```json
{
  "schema": 1,
  "records": [
    {
      "offset": 297,
      "value": 3544585106,
      "field": "身体护甲槽",
      "name": "FS-55 蹂躏者（身体护甲）",
      "note": "",
      "first_seen": "2026-09-19T00:00:00.000+00:00",
      "last_seen": "2026-09-19T00:00:01.000+00:00",
      "count": 2,
      "source_sha256": "原工具保存的来源文件SHA256"
    }
  ]
}
```

示例里的名称/ID 是少量已知记录；时间是演示，不是用户全量目录。可解析版本见 `examples/legacy_catalog.schema1.example.json`。

旧版去重键：

```text
format!("{offset:08X}:{value:08X}")
```

因此同一个身体护甲 ID 被放在头部和身体时，在旧表里可能是两条记录。这两条记录不能错误地生成“一个 Helmet 物品和一个 Armor 物品”。**观察槽位只是 provenance，不是物品类型。**

### 6.2 旧 CSV 的真实列

UTF-8 with BOM，标准 CSV 引号转义：

```text
字段,正文偏移,ID_十六进制,ID_十进制,名称,备注,首次采集_UTC,末次采集_UTC
```

- 偏移通常为 `0x0129`，十六进制 ID 通常为 `0xD3461392`。
- 十进制 ID 与十六进制 ID 同时存在且矛盾：报出行号并拒绝该行，不静默取其一。
- `value` 范围为 `0..=u32::MAX`；不能用 i32，否则很多护甲 ID 变成负数。
- CSV 可能有名称中的逗号、双引号、换行；不能 `split(',')`。
- 旧导出对 `= + - @ \t \r` 起始的文本加了单引号防公式注入。导入器不要任意去掉全部前导单引号；保留原始文本、提示可人工更正。再次导出仍需防公式注入。
- 完整旧 schema 元数据只能从 JSON 保留；CSV 不含 count/source_sha256 时设为缺失，而非伪造。

### 6.3 新目录模型

推荐让 **Item 与 Observation 分开**，并保留 raw legacy record 以便审计：

```text
Item
  item_key: 带类型命名空间的稳定字符串，例如 armor:0xD3461392
  id_u32: u32
  item_type: armor | helmet | cape | unknown
  display_name: 用户可编辑中文名
  aliases: 多名称/冲突名称
  classification: 类型如何确定
  optional metadata: 重量级、被动标签、债券、图片、收藏
  observations: 关联的观察证据

Observation
  observed_offset: payload offset
  observed_slot: head/body/cape/unknown
  first_seen / last_seen / count
  source_digest: 可选，仅本地保留
  source_format: legacy_json / legacy_csv / live_capture / user_manual
```

不用显示名称做唯一键；同名头盔和护甲要分开。ID 作为二进制槽位值是 u32，但目录可以用 `(item_type,id_u32)` 区分不同数据来源的类型冲突。遇到同一 raw ID 被可靠来源赋予不同类型，显示冲突记录，不自动覆盖。

完整可校验的 v2 示例和 JSON Schema 在 `examples/equipment_catalog.v2.example.json` 与 `schemas/equipment_catalog.v2.schema.json`。

### 6.4 导入分类优先级

1. 本工具自有 v2 目录已经人工确认的类型：保持。
2. 同一个 raw ID 已有可靠物品映射，例如用户已测试的 FS-55 Armor：复用为 Armor，即使新 observation 来自 head。
3. 未有映射，但仅从 `0x0129` 身体槽正常采集的记录：可以建议为 Armor，标注 `inferred_from_body_observation`；导入预览让用户一次性确认“这些是我逐项正常穿着身体护甲采集的记录”。
4. 来自 `0x0121`：只确定 observed_slot=head。不能一律归为 Helmet，因为双甲状态已经打破这个假设。
5. 名称包含“护甲/头盔”、字段自定义名字、其他偏移：仅作为提示，不把自然语言当作可信类型系统。
6. 无法分类保留 Unknown，仍能搜索/看 ID，不自动塞进推荐列表。

导入 UI 必须显示：总记录、建议身体护甲数、普通头盔数、未知数、冲突数、去重后物品数。确认提交前不得改写用户原文件；导入应事务化，要么整个选定变更提交，要么保留现有目录。

### 6.5 缺少图片、被动、译名不阻碍使用

用户已采集“ID”，不代表每条都有名称、图片、重量、被动文案。允许只含 ID 的最小可用记录。

- 未命名显示 `未命名护甲 · 0x12345678`，可就地命名。
- 图片缺失使用可区分类型的本地占位图，不下载或伪造。
- 被动字段缺失显示“未录入”，不要根据颜色/名称猜能力。
- 名称冲突保留 aliases；导入预览选择主名称。
- 不修改导入的原始 `catalog.json`；新目录写入自己的 workspace。
- 内置任何被动说明需带来源、核验日期和“装备描述/头部实测”两个分离字段。

### 6.6 目录与权限边界

导入的 JSON、CSV 都按不可信输入处理：大小、条目数、字符串长度上限；不跟随里面的脚本、外部命令或网络 URL。图片路径必须是工作区 assets 内的相对路径；拒绝绝对路径、`..`、UNC、目录越界和超大图片。目录中的 `source_sha256` 只用于来源追踪，不作为绑定某个 Steam 账号的凭据。

---

## 7. 玩家界面：双甲选择必须比 HEX 编辑容易得多

### 7.1 主界面结构

建议产品暂名 **HD2 Armor Desk / HD2 双甲配置器**。最终名称不影响架构。

这是文字布局规格，不是已实现截图：

```text
┌ HD2 双甲配置器 ─ 文件/账号路径 ─ 校验通过 ─ 只读监视中 ─ 打开 ┐
│ 配装     预设     我的护甲     备份     高级                    │
├──────────────────────────────────────────────────────────────┤
│ 当前磁盘 → 当前草稿                  源文件更新时间 / 冲突状态 │
│ ┌ 头部槽 ─────────────────┐ ┌ 身体槽 ──────────────────────┐ │
│ │ FS-55 蹂躏者（身体护甲） │ │ 当前身体：FS-55             │ │
│ │ 标签：身体护甲放于头部   │ │ [✓ 锁定身体，保持当前]     │ │
│ │ [选择头部护甲] [还原头盔]│ │ [选择身体护甲]             │ │
│ └─────────────────────────┘ └────────────────────────────┘ │
│ 披风保持不变；其他武器、角色与未知数据保留                     │
├──────────────────────────────┬───────────────────────────────┤
│ 搜索 名称/编号/ID             │ 所选物品详情                  │
│ 目标：头部 | 身体             │ 名称 + 类型 + ID             │
│ 收藏 / 被动 / 重量 / 债券      │ 可选的被动资料与验证状态      │
│ [护甲卡片][护甲卡片][护甲卡片]│ [用于头部] [用于身体]        │
│ [护甲卡片][护甲卡片][护甲卡片]│                               │
├──────────────────────────────┴───────────────────────────────┤
│ 待修改：头部 旧 → 新；身体保持不变                             │
│ [撤销] [保存为预设]             [另存 SAV…] [应用到源文件…]   │
└──────────────────────────────────────────────────────────────┘
```

### 7.2 头部卡片与身体卡片

两个卡片都同时显示：**目标槽位**和**物品实际类型**。

头部放入护甲时固定文案：`头部槽 · 身体护甲`。不要只展示“FS-55 蹂躏者”，否则会重复本次实验早期对同名物品的误解。

身体锁默认开启。点击“用于身体”时如果锁定，明确请求解除；不能顺手改掉身体。两个槽位的卡片各有“恢复磁盘当前值”按钮，撤销只改草稿不写磁盘。

头部选择器默认筛选 **Armor**，并提供“普通头盔”分组用于恢复。身体选择器只提供明确 Armor；Unknown 可在高级模式手工确认，不自动转型。不要为了复用普通装备下拉框，把 head 的允许类型写死为 Helmet。

### 7.3 推荐的高频操作流

**流程 A：只换头部护甲**

1. 打开文件，看到已验证的现有头部和身体。
2. 搜索目标护甲，点击卡片上的“用于头部”。
3. 查看底栏差异“只改头部”。
4. 选择另存，或在停止游戏写入后确认应用。

完成选择到保存不超过约 4 个主要操作；不要求用户输入 ID、不跳进 HEX 页。

**流程 B：更换一对双甲**

选头部 → 解除身体锁 → 选身体 → 一次预览 → 一次生成/提交。两处写入是一项用户命令，undo 一次可以撤销整对变更。文件中不可出现“只写好头部、身体没写完”的中间状态。

**流程 C：保留当前身体，切换收藏头部**

头部收藏列表/快捷栏可直接选。依然只暂存；不因点击卡片自动覆盖 `.sav`。玩家的高频选择不需要每次风险弹窗，只有真正回写才确认。

**流程 D：恢复正常头盔**

优先提供“本次改为双甲前记录的普通头盔”。如果打开存档时已经双甲、没有已知普通头盔，则选择普通 Helmet 列表；不可把“首次看到的头部值”无条件当普通头盔。默认 B-01 仅可作为已核验名称的一个选项，不能未经同意直接替用户选择。

### 7.4 搜索与筛选

P0：中文名、英文别名、型号 `FS-55` / `TG-8`、八位十六进制、十进制 ID；大小写不敏感；忽略常见空白与型号连接符差异，但 ID 精确匹配不做近似。

支持收藏优先、最近使用。重量/被动/债券筛选是 metadata 存在时的增强项，不因缺少 metadata 隐藏已知护甲。完整未命名 ID 仍必须可见。

卡片点击只选中；明确按钮或双击规则将其用于当前目标。拖拽可作为 P1，不能成为唯一操作方式。主状态应在鼠标之外用键盘完成。

### 7.5 差异预览与保存语义

底部显示的是人可读差异：

```text
头部：FS-55 头盔 → TG-8 身体护甲
身体：保持当前（FS-55 身体护甲）
披风 / 武器 / 角色：不修改
必要的内层/外层校验：自动更新
```

如果名称未知，同时显示 ID。技术详情可展开为偏移、旧 bytes、新 bytes；默认不让用户理解 CRC。

至少区分四种动作：

- **保存预设**：只写工具的配装意图 JSON，不改游戏。
- **另存 SAV**：生成新文件，不覆盖当前源，不让“磁盘当前值”冒充已改变。
- **应用到源文件**：确认、备份、冲突检查后写回。
- **恢复备份**：明确告知这会恢复整个快照，而非只恢复两件护甲。

`Ctrl+S` 默认为“另存 SAV”，`Ctrl+Shift+S` 可绑定源写回确认；快捷键语义在界面提示。`Ctrl+O` 打开，`Ctrl+F` 搜索，`Ctrl+Z/Ctrl+Y` 草稿撤销/重做，F5 读取。

### 7.6 窗口与中文细节

设计目标：默认 1180×780 DIP；最低 900×620 DIP 下保存按钮仍可见，主要面板可滚动。支持 100%/125%/150%/200% Windows 缩放、长中文路径和长装备名称。

Windows 中文输入法组合文本不能被快捷键截断；搜索时保持焦点，不因文件更新重建 InputState。设置系统字体回退，不随包分发系统字体文件。未知图片、缺失字形、渲染器初始化失败都应可诊断，不能出现无提示白屏。

深浅主题可以有，但第一版先保证易读和状态对比。红色仅用于真实错误，黄色表示风险/冲突，蓝色或明显徽标表示未保存；不要仅用颜色传达状态。

---

## 8. 预设：保存“要做什么”，而不是保存某一天的整份档

### 8.1 预设的数据契约

建议 `LoadoutPreset`：

```text
schema_version
preset_id
name / note / tags
head: Keep | Set(ItemRef)
body: Keep | Set(ItemRef)
created_at / updated_at
optional: game_build_observed, effect_notes
```

**`Keep` 与 `Set(0)` 不相同**。空值不表示装备 ID 0，也不表示删除槽位。`ItemRef` 保存 raw ID、类型与名称快照，目录改名不改变它的游戏含义。

P0 不把披风纳入写入预设；保留只读展示。预设不含完整 `.sav`、账户 ID、源文件路径、遥测和 checksum。对外分享应只有装备选择与备注。

### 8.2 应用预设的基线

“应用预设”指把意图展开为基于**最新有效、由用户确认的磁盘快照**的草稿。不是用创建预设当天的整份文件覆盖。

例：昨天预设“头部 FS-55，身体保持”；今天用户换了主武器与角色名片。今天应用预设必须保留今天的主武器和名片。要把这例写成自动化验收测试。

### 8.3 目录项缺失或类型变化

预设引用的 ID 不在当前目录时：显示“未找到目录条目”，仍保留 ID 和名称快照；不自动换成名字接近的护甲。没有可信 Armor 类型时阻止普通玩家模式自动应用，可让用户重新映射/导入缺失目录。

预设中两个槽位引用相同护甲时允许保存，但只提示“同护甲重复；被动是否重复叠加未知”，不宣称 2 倍效果。

### 8.4 同时编辑与监听

套用预设有脏草稿时需确认替换草稿或将其保存为预设；不能悄悄覆盖用户未保存选择。保留 last-good disk 与 draft baseline 两份快照，别让 watcher 的下一帧重置卡片选择。

---

## 9. 状态机、监听与文件身份

### 9.1 不要用一个 bool 表达所有状态

建议用三个正交状态轴，而非一个无法组合的巨型 enum：

```text
DocumentStatus: Empty | Loading | ValidKnown | ValidUnknown | ReadError
WatchStatus: Off | WaitingStable | Following | RetryableError
DraftStatus: Clean | Dirty{base_revision} | Conflict{base_revision,disk_revision}
CommitStatus: Idle | Preparing | BackingUp | Replacing | Verifying | Result
```

`InvalidWriteInProgress` 是 watcher 对未完成写盘的临时读取结果，不会清空上次已确认的有效快照。

### 9.2 所有异步结果带身份

每次打开新路径、切换账号或重新载入文档，递增 `DocumentGeneration`。后台结果带 `{generation,path_identity,raw_sha256}`。旧任务延迟返回时，如果 generation 不匹配，就丢弃，不能把 A 账号的数据画到 B 账号界面或写回 B。

Path identity 至少包括规范化路径；Windows 处理大小写、长路径和同文件别名，必要时记录 volume/file ID。相对路径导入后固定到绝对路径。不要只比较文件名 `testament_new.sav`。

### 9.3 稳定读取算法

保留原工具的思想：默认 polling=500 ms，settle=250 ms；这些是设计默认，不保证捕获每个瞬间状态。

1. 读前获取文件 metadata。
2. 有界读取，短暂打开文件，不长期持有会阻止游戏改名的句柄。
3. 读后 metadata 与长度一致，否则作为 pending 重试。
4. raw SHA256 与上次已接受快照相同，跳过解压。
5. 新 SHA 在至少 settle 时间内稳定后，解压并检查两层校验。
6. 成功才发布新 Snapshot；失败保留 last-good，限频提示并重试。

可用文件系统事件做唤醒优化，但事件不等于完整存档已写好；仍需上述 gate。监听父目录以应对游戏 rename-replace 保存，不依赖一个已经被替换的文件 handle。

连续快速点击产生并消失的中间版本无法事后恢复，不向玩家保证“100% 全事件抓取”。监听暂停不阻止游戏写盘。UI 最小化时可降低轮询频率，无需高帧率空转。

### 9.4 clean 与 dirty 的外部变化

- Clean + 新 Snapshot：更新当前显示与基线，反映新头部/身体。
- Dirty + 相同 raw SHA：不变。
- Dirty + 不同 raw SHA：保留草稿与原基线，进入 Conflict，显示磁盘新的配装，不暗中 merge。
- Conflict：提供“放弃草稿并跟随磁盘”“保存草稿为预设”“以最新磁盘重新应用本次意图并预览”。第三项必须是新的基线、新的修订号和新的确认，不是绕开冲突检查。
- 保存自己的输出后 watcher 再看到同 SHA，不当作外部冲突。

原 Python 的 `conflicts` 比较整个 raw SHA，是保守策略：即便只是压缩输出不同或遥测改变，也需要重新审阅。第一版不必为了减少提示而设计复杂自动三方合并。

---

## 10. 保存事务与恢复：应用的最高优先级

### 10.1 三个不变量

**I1：用户点选卡片、收藏、命名、导入目录时，绝不写游戏源文件。**  
**I2：实际生成的 `.sav` 除明确选定字段与必要校验外，不改变其他逻辑正文字节。**  
**I3：不能以任何静默方式覆盖不是草稿基线的磁盘版本。**

这三个不变量应分别对应单元测试、集成测试和 UI 测试，不只写在 README。

### 10.2 另存

- 默认文件名可以带预设名与时间，但净化 `<>:"/\\|?*`、尾点空格、保留设备名，限长度。
- 用户指定新路径；禁止与当前源文件同身份。所有导出类型（JSON、CSV、正文 BIN）也不得覆写源 `.sav`。
- 目标已存在默认拒绝，或明确“换名”流程；不能先 truncate 再验证。
- 先在内存构建并校验，通过后再创建文件；失败无半截最终文件。
- 另存完成不自动把“当前磁盘源”切换到输出、不标记原文件已更新。草稿可以保留，状态显示“副本已保存，源未改”。

### 10.3 回写源文件

正常玩家模式中“应用到源文件”只有满足以下条件才开放：已知布局、两层校验通过、草稿非空、无冲突、目标物品类型已确认、无进行中的提交。游戏运行状态若能可靠检测到，则阻止回写并引导停止游戏；无法确定时不能显示“游戏已退出”，使用“未确认”并要求用户确认没有外部写入。

不需要查看游戏内存；可选的进程存在检查也只是辅助，不是锁或授权。任何情况下不自动杀进程或关闭 Steam。

事务阶段：

1. 冻结一个不可变 `CommitRequest`：源路径、expected raw SHA、generation、草稿意图。
2. 重新稳定读取源文件，检查 SHA 与基线一致，否则拒绝。
3. 构建目标 bytes；严格解码；检查允许变更范围。
4. 创建独立、唯一、不会覆盖旧备份的原始 bytes 备份；flush/sync 并验证 SHA。备份失败则终止。
5. 在源文件**同目录同卷**创建临时文件，写入目标 bytes，flush/sync；临时文件不能被 watcher 当目标。
6. 再次检查源 SHA 与 generation。
7. 使用经过 Windows 集成测试的替换流程；不要原地逐字节修改活文件，不先删源文件再移动。
8. 重新读取源，校验、核对输出 SHA。
9. 记录 commit receipt：前后 SHA、时间、备份位置、改动字段、结果状态。成功才更新当前基线。

Microsoft `ReplaceFileW` 可替换并保留相应属性，但存在具体失败状态；文档也明确 `REPLACEFILE_WRITE_THROUGH` 标志不受支持。选择该 API 时按文档处理错误，不能假设所有失败都意味着源文件完全没变。官方要求它参与的原文件、替换文件和由它管理的备份在同卷。[W8]

本设计的 workspace 备份可以由程序提前复制验证；若又使用 `ReplaceFileW` 内置备份，需满足同卷要求，不要把另一盘的 workspace 路径直接传入。ACL、只读属性、长路径和文件占用必须实测。

### 10.4 竞争条件与结果分类

双重哈希检查 + replace 仍不是与游戏协调的跨进程 compare-and-swap。检查与替换之间存在窗口；已生成备份也不能消除账号/服务端风险。

建议返回：

- `CommittedVerified`：回读与目标一致，文件层成功。
- `RejectedBeforeWrite`：冲突或前置条件失败，明确未覆盖。
- `CommittedButSuperseded`：替换后检测到外部更新；不能提示成功并自动重试覆盖。
- `Indeterminate`：OS 替换返回复杂错误，无法确认最终文件归属；保留现场与备份，显示处理步骤。

不要用 generic `Err` 自动清理掉唯一的原件备份；不要对冲突进行无限写回重试。

### 10.5 恢复的两种含义

**只恢复普通头盔**：基于最新快照写普通 Helmet ID，保留最新武器/身体等其他字段。应是常用操作。

**恢复备份 `.sav`**：恢复整个快照，可能回滚武器/角色/设置。确认对话框必须说明范围；恢复前也备份当前文件，并适用同样的冲突和停止写入要求。

Steam Cloud 可能在会话前后同步文件，所以文件层成功不证明后续不会被另一份云副本覆盖。工具可展示提醒，但不自动禁用云、不删除 `remotecache.vdf`、不清空 `remote`。[W9]

---

## 11. Rust + GPUI 技术路线

### 11.1 2026-09-19 核验到的上游状态

GPUI 官方仓库仍提示 pre-1.0 与版本间破坏性变化；当前主干 README 出现 `gpui_platform::application()` 的平台入口，Windows 说明为 Win32 窗口与 DirectWrite 文本。不要拿数年前“GPUI 只能在 macOS 使用”的文章当现状，也不要混用历史 `App::new`、`Application::new` 与新平台入口。[W1]

GPUI Component 的旧站当前转向 **GPUI Kit**。本次打开的新入门文档使用 `gpui-kit = "0.6"`，通过该包重导出 GPUI/组件，示例包含 `gpui_kit::application()`、`gpui_kit::init` 与 Root。**这只是本次查到的文档系列，不是本项目已编译验证的依赖组合。**[W2]

为减少输入框、列表、弹窗和中文输入法的重复劳动，推荐评估 GPUI + 兼容的 GPUI Kit 组件层。若组件层导致体积/版本难以控制，可使用明确 pin 的 GPUI 与少量自建组件。无论选哪条路线，不切换框架逃避用户的 Rust+GPUI要求。

### 11.2 第一个里程碑必须是 Windows 最小可运行切片

在功能编码前完成：窗口、中文 Input、一个护甲卡片、选中文件、弹窗、后台任务回主线程、退出。

记录：

```text
rustc -Vv
cargo metadata --locked
Cargo.lock
实际 gpui / gpui-platform / gpui-kit 的版本或 git commit
Windows 版本、MSVC、SDK、渲染器初始化结果
```

不要在项目里提交 `version="*"` 或不带 rev 的浮动 git main。先按所选上游文档跑通，再 pin 整组依赖和 Rust toolchain。`cargo tree -d` 检查是否带入多个不兼容 GPUI 实体类型；不能同一 UI 混用两份 GPUI 的 `Entity`。

官方 Windows 构建说明涉及 Rust、MSVC/Build Tools、Windows SDK 和部分依赖用的 CMake；这是 Zed 项目的说明，不等于本小工具一定需要 Zed 全部依赖。按实际最小目标构建报错核对，最终向用户交付可运行 release 程序，而非要求其安装整套开发环境。[W3]

### 11.3 GPUI 实现注意事项

这些是架构约束，具体 API 拼写以 pin 的源码为准：

- 顶层保留一个 `Entity<WorkspaceView>`；InputState、选择器状态等在构造时创建，不在每次 `render` 新建。
- 子视图持有需要的共享实体/弱引用，避免生命周期循环和刷新后丢焦点。
- UI 线程维护界面状态；文件 I/O、LZ4、哈希、目录导入在后台执行。后台不直接访问 Window 或实体内部状态。
- 后台任务返回纯 Rust 数据，加 generation 校验后在前台 update，最后 notify。
- 保存订阅/任务句柄，避免离开局部作用域后监听停止；窗口关闭取消任务，过期结果不再更新 UI。
- 组件层使用 Root 时按对应版本初始化，并渲染对话框、通知等 overlay 层；否则可能出现“按钮触发了但弹窗看不到”。
- `Item`、`Preset`、`SaveImage` 不依赖 GPUI 类型；`SharedString` 只在 UI 边界转换。
- 虚拟列表/列表行稳定 ID 由 item_key 派生，不能用每帧随机值，否则选中和图片缓存丢失。
- 不在 `render()` 进行文件读取、反复 hash、构造全量 hex 字符串、写 catalog 或 spawn 永久任务。

GPUI 的实体/Render/后台执行器概念来自官方；GPUI Kit 入门文档也强调保留 state entity 与 Subscription。[W1][W2]

### 11.4 建议依赖职责（不是未经测试的锁定清单）

| 依赖方向 | 职责 | 约束 |
|---|---|---|
| gpui / 对应平台层 / 可选 gpui-kit | Windows UI、组件、输入、绘制 | 最小切片通过后固定兼容组合 |
| lz4_flex 的 block API | raw LZ4 | 禁用不需要的 frame；用 fixture 与 Python互解 |
| crc32fast | IEEE CRC32 | 对照 zlib 测试，不是 CRC32C |
| 手写小型 MurmurHash64A 移植 | 内层 hash | 根据本包向量全部通过；不换算法 |
| serde / serde_json / csv | 本地目录、预设、导入导出 | u32 严格解析，未知数据保留/拒绝策略明确 |
| sha2 或同等库 | 文件 SHA256 与冲突检查 | 不是游戏校验的替代品 |
| thiserror | 可区分领域错误 | 不丢阶段、错误码、是否已写入信息 |
| windows / windows-sys | 必需的文件身份与替换桥接 | 只启用用到的 Win32 features |
| 标准线程/通道或GPUI后台执行器 | I/O与解码任务 | 小工具不必额外拉一套复杂服务框架 |
| 可选 notify | 文件事件唤醒 | 不替代稳定读取和校验 |
| tracing（或等价本地日志） | 去敏诊断 | 不打印完整 payload、账号内容或全路径到共享报告 |

### 11.5 轻量级的含义

轻量优先指：运行不依赖 Python/Electron，不联网，不启动常驻服务，不扫描整盘，不按帧处理存档、不把所有大图常驻 GPU。

建议性能目标（设计预算，不是已测结果）：

- 500KiB 级正文正常读取/展示不阻塞输入；主要交互不出现可感知的长停顿。
- 用户在已有目录里搜索/选卡，目标反馈 <50ms；慢导入/保存显示进度。
- 已完成初始化后的空闲无动画、不持续重绘；轮询工作只在需要时解压。
- 将启动时间、空闲 RSS、CPU/GPU 占用和 EXE 大小列入验收记录，先实测再设硬门槛；不拿旧Python内存数值当GPUI承诺。
- 容量测试至少 500 个物品和 10,000 条 observation；这只是压力规模，不宣称游戏有这些数量。

---

## 12. 建议目录与领域接口

### 12.1 工程拆分

```text
hd2-armor-desk/
  Cargo.toml                     workspace
  Cargo.lock
  rust-toolchain.toml
  crates/
    sav_codec/
      src/lib.rs                 有界 decode/encode
      src/container.rs           原始头/块/游标
      src/checksum.rs            MurmurHash64A 与 CRC
      src/layout.rs              KnownWritable / ReadOnly
      tests/golden.rs
    loadout_domain/
      src/lib.rs
      src/catalog.rs             Item、Observation、导入规范化
      src/preset.rs              Keep / Set 配装意图
      src/draft.rs               不可变基线、草稿、撤销
      src/validation.rs          白名单、类型与冲突
    local_io/
      src/lib.rs
      src/monitor.rs             稳定读取和 generation
      src/catalog_store.rs       原子目录保存与迁移
      src/save_transaction.rs    备份、冲突、替换、回读
      src/platform_windows.rs    Windows 文件身份/替换
      src/discovery.rs           受限 Steam 路径发现
    app/
      src/main.rs
      src/workspace.rs
      src/views/loadout.rs
      src/views/catalog.rs
      src/views/presets.rs
      src/views/backups.rs
      src/views/diagnostics.rs
      src/components/armor_card.rs
      src/components/slot_card.rs
      src/actions.rs
  assets/                        只放合法分发的图标/占位图
  schemas/
  tests/fixtures/                复制本包合成向量
  scripts/                       Windows build/test/package
  docs/
```

可以适度合并文件，但不能把全部编解码、UI、存档写回和JSON迁移塞进一个 GPUI view。不需要引入微服务或数据库。

### 12.2 核心类型契约

以下为设计接口草案，不声称已编译；接手者可调整语法，但保留其不变量与边界。

```rust
pub struct ItemId(pub u32);
pub struct PayloadOffset(pub usize);
pub struct Sha256Digest(pub [u8; 32]);
pub struct DocumentGeneration(pub u64);

pub enum ItemType { Armor, Helmet, Cape, Unknown }
pub enum EquipmentSlot { Head, Body, Cape }

pub enum LayoutSupport {
    KnownWritable,
    DecodedReadOnly { reason: String },
}

pub struct ItemRef {
    pub item_key: String,
    pub id: ItemId,
    pub item_type: ItemType,
    pub label_snapshot: String,
}

pub enum SlotIntent {
    Keep,
    Set(ItemRef),
}

pub struct LoadoutIntent {
    pub head: SlotIntent,
    pub body: SlotIntent,
}

pub struct FieldPatch {
    pub offset: PayloadOffset,
    pub before: [u8; 4],
    pub after: [u8; 4],
}
```

`ItemRef`、`SlotIntent` 在实现时派生恰当的 Clone/serde，所有导入反序列化后仍要做语义验证。普通 UI 不允许建立 `Set(Unknown)` 的提交意图。

### 12.3 编解码接口

```text
SaveImage::decode(raw: Vec<u8>) -> Result<SaveImage, DecodeError>
SaveImage::payload() -> &[u8]
SaveImage::raw() -> &[u8]
SaveImage::support() -> LayoutSupport
SaveImage::read_u32(offset: PayloadOffset) -> Result<u32, DecodeError>
SaveImage::encode_patches(patches: &[FieldPatch]) -> Result<Vec<u8>, EncodeError>
```

内部 SaveImage 保留原 raw、每块原压缩 bytes/范围、每块完整解压结果或可复用数据、logical payload、校验报告。验证 `.before` 与原始字节匹配；重叠 patch、受保护头部、大小变化都拒绝。

`encode_patches` 的技术层可支持已知布局中的等长 patch，但 P0 应用层只能生成头/身体白名单。高级 HEX 编辑属于 P2，不应把任意偏移输入泄露到普通玩家表单。

### 12.4 目录与意图接口

```text
import_legacy_json(bytes) -> Result<ImportPreview, ImportError>
import_legacy_csv(bytes) -> Result<ImportPreview, ImportError>
resolve_import(preview, user_resolutions) -> Result<CatalogDelta, ImportError>
apply_catalog_delta(current_catalog, delta) -> Catalog

resolve_intent(save_image, catalog, intent) -> Result<Vec<FieldPatch>, ValidationError>
preview_intent(save_image, intent) -> Result<HumanReadableDiff, ValidationError>
```

ImportPreview 包含可导入条目、被忽略项、冲突、未知分类、源记录；不能直接修改持久目录。resolve_intent 对 `Keep` 不产生 patch；`Set` 与原值相等也不产生 patch。

### 12.5 提交接口与 receipt

```text
prepare_commit(snapshot_revision, intent) -> Result<PreparedCommit, CommitError>
save_copy(prepared_commit, new_path) -> Result<CopyReceipt, CommitError>
commit_to_source(prepared_commit, expected_revision, backup_root)
    -> Result<CommitReceipt, CommitError>
```

PreparedCommit 是不可变结果，包含 `source_identity`、`generation`、`expected_sha`、输出 bytes、before/after 字段及输出 SHA。CopyReceipt 和 CommitReceipt 类型分开，避免另存返回后 UI 错当源文件已更新。

CommitError 中保留 `phase` 和 `write_state`；UI 对 uncertain outcome 不能复用“未修改源文件”提示。

### 12.6 GPUI 与领域层之间

UI 发出命令：OpenSave / ImportCatalog / SetHead / SetBody / LockBody / Undo / ApplyPreset / SaveCopy / Commit / RestoreBackup。

工作线程发布事件：ValidSnapshot / PendingRead / ReadError / ImportPreviewReady / CommitStage / CommitResult。所有会修改当前工作区的事件包含 generation。

不要从 armor card 的 click callback 直接调用文件写入。card 只发领域命令，命令更新草稿，保存服务在另一个显式用户操作中运行。

---

## 13. 工作区文件、迁移与隐私

建议默认 `%LOCALAPPDATA%\HD2ArmorDesk\`，支持 `--workspace <path>` 和便携模式。不是强制放到 EXE 旁边，也不放到 Steam remote 中。

```text
workspace/
  catalog.v2.json
  catalog.imports/               原目录副本或导入报告；不覆盖用户原目录
  presets.json
  settings.json
  backups/                       真实原始 .sav，仅本机
  receipts/                      本地保存结果记录
  assets/                        用户自有图片
  logs/                          轮转、去敏
```

- 目录/预设 JSON 更新先写临时文件，sync 后替换，保留上一次可用版本。
- 导入版本不识别时停止迁移，不能覆盖成空表。
- 数据 schema 升级显式 backup + migration；向前不兼容字段保留或拒绝，不能吞掉数据。
- 旧目录含完整 SHA/时间，为可关联信息；分享导出默认只导出物品 ID、类型、名称/备注，不导出来源快照指纹和私人路径。
- 真实 `.sav`/正文可能含 user_id、session_id、machine_id 等。不要做自动错误报告上传；诊断包默认不包含存档。
- 日志不输出完整 payload。必要的 field ID 不属于完整账户记录，但路径可打码。
- 文件名和图片路径以不可信字符串处理，不执行 shell、不拼接用户输入为命令。
- 首版只接受用户主动打开/导入；不能自动递归搜遍磁盘找存档。
- 备份的删除/保留策略：默认不自动删最后一个成功前备份；清理有确认和大小/数量提示。普通设置变更不删数据。

---

## 14. 测试与证据要求

### 14.1 测试金字塔

1. **纯算法单测**：读写 LE整数、Murmur尾字节、CRC、边界和溢出。
2. **金标准 codec**：本包合成 `.bin` 双向解压/重打包、unknown-layout、两层校验错误。
3. **领域单测**：类型/槽位分离、Keep/Set、白名单、撤销、预设最新基线。
4. **本地 I/O 集成**：分段写入、rename-replace、占用、冲突、备份失败、保存后再被更新。
5. **GPUI 交互**：搜索、选卡、dirty/冲突提示、另存源未改变、操作按钮可见、中文输入。
6. **Windows 真机**：发布构建、中文路径/IME/DPI、实际文件系统行为。
7. **用户游戏验证**：普通头盔对照 → 身体护甲放入头部 → 其他配装不变 → 重启/换装状态；被动效果单独记录。

### 14.2 必须包含的关键回归

| 编号 | 输入/操作 | 预期 |
|---|---|---|
| C01 | 原有效文件，无 patch | 输出 raw 逐字节相同 |
| C02 | 头部换成已知普通 Helmet | payload 只允许 head+inner-hash 范围变化 |
| C03 | head 写 Armor ID | 领域层允许；保留物品类型 Armor |
| C04 | 只修改 body | head/cape/其他字段不变 |
| C05 | 改正文但不更新内层，外层正确 | 严格拒绝；重现旧02缺陷 |
| C06 | 单独损坏外层 CRC | 解压前拒绝 |
| C07 | 删最后一个字节、长度越界、块长度0 | 返回确定错误，不 panic、不超量分配 |
| C08 | 校验有效但正文头不同 | 能诊断则只读，禁止套用已知偏移 |
| C09 | 后续块等长修改 | 那块与第0块可能重压，其他块保持原压缩 bytes |
| C10 | 16MiB以上输入/逻辑长度 | 拒绝，不先按恶意长度分配 |
| D01 | 同名 FS-55 Helmet/Armor 两ID | 两项独立可选，不以名字覆盖 |
| D02 | 同一 Armor ID 在head/body被观察 | 一个物品，多条 provenance，不凭head推Helmet |
| D03 | ID为3544585106 | 正数u32完整保留；不变负数或float |
| D04 | CSV十六/十进制矛盾 | 导入预览报告具体行错误 |
| D05 | 重复导入相同目录 | 幂等；名称/手工备注不丢 |
| D06 | 两套同被动配装 | 不自动显示叠加倍率 |
| P01 | 创建预设后换主武器，再应用预设 | 保留最新武器；只展开目标槽位 |
| P02 | body=Keep | 无body patch；与Set(0)严格不同 |
| W01 | 文件先写半截后补全 | 保留旧有效快照，最终刷新完整版本 |
| W02 | dirty时游戏写入新有效内容 | 冲突，不暗中改基线/丢草稿 |
| W03 | 打开B后A任务迟到 | generation不符，拒绝更新/写回 |
| S01 | 另存成功 | 源bytes不变，不声称源已应用 |
| S02 | 基线SHA不同 | 拒绝覆盖，磁盘新文件保留 |
| S03 | 备份目录权限不足/磁盘满 | 不触碰源文件 |
| S04 | 替换之后外部再写 | CommittedButSuperseded，无自动覆盖循环 |
| S05 | 不确定替换错误 | 保留临时/备份现场，报Indeterminate |
| U01 | 选择卡片/改名/收藏/导入 | 源文件哈希不变 |
| U02 | body锁定时选head | body不变；选择body必须显式解锁 |
| U03 | 900×620 / 200%DPI | 所有关键保存/冲突操作可访问 |
| U04 | 中文IME输入型号/名称 | 组合文本、光标、快捷键正常 |

### 14.3 属性测试/模糊测试

对有界任意输入调用 decoder，不能 panic/越界/无限分配。对有效已知布局的随机允许 patch：`decode(encode(x,p)).payload == apply_and_rehash(x,p)`。随机 no-op 要 raw identical；重复应用相同意图不产生新 dirty。

不把 fuzz 出来的随机 ID 输出为“可供玩家导入”的存档。fixtures 统一 `.bin`，目录明确 `SYNTHETIC_DO_NOT_IMPORT_TO_GAME`。

### 14.4 本交接包实际重新验证了什么

本轮重新执行原 Python 工具的 **42 项测试，全部通过，GUI 在 Linux + Xvfb 事件循环运行**；记录见 `evidence/python_reference_tests.txt`。重新读取两个真实样本并验证无修改保真；没有重新在 Windows 游戏里测试。

Rust/GPUI 应用尚未实现；下一agent不得把这42项Python结果说成Rust程序已通过。新增合成 fixtures 在本包构建时用参考解析器验证并生成manifest，仍不等于客户端认可任意数据。

---

## 15. 错误分类与玩家文案

| 类别 | 给用户看的信息 | 开发诊断 |
|---|---|---|
| FileMissing | 找不到存档，请确认账号与路径 | path identity、OS错误码；不泄露全路径到分享日志 |
| PendingWrite | 游戏正在更新文件，保留上一份有效状态 | 读前后size/mtime/fileID变化 |
| BadOuterCrc | 文件尚未完整写入或已损坏；暂不加载 | stored/calculated CRC、阶段 |
| BadInnerHash | 正文完整性校验失败；禁止写入 | stored/calculated hash，别自动修复 |
| UnknownLayout | 已读取，但此存档布局未验证，只读 | prefix/length/flags摘要 |
| UnknownItem | 找到ID但目录没有名称/类型 | raw ID与observed_slot |
| AmbiguousImport | 有重复或类型冲突，需要确认 | 行号/字段/原始记录索引 |
| SourceConflict | 游戏已更新存档，你的选择保留；请选择重新读取或重新应用 | 基线/当前SHA与generation |
| BackupFailed | 未能创建可靠备份，未覆盖源文件 | I/O阶段/错误码 |
| ReplaceIndeterminate | 保存结果不能确认；请保留备份，勿重复写回 | phase、源/临时/备份实际存在状态 |
| OutputInvalid | 生成结果未通过复核，未保存 | 差异白名单/校验结果 |
| RendererUnavailable | 界面初始化失败，查看本地日志 | GPU/驱动初始化错误，不隐瞒成路径错误 |

错误提示不要一律显示“存档坏了”；监听过程中暂时校验不匹配很可能只是分段写入。持续失败可以升级为错误，但保留可用的上一版数据并注明 stale。

---

## 16. 实施优先级与完成定义

### P0：必须交付

Windows可运行GPUI壳；严格codec；导入旧JSON/CSV；两槽位卡片与名称搜索；身体默认锁定；草稿/undo/差异预览；只读监听；另存；有备份、冲突检查、回读的显式回写；普通头盔恢复；预设Keep/Set；本地日志；已知/未知布局区分；自动化测试与中文README。

### P1：明确有价值但不阻塞第一版

本地图片、收藏快捷栏、metadata编辑、被动/债券/重量筛选、预设导入导出、拖拽到槽位、批量导入冲突处理增强、最近使用、对每个物品保存用户效果测试备注。

### P2：可后续讨论

完整HEX编辑、更多装备类型、跨平台打包、社区目录更新、图片提取器、自动采集字段发现。没有明确需求不要顺带实现，尤其不把任意偏移写入放进默认页面。

### Definition of Done

- 不是截图、静态Demo或“Rust GUI 调 Python”包装；实际Rust codec与读写闭环运行。
- 用户导入旧目录后，无须手动重新输入已采集ID。
- 人能从卡片看清头部是Helmet还是Armor；同名不同类型不混。
- 选择一项头部护甲可在一个主界面完成，身体默认保持。
- 无修改不改变raw；有修改只改变白名单与校验；校验遗漏有回归。
- 新武器/角色状态不会因预设应用回到历史快照。
- 游戏写文件时只读监听不丢草稿；源冲突不能静默覆盖。
- 任意失败不伪称“未写入/已成功”，能找到备份。
- Windows构建、GUI、中文IME、DPI、文件替换都提供真实测试记录。
- 客户端接受跨槽位与被动效果是分开的验收结果，不扩大结论。
- 不打包真实存档、系统字体、用户账号数据和未获许可游戏素材。

---

## 17. 接手 agent 的工作方式与反例

开始就复用本包已有知识，不再反复问用户“有没有第三方sav”“头盔偏移在哪里”“是不是要用CE”。缺少全量目录时实现导入与演示数据，不能猜数据库也不能停工等待。

先保住数据正确性，再提升玩家体验。每个功能先设失败测试与预期，再实现；Windows相关未执行就写“未测试”，不要用Linux测试冒充。提交时说明哪些是实际运行结果、哪些是规格目标。

**禁止的捷径：**

- 只算CRC，不算正文Murmur。
- 在压缩文件偏移0x121直接写。
- 给raw LZ4添加长度前缀，或最后一块只解出逻辑剩余长度。
- 为了让UI正常显示把未知字段清零、把整个结构反序列化再重建。
- 按名字去重、按头部来源自动归Helmet、把u32当i32。
- 把preset做成整份save副本，覆盖后用户主武器又变回原样。
- 点击护甲卡就写磁盘；用“自动保存”让玩家无法预览。
- watcher新数据到来就清掉未保存草稿，或旧后台任务覆盖当前账号。
- 出错时删原存档、删remote、删remotecache，再要求玩家重进游戏。
- 直接把持续变化的文件mmap后当稳定快照；长期锁住游戏写盘路径。
- 不做中文输入法与缩放测试，只在固定像素截图里认为UI已完成。
- 为了适配某篇旧教程混装多份GPUI，或把不可编译代码当最终交付。

---

## 18. 建议用户验收顺序

1. 用真实目录导入，核对FS-55普通头盔与身体护甲为不同项；确认用户新采集护甲名称/ID正确。
2. 打开当前存档，**不编辑**，另存，二进制比较必须完全相同。
3. 游戏运行时只读监听，正常换一件身体护甲，看卡片是否刷新；不尝试并发回写。
4. 保留当前备份，退出游戏后用普通头盔做对照改档，检查其他配置不变。
5. 再将已验证的FS-55身体护甲设为头部，检查实际槽位，确认未整体默认化。
6. 测一件用户自行采集的新护甲，先确认槽位，再独立测试被动。
7. 在游戏里换主武器后重新读取，应用旧预设，确认新主武器没有回滚。
8. 测草稿时外部改文件，工具必须提示冲突并保留草稿。
9. 测“恢复普通头盔”和“恢复完整备份”，确认两者范围不同且文案清楚。
10. 最后再用长期常用配装，不拿唯一可用存档进行破坏性测试。

用户已完成本对话的04/05/06对照；不要求为了重新发现算法而机械重做。Rust移植产生新编码器/IO实现，验收时用其中的少量对照确认没有回归是必要的。

---

## 19. 来源索引与复查原则

文中 `[W1]` 等编号对应 `docs/SOURCES.md`，包含本次查询日期、公开原始文档位置及适用边界。源代码类事实还可以直接在 `reference/python/hd2_core.py` 复算。

本规格不依赖前序聊天链接才能执行：必要算法、数据格式、测试范围、UI行为、风险及路径都已在包内。前序对话仅作为用户实测结果的来源，其精简记录已转录到 `evidence/USER_OBSERVATIONS.md`。

**最终判断原则：源文件字节与测试证据优先；对未知事项保留未知；用户报告的游戏效果不扩张成所有版本通用保证。**
