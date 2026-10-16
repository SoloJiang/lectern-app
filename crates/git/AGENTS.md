# git（`lectern-git`）

用 gix 读仓库、用 imara-diff 计算 diff；结构化 diff 忽略纯格式变化并识别移动的代码。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/git.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，人审过再写代码。
- issue：[#14](https://github.com/SoloJiang/lectern-app/issues/14)、[#21](https://github.com/SoloJiang/lectern-app/issues/21)
- ADR：[0006](../../docs/adr/0006.md)

## 边界

- gix 读取对象与 index 的调用都在后台任务里执行，不在 UI 线程上。
- diff 结果以两端 blob OID 为键缓存。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-git     # 只改本 crate 时最快的反馈
just test -p lectern-git
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
