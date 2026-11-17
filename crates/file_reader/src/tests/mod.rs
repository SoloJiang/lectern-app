//! The crate's tests, one file per prefix (docs/design/file_reader.md「测试」).
//! The session is driven the way the view drives it, with fakes for the
//! services and real files in temporary folders; the views run in a test
//! window.

mod files;
mod focus;
mod history;
mod logs;
mod plugin;
mod rig;
mod session;
mod view;
