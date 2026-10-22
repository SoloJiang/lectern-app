//! Field values (docs/design/logging.md「调用点」). No constructor takes a
//! runtime string: a field is a number, a bool, a `&'static str`, a path, an
//! OID, a UUID, a URL with only scheme, host and path, or an `io::Error`'s
//! kind.

use std::fmt;
use std::io;
use std::path::Path;
use std::time::Duration;
#[cfg(feature = "sink")]
use std::time::SystemTime;

/// A type that can be a log field. Implementations build their value with
/// the constructors of [`Value`].
pub trait Field {
    fn value(&self) -> Value<'_>;
}

/// A field value.
#[derive(Clone, Copy)]
pub struct Value<'a>(pub(crate) Repr<'a>);

#[derive(Clone, Copy)]
pub(crate) enum Repr<'a> {
    Null,
    Bool(bool),
    U64(u64),
    I64(i64),
    U128(u128),
    I128(i128),
    F64(f64),
    Text(&'static str),
    Path(&'a Path),
    Oid(&'a [u8]),
    Uuid([u8; 16]),
    Hex(u64),
    Url(&'a str),
    Io(io::ErrorKind, Option<i32>),
    Panic(PanicText<'a>),
    /// The panic hook's backtrace: frame names (code, not data).
    #[cfg(feature = "sink")]
    Frames(&'a str),
    /// A time the logger writes itself (`until` of verbose logging).
    #[cfg(feature = "sink")]
    Time(SystemTime),
}

/// What a panic payload is, which decides how its text is handled
/// (docs/design/logging.md「隐私」). Built by the caller from the payload's
/// type, never from its content.
#[derive(Clone, Copy)]
pub enum PanicText<'a> {
    /// The payload is a `&'static str`: `panic!("literal")`, `Option::unwrap`.
    Static(&'static str),
    /// The payload is a `String`: a formatted message, which may hold data.
    Formatted(&'a str),
    /// Any other type (`panic_any`).
    Other,
}

impl fmt::Debug for PanicText<'_> {
    /// Never shows a formatted message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PanicText::Static(_) => f.write_str("Static(..)"),
            PanicText::Formatted(s) => write!(f, "Formatted({} bytes)", s.len()),
            PanicText::Other => f.write_str("Other"),
        }
    }
}

impl<'a> Value<'a> {
    pub const fn text(s: &'static str) -> Value<'static> {
        Value(Repr::Text(s))
    }

    /// Written with the home directory as `~`, cut at 1024 bytes; bytes that
    /// are not UTF-8 become U+FFFD.
    pub fn path(p: &'a Path) -> Value<'a> {
        Value(Repr::Path(p))
    }

    /// Lowercase hex, at most 32 bytes (SHA-1 and SHA-256 OIDs).
    pub fn oid(bytes: &'a [u8]) -> Value<'a> {
        Value(Repr::Oid(bytes))
    }

    /// 8-4-4-4-12 lowercase. A session ID that is not a UUID is passed as its
    /// 128-bit hash.
    pub const fn uuid(bytes: [u8; 16]) -> Value<'static> {
        Value(Repr::Uuid(bytes))
    }

    /// 16 lowercase hex digits.
    pub const fn hex(v: u64) -> Value<'static> {
        Value(Repr::Hex(v))
    }

    /// Only `scheme://host[:port]/path`: userinfo, query and fragment are
    /// dropped; `"<url>"` when it does not parse.
    pub fn url(s: &'a str) -> Value<'a> {
        Value(Repr::Url(s))
    }

    /// An `io::Error`'s kind and OS error code; never its message.
    pub fn io(e: &io::Error) -> Value<'static> {
        Value(Repr::Io(e.kind(), e.raw_os_error()))
    }

    /// A panic message, handled by the rules of「隐私」when written. Called
    /// only from `kernel`, `executor` and this crate (architecture check).
    pub fn panic_message(text: PanicText<'a>) -> Value<'a> {
        Value(Repr::Panic(text))
    }

    pub(crate) fn kv_value(&self) -> log::kv::Value<'_> {
        match self.0 {
            Repr::Null => log::kv::Value::null(),
            Repr::Bool(b) => b.into(),
            Repr::U64(v) => v.into(),
            Repr::I64(v) => v.into(),
            Repr::U128(v) => v.into(),
            Repr::I128(v) => v.into(),
            Repr::F64(v) => v.into(),
            Repr::Text(s) => s.into(),
            _ => log::kv::Value::from_display(self),
        }
    }
}

impl fmt::Debug for Value<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Value({self})")
    }
}

/// The text other `log` implementations see through the record's key-values.
/// Lectern's own logger encodes values itself.
impl fmt::Display for Value<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Repr::Null => f.write_str("null"),
            Repr::Bool(b) => write!(f, "{b}"),
            Repr::U64(v) => write!(f, "{v}"),
            Repr::I64(v) => write!(f, "{v}"),
            Repr::U128(v) => write!(f, "{v}"),
            Repr::I128(v) => write!(f, "{v}"),
            Repr::F64(v) => write!(f, "{v}"),
            Repr::Text(s) => f.write_str(s),
            Repr::Path(p) => write!(f, "{}", p.display()),
            Repr::Oid(b) => b.iter().take(32).try_for_each(|x| write!(f, "{x:02x}")),
            Repr::Uuid(u) => write_uuid(f, &u),
            Repr::Hex(v) => write!(f, "{v:016x}"),
            Repr::Url(s) => match url_parts(s) {
                Some((scheme, host, path)) => write!(f, "{scheme}://{host}{path}"),
                None => f.write_str("<url>"),
            },
            Repr::Io(kind, code) => match code {
                Some(code) => write!(f, "{kind:?} ({code})"),
                None => write!(f, "{kind:?}"),
            },
            Repr::Panic(PanicText::Static(s)) => f.write_str(s.lines().next().unwrap_or("")),
            Repr::Panic(PanicText::Formatted(s)) => {
                write!(f, "<formatted panic message, {} bytes>", s.len())
            }
            Repr::Panic(PanicText::Other) => f.write_str("<panic payload of unknown type>"),
            #[cfg(feature = "sink")]
            Repr::Frames(_) => f.write_str("<backtrace>"),
            #[cfg(feature = "sink")]
            Repr::Time(t) => write!(f, "{t:?}"),
        }
    }
}

pub(crate) fn write_uuid(f: &mut impl fmt::Write, u: &[u8; 16]) -> fmt::Result {
    for (i, b) in u.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            f.write_char('-')?;
        }
        write!(f, "{b:02x}")?;
    }
    Ok(())
}

/// `(scheme, host[:port], path)` of an absolute URL; userinfo, query and
/// fragment are dropped. None when there is no `scheme://`.
pub(crate) fn url_parts(s: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = s.split_once("://")?;
    let mut chars = scheme.chars();
    if !chars.next()?.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    let tail = &rest[end..];
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    Some((scheme, host, &tail[..path_end]))
}

impl Field for Value<'_> {
    fn value(&self) -> Value<'_> {
        *self
    }
}

impl Field for bool {
    fn value(&self) -> Value<'_> {
        Value(Repr::Bool(*self))
    }
}

macro_rules! unsigned {
    ($($t:ty),*) => {$(
        impl Field for $t {
            fn value(&self) -> Value<'_> {
                Value(Repr::U64(*self as u64))
            }
        }
    )*};
}

macro_rules! signed {
    ($($t:ty),*) => {$(
        impl Field for $t {
            fn value(&self) -> Value<'_> {
                Value(Repr::I64(*self as i64))
            }
        }
    )*};
}

unsigned!(u8, u16, u32, u64, usize);
signed!(i8, i16, i32, i64, isize);

impl Field for u128 {
    fn value(&self) -> Value<'_> {
        Value(Repr::U128(*self))
    }
}

impl Field for i128 {
    fn value(&self) -> Value<'_> {
        Value(Repr::I128(*self))
    }
}

impl Field for f64 {
    fn value(&self) -> Value<'_> {
        Value(Repr::F64(*self))
    }
}

/// Written as whole microseconds.
impl Field for Duration {
    fn value(&self) -> Value<'_> {
        Value(Repr::U64(
            u64::try_from(self.as_micros()).unwrap_or(u64::MAX),
        ))
    }
}

impl Field for &'static str {
    fn value(&self) -> Value<'_> {
        Value(Repr::Text(self))
    }
}

impl Field for io::Error {
    fn value(&self) -> Value<'_> {
        Value::io(self)
    }
}

/// None is written as `null`.
impl<T: Field> Field for Option<T> {
    fn value(&self) -> Value<'_> {
        match self {
            Some(v) => v.value(),
            None => Value(Repr::Null),
        }
    }
}
