# SYNTHETIC — DO NOT IMPORT TO GAME

这里的 `.bin` 均为人工合成的测试输入，**不是玩家存档、不是双甲预设、不能替换 Steam 文件**。它们只用于 Rust/Python 编解码互验。

`manifest.json` 定义全部样本的 SHA、字段、checksum 与结果：原有 6 个有效旧布局、1 个未知布局、7 个错误样本保持不变，另有 `valid_new_baseline.bin` 覆盖 Observed0107。
新夹具的后续块和最后 36 字节包含非零哨兵，用于发现尾部截断或误写；其数据不来自玩家存档。
`../hd2-armor-desk/scripts/generate_new_fixture.py` 同步生成此目录和 Rust `tests/fixtures/` 的新夹具与 manifest。

`murmur_vectors.json`包含228组原始算法向量：长度0..65及若干边界，seed为0/1/0xFFFFFFFF。向量计算针对input_hex全部bytes，不需要清零第12..15字节；只有存档内层校验才有清零规则。

注意两类验证：

- 用固定fixture解码应得到manifest的payload SHA与字段。
- 自行重压修改后的文件，raw SHA可能因压缩器不同而不同，不能用Python修改后raw SHA强制约束Rust编码器。应核对payload、两层checksum、未改压缩块及白名单。无修改则必须直接返回原raw。

本包Python测试还可以继续生成合成fixture，不需要真实用户存档。
