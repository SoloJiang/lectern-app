# Worktrees: the parallel agent overview, one row per worktree, and its inspector page
# (docs/design/worktrees.md「界面」). Keys start with `worktrees-`.
# Branch names, folder paths, session titles and MR titles are shown as written;
# they arrive as arguments, not as part of a message.

worktrees-entry = Worktrees
worktrees-title = Worktrees
# $active is the number of worktrees with a running session; $total is the number of worktrees.
worktrees-subtitle = { $active } active · { $total ->
    [one] { $total } worktree
   *[other] { $total } worktrees
}
worktrees-refresh = Refresh
worktrees-active = Active
worktrees-idle = Idle
worktrees-only-one = This repository has one worktree. Agents working in other worktrees appear here.
worktrees-not-git = This folder is not a git repository.
# The small labels after a branch name.
worktrees-current = Current
worktrees-locked = Locked
worktrees-missing = Missing
# The small text after a commit that is checked out without a branch, and after a branch without commits.
worktrees-detached = detached
worktrees-unborn = no commits
worktrees-head-unknown = Unknown HEAD
worktrees-running = { $count } running
worktrees-no-sessions = No sessions in the last 14 days
worktrees-uncommitted = { $count } uncommitted
# $base is the short name of the main branch, such as origin/main.
worktrees-vs-base = { $count ->
    [one] { $count } file
   *[other] { $count } files
} vs { $base }
# $limit is the number of changed files after which the count stops.
worktrees-capped = { $limit }+ files
# $tokens is a compact count such as 12.3K.
worktrees-tokens = { $tokens } tokens
worktrees-mr-more = +{ $count } MR
worktrees-detail-title = Worktree
worktrees-detail-empty = No worktree selected
worktrees-usage-branch = Branch Usage
worktrees-usage-mr = MR Usage
worktrees-tokens-input = Input
worktrees-tokens-output = Output
worktrees-tokens-cache-read = Cache read
worktrees-tokens-cache-write = Cache write
worktrees-tokens-review = Review
# $time is a relative time such as "3 days ago".
worktrees-usage-since = Since { $time }
worktrees-usage-inherited = May include context inherited from a parent session
worktrees-usage-backfilling = Still reading earlier sessions
worktrees-usage-not-counted = Not counted on the default branch or a detached HEAD
worktrees-mr-shared = { $count ->
    [one] Shares the branch with { $count } other MR
   *[other] Shares the branch with { $count } other MRs
}
# $base is the short name of the main branch, such as origin/main.
worktrees-compare-title = Compared with { $base }
worktrees-compare-head = Uncommitted Changes
worktrees-committed = { $count ->
    [one] { $count } committed file
   *[other] { $count } committed files
}
worktrees-uncommitted-files = { $count ->
    [one] { $count } uncommitted file
   *[other] { $count } uncommitted files
}
worktrees-total = { $count ->
    [one] { $count } file in total
   *[other] { $count } files in total
}
# $time is a relative time such as "3 minutes ago".
worktrees-counted-at = Counted { $time }
worktrees-review-changes = Review Changes
worktrees-missing-detail = The worktree folder no longer exists.
worktrees-overlaps-title = Overlapping Files
worktrees-overlap-files = { $count ->
    [one] { $count } file
   *[other] { $count } files
}
worktrees-overlap-more = { $count } more
worktrees-no-overlap = No changed files in common with other worktrees
worktrees-overlap-unknown = Too many changes to compare
worktrees-sessions-title = Recent Sessions
worktrees-untitled-session = Untitled session
worktrees-switch = Switch to This Worktree
