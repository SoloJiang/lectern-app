# terminal_view（`lectern-terminal-view`）

终端网格绘制（Conversation 的 Terminal 页）。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：[docs/design/terminal_view.md](../../docs/design/terminal_view.md)（已接受，实现中：第 1 步、第 2 步已实现，第 3 步（装配与真机验证）在 `conversation`、`app` 的 PR 里）。本 crate 是非插件的视图库，由 `conversation` 的 Terminal 页使用。实现按它的「验收」分三步：`ui` 终端配色与 `i18n` 的键、`TerminalPane`、装配与真机验证。
- issue：[#5](https://github.com/SoloJiang/lectern-app/issues/5)、[#12](https://github.com/SoloJiang/lectern-app/issues/12)
- ADR：[0002](../../docs/adr/0002.md)、[0012](../../docs/adr/0012.md)

## 边界

- 终端状态在 `terminal` 的独占线程里；本视图只读网格快照，按行缓存字形。
- 属于收起层，不做底部终端停靠（[ADR 0012](../../docs/adr/0012.md)）。

## 模块

| 文件 | 内容 |
|---|---|
| `pane.rs` | `TerminalPane`：状态、`refresh`、按键 / 鼠标 / 滚轮 / 焦点 / 复制粘贴的处理、输入法（`EntityInputHandler`）、`Render` 与语义节点 |
| `element.rs` | 自建元素 `GridElement`：prepaint 生成绘制列表，paint 重放它、登记输入法与鼠标监听 |
| `layout.rs` | 一行的排版与字形放置（`place`，纯函数）、两代的行缓存 `RowCache` |
| `paint.rs` | 绘制列表 `Op` 和 `build`（纯函数，不开窗口；基准 `terminal_view/snap_full` 计时它） |
| `boxes.rs` | 方框与块元素的几何图元 `box_glyph` |
| `input.rs` | GPUI 事件到 `Input` 的纯函数（按键表、格子、滚轮的整行） |
| `colors.rs`、`geometry.rs`、`actions.rs`、`diag.rs` | `theme` / `theme_of`、`cells` / `grid_size`、Action 与 `key_bindings`、详细级别的日志 |
| `testing.rs` | `test-support`：`FakeTerminals`、`frame()`、`synthetic_line`、`SnapFull`（基准夹具） |

帧里的 `Rgb` 只经 `colors.rs` 换成 GPUI 的颜色。验收的 `rg 'force_width: Some|Hsla::from|rgb\(|hsla\('` 检查 `src` 下的全部文件（注释和测试也在内）：别在那里写 `Hsla::from`、`rgb(`、`hsla(`，也不要强制字宽。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-terminal-view       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
