# Review: session brief, change narrative, floating bar.

review-looks-good = Looks good
    .tooltip = Mark every file as reviewed
review-progress = { $reviewed }/{ $total } reviewed
review-files-changed = { $count ->
    [one] { $count } file changed
   *[other] { $count } files changed
}
review-comments = { $count ->
    [0] No comments
    [one] { $count } comment
   *[other] { $count } comments
}
# $path is shown as written.
review-reviewing = Reviewing { $path }
