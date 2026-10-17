//! Keys: typed, length-prefixed segments.

/// An encoded key. Each segment carries a type tag and, for byte strings, a
/// length, so ("ab", "c") and ("a", "bc") encode differently, and so do a
/// `u32` and the same four bytes.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Key {
    bytes: Vec<u8>,
}

const TAG_BYTES: u8 = 1;
const TAG_STR: u8 = 2;
const TAG_U32: u8 = 3;
const TAG_U64: u8 = 4;

impl Key {
    pub fn new() -> Key {
        Key::default()
    }

    /// Raw bytes: an object ID, a hash.
    pub fn bytes(self, b: &[u8]) -> Key {
        self.with_len(TAG_BYTES, b)
    }

    pub fn str(self, s: &str) -> Key {
        self.with_len(TAG_STR, s.as_bytes())
    }

    pub fn u32(mut self, v: u32) -> Key {
        self.bytes.push(TAG_U32);
        self.bytes.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn u64(mut self, v: u64) -> Key {
        self.bytes.push(TAG_U64);
        self.bytes.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// Encoded length in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// True for `Key::new()` with no segment.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Tag, LEB128 length, bytes.
    fn with_len(mut self, tag: u8, b: &[u8]) -> Key {
        if self.bytes.is_empty() {
            self.bytes.reserve(64);
        }
        self.bytes.push(tag);
        let mut len = b.len();
        loop {
            let low = (len & 0x7f) as u8;
            len >>= 7;
            if len == 0 {
                self.bytes.push(low);
                break;
            }
            self.bytes.push(low | 0x80);
        }
        self.bytes.extend_from_slice(b);
        self
    }
}
