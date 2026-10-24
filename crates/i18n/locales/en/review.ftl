# Review: session brief, change narrative, floating bar (docs/design/review_view.md「界面」).

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

## Toolbar and floating bar

review-title-untitled = Untitled session
review-title-outside = Outside any session
# $base is a reference name or a commit, shown as written.
review-title-range = Changes against { $base }
# Not available until the conversation view exists.
review-send = Send
review-all-reviewed = All reviewed
review-mode-unified = Unified
review-mode-side-by-side = Side by side
review-refresh = Refresh
    .tooltip = Compare the working tree again
review-loading = Preparing the review…
review-cancel = Cancel
review-back = Back

## Brief

review-brief-title = Worth a look first
review-brief-empty = Nothing stands out.
review-brief-omitted = { $count } more
review-brief-pending = Still gathering…
# $path is shown as written.
review-brief-changed-since-seen = { $path } changed after you reviewed it
review-brief-drifted = { $path } has changes Lectern can't attribute
review-brief-edit-failed = An edit to { $path } failed; the agent may think it applied
review-brief-command-only = { $count ->
    [one] { $count } file changed by a command, not by an edit
   *[other] { $count } files changed by a command, not by an edit
}
review-brief-sensitive-agent-instructions = { $path } is loaded by agents as instructions
review-brief-sensitive-ci = { $path } runs in CI
review-brief-sensitive-hooks = { $path } runs on commit
review-brief-sensitive-git-metadata = { $path } changes how git treats files
review-brief-sensitive-symlink = { $path } is a new or retargeted symbolic link
review-brief-sensitive-executable = { $path } became executable
review-brief-sensitive-submodule = { $path } points to another commit
review-brief-deleted = { $count ->
    [one] { $count } tracked file deleted
   *[other] { $count } tracked files deleted
}
review-brief-incomplete-truncated = { $count ->
    [one] { $count } file shown without a full diff: truncated
   *[other] { $count } files shown without a full diff: truncated
}
review-brief-incomplete-too-large = { $count ->
    [one] { $count } file shown without a full diff: too large
   *[other] { $count } files shown without a full diff: too large
}
review-brief-incomplete-binary = { $count ->
    [one] { $count } file shown without a full diff: binary
   *[other] { $count } files shown without a full diff: binary
}
review-brief-incomplete-not-retained = { $count ->
    [one] { $count } file shown without a full diff: content not kept
   *[other] { $count } files shown without a full diff: content not kept
}
review-brief-incomplete-filtered = { $count ->
    [one] { $count } file shown without a full diff: filtered by git
   *[other] { $count } files shown without a full diff: filtered by git
}
review-brief-incomplete-unavailable = { $count ->
    [one] { $count } file shown without a full diff: unreadable
   *[other] { $count } files shown without a full diff: unreadable
}
review-brief-incomplete-listed = { $count ->
    [one] { $count } file shown without a full diff: listed only
   *[other] { $count } files shown without a full diff: listed only
}
review-brief-noise-lockfile = { $count ->
    [one] { $count } lockfile folded
   *[other] { $count } lockfiles folded
}
review-brief-noise-generated = { $count ->
    [one] { $count } generated file folded
   *[other] { $count } generated files folded
}
review-brief-noise-snapshot = { $count ->
    [one] { $count } snapshot folded
   *[other] { $count } snapshots folded
}
# A rule from a provider that has no message of its own; $rule is shown as written.
review-brief-provided = { $rule }
# $branch is shown as written; $tokens is a compact count such as 12.3K.
review-brief-usage-row = { $branch } · { $tokens } tokens · { $percent }% this session
review-brief-usage-shared = { $count ->
    [one] shared with { $count } other MR
   *[other] shared with { $count } other MRs
}
review-brief-usage-unassigned = Not on any branch

## Analyze

review-analyze = Analyze
# $reason is shown as written.
review-analyze-unavailable = Analyze isn't available: { $reason }
review-analyze-running = Analyzing…
review-analyze-stale = Based on an earlier version
review-analyze-partial = Some content wasn't provided to the analysis
review-analyze-failed = Analysis failed: { $reason }
review-analyze-again = Analyze again

## Notices

review-notice-base-current-head = No reflog: the base may not be where this session started.
review-notice-unrecognized = Lectern can't read this session's log; changes are shown by file.
review-notice-folder = This folder isn't a git repository: only the recorded edits are shown.
review-notice-unclaimed = { $count ->
    [one] { $count } more file changed by other sessions or by hand
   *[other] { $count } more files changed by other sessions or by hand
}
review-notice-invalid-paths = { $count ->
    [one] { $count } path couldn't be read
   *[other] { $count } paths couldn't be read
}

## Narrative: turns, steps, hunks

review-earlier-turns = { $count ->
    [one] { $count } earlier turn
   *[other] { $count } earlier turns
}
review-turn = Turn { $ordinal }
review-turn-tokens = { $tokens } tokens
review-turn-running = Running…
review-step-counts = +{ $added } −{ $removed }
review-step-partial = Part of this edit didn't apply
review-step-unaligned = Shown as recorded; it doesn't match the working tree
review-step-survival-partly = Partly overwritten since
review-step-survival-none = Overwritten since
review-hunk-turn = Turn { $ordinal }
review-hunk-other-session = { $count ->
    [one] { $count } other session
   *[other] { $count } other sessions
}
review-hunk-drifted = Drifted
review-hunk-drifted-lines = { $count ->
    [one] { $count } drifted line
   *[other] { $count } drifted lines
}
review-fold = { $count ->
    [one] Show { $count } unchanged line
   *[other] Show { $count } unchanged lines
}
review-noise-folded = { $count ->
    [one] { $count } lockfile, generated file or snapshot folded
   *[other] { $count } lockfiles, generated files and snapshots folded
}
review-noise-show = Show
review-noise-hide = Hide
# $command is shown as written.
review-command = Ran { $command }
review-command-files = { $count ->
    [one] { $count } file changed while it ran
   *[other] { $count } files changed while it ran
}
review-subagent = Spawned a subagent
# $name is the tool's name, shown as written.
review-tool = { $name }
review-tool-failed = { $name } failed
review-injected = Added to the conversation
review-mark-seen = Mark as reviewed (V)
    .tooltip = Mark as reviewed (V)
review-mark-unseen = Mark as not reviewed
    .tooltip = Mark as not reviewed
# $when is a relative time such as "3 min ago".
review-seen-changed = Changed { $when } after you reviewed it
review-open-file = Open file (⇧⌘O)
    .tooltip = Open file (⇧⌘O)

## Progress list (the inspector page)

review-progress-title = Progress
review-progress-empty = No review open
review-progress-noise = Folded files

## Failed states

review-failed-missing = This session or its worktree no longer exists.
review-failed-not-in-project = This session doesn't belong to an open project.
# $reason is one of the review-error-* messages.
review-failed-changes = Lectern can't compare this worktree: { $reason }
review-error-unsupported = unsupported repository format
review-error-no-merge-base = no merge base
review-error-too-many-entries = too many entries
review-error-io = read error
review-error-cancelled = cancelled
