# project（`lectern-project`）

项目模型：仓库 → worktree → 子项目；清单解析（npm / yarn / pnpm、Maven、Gradle、Go、Cargo）。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/project.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#9](https://github.com/SoloJiang/lectern-app/issues/9)
- ADR：[0006](../../docs/adr/0006.md)、[0007](../../docs/adr/0007.md)

## 边界

- 文件列表读 git index，不遍历目录。
- 识别出的文件类型与清单交给插件市场做激活匹配（[ADR 0018](../../docs/adr/0018.md)），匹配不在本 crate 实现。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-project     # 只改本 crate 时最快的反馈
just test -p lectern-project
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
