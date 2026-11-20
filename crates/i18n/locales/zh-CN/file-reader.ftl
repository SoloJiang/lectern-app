# 文件阅读：完整文件的阅读视图、查找栏与各种状态
# （docs/design/file_reader.md「界面」）。键以 `file-reader-` 开头。
# 路径、符号链接的目标、包名和版本原样显示，经参数或原文传入，不是消息的一部分。

file-reader-edit = 编辑…
file-reader-done = 完成
file-reader-save = 保存
file-reader-save-anyway = 仍然保存
file-reader-back =
    .tooltip = 后退（⌘[）
file-reader-forward =
    .tooltip = 前进（⌘]）
file-reader-back-editing =
    .tooltip = 完成编辑后才能后退
# 显示在置灰的“编辑…”按钮上：这个文件为什么不能编辑。
file-reader-not-editable-not-loaded = 正在载入整个文件…
file-reader-not-editable-too-large = 超过 1 MB 的文件只能阅读
file-reader-not-editable-long-line = 有超过 4 KB 的行的文件只能阅读
file-reader-not-editable-not-text = 这个文件不是 UTF-8 文本
file-reader-not-editable-line-endings = 这个文件混用了换行符
file-reader-not-editable-read-only = 历史版本和依赖里的文件只能阅读
file-reader-not-editable-reloading = 磁盘上的文件已改动，正在重新载入
# 编辑期间磁盘上的文件改动时，工具栏下面的一行。
file-reader-changed-on-disk = 编辑期间磁盘上的文件已改动。
file-reader-merge = 合并
file-reader-discard-mine = 放弃我的改动
file-reader-keep-editing = 继续编辑
file-reader-unavailable-while-editing = 这个文件已不可用。请复制你的修改，或放弃它们。
# $name 是文件名，原样显示。
file-reader-save-failed = 无法保存 { $name }。
file-reader-find =
    .placeholder = 查找
# $current 与 $total 是匹配列表里的位置，从 1 数起。
file-reader-find-count = 第 { $current } 处，共 { $total } 处
# 搜索在 10,000 处匹配时停下。
file-reader-find-capped = 第 { $current } 处，共 10,000+ 处
file-reader-missing = 这个文件已不存在。
file-reader-unreadable = Lectern 无法读取这个文件。
file-reader-unsupported = 这一项不是普通文件。
# $target 是链接的目标，原样显示。
file-reader-symlink = 指向 { $target } 的符号链接
# $source 是这个版本的来源，如一次提交，原样显示。
file-reader-from-history = 来自 { $source } 的版本
# 打开文件超过一秒时显示。
file-reader-loading = 正在载入…
# 侧栏里文件树的标题，也是项目侧栏里它的入口的名字。
file-reader-files-title = 文件
file-reader-files-empty = 没有受跟踪的文件
file-reader-files-unavailable = Lectern 无法读取这个 worktree 的 index。
