//! Service interfaces that plugins implement and depend on.
//!
//! Plugins never depend on each other (AGENTS.md architecture rule 7,
//! ADR 0025); they meet only through the traits, data types and events
//! defined here. Each module is one service area:
//!
//! - [`code_intel`]: code navigation (`dyn CodeIntel`) and semantic
//!   navigation providers, designed in `docs/design/code_intel.md`.
//! - [`project`]: the project model (`dyn Projects`), designed in
//!   `docs/design/project.md`.
//! - [`session`]: agent session sources (`dyn SessionSource`), designed in
//!   `docs/design/agents.md`.

pub mod code_intel;
pub mod project;
pub mod session;
