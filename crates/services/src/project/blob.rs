//! Git object IDs (docs/design/project.md「文件列表」, code_intel.md
//! 「对其他 crate 的要求」).

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A git object ID: 20 bytes in a SHA-1 repository, 32 in a SHA-256 one.
/// Folder projects use the SHA-1 of the raw bytes.
///
/// Serialized as lowercase hex (40 or 64 characters); deserializing also
/// accepts uppercase.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BlobId {
    Sha1([u8; 20]),
    Sha256([u8; 32]),
}

impl BlobId {
    /// An ID from its raw bytes; None unless there are 20 or 32.
    pub fn from_bytes(bytes: &[u8]) -> Option<BlobId> {
        if let Ok(b) = <[u8; 20]>::try_from(bytes) {
            Some(BlobId::Sha1(b))
        } else {
            <[u8; 32]>::try_from(bytes).ok().map(BlobId::Sha256)
        }
    }

    /// An ID from 40 or 64 hex digits.
    pub fn from_hex(hex: &str) -> Option<BlobId> {
        let hex = hex.as_bytes();
        if hex.len() != 40 && hex.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, pair) in hex.chunks_exact(2).enumerate() {
            bytes[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
        }
        BlobId::from_bytes(&bytes[..hex.len() / 2])
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            BlobId::Sha1(b) => b,
            BlobId::Sha256(b) => b,
        }
    }
}

fn digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Lowercase hex.
impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.as_bytes() {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobId({self})")
    }
}

impl Serialize for BlobId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BlobId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Hex;
        impl Visitor<'_> for Hex {
            type Value = BlobId;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("40 or 64 hex digits")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<BlobId, E> {
                BlobId::from_hex(v).ok_or_else(|| E::invalid_value(de::Unexpected::Str(v), &self))
            }
        }
        deserializer.deserialize_str(Hex)
    }
}
