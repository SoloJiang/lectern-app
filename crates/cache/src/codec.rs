//! Value encoding and compression.

use std::io::Read;

use ruzstd::decoding::StreamingDecoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};

use crate::{Compression, Entry};

/// `entry.codec`: stored as encoded.
pub(crate) const RAW: u8 = 0;
/// `entry.codec`: a zstd frame.
pub(crate) const ZSTD: u8 = 1;

/// Encoded values below this are never compressed.
pub(crate) const COMPRESS_MIN: usize = 4 << 10;

/// A stored value as read from the database.
pub(crate) struct Raw {
    pub(crate) codec: u8,
    pub(crate) bytes: Vec<u8>,
}

/// Encodes a value and compresses it when its namespace asks for it and it
/// pays: at least 4 KiB, and the result under 90% of the original.
pub(crate) fn encode<E: Entry>(value: &E) -> (u8, Vec<u8>) {
    let mut out = Vec::new();
    value.encode(&mut out);
    if E::NAMESPACE.compression == Compression::Zstd && out.len() >= COMPRESS_MIN {
        let packed = compress_to_vec(&out[..], CompressionLevel::Fastest);
        if packed.len() * 10 < out.len() * 9 {
            return (ZSTD, packed);
        }
    }
    (RAW, out)
}

/// Decompresses if needed and decodes. `None` for an unknown codec, a broken
/// frame or a value the entry type rejects.
pub(crate) fn decode<E: Entry>(raw: Raw) -> Option<E> {
    match raw.codec {
        RAW => E::decode(&raw.bytes),
        ZSTD => {
            let mut decoder = StreamingDecoder::new(&raw.bytes[..]).ok()?;
            let mut out = Vec::new();
            decoder.read_to_end(&mut out).ok()?;
            E::decode(&out)
        }
        _ => None,
    }
}
