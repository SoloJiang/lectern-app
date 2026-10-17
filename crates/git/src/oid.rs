//! Object IDs of either hash algorithm (docs/design/git.md「对象 ID 与路径」).

use std::fmt;

/// The repository's object hash algorithm.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum HashKind {
    Sha1,
    Sha256,
}

impl HashKind {
    /// Digest length in bytes: 20 or 32.
    pub fn byte_len(self) -> usize {
        match self {
            HashKind::Sha1 => 20,
            HashKind::Sha256 => 32,
        }
    }

    /// Digest length in hexadecimal digits: 40 or 64.
    pub fn hex_len(self) -> usize {
        self.byte_len() * 2
    }

    pub(crate) fn to_gix(self) -> gix::hash::Kind {
        match self {
            HashKind::Sha1 => gix::hash::Kind::Sha1,
            HashKind::Sha256 => gix::hash::Kind::Sha256,
        }
    }

    pub(crate) fn from_gix(kind: gix::hash::Kind) -> HashKind {
        // gix's `Kind` is non-exhaustive; only these two are compiled in.
        if kind.len_in_bytes() == 32 {
            HashKind::Sha256
        } else {
            HashKind::Sha1
        }
    }
}

/// A full-length object ID: 20 bytes for SHA-1, 32 for SHA-256. Never
/// abbreviated, never assumed to be one length.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid {
    kind: HashKind,
    /// SHA-1 uses the first 20 bytes; the rest stay zero.
    bytes: [u8; 32],
}

impl Oid {
    pub fn kind(&self) -> HashKind {
        self.kind
    }

    /// 20 or 32 bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.kind.byte_len()]
    }

    /// 40 or 64 lowercase hexadecimal digits.
    pub fn to_hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(self.kind.hex_len());
        for byte in self.as_bytes() {
            out.push(char::from(DIGITS[usize::from(byte >> 4)]));
            out.push(char::from(DIGITS[usize::from(byte & 0xf)]));
        }
        out
    }

    /// Parses exactly `kind.hex_len()` hexadecimal digits (either case).
    pub fn from_hex(kind: HashKind, hex: &str) -> Option<Oid> {
        Oid::from_hex_bytes(kind, hex.as_bytes())
    }

    pub(crate) fn from_hex_bytes(kind: HashKind, hex: &[u8]) -> Option<Oid> {
        if hex.len() != kind.hex_len() {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (out, pair) in bytes.iter_mut().zip(hex.chunks_exact(2)) {
            *out = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Some(Oid { kind, bytes })
    }

    /// The OID of `content` as a blob (`blob <length>\0` followed by the
    /// content), the same as `git hash-object`. Pure computation; nothing is
    /// written to the object database.
    ///
    /// For SHA-1, content that sha1dc flags as part of a collision attack has
    /// no OID git would accept (git refuses to store it). Such content gets
    /// a deterministic stand-in: the first 20 bytes of the SHA-256 blob OID.
    /// It never equals an OID in a repository, so it can only miss caches.
    pub fn for_blob(kind: HashKind, content: &[u8]) -> Oid {
        match gix::objs::compute_hash(kind.to_gix(), gix::objs::Kind::Blob, content) {
            Ok(id) => Oid::from_gix(&id),
            Err(_) => {
                let sha256 = Oid::for_blob(HashKind::Sha256, content);
                let mut bytes = [0u8; 32];
                bytes[..kind.byte_len()].copy_from_slice(&sha256.as_bytes()[..kind.byte_len()]);
                Oid { kind, bytes }
            }
        }
    }

    /// Whether every byte is zero (git's "no object" marker in reflogs and
    /// index checksums).
    pub(crate) fn is_null(&self) -> bool {
        self.bytes.iter().all(|&b| b == 0)
    }

    pub(crate) fn from_gix(id: &gix::hash::oid) -> Oid {
        let kind = HashKind::from_gix(id.kind());
        let mut bytes = [0u8; 32];
        bytes[..kind.byte_len()].copy_from_slice(id.as_bytes());
        Oid { kind, bytes }
    }

    /// `None` when the slice has the wrong length for `kind`.
    pub(crate) fn from_bytes(kind: HashKind, digest: &[u8]) -> Option<Oid> {
        if digest.len() != kind.byte_len() {
            return None;
        }
        let mut bytes = [0u8; 32];
        bytes[..kind.byte_len()].copy_from_slice(digest);
        Some(Oid { kind, bytes })
    }

    pub(crate) fn to_gix(self) -> gix::ObjectId {
        gix::ObjectId::from_bytes_or_panic(self.as_bytes())
    }
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({:?}, {})", self.kind, self.to_hex())
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_oid_hex_round_trip_for_both_kinds() {
        let sha1 = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        let oid = Oid::from_hex(HashKind::Sha1, sha1).unwrap();
        assert_eq!(oid.to_hex(), sha1);
        assert_eq!(oid.as_bytes().len(), 20);
        let sha256 = "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813";
        let oid = Oid::from_hex(HashKind::Sha256, sha256).unwrap();
        assert_eq!(oid.to_hex(), sha256);
        assert_eq!(oid.as_bytes().len(), 32);
        assert_eq!(Oid::from_hex(HashKind::Sha256, sha1), None);
        assert_eq!(Oid::from_hex(HashKind::Sha1, sha256), None);
        assert_eq!(Oid::from_hex(HashKind::Sha1, &"g".repeat(40)), None);
        assert_eq!(
            Oid::from_hex(HashKind::Sha1, &sha1.to_uppercase())
                .unwrap()
                .to_hex(),
            sha1
        );
    }

    #[test]
    fn repo_oid_for_blob_matches_git_hash_object() {
        // `git hash-object /dev/null` and `printf 'hello\n' | git hash-object --stdin`,
        // with and without `--object-format=sha256`.
        assert_eq!(
            Oid::for_blob(HashKind::Sha1, b"").to_hex(),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            Oid::for_blob(HashKind::Sha1, b"hello\n").to_hex(),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        assert_eq!(
            Oid::for_blob(HashKind::Sha256, b"").to_hex(),
            "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813"
        );
        assert_eq!(
            Oid::for_blob(HashKind::Sha256, b"hello\n").to_hex(),
            "2cf8d83d9ee29543b34a87727421fdecb7e3f3a183d337639025de576db9ebb4"
        );
    }

    #[test]
    fn repo_oid_orders_and_distinguishes_kinds() {
        let a = Oid::from_bytes(HashKind::Sha1, &[0; 20]).unwrap();
        let b = Oid::from_bytes(HashKind::Sha256, &[0; 32]).unwrap();
        assert_ne!(a, b);
        assert!(a.is_null() && b.is_null());
        assert!(a < b);
        assert_eq!(Oid::from_bytes(HashKind::Sha1, &[0; 32]), None);
    }
}
