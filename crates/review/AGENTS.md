# review（`lectern-review`）

审阅的领域逻辑：会话改动归因、片段对齐、已看状态、Drifted 判断、审阅意见攒批与回传。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：[docs/design/review.md](../../docs/design/review.md)。
- issue：[#15](https://github.com/SoloJiang/lectern-app/issues/15)、[#22](https://github.com/SoloJiang/lectern-app/issues/22)
- ADR：[0012](../../docs/adr/0012.md)、[0019](../../docs/adr/0019.md)、[0035](../../docs/adr/0035.md)、[0044](../../docs/adr/0044.md)

## 边界

- 归因结果的缓存键是文件内容的 blob OID 加上会话轮次历史的摘要（架构规则 5）：两个会话的最终内容相同时，改动叙事仍然不同，不能只按 OID 命中。
- 已看状态和审阅意见可以从 git 对象、会话 JSONL 和用户标注日志重建。
- 项目主页「会话外的改动」的计数比较用 `retained_bytes = 0`、`total_patch_bytes = 0` 的 `Limits`，改动路径超过 4000 个时只保留计数、不保留改动集（[ADR 0035](../../docs/adr/0035.md)）。
- 已看标记的键包含审阅对象（会话或比较范围）、路径、改动前后两个 blob OID、改动前后的文件模式和改动状态；重命名按（旧路径，新路径）成对匹配，旧路径之后被一个无关的新文件占用时，不当作同一个改动。同一审阅对象和路径的这些值任一变化时，标为「审过之后又被改过」，作为第三种状态交给视图。设计依据见 [pulls.review 调研](../../docs/research/pulls-review.md#对-lectern-的建议)。
- 生成文件、lockfile、快照用同一份模式列表识别，同时决定默认折叠、规则分组和模型输入里的占位。
- 标注日志（[ADR 0044](../../docs/adr/0044.md)）只写 `data_dir` 下本 crate 自己的文件，用 `std::fs` 直接读写，不经 `platform`；除此以外不访问文件系统，工作区只经 `git` 读取。压缩只在内存索引来自对这个文件的一次完整读取加本进程之后的追加、且文件的设备号、inode 和长度没有变时进行，否则只追加。
- 会话简报默认由规则生成；模型分析只在用户执行 Analyze 时进行（[ADR 0019](../../docs/adr/0019.md)）。模型输出经过结构校验、覆盖检查、一次纠错重试和对账后才交给视图；任何文件都不能因为模型漏掉而从界面上消失，没被分到的文件进入「未分类」。行备注只挂在 diff 里显示的行上，否则降为文件备注。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-review     # 只改本 crate 时最快的反馈
just test -p lectern-review
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 标注日志的测试（`src/log/tests.rs`）用 `tempfile` 的临时目录和合成记录。IO 故障经 `LogFile::set_faults` 注入（读取、追加、写压缩的临时文件写到一半、改名之前），尺寸上限经 `LogFile::set_limits` 调小；`log_compacts_above_4_mib` 用真实的 4 MiB。日志的行格式有 insta 快照（`log_format`），改格式要同时改设计文档「标注日志」。
- 前置条件 A（`services` 的 `changes`、`review` 模块）合并之前，用到的类型是 `src/types.rs` 里的替身；第 3 步接入审阅引擎时删掉它，改用 `lectern_services` 的类型。
- 性能：`lectern-bench` 的 `review/log_load_16k`（`just bench --measure review/`），夹具是 `testing::write_log`。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
