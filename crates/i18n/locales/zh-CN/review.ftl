# 审阅：会话简报、改动叙事、底部浮条。

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
