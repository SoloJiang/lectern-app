# 项目主页：项目侧栏，以及带会话卡片的主区
# （docs/design/project_home.md「界面」）。键以 `project-home-` 开头。
# 会话标题、分支名、子项目名和 agent 名原样显示，作为参数传入，不写进消息。

project-home-overview = 概览
project-home-sessions = 会话
project-home-sessions-empty = 最近 14 天没有会话
# 最新的几个会话下面的一行，点开显示其余的，再点收起。
project-home-sessions-more = 还有 { $count } 个
project-home-sessions-less = 收起
# 其他 crate 加入的条目（Worktrees、Files 等）上面的小标题。
project-home-views = 项目
project-home-untitled-session = 未命名会话
# 运行中的会话行尾绿点的无障碍名称。
project-home-running = 运行中
# 会话有权限请求在等用户确认时，会话行尾橙点的无障碍名称。
project-home-waiting = 等你处理
project-home-packages = 包
# 子项目行尾数字的无障碍名称和提示。
project-home-changed-today = 今天 { $count }
project-home-usage = 用量
# $tokens 是紧凑的数量，如 12.3K。
project-home-usage-7d = { $tokens } token · 7 天
# $agent 是 agent 的名字；$window 是 project-home-window-hours 或 -days。
project-home-limit = { $agent } { $window }额度 · { $percent }%
project-home-limit-no-window = { $agent } 额度 · { $percent }%
project-home-window-hours = { $count } 小时
project-home-window-days = { $count } 天
project-home-title = { $count ->
    [0] 没有待审的会话
   *[other] { $count } 个会话待审
}
# $branch 是当前分支。
project-home-subtitle = { $branch } · { $count } 个 worktree
# 大标题下面的两句话；数为 0 的那句省略。
project-home-lede-running = { $count } 个 agent 正在运行。
project-home-lede-outside = { $count } 个文件在会话之外被改动。
project-home-needs-review = 待审阅
project-home-outside = 会话外的改动
# $branch 是当前分支。
project-home-outside-files = { $branch } 上有 { $count } 个文件改动
project-home-outside-review = 审阅
project-home-outside-capped = { $count } 个改动路径（未归属）
project-home-done = 最近完成
project-home-all-reviewed = 所有会话都已审阅。
project-home-empty = 这个项目还没有 agent 会话。
# 进度环的无障碍名称；旁边显示的数字是 -short。
project-home-progress = 已看 { $seen }/{ $total }
project-home-progress-short = { $seen }/{ $total }

## 卡片第一行末尾的状态词

project-home-status-running = 运行中
# 暂不显示：`agents` 还没有会话的结束状态
# （docs/design/project_home.md「对其他 crate 的要求」F）。
project-home-status-waiting = 等你处理
project-home-status-ended = 已结束

## 「最近完成」行右边的结果

# 暂不显示：要 git 提交和合并请求的数据
# （docs/design/project_home.md「界面」）；在那之前这一行显示相对时间。
project-home-done-committed = 已提交
project-home-done-merged = MR #{ $number } 已合并

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
