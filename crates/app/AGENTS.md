# app（`lectern-app`）

二进制 `lectern`：启动、按 profile 装配插件、首帧后恢复窗口快照。

- 层：入口层。可以依赖任何层；只有本 crate 可以依赖 tool 层，且只能作为 dev-dependency 或不在 `default` feature 里的可选依赖。其他 crate 不得依赖本 crate。
- 设计文档：[docs/design/app.md](../../docs/design/app.md)
- issue：[#199](https://github.com/SoloJiang/lectern-app/issues/199)、[#205](https://github.com/SoloJiang/lectern-app/issues/205)、[#7](https://github.com/SoloJiang/lectern-app/issues/7)、[#38](https://github.com/SoloJiang/lectern-app/issues/38)
- ADR：[0017](../../docs/adr/0017.md)、[0018](../../docs/adr/0018.md)

## 边界

- 首帧前只建窗口、画骨架，外加 `lectern_ui::init`（内存操作：内置的 `Palette`、减弱动态效果、登记 `platform` 的通知；不读系统色）和骨架窗口的 `lectern_ui::attach`；`lectern_ui::ready`（`gpui_component::init`、系统色）在 `startup::release` 里、`Kernel::start` 之前。插件激活、SQLite、文件监听、会话扫描、读窗口快照都放在首帧之后（[ADR 0017](../../docs/adr/0017.md)、[ui 设计「对其他 crate 的要求」](../../docs/design/ui.md#对其他-crate-的要求)）。
- 只做装配：把 `platform` 的实现注入 `fs_watch`、`kernel` 等定义的接口。不写业务逻辑。
- `ui_probe` 只在 `ui-probe` feature 下编译（不在 `default` 里，`just` 的入口打开它）；运行时只在 probe profile 里启动（[ui_probe 设计](../../docs/design/ui_probe.md)）。

## 测试

依赖 GPUI，只在 macOS 上编译和测试：

```sh
just test -p lectern-app
just ci                 # macOS 上包含构建 lectern 与性能基准，与 CI 相同
```

- 冷启动、空闲内存由 `bench` 测量（[#7](https://github.com/SoloJiang/lectern-app/issues/7)），安装包体积由 [#38](https://github.com/SoloJiang/lectern-app/issues/38) 检查。
- 装配逻辑用 test profile 注入模拟服务验证，不依赖真实的项目目录。
