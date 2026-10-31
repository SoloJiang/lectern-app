# Conversation：检查器的对话页，含 Messages 页和 Terminal 页
# （docs/design/conversation.md「界面」）。键以 `conversation-` 开头。
# agent 与程序的名字、文件路径、命令和 agent 自己的错误文字作为参数传入，不写进消息。


# 页与标签
conversation-tab = 对话
conversation-page-messages = 消息
conversation-page-terminal = 终端

# 状态行。$percent 是 0 到 100 的整数。
conversation-status-history = 未连接
conversation-status-connecting = 正在连接…
conversation-status-idle = 空闲
conversation-status-working = 运行中
conversation-status-stopping = 正在停止…
conversation-status-waiting = 等待你确认
conversation-status-terminal = 在终端中运行
conversation-status-failed = 已断开
conversation-context = 上下文 { $percent }%

# 对话列表
conversation-earlier = 显示之前的 { $count } 轮
conversation-notes-sent = { $count } 条审阅意见
conversation-tool-read = 读取 { $target }
conversation-tool-edit = 修改 { $target }
conversation-tool-delete = 删除 { $target }
conversation-tool-move = 移动 { $target }
conversation-tool-search = 搜索 { $target }
conversation-tool-execute = 运行 { $target }
conversation-tool-fetch = 获取 { $target }
conversation-tool-agent = 派出子 agent
conversation-tool-more = 另有 { $count } 项
conversation-tool-failed = 失败
conversation-tool-denied = 已拒绝
conversation-tool-cancelled = 已取消

# 一轮结束的原因
conversation-ended-max-tokens = 回复达到了输出上限。
conversation-ended-max-turns = 这一轮达到了请求次数上限。
conversation-ended-refusal = agent 拒绝继续。
conversation-ended-cancelled = 已停止。
conversation-truncated = 只显示前 64 KB。

# 计划
conversation-plan = 计划 { $done }/{ $total }

# 权限请求卡片。按钮文字按选项的种类，不用 agent 自己写的名字。
conversation-permission-execute = 运行命令？
conversation-permission-edit = 修改 { $count } 个文件？
conversation-permission-other = 允许这个工具？
conversation-permission-workdir = 位置：{ $path }
conversation-permission-no-detail = agent 没有给出详细内容。
conversation-permission-omitted = 另有 { $count } 行
conversation-permission-allow-once = 允许
conversation-permission-allow-always = 总是允许
conversation-permission-reject-once = 拒绝
conversation-permission-reject-always = 总是拒绝
conversation-permission-more = 还有 { $count } 个请求

# 回复框
conversation-reply-label = 回复
conversation-reply-placeholder = 回复 { $agent }
conversation-send = 发送
conversation-stop = 停止

# 回复框不可用的原因。$program 是适配器的程序名，$agent 是产品名，都原样显示。
conversation-unavailable-not-resumable = 这个会话不能在这里继续。
conversation-unavailable-outside-project = 会话的文件夹不在已打开的项目里。
conversation-unavailable-not-installed = 安装 { $program } 后才能在这里继续这个会话。
conversation-unavailable-unsupported = Lectern 不支持这个 { $agent } 适配器。
conversation-unavailable-too-many = 已有 3 个对话在运行。先停止一个，再重试。
conversation-unavailable-service = 对话功能不可用。
conversation-in-terminal = 这个会话正在终端页中运行。
conversation-show-terminal = 显示终端
conversation-elsewhere = 这个会话可能还在别处运行。

# 与 agent 的连接出错。$agent 是产品名，原样显示。
conversation-error-spawn = 无法启动 { $program }。
conversation-error-unsupported = { $agent } 的适配器不能继续这个会话。
conversation-error-load = { $agent } 无法打开这个会话。
conversation-error-protocol = { $agent } 发来的回复无法识别。
conversation-error-exited = { $agent } 意外退出。
conversation-error-auth = 请先在 { $agent } 的命令行里登录，再重新发送。
conversation-error-prompt = { $agent } 返回了错误：
conversation-dismiss = 关闭

# 空状态与载入中
conversation-empty-no-session = 这个审阅不来自 agent 会话。
conversation-empty-missing = 会话日志已不存在。
conversation-empty-turns = 还没有消息。
conversation-unrecognized = 无法识别这个会话的格式。
conversation-error-history = 无法读取这个会话。
conversation-loading = 正在载入对话…

# Terminal 页。$command 是恢复命令，原样显示。
conversation-terminal-explain = 在这一页运行 { $command }。
conversation-terminal-resume = 在终端中恢复
conversation-terminal-disconnect-resume = 断开并恢复
conversation-terminal-linked = 先在「消息」页停止对话，再在这里恢复。
conversation-terminal-starting = 正在启动…
conversation-terminal-running = 运行中
conversation-terminal-exited = 已退出（{ $code }）
conversation-terminal-signaled = 被信号结束
conversation-terminal-close = 关闭
conversation-terminal-not-resumable = 这个会话不能恢复。
conversation-terminal-outside-project = 会话的文件夹不在已打开的项目里。
conversation-terminal-cwd-missing = 会话的文件夹已不存在。
conversation-terminal-too-many = 打开的终端太多。先关闭一个，再重试。
conversation-terminal-not-installed = 安装 { $program } 后才能恢复这个会话。
conversation-terminal-failed = 无法启动终端。
conversation-terminal-unavailable = 终端不可用。
conversation-leave-title = 还有 { $count } 个终端在运行
conversation-leave-detail = 终端关闭后，其中的会话也会停止。
conversation-leave-end = 结束终端
conversation-leave-cancel = 取消
