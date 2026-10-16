# cache（`lectern-cache`）

内容寻址的 SQLite 缓存：解析、高亮、索引、diff 等派生数据以 git blob OID 为键（架构规则 5）。

- 层：基础设施层。只依赖基础设施和 kernel 层；不依赖 GPUI。
- 设计文档：`docs/design/cache.md`，尚未编写。写代码前先按[模板](../../docs/design/TEMPLATE.md)写好，人审过再动手。
- issue：[#17](https://github.com/SoloJiang/lectern-app/issues/17)、[#31](https://github.com/SoloJiang/lectern-app/issues/31)
- ADR：[0006](../../docs/adr/0006.md)、[0017](../../docs/adr/0017.md)

## 边界

- 首帧之后才打开数据库。
- SQLite 页缓存 8 MB，缓存按字节 LRU；缓存可以随时删除重建，不存只有这里才有的数据。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-cache     # 只改本 crate 时最快的反馈
just test -p lectern-cache
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
