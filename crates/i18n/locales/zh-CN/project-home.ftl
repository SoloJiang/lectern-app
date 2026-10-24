# 项目主页：项目侧栏，以及带会话卡片的主区
# （docs/design/project_home.md「界面」）。键以 `project-home-` 开头。
# 会话标题、分支名、子项目名和 agent 名原样显示，作为参数传入，不写进消息。

project-home-overview = 概览
project-home-sessions = 会话
project-home-sessions-empty = 最近 14 天没有会话
project-home-current-worktree = 当前
project-home-other-worktrees = 其他 worktree
project-home-untitled-session = 未命名会话
project-home-running = 运行中
project-home-packages = 包
project-home-changed-today = 今天 { $count }
# $tokens 是紧凑的数量，如 12.3K。
project-home-usage-7d = { $tokens } token · 7 天
# $agent 是 agent 的名字。
project-home-limit = { $agent } 已用 { $percent }%
project-home-title = { $count ->
    [0] 没有待审的会话
   *[other] { $count } 个会话待审
}
project-home-subtitle = { $count ->
    [0] 已全部审阅
   *[other] 待审 { $count } 个
}
project-home-needs-review = 待审阅
project-home-outside = 会话外的改动
project-home-outside-files = { $count } 个改动的文件
project-home-outside-capped = { $count } 个改动路径（未归属）
project-home-done = 最近完成
project-home-all-reviewed = 所有会话都已审阅。
project-home-empty = 这个项目还没有 agent 会话。
project-home-progress = 已看 { $seen }/{ $total }

## 会话卡片第二行的信号

project-home-signal-drifted = 已漂移
project-home-signal-critical = { $count } 处需注意
project-home-signal-changed = 已看后又改了 { $count } 个文件
# 暂不显示：接下来两项要 code_intel 的简报规则，测试结果要合并请求的流水线
# （docs/design/project_home.md「界面」）。
project-home-signal-public-package = 改到公共包
project-home-signal-api = API 有变化
project-home-signal-tests = 测试 { $result }

## agent 名

project-home-agent-claude-code = Claude Code
project-home-agent-codex = Codex
