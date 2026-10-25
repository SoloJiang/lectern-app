//! Unit tests (docs/design/app.md「测试」). The startup tests that launch the
//! real binary are in `tests/startup.rs`.

mod args;
mod leave;
mod logs;
#[cfg(feature = "ui-probe")]
mod probe;
mod profile;
mod quit;
mod startup;
mod ui;
mod window_state;
