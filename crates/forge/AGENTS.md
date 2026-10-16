# forge（`lectern-forge`）

GitHub 与 GitLab（含自部署）的 MR 与 CI 状态。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/forge.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#25](https://github.com/SoloJiang/lectern-app/issues/25)
- ADR：[0008](../../docs/adr/0008.md)、[0019](../../docs/adr/0019.md)

## 边界

- MVP 调用 `gh` / `glab` 命令行工具，复用它们已有的登录态（[ADR 0008](../../docs/adr/0008.md)），本 crate 不保存凭据；命令行进程按需启动、用完即退。改成直接持有凭据要先修订 ADR 0008。
- 网络请求在后台任务里执行，离线时返回缓存或明确的离线状态。
- 代码平台的接口按能力定义：取 diff 是必需的，快速判断是否过期、加载完整文件、读写审阅评论都是可选能力，视图按能力决定显示哪些功能，不判断平台种类。
- 请求处理限流和条件请求（ETag），被限流时返回缓存和明确的状态。
- 读取 PR 评论里带 `<!-- pulls.review data -->` 标记的分析结果，作为分组参考；解析失败的忽略，不向 PR 写入 Lectern 自己的分析（[ADR 0019](../../docs/adr/0019.md)）。设计依据见 [pulls.review 调研](../../docs/research/pulls-review.md#对-lectern-的建议)。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-forge     # 只改本 crate 时最快的反馈
just test -p lectern-forge
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
