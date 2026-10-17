//! Service interfaces that plugins implement and depend on.
//!
//! Plugins never depend on each other (AGENTS.md architecture rule 7,
//! ADR 0025); they meet only through the traits, data types and events
//! defined here. Each module is one service area:
//!
//! - [`project`]: the project model (`dyn Projects`), designed in
//!   `docs/design/project.md`.

pub mod project;
