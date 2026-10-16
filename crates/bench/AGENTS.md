# bench（`lectern-bench`）

性能预算基准：冷启动、打开项目、打开文件、打开 Diff、切换 worktree、空闲内存，以及每个插件的激活耗时和常驻内存。超标即 CI 失败。

- 层：tool 层，只用于调试与测试构建，可以依赖任何层。只有 `app` 可以依赖本 crate，本 crate 不进入发布构建。
- 设计文档：`docs/design/bench.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#7](https://github.com/SoloJiang/lectern-app/issues/7)、[#38](https://github.com/SoloJiang/lectern-app/issues/38)
- ADR：[0017](../../docs/adr/0017.md)、[0018](../../docs/adr/0018.md)

## 边界

- 预算数字以[技术调研「性能预算」](../../docs/design/tech-research.md#性能预算)和 [ADR 0017](../../docs/adr/0017.md) 为准，改预算要写 ADR。
- 不进入发布构建。

## 测试

```sh
just bench               # 在 macOS 上跑，结果才有意义
```

- 基准报告列出每项的实测值和预算；基准在 CI 的 macOS 任务里运行。
- 基准用的样例项目放 `tests/fixtures/`，尺寸接近真实 monorepo。
