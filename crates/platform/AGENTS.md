# platform（`lectern-platform`）

macOS 专属能力：FSEvents、`open -a`、钥匙串、Seatbelt、代码签名校验、Claude Code statusline 安装。macOS 系统绑定只能出现在本 crate（架构规则 6）。

- 层：基础设施层。只依赖基础设施和 kernel 层；不依赖 GPUI。
- 设计文档：`docs/design/platform.md`，尚未编写。写代码前先按[模板](../../docs/design/TEMPLATE.md)写好，人审过再动手。
- issue：[#27](https://github.com/SoloJiang/lectern-app/issues/27)、[#32](https://github.com/SoloJiang/lectern-app/issues/32)、[#39](https://github.com/SoloJiang/lectern-app/issues/39)
- ADR：[0004](../../docs/adr/0004.md)、[0014](../../docs/adr/0014.md)、[0018](../../docs/adr/0018.md)

## 边界

- 实现其他 crate 定义的接口（`fs_watch`、`kernel` 的插件宿主等），由 `app` 注入；不反过来依赖它们的实现。
- statusline 默认开启：用户已有 statusline 时保留原命令，Lectern 的脚本存一份输入后原样转给它，关闭时恢复原配置（[ADR 0014](../../docs/adr/0014.md)）。

## 测试

本 crate 不依赖 GPUI，在 Linux 上也编译：macOS 专属代码放在 `#[cfg(target_os = "macos")]` 后面，Linux 上只编译与平台无关的部分。

```sh
cargo check -p lectern-platform
just test -p lectern-platform   # macOS 上覆盖全部代码
```

- 调系统 API 的行为只能在 macOS CI 上验证；能抽成纯函数的部分（如生成 Seatbelt 配置、合并 statusline 配置）写单元测试和 insta 快照，在任何平台都跑。
