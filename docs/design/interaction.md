# 交互规范

Lectern 不是编辑器：打开项目，先看 agent 改了什么。入口是项目或文件夹；打开后进入项目主页，首先看到这个项目里等你审的 agent 会话；审阅时按轮次读「改动叙事」。代码、对话、编辑都按需呼出，不常驻。

交互设计画布（含可点击的审阅原型）：https://claude.ai/artifact/LVSXNi9xPviRgzdycnskp9 。决策记录见 [ADR 0012](../adr/0012.md)、[ADR 0013](../adr/0013.md)。

## 核心闭环

1. **Open folder**：打开任意文件夹或仓库。
2. **Project home**：看这个项目里待审的会话、运行中的 agent、会话外的改动。
3. **Review**：顶部是会话简报（值得先看的几处），下面按轮次读 prompt、agent 说明和改动。
4. 改得对：**Looks good**，全部标记已看，进入提交或开 MR（GitHub / GitLab）。
5. 有问题：在行上写 **Review notes**，攒成一批用 ⌘↵ 发回会话；agent 的修改作为新一轮出现，回到第 3 步只看新轮次。

## 三层结构

| 层 | 内容 | 规则 |
|---|---|---|
| 常驻 | Project sidebar（Overview、Sessions、Packages）、Project home、Review（会话简报 + 改动叙事）、Progress、Review notes 发送栏 | 打开就看得到，占据主要空间 |
| 按需呼出 | Context drawer（调用方、测试、历史）、Conversation（⌘\）、⌘K、Worktrees、Files | 从当前视图触发，Esc 回到原处 |
| 收起 | 完整文件阅读（⌘⇧O）、编辑（Edit… / ⌘E）、终端原始输出（Conversation 的 Terminal 页） | 存在，但不出现在默认界面 |

不做的 IDE 元素：活动栏、文件标签页、常驻文件树、状态栏、底部终端停靠、编辑器 / 索引 / 语言服务器状态、生效的 agent 指令。

## 屏幕

| 画板 | 屏幕 | 要点 |
|---|---|---|
| 01 | Welcome | 最近项目；Open folder / Clone；从 Claude Code 与 Codex 历史里发现的、有会话但未打开过的文件夹；语言选择 |
| 02 | Project home | 侧栏：Overview、Sessions（按 worktree 分组）、Packages（今天改动数）、Worktrees / Files / MR。主区：Needs review（会话卡片带信号：公共包改动、API 变化、已漂移、测试结果）、Outside any session、Recently done |
| 03 | Review | 会话简报；按轮次的改动叙事；文件头勾选已看；行内 Review note；Drifted 提示；右侧 Progress；底部发送栏 |
| 04 | Context drawer | 在 Diff 里点符号打开；Definition / Callers / Tests / History；标出未跟着改的调用方，可直接加审阅意见；Open full file 和 Edit 是次要按钮 |
| 05 | Conversation | 结构化对话、工具调用、已发出的审阅意见、权限请求；Terminal 页是原始输出；输入框 |
| 06 | Worktrees | 每个 worktree 一张卡片；右侧对比所选 worktree 与 main，并提示与其他 worktree 是否有文件重叠 |

## 窗口结构

| 区域 | 内容 |
|---|---|
| Welcome | 没有打开项目时出现 |
| Title bar | 项目切换、⌘K、New session；审阅时显示返回项目和会话名 |
| Project sidebar（左） | 项目主页常驻；进入审阅后收起 |
| 阅读栏（中） | 最大宽度 960px，从上往下读 |
| Progress（右） | 审阅时显示本会话文件和已看状态 |
| 抽屉（右侧覆盖） | Context 或 Conversation，同一时间只开一个 |
| 底部栏 | 只在审阅时出现：审阅意见数量和发送按钮 |

## 快捷键

| 键 | 动作 |
|---|---|
| ⌘O | 打开文件夹（新窗口） |
| ⌘K | 搜索文件、符号、会话、命令 |
| ↑ ↓ · ↵ | 在项目主页选会话，进入审阅 |
| J / K | 下一处 / 上一处改动块 |
| V | 标记已看，跳到下一个未看文件 |
| C | 在当前行加审阅意见 |
| ⌘↵ | 把攒下的审阅意见发给 agent |
| Space | 光标在符号上时打开 Context drawer |
| ⌘\ | 打开 / 关闭 Conversation |
| ⌘⇧O · ⌘E | 打开完整文件 · 编辑 |
| Esc | 关闭抽屉；再按一次回到项目主页 |

每个操作都是一个具名 Action，都能绑定快捷键，也能被 UI 探针触发。

## 语言

- 默认英文；首次启动跟随系统语言，简体中文系统显示中文。设置里切换，立即生效。
- 人和 agent 写的内容（prompt、说明、评论）原样显示，不翻译。代码、路径、命令、快捷键不翻译。
- 先按英文排版，再用中文验证；按钮不定宽，关键标签不截断。
- 相对时间、数量、复数按语言格式化（`2/6 reviewed` ↔ `已看 2/6`）。
- 文案集中在 Fluent（.ftl）资源文件，CI 检查 en 与 zh-CN 的键一致。
- 字体：IBM Plex Sans，中文回退 PingFang SC；代码用 IBM Plex Mono。

## 状态与异常

| 状态 | 处理 |
|---|---|
| 项目里还没有会话 | 会话按 cwd 匹配到项目；没有时主页显示最近提交和 New session |
| 加载 | 100 ms 内不显示加载态；100 ms – 1 s 显示骨架；超过 1 s 显示进度并可取消 |
| 已漂移 | 会话结束后代码又被改过：对不上的改动块不显示归因，不做猜测 |
| 会话格式无法识别 | 降级为按文件显示，隐藏轮次和说明，不弹错误 |
| 与 agent 同时改了文件 | 只在你编辑过的文件上提示三方合并，不自动覆盖任何一方 |
| Agent 请求权限 | 出现在 Conversation 里；面板关着时，项目侧栏里对应会话显示等待标记 |

## 视觉

深色界面：背景 `#111214`，面板 `#17181B` / `#1C1D21`，边框 `#2C2E34`，正文 `#ECEAE4`，次要文字 `#A3A5AC`，强调色黄铜 `#E3A857`。增删行用绿 / 红底色并带 `+` / `−` 符号，不只靠颜色区分。
