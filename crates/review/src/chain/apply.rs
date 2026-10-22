//! Applying one change step's edits to a version (docs/design/review.md
//! 「改动链」第 2 步). Exact matches only: an edit that does not match makes
//! the whole step not apply.

use lectern_services::changes::ChangePath;

use super::{Edit, Op, lines};

/// The version and path after `edits` (a call's edits, all paths) on
/// `version` at `path`: in order, the edits on the path the chain is on,
/// following moves. None when one of them does not apply or none is on the
/// path; a call is atomic, as the tools apply a `MultiEdit` or a multi-file
/// patch. `worktree_cr`: the worktree side has a `\r`, so a `Write`'s CRLF
/// stays as written.
pub(super) fn step(
    version: Option<&[u8]>,
    path: &ChangePath,
    edits: &[Edit],
    worktree_cr: bool,
) -> Option<(Option<Vec<u8>>, ChangePath)> {
    let mut current: Option<Vec<u8>> = version.map(<[u8]>::to_vec);
    let mut path = path.clone();
    let mut any = false;
    for edit in edits {
        if edit.path != path {
            continue;
        }
        any = true;
        current = match &edit.op {
            Op::Write { content } => Some(write(content, worktree_cr)),
            Op::Replace { old, new, all } => Some(replace(current.as_deref(), old, new, *all)?),
            Op::Patch { hunks, move_to } => {
                let patched = patch(current.as_deref().unwrap_or_default(), hunks)?;
                if let Some(to) = move_to {
                    path = to.clone();
                }
                Some(patched)
            }
            // Deleting a file that is not there fails, as the tools do.
            Op::Delete => {
                current.as_ref()?;
                None
            }
        };
    }
    any.then_some((current, path))
}

/// `Write` / `Add`. The worktree side is canonical (CRLF made LF under
/// `core.autocrlf`), the edit is raw: when the worktree has no `\r`, CRLF
/// in the content is made LF, git's own conversion.
fn write(content: &str, worktree_cr: bool) -> Vec<u8> {
    if !worktree_cr && content.contains("\r\n") {
        content.replace("\r\n", "\n").into_bytes()
    } else {
        content.as_bytes().to_vec()
    }
}

/// `Replace`: `old` must occur exactly once (`all`: at least once, every
/// occurrence replaced). An empty `old` creates the file, only where there
/// is none or it is empty. When `old` is not found, the version has no
/// `\r` and `old` has one, it is tried again with CRLF made LF in `old`
/// and `new`.
fn replace(version: Option<&[u8]>, old: &str, new: &str, all: bool) -> Option<Vec<u8>> {
    let version = version.unwrap_or_default();
    if old.is_empty() {
        return version.is_empty().then(|| new.as_bytes().to_vec());
    }
    let found = occurrences(version, old.as_bytes());
    if found.is_empty() && !version.contains(&b'\r') && old.contains('\r') {
        let (old, new) = (old.replace("\r\n", "\n"), new.replace("\r\n", "\n"));
        return replace_at(
            version,
            &occurrences(version, old.as_bytes()),
            &old,
            &new,
            all,
        );
    }
    replace_at(version, &found, old, new, all)
}

fn replace_at(version: &[u8], found: &[usize], old: &str, new: &str, all: bool) -> Option<Vec<u8>> {
    // Found more than once: the tool replaces only a unique match, so the
    // version is not the one the agent saw.
    if found.is_empty() || (!all && found.len() != 1) {
        return None;
    }
    let mut out = Vec::with_capacity(version.len() + found.len() * new.len());
    let mut at = 0;
    for &start in found {
        out.extend_from_slice(&version[at..start]);
        out.extend_from_slice(new.as_bytes());
        at = start + old.len();
    }
    out.extend_from_slice(&version[at..]);
    Some(out)
}

/// Starts of the non-overlapping occurrences of `needle`, left to right.
fn occurrences(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let Some((&first, _)) = needle.split_first() else {
        return out;
    };
    let mut at = 0;
    while at + needle.len() <= haystack.len() {
        let Some(i) = haystack[at..=haystack.len() - needle.len()]
            .iter()
            .position(|&b| b == first)
        else {
            break;
        };
        let start = at + i;
        if &haystack[start..start + needle.len()] == needle {
            out.push(start);
            at = start + needle.len();
        } else {
            at = start + 1;
        }
    }
    out
}

/// One hunk of a patch.
#[derive(Debug, PartialEq, Eq)]
struct Hunk {
    /// The old side's start from a `@@ -a,b +c,d @@` header (1-based; for an
    /// insertion, the line it goes after). None for `apply_patch`'s `@@` and
    /// `@@ <context>`, which carry no line numbers.
    start: Option<u32>,
    old: Vec<Vec<u8>>,
    new: Vec<Vec<u8>>,
}

/// Parses the hunks of a unified diff without file headers: Claude Code's
/// rendered `structuredPatch`, Codex's `apply_patch` body or
/// `FileChange::Update`. Lines are ` ` context, `-` removed, `+` added; an
/// empty line is an empty context line; `\ No newline at end of file`
/// takes the newline off the line before it. Lines before the first `@@`
/// form a hunk without a line number (`apply_patch` may omit the first
/// `@@`). Anything else is not a patch this reads: None.
fn parse(text: &str) -> Option<Vec<Hunk>> {
    #[derive(Clone, Copy)]
    enum Last {
        None,
        Context,
        Old,
        New,
    }
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut last = Last::None;
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() {
        return None;
    }
    for line in body.split('\n') {
        if let Some(header) = line.strip_prefix("@@") {
            hunks.push(Hunk {
                start: header_start(header),
                old: Vec::new(),
                new: Vec::new(),
            });
            last = Last::None;
            continue;
        }
        if hunks.is_empty() {
            hunks.push(Hunk {
                start: None,
                old: Vec::new(),
                new: Vec::new(),
            });
        }
        let h = hunks.last_mut()?;
        let with_newline = |rest: &str| {
            let mut l = rest.as_bytes().to_vec();
            l.push(b'\n');
            l
        };
        match line.as_bytes().first() {
            None => {
                h.old.push(b"\n".to_vec());
                h.new.push(b"\n".to_vec());
                last = Last::Context;
            }
            Some(b' ') => {
                h.old.push(with_newline(&line[1..]));
                h.new.push(with_newline(&line[1..]));
                last = Last::Context;
            }
            Some(b'-') => {
                h.old.push(with_newline(&line[1..]));
                last = Last::Old;
            }
            Some(b'+') => {
                h.new.push(with_newline(&line[1..]));
                last = Last::New;
            }
            Some(b'\\') => {
                let strip = |side: &mut Vec<Vec<u8>>| side.last_mut().map(|l| l.pop());
                match last {
                    Last::Context => {
                        strip(&mut h.old)?;
                        strip(&mut h.new)?;
                    }
                    Last::Old => {
                        strip(&mut h.old)?;
                    }
                    Last::New => {
                        strip(&mut h.new)?;
                    }
                    Last::None => return None,
                }
            }
            Some(_) => return None,
        }
    }
    Some(hunks)
}

/// The old side's start in a ` -a[,b] +c[,d] @@` header; None for a bare
/// `@@` or `@@ <context>`.
fn header_start(header: &str) -> Option<u32> {
    let rest = header.strip_prefix(" -")?;
    let (old, rest) = rest.split_once(' ')?;
    if !rest.starts_with('+') || !rest.contains("@@") {
        return None;
    }
    old.split(',').next()?.parse().ok()
}

/// Applies a patch: each hunk's old side (context and removed lines) must
/// equal the version's lines byte for byte. It is tried first where the
/// header puts it (shifted by the offset the hunks before it were found
/// at), then at the nearest position that matches, as `git apply` does;
/// hunks apply in order and never overlap. A hunk without a line number
/// takes the first match after the hunk before it, and a pure insertion
/// without one goes at the end. No fuzzy matching: one hunk that does not
/// match and the patch does not apply.
fn patch(version: &[u8], text: &str) -> Option<Vec<u8>> {
    let hunks = parse(text)?;
    let have: Vec<&[u8]> = lines(version).collect();
    let mut out = Vec::with_capacity(version.len() + text.len());
    let mut cursor = 0usize;
    let mut offset: i64 = 0;
    for h in &hunks {
        let n = h.old.len();
        let want = h.start.map(|s| {
            let s = i64::from(s) - i64::from(n > 0);
            s + offset
        });
        let at = if n == 0 {
            let at = want.unwrap_or(have.len() as i64);
            if at < cursor as i64 || at > have.len() as i64 {
                return None;
            }
            at as usize
        } else {
            find(&have, &h.old, cursor, want)?
        };
        if let Some(want) = want {
            offset += at as i64 - want;
        }
        for l in &have[cursor..at] {
            out.extend_from_slice(l);
        }
        for l in &h.new {
            out.extend_from_slice(l);
        }
        cursor = at + n;
    }
    for l in &have[cursor..] {
        out.extend_from_slice(l);
    }
    Some(out)
}

/// The position at or after `from` where `old` matches `have`, nearest to
/// `want` (ties to the earlier one); without `want`, the first one.
fn find(have: &[&[u8]], old: &[Vec<u8>], from: usize, want: Option<i64>) -> Option<usize> {
    let n = old.len();
    if have.len() < n || from > have.len() - n {
        return None;
    }
    let matches = |p: usize| {
        have[p..p + n]
            .iter()
            .zip(old)
            .all(|(a, b)| *a == b.as_slice())
    };
    let last = have.len() - n;
    let Some(want) = want else {
        return (from..=last).find(|&p| matches(p));
    };
    let want = want.clamp(from as i64, last as i64) as usize;
    // Outward from `want`: below first on a tie.
    let mut d = 0usize;
    loop {
        let below = want.checked_sub(d).filter(|&p| p >= from);
        let above = Some(want + d).filter(|&p| p <= last && d > 0);
        if below.is_none() && above.is_none() {
            return None;
        }
        if let Some(p) = below.filter(|&p| matches(p)) {
            return Some(p);
        }
        if let Some(p) = above.filter(|&p| matches(p)) {
            return Some(p);
        }
        d += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_patch_parses_headers_and_markers() {
        let hunks = parse(
            "@@ -2,2 +2,2 @@\n a\n-b\n\\ No newline at end of file\n+B\n\\ No newline at end of file\n",
        )
        .unwrap();
        assert_eq!(
            hunks,
            [Hunk {
                start: Some(2),
                old: vec![b"a\n".to_vec(), b"b".to_vec()],
                new: vec![b"a\n".to_vec(), b"B".to_vec()],
            }]
        );
        assert_eq!(parse("@@ fn main\n-x\n+y\n").unwrap()[0].start, None);
        assert_eq!(parse("@@ -0,0 +1 @@\n+x\n").unwrap()[0].start, Some(0));
        assert!(parse("@@\n*x\n").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn chain_patch_finds_the_nearest_match() {
        let version = b"x\na\nx\nb\nx\n";
        // Header says line 4 (`b`); the nearest `x` to index 3 is index 2.
        assert_eq!(
            patch(version, "@@ -4,1 +4,1 @@\n-x\n+y\n").unwrap(),
            b"x\na\ny\nb\nx\n"
        );
        // Without a number: the first match.
        assert_eq!(patch(version, "@@\n-x\n+y\n").unwrap(), b"y\na\nx\nb\nx\n");
        // Insertion without a number goes at the end.
        assert_eq!(patch(b"a\n", "@@\n+b\n").unwrap(), b"a\nb\n");
        assert!(patch(version, "@@ -1 +1 @@\n-z\n+y\n").is_none());
    }

    #[test]
    fn chain_replace_rules() {
        assert_eq!(replace(Some(b"a b a"), "b", "c", false).unwrap(), b"a c a");
        assert!(replace(Some(b"a b a"), "a", "c", false).is_none());
        assert_eq!(replace(Some(b"a b a"), "a", "c", true).unwrap(), b"c b c");
        assert!(replace(Some(b"a b a"), "z", "c", true).is_none());
        assert_eq!(replace(None, "", "new\n", false).unwrap(), b"new\n");
        assert!(replace(Some(b"x"), "", "new\n", false).is_none());
        assert_eq!(occurrences(b"aaaa", b"aa"), [0, 2]);
    }
}
