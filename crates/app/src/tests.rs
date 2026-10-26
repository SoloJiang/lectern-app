//! Unit tests (docs/design/app.md「测试」). The startup tests that launch the
//! real binary are in `tests/startup.rs`.

mod args;
#[cfg(feature = "bench-probe")]
mod bench;
mod leave;
mod logs;
#[cfg(feature = "ui-probe")]
mod probe;
mod profile;
mod quit;
mod startup;
mod ui;
mod window_state;
