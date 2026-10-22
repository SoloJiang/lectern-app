//! Free text: third-party messages and panic messages
//! (docs/design/logging.md「隐私」). All three rules first keep only the
//! first line and replace credential-shaped strings with `<redacted>`; cuts
//! fall on character boundaries and the home directory is written as `~`.
//!
//! 1. Literal (`PanicText::Static`, third-party `as_str()` Some): ≤ 256 bytes.
//! 2. Formatted panic message: only its length, and for a message starting
//!    with a known std prefix, the prefix and the numbers outside quotes or
//!    the leading identifier of the `Err` value's Debug output.
//! 3. Formatted third-party message: quoted segments masked, the text after
//!    the first `: ` masked, ≤ 128 bytes.

use std::ops::Deref;

use super::encode::{cut, strip_home};

/// Input beyond this many bytes of the first line is not read.
pub(crate) const INPUT: usize = 1024;

/// A fixed-size string buffer. A push that does not fit is cut on a
/// character boundary and later pushes are ignored.
pub(crate) struct Buf<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Buf<N> {
    pub(crate) fn new() -> Buf<N> {
        Buf {
            bytes: [0; N],
            len: 0,
        }
    }

    pub(crate) fn push(&mut self, s: &str) {
        let s = cut(s, N - self.len);
        self.bytes[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
        self.len += s.len();
    }

    fn truncate(&mut self, max: usize) {
        let s = cut(self, max);
        self.len = s.len();
    }
}

impl<const N: usize> Deref for Buf<N> {
    type Target = str;

    fn deref(&self) -> &str {
        // Only whole `&str`s cut on character boundaries are pushed.
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

/// The text up to the first line break.
fn first_line(s: &str) -> &str {
    s.split(['\n', '\r']).next().unwrap_or("")
}

/// The first line, at most [`INPUT`] bytes; when that cuts the line, the
/// word the cut falls in is dropped too, so no part of a credential that
/// crosses the cut survives unmasked.
fn bounded(s: &str, already_cut: bool) -> &str {
    let line = first_line(s);
    let short = cut(line, INPUT);
    if short.len() < line.len() || (already_cut && short.len() == line.len()) {
        short.trim_end_matches(|c: char| is_b64(c) || is_word(c))
    } else {
        short
    }
}

const PREFIXES: [&str; 6] = ["sk-", "ghp_", "gho_", "github_pat_", "glpat-", "xox"];

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
}

fn is_b64(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-')
}

/// Calls `f(from, to)` for each maximal run of ASCII bytes matching `pred`.
fn for_runs(b: &[u8], pred: fn(char) -> bool, mut f: impl FnMut(usize, usize)) {
    let matches = |x: u8| x.is_ascii() && pred(char::from(x));
    let mut i = 0;
    while i < b.len() {
        if matches(b[i]) {
            let start = i;
            while i < b.len() && matches(b[i]) {
                i += 1;
            }
            f(start, i);
        } else {
            i += 1;
        }
    }
}

/// Credential-shaped strings replaced with `<redacted>`: words starting
/// with a known token prefix, the word after `Bearer `, JWT-shaped words
/// (`eyJ…` with two dots), and runs of 32 or more `[A-Za-z0-9+/=_-]`.
pub(crate) fn mask_credentials(s: &str) -> Buf<4096> {
    let s = cut(s, INPUT);
    let b = s.as_bytes();
    let len = b.len();
    let mut masked = [false; INPUT];
    let mut mark = |from: usize, to: usize| masked[from..to].iter_mut().for_each(|m| *m = true);
    for_runs(b, is_word, |from, to| {
        let word = &s[from..to];
        let jwt = word.starts_with("eyJ") && word.matches('.').count() >= 2;
        if jwt || PREFIXES.iter().any(|p| word.starts_with(p)) {
            mark(from, to);
        }
    });
    for_runs(b, is_b64, |from, to| {
        if to - from >= 32 {
            mark(from, to);
        }
    });
    let mut at = 0;
    while let Some(i) = s[at..].find("Bearer ") {
        let from = at + i + "Bearer ".len();
        let to = s[from..]
            .find(char::is_whitespace)
            .map_or(len, |j| from + j);
        if to > from {
            mark(from, to);
        }
        at = from;
    }
    let mut out = Buf::new();
    let mut i = 0;
    while i < len {
        if masked[i] {
            out.push("<redacted>");
            while i < len && masked[i] {
                i += 1;
            }
        } else {
            let mut j = i + 1;
            while j < len && !masked[j] {
                j += 1;
            }
            out.push(&s[i..j]);
            i = j;
        }
    }
    out
}

/// Every occurrence of the home directory as a whole leading component of a
/// path written as `~`.
fn home_tilde(s: &str, home: Option<&str>) -> Buf<4096> {
    let mut out = Buf::new();
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        out.push(s);
        return out;
    };
    let mut rest = s;
    while let Some(i) = rest.find(home) {
        let after = &rest[i + home.len()..];
        let whole = strip_home(&rest.as_bytes()[i..], home).is_some();
        let starts = i == 0 || !is_word(rest[..i].chars().next_back().unwrap_or(' '));
        out.push(&rest[..i]);
        out.push(if whole && starts { "~" } else { home });
        rest = after;
    }
    out.push(rest);
    out
}

/// Rule 1.
pub(crate) fn literal(s: &str, home: Option<&str>) -> Buf<256> {
    let masked = mask_credentials(bounded(s, false));
    let homed = home_tilde(&masked, home);
    let mut out = Buf::new();
    out.push(&homed);
    out
}

/// Rule 3. `cut`: the text was already cut while formatting.
pub(crate) fn third_party(s: &str, cut: bool, home: Option<&str>) -> Buf<128> {
    let masked = mask_credentials(bounded(s, cut));
    let mut quoted: Buf<4096> = Buf::new();
    let mut rest: &str = &masked;
    while let Some(i) = rest.find(['"', '\'', '`']) {
        quoted.push(&rest[..i]);
        let q = rest[i..].chars().next().unwrap_or('"');
        let inner = &rest[i + 1..];
        match inner.find(q) {
            Some(j) => {
                let content = &inner[..j];
                match q {
                    '`' if keep_in_backticks(content) => {
                        quoted.push("`");
                        quoted.push(content);
                        quoted.push("`");
                    }
                    '`' => quoted.push("`\u{2026}`"),
                    '"' => quoted.push("\"\u{2026}\""),
                    _ => quoted.push("'\u{2026}'"),
                }
                rest = &inner[j + 1..];
            }
            None => {
                quoted.push("\u{2026}");
                rest = "";
            }
        }
    }
    quoted.push(rest);
    let mut colon: Buf<4096> = Buf::new();
    match quoted.find(": ") {
        Some(i) => {
            colon.push(&quoted[..i + 2]);
            colon.push("\u{2026}");
        }
        None => colon.push(&quoted),
    }
    let homed = home_tilde(&colon, home);
    let mut out = Buf::new();
    out.push(&homed);
    out
}

/// What survives in backticks: `Option::…()` / `Result::…()` method names
/// and the variants `None`, `Some`, `Ok`, `Err`.
fn keep_in_backticks(content: &str) -> bool {
    if matches!(content, "None" | "Some" | "Ok" | "Err") {
        return true;
    }
    let method = content
        .strip_prefix("Option::")
        .or_else(|| content.strip_prefix("Result::"))
        .and_then(|m| m.strip_suffix("()"));
    method.is_some_and(|m| !m.is_empty() && m.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
}

/// How a known std prefix is followed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    Nothing,
    Numbers,
    Variant,
}

/// The design's table, plus the forms Rust 1.97 writes for `str` slicing
/// (`start byte index`, `end byte index`, `byte range starts at`).
const STD_PREFIXES: [(&str, After); 13] = [
    (
        "called `Result::unwrap()` on an `Err` value",
        After::Variant,
    ),
    (
        "called `Result::unwrap_err()` on an `Ok` value",
        After::Nothing,
    ),
    ("index out of bounds", After::Numbers),
    ("byte index", After::Numbers),
    ("start byte index", After::Numbers),
    ("end byte index", After::Numbers),
    ("byte range starts at", After::Numbers),
    ("begin <= end", After::Numbers),
    ("range start index", After::Numbers),
    ("range end index", After::Numbers),
    ("slice index starts at", After::Numbers),
    ("assertion `left == right` failed", After::Nothing),
    ("assertion `left != right` failed", After::Nothing),
];

/// Rule 2's result.
pub(crate) struct PanicSummary {
    pub len: usize,
    pub prefix: Option<&'static str>,
    nums: [u64; 6],
    count: usize,
    variant: Buf<32>,
}

impl PanicSummary {
    pub(crate) fn nums(&self) -> &[u64] {
        &self.nums[..self.count]
    }

    pub(crate) fn variant(&self) -> Option<&str> {
        (!self.variant.is_empty()).then_some(&*self.variant)
    }
}

/// Text with every quoted segment removed whole; an unpaired quote removes
/// the rest of the line.
fn without_quotes(s: &str) -> Buf<4096> {
    let mut out = Buf::new();
    let mut rest = s;
    while let Some(i) = rest.find(['"', '\'', '`']) {
        out.push(&rest[..i]);
        let q = rest[i..].chars().next().unwrap_or('"');
        match rest[i + 1..].find(q) {
            Some(j) => rest = &rest[i + 1 + j + 1..],
            None => return out,
        }
    }
    out.push(rest);
    out
}

/// Rule 2.
pub(crate) fn formatted_panic(s: &str) -> PanicSummary {
    let mut summary = PanicSummary {
        len: s.len(),
        prefix: None,
        nums: [0; 6],
        count: 0,
        variant: Buf::new(),
    };
    let masked = mask_credentials(bounded(s, false));
    let Some((prefix, after)) = STD_PREFIXES.iter().find(|(p, _)| masked.starts_with(p)) else {
        return summary;
    };
    summary.prefix = Some(prefix);
    let rest = without_quotes(&masked[prefix.len()..]);
    match after {
        After::Nothing => {}
        After::Numbers => {
            let mut digits = rest
                .split(|c: char| !c.is_ascii_digit())
                .filter(|d| !d.is_empty());
            while summary.count < summary.nums.len() {
                let Some(d) = digits.next() else { break };
                summary.nums[summary.count] = d.parse().unwrap_or(u64::MAX);
                summary.count += 1;
            }
        }
        After::Variant => {
            let debug = rest.trim_start_matches([':', ' ']);
            let end = debug.find(['{', '(', ' ']).unwrap_or(debug.len());
            let ident = &debug[..end];
            if !ident.is_empty()
                && ident.len() <= 32
                && ident
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                summary.variant.push(ident);
            }
        }
    }
    summary.variant.truncate(32);
    summary
}
