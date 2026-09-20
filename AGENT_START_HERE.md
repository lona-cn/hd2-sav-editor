# 接手 agent：从这里开始

用户要你**实现**一个 Windows 上的 Rust + GPUI 双甲配置器。用户已经用此前的Python工具采集了全部身体护甲ID；本包给你格式、代码、测试、UI与安全规格，不要求重新进行同样的逆向调查。

## 先做的事

1. 读 `HANDOFF.md`，重点是已验证边界、双层checksum、ID目录结构和玩家双槽交互。
2. 读 `reference/python/hd2_core.py`；运行原测试只用于建立oracle，不作为最终产品运行依赖。
3. 按 `docs/IMPLEMENTATION_PLAN.md` 从Windows GPUI最小切片与Rust codec做起。
4. 用 `fixtures/manifest.json` 与228个Murmur向量验收Rust移植。
5. 导入旧工具workspace/catalog.json；用户全量目录本轮未包含，先用examples实现导入和UI，不要猜真实新ID。
6. 完成卡片选择、预设、另存、冲突保护和有备份的显式回写，提供真实Windows可执行程序与测试证据。

## 最重要的事实

- 解压正文head偏移`0x121`、body`0x129`、cape`0x125`，都是未对齐LE u32。
- head可以引用物品类型Armor：用户已测试FS-55成功；不能把head选择器只限制为Helmet。
- 写入必须更新正文MurmurHash64A-low32、重压raw LZ4，再更新外层CRC32。
- 旧02因漏内层校验触发默认配置；修正版04/05/06已得到用户成功报告。不要退回只算CRC的实现。
- 目录去重与类型判定不能只看来源offset或显示名字。
- 预设只存Keep/Set装备意图；应用到最新确认的存档，不拿旧save回滚当前配装。
- 自动监视只读，click卡片不写盘；写回与游戏并发不安全，不保证热加载。
- 样本fixture均为合成`.bin`，不可导入游戏。本包没有真实sav/用户全量ID表。

## 交付边界

这是给你实现的handoff，不是已经完成的Rust程序。别只返回新一份方案、截图或调用Python的GUI壳。不要因为缺用户全量目录而停止通用实现；目录通过用户本地导入即可。

不要把未知被动效果、客户端build、GPUI兼容版本或账号风险填成保证。锁定实际可编译的版本，并把Linux测试、Windows测试和游戏实测分开记录。

用户关注的是**极易使用的可视化双甲配置**，不是全功能HEX编辑器。默认锁身体，头部卡片清楚写“身体护甲放入头部”，按名字选好后预览并保存即可。
