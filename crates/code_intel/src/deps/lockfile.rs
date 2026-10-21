//! The npm lockfile's identities (「身份与包的路径表」): the `integrity` and
//! `version` of the wanted keys of `packages`, read with serde_json's
//! streaming deserializer over 1 MiB ranges of the open file
//! (`OpenFile::read_ranges`), every other value skipped as `IgnoredAny`. So
//! the memory is one range and the result, whatever the lockfile's size.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;

use lectern_platform::{FileIdentity, OpenFile};
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};

/// Lockfiles larger than this are not read (ADR 0033): every package takes
/// the fallback identity.
pub(crate) const LOCKFILE_MAX: u64 = 64 << 20;

/// One read of the lockfile: a range of at most this many bytes.
pub(crate) const LOCKFILE_RANGE: u64 = 1 << 20;

/// What the lockfile says of one package directory.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct Locked {
    pub integrity: Option<String>,
    pub version: Option<String>,
}

// The ranges read on this thread (tests: each at most `LOCKFILE_RANGE`, in
// order, end to end).
#[cfg(test)]
thread_local! {
    pub(crate) static RANGES: std::cell::RefCell<Vec<std::ops::Range<u64>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The `packages` entries of `wanted` (keys as in the lockfile: package
/// directories relative to its directory). None when the file is over
/// [`LOCKFILE_MAX`], changed while read, is not JSON or has no `packages`
/// object: then every package takes the fallback identity.
pub(crate) fn read_locked(
    file: &OpenFile,
    wanted: &HashSet<String>,
) -> Option<HashMap<String, Locked>> {
    let len = file.meta().size;
    if len > LOCKFILE_MAX {
        return None;
    }
    let mut reader = Ranges {
        file,
        identity: file.identity(),
        len,
        at: 0,
        buf: Vec::new(),
        pos: 0,
        changed: false,
    };
    // serde_json reads byte by byte: a `BufReader` in front takes those
    // reads from its buffer (std's byte reads of a `BufReader` do not go
    // through `read`): a 4 MiB lockfile in about half the time.
    let end = {
        let buffered = io::BufReader::with_capacity(64 << 10, &mut reader);
        let mut de = serde_json::Deserializer::from_reader(buffered);
        let found = de
            .deserialize_map(Top { wanted })
            .ok()
            .and_then(|found| found);
        // The rest must be blanks only, and the file the version opened.
        de.end().is_ok().then_some(found)
    };
    end.filter(|_| !reader.changed).flatten()
}

/// `io::Read` over the file by ranges: one range held at a time.
struct Ranges<'a> {
    file: &'a OpenFile,
    identity: FileIdentity,
    len: u64,
    at: u64,
    buf: Vec<u8>,
    pos: usize,
    /// A range read saw another version of the file.
    changed: bool,
}

impl io::Read for Ranges<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.buf.len() {
            if self.at >= self.len {
                return Ok(0);
            }
            let end = (self.at + LOCKFILE_RANGE).min(self.len);
            // The previous range goes before the next is read.
            self.buf = Vec::new();
            self.pos = 0;
            #[cfg(test)]
            RANGES.with(|r| r.borrow_mut().push(self.at..end));
            let mut read = self
                .file
                .read_ranges(std::slice::from_ref(&(self.at..end)))?;
            if read.meta.identity() != self.identity {
                self.changed = true;
                return Err(io::Error::other("the lockfile changed while it was read"));
            }
            self.buf = read.pieces.pop().unwrap_or_default();
            if self.buf.is_empty() {
                // Shorter than at open: changed.
                self.changed = true;
                return Ok(0);
            }
            self.at += self.buf.len() as u64;
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// The lockfile's top object: only `packages` is looked into.
struct Top<'w> {
    wanted: &'w HashSet<String>,
}

impl<'de> Visitor<'de> for Top<'_> {
    type Value = Option<HashMap<String, Locked>>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an npm lockfile")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut found = None;
        while let Some(key) = map.next_key::<Key>()? {
            if key.0 && found.is_none() {
                found = Some(map.next_value_seed(Packages {
                    wanted: self.wanted,
                })?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(found)
    }
}

/// A key of the top object: whether it is `packages`, without keeping it.
struct Key(bool);

impl<'de> serde::Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Key, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Key;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a key")
            }
            fn visit_str<E>(self, v: &str) -> Result<Key, E> {
                Ok(Key(v == "packages"))
            }
        }
        d.deserialize_str(V)
    }
}

struct Packages<'w> {
    wanted: &'w HashSet<String>,
}

impl<'de> DeserializeSeed<'de> for Packages<'_> {
    type Value = HashMap<String, Locked>;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Packages<'_> {
    type Value = HashMap<String, Locked>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the packages object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut out = HashMap::new();
        while let Some(key) = map.next_key::<String>()? {
            if self.wanted.contains(&key) && !out.contains_key(&key) {
                let locked = map.next_value_seed(Entry)?;
                out.insert(key, locked);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(out)
    }
}

/// One package's entry: `integrity` and `version`, the rest skipped.
struct Entry;

impl<'de> DeserializeSeed<'de> for Entry {
    type Value = Locked;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Locked, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Entry {
    type Value = Locked;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a package entry")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Locked, A::Error> {
        let mut locked = Locked::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "integrity" => locked.integrity = map.next_value::<Option<String>>()?,
                "version" => locked.version = map.next_value::<Option<String>>()?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(locked)
    }

    // Anything but an object (a malformed entry): no identity.
    fn visit_str<E>(self, _: &str) -> Result<Locked, E> {
        Ok(Locked::default())
    }

    fn visit_bool<E>(self, _: bool) -> Result<Locked, E> {
        Ok(Locked::default())
    }

    fn visit_unit<E>(self) -> Result<Locked, E> {
        Ok(Locked::default())
    }

    fn visit_i64<E>(self, _: i64) -> Result<Locked, E> {
        Ok(Locked::default())
    }

    fn visit_u64<E>(self, _: u64) -> Result<Locked, E> {
        Ok(Locked::default())
    }

    fn visit_f64<E>(self, _: f64) -> Result<Locked, E> {
        Ok(Locked::default())
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Locked, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Locked::default())
    }
}
