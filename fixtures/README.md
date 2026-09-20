# SYNTHETIC — DO NOT IMPORT TO GAME

这里的14个`.bin`均为人工合成的测试输入，**不是玩家存档、不是双甲预设、不能替换Steam文件**。它们只用于Rust/Python编解码互验。

`manifest.json`定义6个有效已知布局、1个校验有效但未知正文头（应只读）、7个预期拒绝样本的实际SHA、字段、checksum与结果。

`murmur_vectors.json`包含228组原始算法向量：长度0..65及若干边界，seed为0/1/0xFFFFFFFF。向量计算针对input_hex全部bytes，不需要清零第12..15字节；只有存档内层校验才有清零规则。

注意两类验证：

- 用固定fixture解码应得到manifest的payload SHA与字段。
- 自行重压修改后的文件，raw SHA可能因压缩器不同而不同，不能用Python修改后raw SHA强制约束Rust编码器。应核对payload、两层checksum、未改压缩块及白名单。无修改则必须直接返回原raw。

本包Python测试还可以继续生成合成fixture，不需要真实用户存档。
