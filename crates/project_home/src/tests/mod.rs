//! The tests of docs/design/project_home.md「测试」that open no window:
//! `snapshot::build` (`build.rs`), the plugins in a real kernel (`plugin.rs`)
//! and the canary (`logs.rs`). None needs GPUI.

mod build;
mod logs;
mod plugin;
