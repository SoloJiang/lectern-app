---
name: gpui
description: How to write GPUI and gpui-component code in Lectern's view crates, using the upstream gpui-component agent guides pinned to the version Lectern locks. Use before writing or reviewing code that uses gpui or gpui_component - views, elements, entities, actions and keybindings, focus, async tasks, layout, styling, or #[gpui::test] tests.
---

# GPUI 与 gpui-component

Lectern 锁定 `gpui-pre =0.3.9` 与 `gpui-component =0.7.3`（[ADR 0016](../../../docs/adr/0016.md)）。gpui-component 仓库在 `v0.7.3` 标签下自带面向 agent 的指南。本技能只指向这个版本的原文。

## 取得指南

```sh
git clone --depth 1 --branch v0.7.3 https://github.com/longbridge/gpui-component ../gpui-component-v0.7.3
```

要读的文件（路径相对于 `skills/gpui-kit/`）：

| 文件 | 读取时机 |
|---|---|
| `references/coding-guides.md` | 写任何视图代码之前：`RenderOnce` 与 `Entity<T>` 的选择、状态归属、`ElementId`、事件、焦点、异步、测试 |
| `references/gpui/*.md` | 用到某一块 GPUI 机制时：action、async、context、element、entity、event、focus-handle 等 |
| `references/conventions.md`、`references/usage.md` | 选用和组合 gpui-component 的控件 |
| `references/recipes.md` | 需要一个完整、能编译的例子时 |

只读与 `v0.7.3` 对应的文件。GPUI 快照之间的接口变化很大，不要用 `main` 分支或训练数据里的 API。

## 名字对照

上游指南基于汇总 crate `gpui-kit` 编写，Lectern 直接依赖它下面的各个 crate：

| 指南里 | Lectern 里 |
|---|---|
| `use gpui_kit::*;` | `use gpui::*;`（workspace 里 `gpui` 是 `gpui-pre` 的别名） |
| `gpui_kit::component::…` | `gpui_component::…` |
| `gpui_kit::platform` | `gpui_platform`（`gpui-pre-platform`，带 `font-kit`） |
| `gpui_kit::base`、`gpui_kit::assets` | gpui-component 的依赖；需要直接用时先确认它们在 workspace 依赖里，不要自己加版本 |

## 与 Lectern 规则的冲突

Lectern 的规则优先：

- 视觉与动效按 macOS 原生标准（[ADR 0015](../../../docs/adr/0015.md)）：只用系统字体、系统语义色和系统控件，颜色与动画从 `lectern-ui` 取。上游的 `gpui-kit-design-guides` 和主题系统只作参考，不照搬它的配色、圆角和动效。
- 界面结构按[交互规范](../../../docs/design/interaction.md)，新增界面元素先对照三层结构。
- 界面字符串走 `lectern-i18n`，不写字面量。
- 视图只读快照、只发命令，不做 IO（AGENTS.md 架构规则 4）。
- 未经确认不得升级 GPUI 或 gpui-component（AGENTS.md 禁止事项），也不要引入 `gpui-kit`。
