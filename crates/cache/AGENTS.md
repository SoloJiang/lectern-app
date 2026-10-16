# cache（`lectern-cache`）

内容寻址的 SQLite 缓存：解析、高亮、索引、diff 等派生数据以 git blob OID 为键（架构规则 5）。

- 层：基础设施层。只依赖基础设施和 kernel 层；不依赖 GPUI。
- 设计文档：`docs/design/cache.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#17](https://github.com/SoloJiang/lectern-app/issues/17)、[#31](https://github.com/SoloJiang/lectern-app/issues/31)
- ADR：[0006](../../docs/adr/0006.md)、[0017](../../docs/adr/0017.md)、[0019](../../docs/adr/0019.md)

## 边界

- 首帧之后才打开数据库。
- SQLite 页缓存 8 MB，缓存按字节 LRU；缓存可以随时删除重建，不存只有这里才有的数据。
- 缓存放在 `platform` 提供的应用缓存目录下，按仓库 git 公共目录的绝对规范路径区分（`git rev-parse --path-format=absolute --git-common-dir` 或解析后取规范路径），同一仓库的 worktree 共用，不在用户的 `.git` 里写文件。
- 模型分析结果的键是完整分析请求的哈希：规范化后的提示词、每个文件规范化后的完整 patch 的哈希（含新旧路径、OID、文件模式和状态）、被审阅 worktree 的指纹、schema 版本、模型 ID，以及调用的 agent、它的命令行版本、影响输出的模型服务配置、快照处理规则的版本（与基准快照的键相同，规则变了旧结果不再命中）和分析流程的版本（提示词模板、校验、覆盖检查、纠错、对账、启动参数、上限任何一项变化都升版本）（[ADR 0019](../../docs/adr/0019.md)）。规则生成的结果不缓存，每次重算。
- Analyze 的快照按字节 LRU 淘汰；只缓存不含工作区改动的基准快照（按树 OID 加处理规则的版本，规则随 agent 命令行版本的自动加载清单变化，写出后只读），叠加了工作区改动的克隆属于单次分析，不进缓存；正在使用的快照有引用计数，不参与淘汰（ADR 0019）。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-cache     # 只改本 crate 时最快的反馈
just test -p lectern-cache
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
