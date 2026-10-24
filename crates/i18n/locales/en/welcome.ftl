# Welcome window (docs/design/interaction.md, screen 01; docs/design/welcome.md「界面」).

welcome-title = Welcome to { -app-name }
welcome-subtitle = Open a folder to see what your agents changed.
welcome-open-folder = Open Folder…
    .tooltip = Open any folder or repository (⌘O)
welcome-clone = Clone…
welcome-recent = Recent Projects
welcome-recent-empty = No recent projects
welcome-recent-missing = Folder no longer exists
welcome-recent-remove = Remove from Recents
    .tooltip = Remove from Recents
welcome-discovered = From Claude Code and Codex
    .tooltip = Folders found in agent session history that you have not opened yet
welcome-discovered-empty = No agent sessions found yet
welcome-row-sessions = { $count ->
    [one] { $count } session
   *[other] { $count } sessions
}
welcome-row-running = { $count } running
welcome-language = Language
welcome-language-not-saved = The language setting couldn't be saved. The next launch uses the previous setting.

## Why a folder could not be opened, one message per OpenError.

welcome-open-failed-not-found = That folder no longer exists.
welcome-open-failed-not-a-directory = That path isn't a folder.
welcome-open-failed-permission = { -app-name } can't read that folder.
welcome-open-failed-repo = The repository in that folder can't be read.
welcome-open-failed-io = That folder couldn't be opened.
