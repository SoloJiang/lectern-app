# Worktree：并行 agent 总览，每个 worktree 一行，以及它的检查器页
# （docs/design/worktrees.md「界面」）。键以 `worktrees-` 开头。
# 分支名、目录路径、会话标题和 MR 标题原样显示，作为参数传入，不写进消息。

worktrees-entry = Worktree
worktrees-title = Worktree
# $active 是有运行中会话的 worktree 数；$total 是 worktree 总数。
worktrees-subtitle = { $active } 个活动 · 共 { $total } 个
worktrees-refresh = 刷新
worktrees-active = 活动
worktrees-idle = 空闲
worktrees-only-one = 这个仓库只有一个 worktree。在其他 worktree 里工作的 agent 会列在这里。
worktrees-not-git = 这个文件夹不是 git 仓库。
# 分支名后面的小标记。
worktrees-current = 当前
worktrees-locked = 已锁定
worktrees-missing = 已缺失
# 没有分支而检出某个提交时、以及分支还没有提交时，名字后面的小字。
worktrees-detached = 分离 HEAD
worktrees-unborn = 没有提交
worktrees-head-unknown = HEAD 无法读取
worktrees-running = { $count } 个运行中
worktrees-no-sessions = 最近 14 天没有会话
worktrees-uncommitted = { $count } 个未提交
# $base 是主干的短名，如 origin/main。
worktrees-vs-base = 相对 { $base } { $count } 个文件
# $limit 是计数停止时的改动文件数。
worktrees-capped = { $limit } 个以上的文件
# $tokens 是紧凑的计数，如 12.3K。
worktrees-tokens = { $tokens } token
worktrees-mr-more = 另 { $count } 个 MR
worktrees-detail-title = Worktree
worktrees-detail-empty = 没有选中的 worktree
worktrees-usage-branch = 分支用量
worktrees-usage-mr = MR 用量
worktrees-tokens-input = 输入
worktrees-tokens-output = 输出
worktrees-tokens-cache-read = 缓存读取
worktrees-tokens-cache-write = 缓存写入
worktrees-tokens-review = 审阅
# $time 是相对时间，如“3 天前”。
worktrees-usage-since = 自 { $time } 起
worktrees-usage-inherited = 可能含从父会话继承的上下文
worktrees-usage-backfilling = 仍在读取较早的会话
worktrees-usage-not-counted = 默认分支和分离 HEAD 不统计
worktrees-mr-shared = 与另外 { $count } 个 MR 共用分支
# $base 是主干的短名，如 origin/main。
worktrees-compare-title = 与 { $base } 对比
worktrees-compare-head = 未提交的改动
worktrees-committed = 已提交 { $count } 个文件
worktrees-uncommitted-files = 未提交 { $count } 个文件
worktrees-total = 合计 { $count } 个文件
# $time 是相对时间，如“3 分钟前”。
worktrees-counted-at = { $time }统计
worktrees-review-changes = 审阅改动
worktrees-missing-detail = worktree 的目录已不存在。
worktrees-overlaps-title = 文件重叠
worktrees-overlap-files = { $count } 个文件
worktrees-overlap-more = 另有 { $count } 个
worktrees-no-overlap = 与其他 worktree 没有共同改动的文件
worktrees-overlap-unknown = 改动太多，没有比较
worktrees-sessions-title = 最近会话
worktrees-untitled-session = 未命名会话
worktrees-switch = 切换到这个 worktree
