//! Tests of the watcher through its public interface, with `FakeBackend`,
//! `FakeSource`, the manual watcher and `Executor::manual()`.

mod coalesce;
mod config;
mod dir;
mod filter;
mod flush;
mod health;
mod lifecycle;
mod quiescence;
mod reflog;
mod revision;
mod support;
