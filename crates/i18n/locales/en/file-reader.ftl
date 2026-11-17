# File reader: the full-file reading view, its find bar and its states
# (docs/design/file_reader.md「界面」). Keys start with `file-reader-`.
# Paths, symbolic link targets, package names and versions are shown as written;
# they arrive as arguments or as verbatim text, not as part of a message.

file-reader-back =
    .tooltip = Back (⌘[)
file-reader-forward =
    .tooltip = Forward (⌘])
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
