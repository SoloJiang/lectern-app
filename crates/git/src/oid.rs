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
        let mut hasher = BlobHasher::new(kind, content.len() as u64);
        hasher.update(content);
        // The length is right, so `None` means a flagged SHA-1 collision.
        hasher
            .finish()
            .unwrap_or_else(|| Oid::stand_in(Oid::for_blob(HashKind::Sha256, content)))
    }

    /// `for_blob`'s value for SHA-1 content that sha1dc flags: the first 20
    /// bytes of the content's SHA-256 blob OID.
    fn stand_in(sha256: Oid) -> Oid {
        let kind = HashKind::Sha1;
        let mut bytes = [0u8; 32];
        bytes[..kind.byte_len()].copy_from_slice(&sha256.as_bytes()[..kind.byte_len()]);
        Oid { kind, bytes }
    }

    /// The empty tree's OID (`4b825dc…` for SHA-1): the base of a root
    /// commit or an unborn branch. It needs no object in the database.
    pub fn empty_tree(kind: HashKind) -> Oid {
        Oid::from_gix(&gix::ObjectId::empty_tree(kind.to_gix()))
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

/// The blob OID of content given in pieces (`blob <len>\0` followed by the
/// content): the same value as [`Oid::for_blob`] of the whole content (except
/// flagged SHA-1 collisions, see [`BlobHasher::finish`]), for files too large
/// to hold in memory at once. `for_blob` goes through it, so there is one
/// implementation. Pure computation; no IO, never blocks, callable on any
/// thread.
///
/// ```
/// use lectern_git::{BlobHasher, HashKind, Oid};
///
/// let mut hasher = BlobHasher::new(HashKind::Sha1, 6);
/// hasher.update(b"hel");
/// hasher.update(b"lo\n");
/// assert_eq!(hasher.finish(), Some(Oid::for_blob(HashKind::Sha1, b"hello\n")));
/// ```
pub struct BlobHasher {
    kind: HashKind,
    len: u64,
    given: u64,
    hasher: gix::hash::Hasher,
}

impl BlobHasher {
    /// `len` is the content's total length, which the blob header records.
    pub fn new(kind: HashKind, len: u64) -> BlobHasher {
        let mut hasher = gix::hash::hasher(kind.to_gix());
        hasher.update(&gix::objs::encode::loose_header(gix::objs::Kind::Blob, len));
        BlobHasher {
            kind,
            len,
            given: 0,
            hasher,
        }
    }

    /// The next piece of the content.
    pub fn update(&mut self, bytes: &[u8]) {
        self.given = self.given.saturating_add(bytes.len() as u64);
        self.hasher.update(bytes);
    }

    /// `None` when the pieces given do not add up to `len` (the header would
    /// not describe the content, so no OID is right), or when sha1dc flags
    /// SHA-1 content as part of a collision attack. In the second case
    /// `for_blob` gives a stand-in made from the content's SHA-256, which a
    /// stream can only offer by hashing everything twice (about 65% more
    /// time per byte); the caller treats `None` as "no match" and falls back
    /// to reading the whole content (docs/design/git.md「与设计的差异」).
    pub fn finish(self) -> Option<Oid> {
        if self.given != self.len {
            return None;
        }
        let id = self.hasher.try_finalize().ok()?;
        Some(Oid::from_gix(&id))
    }
}

impl fmt::Debug for BlobHasher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlobHasher")
            .field("kind", &self.kind)
            .field("len", &self.len)
            .field("given", &self.given)
            .finish_non_exhaustive()
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

    const KINDS: [HashKind; 2] = [HashKind::Sha1, HashKind::Sha256];

    /// `content` hashed in pieces of the given lengths (the last piece takes
    /// the rest).
    fn streamed(kind: HashKind, content: &[u8], pieces: &[usize]) -> Option<Oid> {
        let mut hasher = BlobHasher::new(kind, content.len() as u64);
        let mut rest = content;
        for &n in pieces {
            let (piece, tail) = rest.split_at(n.min(rest.len()));
            hasher.update(piece);
            rest = tail;
        }
        hasher.update(rest);
        hasher.finish()
    }

    /// Deterministic bytes and piece lengths (xorshift), no `rand` needed.
    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn repo_blob_hasher_matches_git_for_the_empty_blob() {
        for (kind, hex) in [
            (HashKind::Sha1, "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"),
            (
                HashKind::Sha256,
                "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813",
            ),
        ] {
            let want = Oid::from_hex(kind, hex);
            assert_eq!(BlobHasher::new(kind, 0).finish(), want);
            // Empty pieces change nothing.
            assert_eq!(streamed(kind, b"", &[0, 0, 0]), want);
            assert_eq!(Some(Oid::for_blob(kind, b"")), want);
        }
    }

    #[test]
    fn repo_blob_hasher_matches_for_blob_however_the_content_is_split() {
        let mut state = 0x9e37_79b9_7f4a_7c15;
        let content: Vec<u8> = (0..10_000).map(|_| xorshift(&mut state) as u8).collect();
        for kind in KINDS {
            let oid = Oid::for_blob(kind, &content);
            assert_eq!(oid.kind(), kind);
            let want = Some(oid);
            assert_eq!(streamed(kind, &content, &[]), want, "one piece");
            assert_eq!(
                streamed(kind, &content, &vec![1; content.len()]),
                want,
                "1-byte pieces"
            );
            // Lengths around the 64-byte block, and empty pieces.
            assert_eq!(streamed(kind, &content, &[0, 63, 1, 64, 65, 0, 127]), want);
            for _ in 0..20 {
                let pieces: Vec<usize> = (0..30)
                    .map(|_| (xorshift(&mut state) % 700) as usize)
                    .collect();
                assert_eq!(streamed(kind, &content, &pieces), want, "{pieces:?}");
            }
            let hello = Some(Oid::for_blob(kind, b"hello\n"));
            assert_eq!(streamed(kind, b"hello\n", &[2, 3]), hello);
        }
    }

    #[test]
    fn repo_blob_hasher_matches_for_blob_on_mebibyte_ranges() {
        // How code_intel reads a large file: 1 MiB ranges, a partial last one.
        const MIB: usize = 1 << 20;
        let mut state = 0x2545_f491_4f6c_dd1d;
        let content: Vec<u8> = (0..3 * MIB + 12_345)
            .map(|_| xorshift(&mut state) as u8)
            .collect();
        for kind in KINDS {
            let mut hasher = BlobHasher::new(kind, content.len() as u64);
            for range in content.chunks(MIB) {
                hasher.update(range);
            }
            assert_eq!(hasher.finish(), Some(Oid::for_blob(kind, &content)));
        }
    }

    #[test]
    fn repo_blob_hasher_is_none_when_the_length_does_not_add_up() {
        for kind in KINDS {
            let mut short = BlobHasher::new(kind, 6);
            short.update(b"hello");
            assert_eq!(short.finish(), None);
            let mut long = BlobHasher::new(kind, 6);
            long.update(b"hello\n");
            long.update(b"!");
            assert_eq!(long.finish(), None);
            let mut nothing = BlobHasher::new(kind, 1);
            nothing.update(b"");
            assert_eq!(nothing.finish(), None);
            assert_eq!(
                BlobHasher::new(kind, 0).finish().map(|o| o.kind()),
                Some(kind)
            );
            let mut extra = BlobHasher::new(kind, 0);
            extra.update(b"x");
            assert_eq!(extra.finish(), None);
        }
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
