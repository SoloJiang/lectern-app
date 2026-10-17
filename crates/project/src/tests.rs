//! Tests of the project model and the plugin (docs/design/project.md
//! 「测试」): `open_*`, `worktrees_*`, `locate_*`, `resolve_*`, `facts_*`
//! (the model's submissions; the pattern semantics are in `facts/tests.rs`),
//! `plugin_*` and `real_repo_*`.

mod facts;
mod locate;
mod open;
mod plugin;
mod real_repo;
mod resolve;
mod support;
mod worktrees;
