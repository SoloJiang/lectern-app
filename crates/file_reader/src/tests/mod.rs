//! The crate's tests, one file per prefix (docs/design/file_reader.md「测试」).
//! No GPUI: the session is driven the way the view will drive it, with
//! fakes for the services and real files in temporary folders.

mod focus;
mod history;
mod logs;
mod plugin;
mod rig;
mod session;
