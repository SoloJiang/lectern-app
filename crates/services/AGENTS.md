# services（`lectern-services`）

插件实现和依赖的服务接口：`SessionSource`、`ForgeProvider`、`NavigationProvider`、`ProjectDetector` 等。插件只依赖接口，不依赖彼此（架构规则 7）。

- 层：kernel 层。只依赖 kernel 层，不依赖 GPUI；任何层都可以依赖本 crate。
- 设计文档：`docs/design/services.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#8](https://github.com/SoloJiang/lectern-app/issues/8)
- ADR：[0010](../../docs/adr/0010.md)、[0018](../../docs/adr/0018.md)

## 边界

- 只有 trait、数据类型和服务 ID，没有实现。
- 同一组接口同时用于内置插件和独立进程插件（[ADR 0018](../../docs/adr/0018.md)），类型要能通过 JSON-RPC 序列化。
- 接口变动会影响所有实现它的插件，改动前在设计文档里列出受影响的 crate。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-services     # 只改本 crate 时最快的反馈
just test -p lectern-services
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
