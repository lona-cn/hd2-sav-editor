# 来源、版本边界与引用说明

核验日期：2026-09-19。上游页面为本次读取的内容，不等于依赖版本已编译、已锁定或未来不变。最终实现须保存实际Cargo.lock与源码rev。

## 本地第一手材料

- **L1：** 本包 `reference/python/hd2_core.py`，来自之前交付的HD2 SAV Inspector ZIP。本轮逐段复核了容器、哈希、目录、监听与保存逻辑。
- **L2：** `reference/python/tests/` 与 `evidence/python_reference_tests.txt`。本轮重新运行42项测试，Linux/Xvfb全部通过。不是Rust或Windows游戏测试。
- **L3：** `reference/legacy/INSPECTOR_README_zh.md`。旧工具功能及输出格式的说明；代码与说明有冲突时，先复核实际代码和测试。
- **L4：** `evidence/USER_OBSERVATIONS.md`。用户对01/02/04/05/06及实时写盘、全ID采集的报告。仅覆盖报告的实际现象。
- **L5：** `evidence/real_sample_recheck.json`。本轮对两份已上传样本的只读复核，真实样本本身未分发。
- **L6：** `fixtures/manifest.json`。合成测试输入的实际校验摘要；不用于证明真实客户端接受合成数据。

## 公开技术来源

### [W1] GPUI 官方 README

https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md

用途：GPUI pre-1.0状态、实体/Render/后台执行、平台入口与Windows支持说明。当前页面出现平台拆分入口；旧示例可能不兼容。不能直接用浮动main依赖复现未来构建。

### [W2] GPUI Kit 官方入门与旧站迁移

https://gpui-kit.com/docs/getting-started/

https://longbridge.github.io/gpui-component/docs/getting-started

用途：当前入门示例显示0.6系列、初始化、Root、保留state entity与Subscription。旧站注明GPUI Component已转向GPUI Kit。手册中的这条路线是建议，实际依赖组合必须在Windows最小切片中验证。

### [W3] Zed 官方 Windows 构建说明

https://zed.dev/docs/development/windows

用途：MSVC/Build Tools、SDK、CMake等平台依赖核对。文档面向Zed整项目，不把所有依赖当成独立GPUI小工具的强制最小集合。

### [W4] GPUI 官方站

https://gpui.rs/

用途：框架入口与官方示例索引。示例须与选定依赖版本一致，不拼接不同年代的API。

### [W5] MurmurHash 作者参考实现

https://raw.githubusercontent.com/aappleby/smhasher/master/src/MurmurHash2.cpp

用途：MurmurHash64A算法参考。存档的seed、覆盖范围、自身字段清零、截取低32位由本地样本推导，不是此源码定义的HD2格式。

### [W6] lz4_flex 官方crate文档

https://docs.rs/lz4_flex/latest/lz4_flex/block/index.html

用途：区分raw block与带长度前缀API、固定输出buffer解压。用于实现时应固定具体crate版本，不盲从/latest。

### [W7] crc32fast 官方crate文档

https://docs.rs/crc32fast/latest/crc32fast/

用途：确认CRC32 IEEE而非CRC32C。计算覆盖范围来自已知存档与Python参考代码。

### [W8] Microsoft ReplaceFileW

https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew

用途：文件替换、备份、同卷约束、权限与失败语义。文档明确REPLACEFILE_WRITE_THROUGH不受支持。不能把一次OS替换等同于与游戏协调的CAS，也不能将所有失败都当作原件未变。

### [W9] Valve Steam Cloud

https://partner.steamgames.com/doc/features/cloud

用途：云同步与本地副本可能相互覆盖的边界；不是HD2正文结构来源，不证明游戏热加载本地改档。

## 不应继续当事实引用的旧推测

- “存档只需要外层CRC”：已被正文Murmur校验和用户02/04对照否定。
- “0x121仅凭邻近披风而猜测”：后来已有两种普通头盔和一种护甲的受控游戏观察。
- “头部成功载入等于所有被动必定生效”：未验证。
- “已知旧数据库含最新联动所有ID”：不成立，用户全量目录必须本地导入。
- “目录中观察偏移就是物品类型”：已被头部Armor案例否定。
- “带备份就能无风险边玩边回写”：错误，不与游戏协调仍有竞争条件。
