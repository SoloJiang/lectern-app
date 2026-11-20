# File reader: the full-file reading view, its find bar and its states
# (docs/design/file_reader.md「界面」). Keys start with `file-reader-`.
# Paths, symbolic link targets, package names and versions are shown as written;
# they arrive as arguments or as verbatim text, not as part of a message.

file-reader-edit = Edit…
file-reader-done = Done
file-reader-save = Save
file-reader-save-anyway = Save Anyway
file-reader-back =
    .tooltip = Back (⌘[)
file-reader-forward =
    .tooltip = Forward (⌘])
file-reader-back-editing =
    .tooltip = Finish editing to go back
# Shown on a disabled Edit… button: why the file can't be edited.
file-reader-not-editable-not-loaded = Loading the whole file…
file-reader-not-editable-too-large = Files over 1 MB open read-only
file-reader-not-editable-long-line = Files with lines over 4 KB open read-only
file-reader-not-editable-not-text = This file isn't UTF-8 text
file-reader-not-editable-line-endings = This file mixes line endings
file-reader-not-editable-read-only = Files from history and dependencies are read-only
file-reader-not-editable-reloading = The file changed on disk and is reloading
# The row under the toolbar when the file changed on disk during an edit.
file-reader-changed-on-disk = This file changed on disk while you were editing.
file-reader-merge = Merge
file-reader-discard-mine = Discard My Changes
file-reader-keep-editing = Keep Editing
file-reader-unavailable-while-editing = This file is no longer available. Copy your changes or discard them.
# $name is the file's name, shown as written.
file-reader-save-failed = Couldn't save { $name }.
file-reader-find =
    .placeholder = Find
# $current and $total are positions in the list of matches, counted from 1.
file-reader-find-count = { $current } of { $total }
# The search stops at 10,000 matches.
file-reader-find-capped = { $current } of 10,000+
file-reader-missing = This file no longer exists.
file-reader-unreadable = Lectern can't read this file.
file-reader-unsupported = This item isn't a regular file.
# $target is the link's target, shown as written.
file-reader-symlink = Symbolic link to { $target }
# $source is where the version comes from, such as a commit, shown as written.
file-reader-from-history = Version from { $source }
# Shown when opening the file takes longer than a second.
file-reader-loading = Loading…
# The title of the Files tree in the sidebar, and of its entry in the project sidebar.
file-reader-files-title = Files
file-reader-files-empty = No tracked files
file-reader-files-unavailable = Lectern can't read this worktree's index.
