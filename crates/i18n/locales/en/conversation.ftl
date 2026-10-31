# Conversation: the inspector's Conversation page, with its Messages and Terminal pages
# (docs/design/conversation.md「界面」). Keys start with `conversation-`.
# Agent and program names, file paths, commands and the agent's own error text
# arrive as arguments; they are never part of a message.


# Page and tabs
conversation-tab = Conversation
conversation-page-messages = Messages
conversation-page-terminal = Terminal

# Status line. $percent is a whole number from 0 to 100.
conversation-status-history = Not connected
conversation-status-connecting = Connecting…
conversation-status-idle = Idle
conversation-status-working = Working
conversation-status-stopping = Stopping…
conversation-status-waiting = Waiting for you
conversation-status-terminal = Running in Terminal
conversation-status-failed = Disconnected
conversation-context = context { $percent }%

# Conversation list
conversation-earlier = { $count ->
    [one] Show { $count } earlier turn
   *[other] Show { $count } earlier turns
}
conversation-notes-sent = { $count ->
    [one] { $count } review note
   *[other] { $count } review notes
}
conversation-tool-read = Read { $target }
conversation-tool-edit = Edit { $target }
conversation-tool-delete = Delete { $target }
conversation-tool-move = Move { $target }
conversation-tool-search = Search { $target }
conversation-tool-execute = Run { $target }
conversation-tool-fetch = Fetch { $target }
conversation-tool-agent = Start a subagent
conversation-tool-more = { $count } more
conversation-tool-failed = failed
conversation-tool-denied = denied
conversation-tool-cancelled = cancelled

# Why a turn ended
conversation-ended-max-tokens = The reply reached the output limit.
conversation-ended-max-turns = The turn reached the request limit.
conversation-ended-refusal = The agent declined to continue.
conversation-ended-cancelled = Stopped.
conversation-truncated = Only the first 64 KB is shown.

# Plan
conversation-plan = Plan { $done }/{ $total }

# Permission card. Button texts follow the option kind, never the agent's own wording.
conversation-permission-execute = Run a command?
conversation-permission-edit = { $count ->
    [one] Change { $count } file?
   *[other] Change { $count } files?
}
conversation-permission-other = Allow this tool?
conversation-permission-workdir = in { $path }
conversation-permission-no-detail = The agent didn't send the details.
conversation-permission-omitted = { $count ->
    [one] { $count } more line
   *[other] { $count } more lines
}
conversation-permission-allow-once = Allow
conversation-permission-allow-always = Always Allow
conversation-permission-reject-once = Deny
conversation-permission-reject-always = Always Deny
conversation-permission-more = { $count ->
    [one] { $count } more request
   *[other] { $count } more requests
}

# Reply box
conversation-reply-label = Reply
conversation-reply-placeholder = Reply to { $agent }
conversation-send = Send
conversation-stop = Stop

# Why the reply box is unavailable. $program is the adapter program name and $agent a product name, both shown as written.
conversation-unavailable-not-resumable = This session can't be continued here.
conversation-unavailable-outside-project = The session's folder isn't in an open project.
conversation-unavailable-not-installed = Install { $program } to continue this session here.
conversation-unavailable-unsupported = Lectern doesn't support this { $agent } adapter.
conversation-unavailable-too-many = Three conversations are running. Stop one, then try again.
conversation-unavailable-service = Conversations aren't available.
conversation-in-terminal = This session is running in the Terminal page.
conversation-show-terminal = Show Terminal
conversation-elsewhere = This session may still be running somewhere else.

# Errors from the agent connection. $agent is a product name, shown as written.
conversation-error-spawn = Couldn't start { $program }.
conversation-error-unsupported = { $agent } can't continue this session through its adapter.
conversation-error-load = { $agent } couldn't open this session.
conversation-error-protocol = { $agent } sent a reply that Lectern can't read.
conversation-error-exited = { $agent } stopped unexpectedly.
conversation-error-auth = Sign in to { $agent } from its command line, then send again.
conversation-error-prompt = { $agent } returned an error:
conversation-dismiss = Dismiss

# Empty and loading states
conversation-empty-no-session = This review isn't from an agent session.
conversation-empty-missing = The session's log no longer exists.
conversation-empty-turns = No messages yet.
conversation-unrecognized = Lectern can't read this session's format.
conversation-error-history = Couldn't read this session.
conversation-loading = Loading the conversation…

# Terminal page. $command is the resume command, shown as written.
conversation-terminal-explain = Runs { $command } on this page.
conversation-terminal-resume = Resume in Terminal
conversation-terminal-disconnect-resume = Disconnect and Resume
conversation-terminal-linked = Stop the conversation on the Messages page, then resume here.
conversation-terminal-starting = Starting…
conversation-terminal-running = Running
conversation-terminal-exited = Exited ({ $code })
conversation-terminal-signaled = Ended by a signal
conversation-terminal-close = Close
conversation-terminal-not-resumable = This session can't be resumed.
conversation-terminal-outside-project = The session's folder isn't in an open project.
conversation-terminal-cwd-missing = The session's folder no longer exists.
conversation-terminal-too-many = Too many terminals are open. Close one, then try again.
conversation-terminal-not-installed = Install { $program } to resume this session.
conversation-terminal-failed = Couldn't start the terminal.
conversation-terminal-unavailable = The terminal isn't available.
conversation-leave-title = { $count ->
    [one] { $count } terminal still running
   *[other] { $count } terminals still running
}
conversation-leave-detail = Their sessions stop when the terminals close.
conversation-leave-end = End Terminals
conversation-leave-cancel = Cancel
