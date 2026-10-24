# Project home: the project sidebar and the main screen with the session cards
# (docs/design/project_home.md「界面」). Keys start with `project-home-`.
# Session titles, branch names, package names and agent names are shown as
# written; they arrive as arguments, not as part of a message.

project-home-overview = Overview
project-home-sessions = Sessions
project-home-sessions-empty = No sessions in the last 14 days
project-home-current-worktree = current
project-home-other-worktrees = Other worktrees
project-home-untitled-session = Untitled session
project-home-running = Running
project-home-packages = Packages
project-home-changed-today = { $count } today
# $tokens is a compact count such as 12.3K.
project-home-usage-7d = { $tokens } tokens · 7 days
# $agent is the agent's name.
project-home-limit = { $agent } { $percent }% used
project-home-title = { $count ->
    [0] Nothing needs review
    [one] { $count } session needs review
   *[other] { $count } sessions need review
}
project-home-subtitle = { $count ->
    [0] All caught up
   *[other] { $count } to review
}
project-home-needs-review = Needs Review
project-home-outside = Outside Any Session
project-home-outside-files = { $count ->
    [one] { $count } changed file
   *[other] { $count } changed files
}
project-home-outside-capped = { $count } changed paths (not attributed)
project-home-done = Recently Done
project-home-all-reviewed = Every session is reviewed.
project-home-empty = No agent sessions in this project yet.
project-home-progress = { $seen }/{ $total } reviewed

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
