//! Tests of the project model and the plugin (docs/design/project.md
//! 「测试」): `open_*`, `worktrees_*`, `locate_*`, `resolve_*`, `facts_*`
//! (the model's submissions; the pattern semantics are in `facts/tests.rs`),
//! `dirs_*` (the directory table), `plugin_*` and `real_repo_*`; `shape_*`
//! checks bench's synthetic repositories (`testing::Shape`).

mod dirs;
mod facts;
mod locate;
mod open;
mod plugin;
mod real_repo;
mod refresh;
mod resolve;
mod shape;
mod support;
mod worktrees;
