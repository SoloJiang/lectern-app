# Review: session brief, change narrative, floating bar (docs/design/review_view.md「界面」).

review-looks-good = Looks Good
    .tooltip = Mark every file as reviewed
# The accessible name of the progress bar; the Files page shows review-files-reviewed.
review-progress = { $reviewed }/{ $total } reviewed
# The line above the Files page's bar and the percentage at its end.
review-files-reviewed = { $reviewed } of { $total } reviewed
review-files-percent = { $percent }%
review-files-changed = { $count ->
    [one] { $count } file changed
   *[other] { $count } files changed
}
review-comments = { $count ->
    [0] No review notes
    [one] { $count } review note
   *[other] { $count } review notes
}
# $path is shown as written.
review-reviewing = Reviewing { $path }

## Toolbar and floating bar

review-title-untitled = Untitled session
review-title-outside = Outside any session
# $base is a reference name or a commit, shown as written.
review-title-range = Changes against { $base }
# Not available until the conversation view exists. $agent is a product name, shown as written.
review-send = Send
review-send-to = Send to { $agent }
review-send-to-shortcut = ⌘↩
review-mark-reviewed = Mark Reviewed
review-mark-reviewed-shortcut = V
review-hide-formatting = Hide formatting
# Not shown until structured diffs exist (#21).
review-formatting-hidden = { $count ->
    [one] { $count } formatting-only line hidden
   *[other] { $count } formatting-only lines hidden
}
review-all-reviewed = All reviewed
review-mode-unified = Unified
review-mode-side-by-side = Side by side
# The View menu's items.
review-refresh = Refresh
    .tooltip = Compare the working tree again
review-loading = Preparing the review…
review-cancel = Cancel
review-back = Back

## Header

review-stats-turns = { $count ->
    [one] { $count } turn
   *[other] { $count } turns
}
review-stats-files = { $count ->
    [one] { $count } file
   *[other] { $count } files
}
# Not shown until the review counts the packages a change touches.
review-stats-files-packages = { $files ->
    [one] { $files } file
   *[other] { $files } files
} in { $packages ->
    [one] { $packages } package
   *[other] { $packages } packages
}

## Brief

review-brief-title = Worth a look first
review-brief-source = from local rules
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
# $agent is a product name, shown as written.
review-analyze-with = Analyze with { $agent }
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
review-added = +{ $count }
review-removed = −{ $count }
review-turn-files = { $count ->
    [one] { $count } file
   *[other] { $count } files
}
review-turn-none = no changes
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
review-open-file = Open File
review-open-file-shortcut = ⇧⌘O
# $time is a time of day. Not shown until the review knows when a drifted block was edited.
review-drift-note = Drifted — you edited these lines at { $time }, after the session.
review-drift-note-untimed = Drifted — these lines changed after the session.
review-compare = Compare

## Progress list (the inspector page)

review-progress-title = Files
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

## Review notes (#22). The prompt itself is English and not translated (it is read by the agent).

review-copy-prompt = Copy as Prompt
    .tooltip = Copy the review notes as a prompt, then paste it into the session
review-notes-discard = Discard
review-notes-discard-confirm = { $count ->
    [one] Discard { $count } review note?
   *[other] Discard { $count } review notes?
}
    .detail = They haven't been sent. This can't be undone.
review-notes-sending = Sending…
review-notes-copied = { $count ->
    [one] Copied { $count } review note. Paste it into the session.
   *[other] Copied { $count } review notes. Paste them into the session.
}
review-notes-pasted = { $count ->
    [one] Pasted { $count } review note into the terminal. Press Return there to send.
   *[other] Pasted { $count } review notes into the terminal. Press Return there to send.
}
review-notes-sent = { $count ->
    [one] Sent { $count } review note
   *[other] Sent { $count } review notes
}
# $reason is the text of one of the review-notes-error-* messages.
review-notes-not-sent = Not sent: { $reason }
review-notes-error-starting = the terminal is still starting
review-notes-error-busy = the agent is working
review-notes-error-no-session = no session can receive them
review-notes-error-timed-out = no answer in 10 seconds
review-notes-error-clipboard = the clipboard couldn't be written
review-notes-error-failed = the session didn't take them
review-notes-full = Up to { $count } notes can be unsent at a time. Send or discard some first.
review-notes-copy-again = Copy Again
    .tooltip = { $count ->
        [one] Copy the { $count } review note you sent last, with the same marker
       *[other] Copy the { $count } review notes you sent last, with the same marker
    }
review-notes-not-copied = Not copied: { $reason }
review-notes-elsewhere = Notes on files no longer in this review
review-note-placeholder = Write a note for the agent
review-note-add = Add Note
review-note-save = Save
review-note-too-long = This note is too long.
review-note-add-here = Add Note
    .tooltip = Add a review note (C)
review-note-edit = Edit
review-note-delete = Delete
review-note-pending = Not sent
# $when is a relative time, e.g. "2 minutes ago".
review-note-sent = Sent { $when }
review-note-copied = Copied { $when }
# $line is 1-based.
review-note-drifted = Line { $line } changed after this note was written
review-turn-answers = { $count ->
    [one] answers { $count } review note
   *[other] answers { $count } review notes
}
