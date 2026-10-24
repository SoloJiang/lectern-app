# 欢迎窗口（docs/design/interaction.md，屏幕 01；docs/design/welcome.md「界面」）。

welcome-title = 欢迎使用 { -app-name }
welcome-subtitle = 打开一个文件夹，看 agent 改了什么。
welcome-open-folder = 打开文件夹…
    .tooltip = 打开任意文件夹或仓库（⌘O）
welcome-clone = 克隆…
welcome-recent = 最近的项目
welcome-recent-empty = 没有最近的项目
welcome-recent-missing = 文件夹已不存在
welcome-recent-remove = 从最近的项目移除
    .tooltip = 从最近的项目移除
welcome-discovered = 来自 Claude Code 与 Codex
    .tooltip = 在 agent 会话历史里发现、尚未打开过的文件夹
welcome-discovered-empty = 还没有发现 agent 会话
welcome-row-sessions = { $count } 个会话
welcome-row-running = { $count } 个运行中
welcome-language = 语言
welcome-language-not-saved = 语言设置未能保存，下次启动仍使用上一次保存的设置。

## 文件夹打不开的原因，每种 OpenError 一条消息。

welcome-open-failed-not-found = 这个文件夹已不存在。
welcome-open-failed-not-a-directory = 这个路径不是文件夹。
welcome-open-failed-permission = { -app-name } 没有权限读取这个文件夹。
welcome-open-failed-repo = 这个文件夹里的仓库无法读取。
welcome-open-failed-io = 这个文件夹无法打开。
