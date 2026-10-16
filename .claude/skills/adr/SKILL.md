---
name: adr
description: Record an architecture decision for Lectern as a new ADR in docs/adr/ and add it to the index. Use before changing the architecture, a dependency choice or a performance budget, when a change would contradict an existing ADR, or when asked to write or revise an ADR.
---

# 写 ADR

架构、依赖选型和性能预算的变更由负责推进的 agent 决定，写成 ADR 并标为已接受，不必等人确认。产品方向（定位、目标用户、界面结构的大改）由人决定：遇到这类问题，写清选项后问人，不要写成已接受的 ADR。

## 步骤

1. 读 [docs/adr/README.md](../../../docs/adr/README.md) 的索引，找出相关的已有 ADR 并读全文。新决策与已有 ADR 冲突时，新 ADR 要写明「修订：ADR xxxx」，并同时更新两处：旧 ADR 文件自身的状态行（如 `- 状态：已接受，已被 [ADR NNNN](NNNN.md) 修订`，参照 0003、0005、0010），以及索引里的状态（「已被 NNNN 修订」）。只读旧文件的 agent 也要能看出它已被修订。
2. 用下一个编号新建 `docs/adr/<NNNN>.md`，格式与现有文件一致：

   ```markdown
   # ADR NNNN：<一句话的决策>

   - 状态：已接受
   - 日期：YYYY-MM-DD
   - 修订：[ADR xxxx](xxxx.md)（被修订的部分）   <!-- 没有就删掉这一行 -->

   ## 背景

   为什么现在要做这个决定：需求、约束、实测数据。

   ## 决策

   具体决定了什么。数字（预算、上限、版本）写死，不写「尽量」。

   ## 影响

   哪些 crate、命令、CI、文档要跟着变。

   ## 来源

   需求出处、相关 ADR、外部文档链接。
   ```

3. 在索引表格末尾加一行：`| [NNNN](NNNN.md) | <决策> | 已接受 |`。
4. 同步受影响的文档：根目录 `AGENTS.md`、`docs/design/tech-research.md`（性能预算表等）、相关 crate 的 AGENTS.md 和设计文档。
5. ADR 和它直接带来的文档改动放在同一个 PR；代码改动另开 PR。

## 检查

- 决策可以被验证：预算有测量方式，规则有对应的检查脚本或 CI 步骤（还没有的，在「影响」里写明由哪个 issue 补上）。
- 每个外部事实都有来源链接。
