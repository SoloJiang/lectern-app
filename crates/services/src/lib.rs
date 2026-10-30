//! Service interfaces that plugins implement and depend on.
//!
//! Plugins never depend on each other (AGENTS.md architecture rule 7,
//! ADR 0025); they meet only through the traits, data types and events
//! defined here. Each module is one service area:
//!
//! - [`changes`]: change sources (`ChangeSource`, not a kernel service) and
//!   change lists, designed in `docs/design/review.md`.
//! - [`code_intel`]: code navigation (`dyn CodeIntel`) and semantic
//!   navigation providers, designed in `docs/design/code_intel.md`.
//! - [`project`]: the project model (`dyn Projects`), designed in
//!   `docs/design/project.md`; its directory table (`DirTable`) in
//!   `docs/design/file_reader.md`.
//! - [`reader`]: full file reading (`dyn Reader`), designed in
//!   `docs/design/file_reader.md`.
//! - [`review`]: review snapshots, commands and the project overview
//!   (`dyn Review`) and brief rule providers (`BriefRules`, not a kernel
//!   service), designed in `docs/design/review.md`.
//! - [`session`]: agent session sources (`dyn SessionSource`), designed in
//!   `docs/design/agents.md`.
//! - [`terminal`]: terminals running agent sessions and shells
//!   (`dyn Terminals`), their input and viewport frames, designed in
//!   `docs/design/terminal.md`.
//! - [`usage`]: token usage and rate limit snapshots, project merge commands
//!   and Analyze metering (`dyn Usage`) and MR sources (`MrSource`, not a
//!   kernel service), designed in `docs/design/usage.md`.
//! - [`workspace`]: window contributions, navigation commands and events
//!   (`dyn Contributions`) and leave guards (`LeaveGuard`, not a kernel
//!   service), designed in `docs/design/workspace.md`.

pub mod changes;
pub mod code_intel;
pub mod project;
pub mod reader;
pub mod review;
pub mod session;
pub mod terminal;
pub mod usage;
pub mod workspace;
