# Lectern

English · [简体中文](README.zh-CN.md)

Lectern is a native macOS app for Apple Silicon. You use it to review the code that Claude Code and Codex write in your repositories. For each agent session, it shows what you asked for, what the agent changed, and which parts of the change still need checking. Lectern is built for reading and reviewing code; file editing and the terminal are available and are hidden until you open them.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/review-dark.png">
  <img src="docs/images/review-light.png" alt="Reviewing a Claude Code session in Lectern">
</picture>

## Project overview

A project in Lectern is a folder or a git repository. When you open one, the project page lists three groups:

- **Needs Review:** agent sessions with changes you haven't reviewed yet. Sessions that are still running are listed here too, with their status.
- **Outside Any Session:** changes in the working tree that don't belong to any agent session.
- **Recently Done:** sessions you have finished reviewing.

Monorepos and multiple git worktrees are supported without extra configuration.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/overview-dark.png">
  <img src="docs/images/overview-light.png" alt="Project overview with sessions waiting for review">
</picture>

## Session review

Lectern shows a session in the order it happened. For each turn you see your prompt, the agent's reply, and the diff produced in that turn. Every changed line is linked to the turn that changed it, so you can see why a line changed. Lines that someone edited after the session ended are marked separately from the agent's changes. If Lectern doesn't recognize a session's log format, it shows a regular file-by-file diff instead.

While reviewing, you can:

- **Check the impact of a change.** The context panel lists the callers and tests of each changed symbol, and marks callers that the agent did not update. Results come from a local index. For Java, more precise results are available from an optional language server that you enable yourself.
- **Ask for an analysis.** The brief at the top of a session lists what to check first. It is built from local rules and costs no tokens. Analyze asks the Claude Code or Codex already signed in on your Mac to read the change and point out what's critical. It runs only when you ask, in a sandbox where it can read the change but can't write files or reach other hosts.
- **Send review notes to the agent.** You write notes on specific lines as you read. Lectern collects them and sends them to the session in one batch, with file paths and line numbers, so the agent can address them in its next turn.
- **Skip what doesn't need review.** Changes that only affect formatting are collapsed. Files you have marked as reviewed keep that mark.

## Parallel worktrees

When several agents run at the same time in separate git worktrees, Lectern lists the worktrees in two groups, Active and Idle. Selecting a worktree opens it in the inspector, which compares it with main: the files changed, the number of commits ahead, the tokens spent on the branch, and whether any of those files are also changed in another worktree.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/worktrees-dark.png">
  <img src="docs/images/worktrees-light.png" alt="Worktrees running agents in parallel">
</picture>

## Token usage and limits

Lectern counts tokens per project and per merge request. Project totals cover the last 7, 14 or 30 days. An MR's total adds up every session and turn that went into it; branches without an MR yet are counted the same way. Per-session and per-turn numbers remain available as a breakdown, and an MR's tokens are split into writing and review (Lectern's own analyses). Lectern also shows how much of your Codex and Claude Code usage limits you have used. Claude Code's status line sends its data to Lectern, so its limit windows update in real time.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/usage-dark.png">
  <img src="docs/images/usage-light.png" alt="Usage: tokens per day and limit windows">
</picture>

## Performance and platform

Lectern is developed against the following budgets ([ADR 0017](docs/adr/0017.md), [ADR 0018](docs/adr/0018.md)). Benchmarks in CI check every change against them.

- **Startup:** the first frame appears within 20 ms of launch. Everything else loads after that.
- **Memory:** under 60 MB with an empty window, and under 100 MB with a large monorepo open. A language server starts only when you enable it and stops when idle. Lectern runs no resident runtime such as Node or a JVM.
- **Install size:** under 80 MB. Support for additional languages, code hosts and agents is distributed as plugins and downloaded only when a project needs it. Plugins that run code are sandboxed and can only use the permissions they declare.
- **macOS integration:** Lectern uses the system fonts, colors and controls, follows the light or dark appearance setting, and uses standard macOS animations.

## Supported tools

- **Agents:** Claude Code and Codex. Lectern reads their local session files.
- **Code hosts:** GitHub and GitLab, including self-hosted instances of either.
- **Interface languages:** English and Simplified Chinese.
- **Hardware:** Macs with Apple Silicon.

## Status

Lectern is in early development and can't be used yet. The screenshots above are from the [interaction design](docs/design/interaction.md); they switch with your light or dark appearance setting.

## Development

Lectern is written in Rust. It uses [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) and [gpui-component](https://github.com/longbridge/gpui-component) for the interface, tree-sitter for parsing, gix for git, and [libghostty-vt](https://github.com/ghostty-org/ghostty) for the terminal. The design documents are currently written in Chinese:

- [Interaction spec](docs/design/interaction.md) and the [design canvas](https://claude.ai/artifact/LVSXNi9xPviRgzdycnskp9)
- [Technical research](docs/design/tech-research.md) and [architecture decision records](docs/adr/)
- [AGENTS.md](AGENTS.md): the rules coding agents follow in this repository

## License

Lectern is dual-licensed under MIT or Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE). Parts of the design follow ideas from Zed, but Lectern contains no GPL-licensed code from Zed.
