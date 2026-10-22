//! The cold segment format (docs/design/usage.md「冷热分层」): a header
//! (format version, table, table version, columns, row count), then the
//! rows column by column. Integers are stored as the zigzag varint of their
//! difference from the column's previous integer; strings and blobs as a
//! length and their bytes (strings and paths are already intern IDs in the
//! tables). The whole segment is one zstd frame of `ruzstd`'s fastest level,
//! with the content checksum.

use std::io::Read;

use rusqlite::types::Value;
use ruzstd::decoding::StreamingDecoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};

const MAGIC: &[u8; 4] = b"LCSG";

/// The format version, in the header.
pub(crate) const FORMAT: u8 = 1;

const NULL: u8 = 0;
const INTEGER: u8 = 1;
const REAL: u8 = 2;
const TEXT: u8 = 3;
const BLOB: u8 = 4;

/// One table's rows, as the archive reads them (`SELECT *`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Segment {
    pub(crate) table: String,
    /// `PRAGMA user_version` when it was written.
    pub(crate) schema: i64,
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<Value>>,
}

impl Segment {
    /// The index of a column.
    pub(crate) fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }
}

/// Builds a segment row by row, knowing its encoded size as it grows.
pub(crate) struct Encoder {
    table: String,
    schema: i64,
    columns: Vec<String>,
    data: Vec<Vec<u8>>,
    previous: Vec<i64>,
    rows: usize,
    bytes: usize,
}

impl Encoder {
    pub(crate) fn new(table: &str, schema: i64, columns: Vec<String>) -> Encoder {
        let n = columns.len();
        Encoder {
            table: table.to_owned(),
            schema,
            columns,
            data: vec![Vec::new(); n],
            previous: vec![0; n],
            rows: 0,
            bytes: 0,
        }
    }

    /// The encoded size of the rows so far (the header not counted).
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Adds a row; it has one value per column.
    pub(crate) fn push(&mut self, row: &[Value]) {
        debug_assert_eq!(row.len(), self.columns.len());
        for (i, value) in row.iter().enumerate() {
            let out = &mut self.data[i];
            let before = out.len();
            match value {
                Value::Null => out.push(NULL),
                Value::Integer(v) => {
                    out.push(INTEGER);
                    varint(out, zigzag(v.wrapping_sub(self.previous[i])));
                    self.previous[i] = *v;
                }
                Value::Real(v) => {
                    out.push(REAL);
                    out.extend_from_slice(&v.to_bits().to_le_bytes());
                }
                Value::Text(s) => {
                    out.push(TEXT);
                    varint(out, s.len() as u64);
                    out.extend_from_slice(s.as_bytes());
                }
                Value::Blob(b) => {
                    out.push(BLOB);
                    varint(out, b.len() as u64);
                    out.extend_from_slice(b);
                }
            }
            self.bytes += out.len() - before;
        }
        self.rows += 1;
    }

    /// The uncompressed segment.
    pub(crate) fn finish(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes + 64);
        out.extend_from_slice(MAGIC);
        out.push(FORMAT);
        bytes(&mut out, self.table.as_bytes());
        varint(&mut out, zigzag(self.schema));
        varint(&mut out, self.columns.len() as u64);
        for c in &self.columns {
            bytes(&mut out, c.as_bytes());
        }
        varint(&mut out, self.rows as u64);
        for column in &self.data {
            bytes(&mut out, column);
        }
        out
    }
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn bytes(out: &mut Vec<u8>, b: &[u8]) {
    varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Option<u8> {
        let (&b, rest) = self.rest.split_first()?;
        self.rest = rest;
        Some(b)
    }

    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.rest.len() < n {
            return None;
        }
        let (head, rest) = self.rest.split_at(n);
        self.rest = rest;
        Some(head)
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = usize::try_from(self.varint()?).ok()?;
        self.take(n)
    }
}

/// The rows of an uncompressed segment; None when it is not one.
pub(crate) fn decode(raw: &[u8]) -> Option<Segment> {
    let mut r = Reader { rest: raw };
    if r.take(4)? != MAGIC || r.byte()? != FORMAT {
        return None;
    }
    let table = String::from_utf8(r.bytes()?.to_vec()).ok()?;
    let schema = unzigzag(r.varint()?);
    let n = usize::try_from(r.varint()?).ok()?;
    let mut columns = Vec::with_capacity(n.min(64));
    for _ in 0..n {
        columns.push(String::from_utf8(r.bytes()?.to_vec()).ok()?);
    }
    let count = usize::try_from(r.varint()?).ok()?;
    let mut rows: Vec<Vec<Value>> = (0..count.min(1 << 24))
        .map(|_| Vec::with_capacity(n))
        .collect();
    if rows.len() != count {
        return None;
    }
    for _ in 0..n {
        let mut c = Reader { rest: r.bytes()? };
        let mut previous = 0i64;
        for row in rows.iter_mut() {
            let value = match c.byte()? {
                NULL => Value::Null,
                INTEGER => {
                    let v = previous.wrapping_add(unzigzag(c.varint()?));
                    previous = v;
                    Value::Integer(v)
                }
                REAL => {
                    let b: [u8; 8] = c.take(8)?.try_into().ok()?;
                    Value::Real(f64::from_bits(u64::from_le_bytes(b)))
                }
                TEXT => Value::Text(String::from_utf8(c.bytes()?.to_vec()).ok()?),
                BLOB => Value::Blob(c.bytes()?.to_vec()),
                _ => return None,
            };
            row.push(value);
        }
        if !c.rest.is_empty() {
            return None;
        }
    }
    if !r.rest.is_empty() {
        return None;
    }
    Some(Segment {
        table,
        schema,
        columns,
        rows,
    })
}

/// One zstd frame, fastest level, with the content checksum.
pub(crate) fn compress(raw: &[u8]) -> Vec<u8> {
    compress_to_vec(raw, CompressionLevel::Fastest)
}

/// The content of a frame; None when it is broken or its checksum does not
/// match.
pub(crate) fn decompress(packed: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = StreamingDecoder::new(packed).ok()?;
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    let frame = decoder.into_frame_decoder();
    match (
        frame.get_checksum_from_data(),
        frame.get_calculated_checksum(),
    ) {
        (Some(stored), Some(computed)) if stored == computed => Some(out),
        _ => None,
    }
}
