//! Runtime logging for Lectern (docs/design/logging.md, ADR 0046).
//!
//! Call sites use the five macros with a literal message and typed fields:
//!
//! ```
//! use lectern_logging::{info, warn, Value};
//! # let file = std::path::Path::new("/tmp/a.db");
//! # let elapsed = std::time::Duration::from_micros(12);
//! warn!("partition disabled"; path = Value::path(file), sqlite = 11);
//! info!("plugin activated"; took_us = elapsed);
//! ```
//!
//! A message is a string literal and a field value implements [`Field`],
//! whose values only come from the constructors of [`Value`]: no runtime
//! string reaches a record. The thread's correlation scope ([`scope`],
//! [`Ctx`]) is added to every record written on that thread.
//!
//! The `sink` feature (only `app` and tool crates enable it) installs the
//! logger: the buffer before the first frame, the `lectern-log` writer
//! thread, rotation and cleanup, verbose logging, the panic hook and export.
//! The `test-support` feature adds [`testing`].

mod callsite;
mod clock;
mod filter;
mod hooks;
mod scope;
mod value;

#[cfg(any(feature = "sink", feature = "test-support"))]
mod engine;
#[cfg(feature = "sink")]
mod sink;
#[cfg(feature = "test-support")]
pub mod testing;

#[cfg(all(test, feature = "sink"))]
mod tests;

pub use scope::{Ctx, PluginTag, Scope, current, scope};
pub use value::{Field, PanicText, Value};

pub use filter::Filter;
#[cfg(feature = "sink")]
pub use sink::{
    AlreadyInstalled, Config, Early, EarlyConfig, ExportError, ExportOptions, Exported,
    FilterSource, Flushed, Header, Logging, Status, Verbose, install_early,
};

/// What the macros expand to. Not part of the interface.
#[doc(hidden)]
pub mod __private {
    pub use crate::callsite::{Callsite, check_fields, check_message, emit};
    pub use log::{Level, max_level};
}

/// An error: an operation failed, a result or data was given up.
///
/// `error!("literal message"; name = value, ...)`: at most 12 fields, each
/// value implements [`Field`].
#[macro_export]
macro_rules! error {
    ($($t:tt)*) => { $crate::__log!($crate::__private::Level::Error, $($t)*) };
}

/// Degraded and carrying on: a skipped line, a retry, a slow path.
#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { $crate::__log!($crate::__private::Level::Warn, $($t)*) };
}

/// A lifecycle event: startup time, a plugin activated, a project opened.
#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { $crate::__log!($crate::__private::Level::Info, $($t)*) };
}

/// Detail for verbose logging. Not limited per call site.
#[macro_export]
macro_rules! debug {
    ($($t:tt)*) => { $crate::__log!($crate::__private::Level::Debug, $($t)*) };
}

/// Fine detail for verbose logging. Not limited per call site.
#[macro_export]
macro_rules! trace {
    ($($t:tt)*) => { $crate::__log!($crate::__private::Level::Trace, $($t)*) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __log {
    ($level:expr, $msg:literal $(;)?) => {
        $crate::__log!(@emit $level, $msg;)
    };
    ($level:expr, $msg:literal; $($key:ident = $value:expr),+ $(,)?) => {
        $crate::__log!(@emit $level, $msg; $($key = $value),+)
    };
    (@emit $level:expr, $msg:literal; $($key:ident = $value:expr),*) => {{
        $($crate::__reserved_key!($key);)*
        const _: () = $crate::__private::check_message($msg);
        const _: () = $crate::__private::check_fields(&[$(::core::stringify!($key)),*]);
        static __LECTERN_CALLSITE: $crate::__private::Callsite = $crate::__private::Callsite::new(
            ::core::module_path!(),
            ::core::file!(),
            ::core::line!(),
            $level,
            $msg,
        );
        if $level <= $crate::__private::max_level() {
            if let ::core::option::Option::Some(permit) = __LECTERN_CALLSITE.permit() {
                $crate::__private::emit(
                    &__LECTERN_CALLSITE,
                    permit,
                    &[$((::core::stringify!($key), $crate::Field::value(&$value))),*],
                );
            }
        }
    }};
}

/// The record's own keys and the scope's keys cannot be field names.
#[doc(hidden)]
#[macro_export]
macro_rules! __reserved_key {
    (t) => {
        ::core::compile_error!("`t` is a reserved log key")
    };
    (up) => {
        ::core::compile_error!("`up` is a reserved log key")
    };
    (lv) => {
        ::core::compile_error!("`lv` is a reserved log key")
    };
    (tg) => {
        ::core::compile_error!("`tg` is a reserved log key")
    };
    (th) => {
        ::core::compile_error!("`th` is a reserved log key")
    };
    (at) => {
        ::core::compile_error!("`at` is a reserved log key")
    };
    (msg) => {
        ::core::compile_error!("`msg` is a reserved log key")
    };
    (n) => {
        ::core::compile_error!("`n` is a reserved log key")
    };
    (trunc) => {
        ::core::compile_error!("`trunc` is a reserved log key")
    };
    (plugin) => {
        ::core::compile_error!("`plugin` is a scope key; set it with `scope`")
    };
    (act) => {
        ::core::compile_error!("`act` is a scope key; set it with `scope`")
    };
    (task) => {
        ::core::compile_error!("`task` is a scope key; set it with `scope`")
    };
    (session) => {
        ::core::compile_error!("`session` is a scope key; set it with `scope`")
    };
    (review) => {
        ::core::compile_error!("`review` is a scope key; set it with `scope`")
    };
    (req) => {
        ::core::compile_error!("`req` is a scope key; set it with `scope`")
    };
    (analysis) => {
        ::core::compile_error!("`analysis` is a scope key; set it with `scope`")
    };
    ($other:ident) => {};
}
