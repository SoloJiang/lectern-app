# Project home: the project sidebar and the main screen with the session cards
# (docs/design/project_home.md「界面」). Keys start with `project-home-`.
# Session titles, branch names, package names and agent names are shown as
# written; they arrive as arguments, not as part of a message.

project-home-overview = Overview
project-home-sessions = Sessions
project-home-sessions-empty = No sessions in the last 14 days
# The row under the newest sessions that unfolds the rest, and folds it again.
project-home-sessions-more = { $count } more
project-home-sessions-less = Show less
# The small heading over the entries other crates add (Worktrees, Files, ...).
project-home-views = Project
project-home-untitled-session = Untitled session
# The accessible name of the green dot at the end of a running session's row.
project-home-running = Running
project-home-packages = Packages
# The accessible name and hint of the number at the end of a package row.
project-home-changed-today = { $count } today
project-home-usage = Usage
# $tokens is a compact count such as 12.3K.
project-home-usage-7d = { $tokens } tokens · 7 days
# $agent is the agent's name; $window is project-home-window-hours or -days.
project-home-limit = { $agent } { $window } limit · { $percent }%
project-home-limit-no-window = { $agent } limit · { $percent }%
project-home-window-hours = { $count }-hour
project-home-window-days = { $count }-day
project-home-title = { $count ->
    [0] Nothing needs review
    [one] { $count } session needs review
   *[other] { $count } sessions need review
}
# $branch is the current branch.
project-home-subtitle = { $branch } · { $count ->
    [one] { $count } worktree
   *[other] { $count } worktrees
}
# The two sentences under the large title; each is left out when its count is 0.
project-home-lede-running = { $count ->
    [one] { $count } agent is running.
   *[other] { $count } agents are running.
}
project-home-lede-outside = { $count ->
    [one] { $count } file changed outside any session.
   *[other] { $count } files changed outside any session.
}
project-home-needs-review = Needs Review
project-home-outside = Outside Any Session
# $branch is the current branch.
project-home-outside-files = { $count ->
    [one] { $count } file changed on { $branch }
   *[other] { $count } files changed on { $branch }
}
project-home-outside-review = Review
project-home-outside-capped = { $count } changed paths (not attributed)
project-home-done = Recently Done
project-home-all-reviewed = Every session is reviewed.
project-home-empty = No agent sessions in this project yet.
# The accessible name of the progress ring; the figures beside it are -short.
project-home-progress = { $seen }/{ $total } reviewed
project-home-progress-short = { $seen }/{ $total }

## The status word at the end of a card's first line

project-home-status-running = running
# Not shown yet: `agents` has no ended state for a session
# (docs/design/project_home.md「对其他 crate 的要求」F).
project-home-status-waiting = waiting for you
project-home-status-ended = ended

## What a Recently Done row says on the right

# Not shown yet: they need git commits and merge requests
# (docs/design/project_home.md「界面」); the row shows the time meanwhile.
project-home-done-committed = Committed
project-home-done-merged = MR #{ $number } merged

## Signals on a session card's second line

project-home-signal-drifted = Drifted
project-home-signal-critical = { $count } critical
project-home-signal-changed = { $count ->
    [one] { $count } file changed since seen
   *[other] { $count } files changed since seen
}
# Not shown yet: the next two need the code_intel brief rules, the test result
# needs the merge request pipeline (docs/design/project_home.md「界面」).
project-home-signal-public-package = Public package changed
project-home-signal-api = API changed
project-home-signal-tests = Tests { $result }

## Agent names

project-home-agent-claude-code = Claude Code
project-home-agent-codex = Codex
