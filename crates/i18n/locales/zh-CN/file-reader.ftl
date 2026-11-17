# 文件阅读：完整文件的阅读视图、查找栏与各种状态
# （docs/design/file_reader.md「界面」）。键以 `file-reader-` 开头。
# 路径、符号链接的目标、包名和版本原样显示，经参数或原文传入，不是消息的一部分。

file-reader-back =
    .tooltip = 后退（⌘[）
file-reader-forward =
    .tooltip = 前进（⌘]）
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
