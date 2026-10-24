# 审阅：会话简报、改动叙事、底部浮条（docs/design/review_view.md「界面」）。

review-looks-good = 看起来没问题
    .tooltip = 将所有文件标记为已看
review-progress = 已看 { $reviewed }/{ $total }
review-files-changed = { $count } 个文件有改动
review-comments = { $count ->
    [0] 没有审阅意见
   *[other] { $count } 条审阅意见
}
# $path 原样显示。
review-reviewing = 正在审阅 { $path }

## 工具栏与浮条

review-title-untitled = 未命名会话
review-title-outside = 会话外的改动
# $base 是引用名或提交，原样显示。
review-title-range = 相对 { $base } 的改动
# 对话视图完成之前不可用。
review-send = 发送
review-all-reviewed = 全部已看
review-mode-unified = 统一
review-mode-side-by-side = 双栏
review-refresh = 刷新
    .tooltip = 重新比较工作区
review-loading = 正在准备审阅…
review-cancel = 取消
review-back = 返回

## 简报

review-brief-title = 值得先看
review-brief-empty = 没有需要特别注意的地方。
review-brief-omitted = 还有 { $count } 条
review-brief-pending = 还在补充…
# $path 原样显示。
review-brief-changed-since-seen = { $path } 在你看过之后又被改过
review-brief-drifted = { $path } 有无法对齐的改动
review-brief-edit-failed = 对 { $path } 的一次改动失败了，agent 可能以为已经改上
review-brief-command-only = { $count } 个文件由命令改动，不是由编辑
review-brief-sensitive-agent-instructions = { $path } 会被 agent 当作指令加载
review-brief-sensitive-ci = { $path } 会在 CI 里运行
review-brief-sensitive-hooks = { $path } 会在提交时运行
review-brief-sensitive-git-metadata = { $path } 改变 git 对文件的处理
review-brief-sensitive-symlink = { $path } 是新增或改了目标的符号链接
review-brief-sensitive-executable = { $path } 变成了可执行文件
review-brief-sensitive-submodule = { $path } 指向了另一个提交
review-brief-deleted = 删除了 { $count } 个受跟踪的文件
review-brief-incomplete-truncated = { $count } 个文件没有完整的 diff：已截断
review-brief-incomplete-too-large = { $count } 个文件没有完整的 diff：过大
review-brief-incomplete-binary = { $count } 个文件没有完整的 diff：二进制
review-brief-incomplete-not-retained = { $count } 个文件没有完整的 diff：内容未保留
review-brief-incomplete-filtered = { $count } 个文件没有完整的 diff：经 git 过滤
review-brief-incomplete-unavailable = { $count } 个文件没有完整的 diff：无法读取
review-brief-incomplete-listed = { $count } 个文件没有完整的 diff：只列出
review-brief-noise-lockfile = 已折叠 { $count } 个 lockfile
review-brief-noise-generated = 已折叠 { $count } 个生成文件
review-brief-noise-snapshot = 已折叠 { $count } 个快照
# 提供者的规则没有自己的消息时显示；$rule 原样显示。
review-brief-provided = { $rule }
# $branch 原样显示；$tokens 是紧凑的数量，如 12.3K。
review-brief-usage-row = { $branch } · { $tokens } token · 本会话占 { $percent }%
review-brief-usage-shared = 与另外 { $count } 个 MR 共用
review-brief-usage-unassigned = 不属于任何分支

## 分析

review-analyze = 分析
# $reason 原样显示。
review-analyze-unavailable = 无法分析：{ $reason }
review-analyze-running = 正在分析…
review-analyze-stale = 基于较早的版本
review-analyze-partial = 部分内容未提供给分析
review-analyze-failed = 分析失败：{ $reason }
review-analyze-again = 重新分析

## 提示

review-notice-base-current-head = 没有 reflog：基准可能不是会话开始时的提交。
review-notice-unrecognized = Lectern 无法读取这个会话的日志，改动按文件显示。
review-notice-folder = 这个文件夹不是 git 仓库，只显示记录到的改动。
review-notice-unclaimed = 另有 { $count } 个文件由其他会话或手工改动
review-notice-invalid-paths = { $count } 个路径无效，未读取

## 叙事：轮次、步骤、hunk

review-earlier-turns = 之前的 { $count } 轮
review-turn = 第 { $ordinal } 轮
review-turn-tokens = { $tokens } token
review-turn-running = 进行中…
review-step-counts = +{ $added } −{ $removed }
review-step-partial = 这次改动有一部分没有应用上
review-step-unaligned = 按记录显示，与工作区对不上
review-step-survival-partly = 之后被部分覆盖
review-step-survival-none = 之后被覆盖
review-hunk-turn = 第 { $ordinal } 轮
review-hunk-other-session = 另外 { $count } 个会话
review-hunk-drifted = 已漂移
review-hunk-drifted-lines = { $count } 行已漂移
review-fold = 显示 { $count } 行未改动的内容
review-noise-folded = 已折叠 { $count } 个 lockfile、生成文件和快照
review-noise-show = 显示
review-noise-hide = 隐藏
# $command 原样显示。
review-command = 运行了 { $command }
review-command-files = 运行期间改动了 { $count } 个文件
review-subagent = 派出了子 agent
# $name 是工具名，原样显示。
review-tool = { $name }
review-tool-failed = { $name } 失败
review-injected = 注入的内容
review-mark-seen = 标记为已看（V）
    .tooltip = 标记为已看（V）
review-mark-unseen = 标记为未看
    .tooltip = 标记为未看
# $when 是相对时间，如“3 分钟前”。
review-seen-changed = 看过之后在{ $when }又被改过
review-open-file = 打开文件（⇧⌘O）
    .tooltip = 打开文件（⇧⌘O）

## 进度列表（检查器的页）

review-progress-title = 进度
review-progress-empty = 没有打开的审阅
review-progress-noise = 已折叠的文件

## 失败态

review-failed-missing = 这个会话或它的 worktree 已不存在。
review-failed-not-in-project = 这个会话不属于已打开的项目。
# $reason 是 review-error-* 消息之一。
review-failed-changes = Lectern 无法比较这个 worktree：{ $reason }
review-error-unsupported = 仓库格式不支持
review-error-no-merge-base = 没有 merge-base
review-error-too-many-entries = 条目过多
review-error-io = 读取出错
review-error-cancelled = 已取消
