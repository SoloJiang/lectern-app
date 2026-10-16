# app（`lectern-app`）

二进制 `lectern`：启动、按 profile 装配插件、首帧后恢复窗口快照。

- 层：入口层。可以依赖任何层；只有本 crate 可以依赖 tool 层，且只能作为 dev-dependency 或不在 `default` feature 里的可选依赖。其他 crate 不得依赖本 crate。
- 设计文档：`docs/design/app.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，人审过再写代码。
- issue：[#7](https://github.com/SoloJiang/lectern-app/issues/7)、[#38](https://github.com/SoloJiang/lectern-app/issues/38)
- ADR：[0017](../../docs/adr/0017.md)、[0018](../../docs/adr/0018.md)

## 边界

- 首帧前只建窗口、画骨架；插件激活、SQLite、文件监听、会话扫描、读窗口快照都放在首帧之后（[ADR 0017](../../docs/adr/0017.md)）。
- 只做装配：把 `platform` 的实现注入 `fs_watch`、`kernel` 等定义的接口。不写业务逻辑。
- `ui_probe` 只在 probe profile 和调试构建中装配。

## 测试

依赖 GPUI，只在 macOS 上编译和测试：

```sh
just test -p lectern-app
cargo build --locked -p lectern-app   # 与 macOS CI 的构建步骤相同
```

- 冷启动、空闲内存由 `bench` 测量（[#7](https://github.com/SoloJiang/lectern-app/issues/7)），安装包体积由 [#38](https://github.com/SoloJiang/lectern-app/issues/38) 检查。
- 装配逻辑用 test profile 注入模拟服务验证，不依赖真实的项目目录。
