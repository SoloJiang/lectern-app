//! Claims: what a session's turn index says it did to a worktree's paths
//! (docs/design/review.md「改动步与声明」「归因的缓存」).
//!
//! A session's change steps (each completed-or-not edit call with a
//! result, per path relative to the worktree root) and its command windows
//! are derived from the turn index alone, without `load`, and kept in
//! `review.claims` (the app partition) under the source and session IDs.
//! The value carries the identity of the index it came from (the session's
//! `last_activity` and the cursor's adapter version, as ADR 0026 does for
//! values with positions): [`Claims::valid`] tells whether it still stands.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_cache::{Compression, Entry, Key, Namespace};
use lectern_git::RepoPath;
use lectern_services::changes::ChangePath;
use lectern_services::session::{
    ContentRef, SessionKey, SessionSummary, Timestamp, ToolCallId, ToolKind, ToolStatus, TurnId,
    TurnIndex, TurnItem,
};

use crate::chain::codec::{Reader, bytes, uint};

/// A command's window opens this long before its call started
/// (「改动步与声明」: log times and the command's own run differ).
pub(crate) const COMMAND_BEFORE_MS: i64 = 1000;
/// And closes this long after its result.
pub(crate) const COMMAND_AFTER_MS: i64 = 2000;

/// One session's claims.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Claims {
    /// `SessionSummary::last_activity` of the index they came from.
    pub last_activity: Timestamp,
    /// `TurnIndex::cursor`'s adapter version.
    pub adapter_version: u32,
    /// In index order (turns by ordinal, calls in turn order).
    pub calls: Arc<[ClaimCall]>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ClaimCall {
    pub turn: TurnId,
    pub ordinal: u32,
    pub call: ToolCallId,
    pub kind: ClaimKind,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum ClaimKind {
    /// An edit call with its result: its paths in the worktree (those
    /// outside it dropped), when it finished and how.
    Edit {
        paths: Arc<[ChangePath]>,
        at: Timestamp,
        status: ToolStatus,
        input: ContentRef,
        result: Option<ContentRef>,
    },
    /// A finished command: `[started_at - 1 s, finished + 2 s]`.
    Command { from: Timestamp, to: Timestamp },
}

impl ClaimCall {
    /// Where the call is in the session file, the tie-break after time
    /// (「改动步与声明」: `Positioned` order).
    pub(crate) fn position(&self) -> u64 {
        match &self.kind {
            ClaimKind::Edit { input, .. } => input.pos(),
            ClaimKind::Command { .. } => u64::MAX,
        }
    }
}

impl Claims {
    /// Whether the value may stand for `summary`'s current index: the same
    /// `last_activity`, the summary's tail fields computed, and (when this
    /// run already knows it) the source's current adapter version.
    pub(crate) fn valid(&self, summary: &SessionSummary, adapter: Option<u32>) -> bool {
        self.last_activity == summary.last_activity
            && !summary.tail_pending
            && adapter.is_none_or(|v| v == self.adapter_version)
    }

    /// Derives the claims from a turn index. `rel` makes the logged
    /// absolute paths relative to the worktree root.
    pub(crate) fn derive(index: &TurnIndex, rel: &Relative, last_activity: Timestamp) -> Claims {
        let mut calls = Vec::new();
        for turn in index.turns.iter() {
            for item in turn.items.iter() {
                let TurnItem::Tool(call) = item else {
                    continue;
                };
                // A call without a result (still running, or no result in
                // the log) does not count yet: the next `SessionsChanged`
                // brings it.
                let Some((finished, status)) = call.finished else {
                    continue;
                };
                let kind = match &call.kind {
                    ToolKind::Edit { paths } => {
                        let mut rels: Vec<ChangePath> = Vec::new();
                        for p in paths.iter() {
                            if let Some(r) = rel.rel(p)
                                && !rels.contains(&r)
                            {
                                rels.push(r);
                            }
                        }
                        if rels.is_empty() {
                            continue;
                        }
                        ClaimKind::Edit {
                            paths: rels.into(),
                            at: finished,
                            status,
                            input: call.input,
                            result: call.result,
                        }
                    }
                    ToolKind::Execute { .. } => ClaimKind::Command {
                        from: Timestamp(call.started_at.0.saturating_sub(COMMAND_BEFORE_MS)),
                        to: Timestamp(finished.0.saturating_add(COMMAND_AFTER_MS)),
                    },
                    _ => continue,
                };
                calls.push(ClaimCall {
                    turn: turn.id.clone(),
                    ordinal: turn.ordinal,
                    call: call.id.clone(),
                    kind,
                });
            }
        }
        Claims {
            last_activity,
            adapter_version: index.cursor.adapter_version().unwrap_or(0),
            calls: calls.into(),
        }
    }
}

/// Makes a session's logged paths relative to the worktree root
/// (「改动步与声明」): the root's prefix first; otherwise the session's own
/// cwd as logged replaced by the canonical cwd it was placed at (`/tmp`
/// against `/private/tmp`); a relative path is under the placed cwd.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Relative {
    pub root: PathBuf,
    /// The cwd as the log has it.
    pub logged_cwd: PathBuf,
    /// The canonical cwd: the root joined with `Location::rel`.
    pub placed_cwd: PathBuf,
}

impl Relative {
    pub(crate) fn new(root: &Path, logged_cwd: &Path, rel: &str) -> Relative {
        Relative {
            root: root.to_path_buf(),
            logged_cwd: logged_cwd.to_path_buf(),
            placed_cwd: if rel.is_empty() {
                root.to_path_buf()
            } else {
                root.join(rel)
            },
        }
    }

    /// None when the path is outside the worktree, or not a valid path.
    pub(crate) fn rel(&self, path: &Path) -> Option<ChangePath> {
        let absolute;
        let path = if path.is_absolute() {
            path
        } else {
            absolute = self.placed_cwd.join(path);
            &absolute
        };
        let inside = match path.strip_prefix(&self.root) {
            Ok(inside) => inside.to_path_buf(),
            Err(_) => {
                let rest = path.strip_prefix(&self.logged_cwd).ok()?;
                self.placed_cwd
                    .join(rest)
                    .strip_prefix(&self.root)
                    .ok()?
                    .to_path_buf()
            }
        };
        let mut out: Vec<u8> = Vec::new();
        for part in inside.components() {
            if !out.is_empty() {
                out.push(b'/');
            }
            out.extend_from_slice(part.as_os_str().as_encoded_bytes());
        }
        let path = RepoPath::new(&out)?;
        Some(ChangePath(Arc::clone(path.shared())))
    }
}

/// `review.claims` holds one session's claims.
pub(crate) struct CachedClaims(pub Claims);

pub(crate) fn key(session: &SessionKey) -> Key {
    Key::new().str(&session.source.0).str(&session.id)
}

impl Entry for CachedClaims {
    const NAMESPACE: Namespace = Namespace {
        name: "review.claims",
        version: 1,
        compression: Compression::None,
    };

    fn encode(&self, out: &mut Vec<u8>) {
        encode(&self.0, out);
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        decode(bytes).map(CachedClaims)
    }
}

/// LEB128 integers, length-prefixed strings:
///
/// ```text
/// last_activity (zigzag), adapter_version, n calls, then per call:
///   turn, ordinal, call, tag
///   0 edit:    n paths (bytes each), at (zigzag), status (u8),
///              input (pos, len, part), result (0, or 1 and pos, len, part)
///   1 command: from, to (zigzag)
/// ```
pub(crate) fn encode(claims: &Claims, out: &mut Vec<u8>) {
    int(out, claims.last_activity.0);
    uint(out, u64::from(claims.adapter_version));
    uint(out, claims.calls.len() as u64);
    for c in claims.calls.iter() {
        bytes(out, c.turn.0.as_bytes());
        uint(out, u64::from(c.ordinal));
        bytes(out, c.call.0.as_bytes());
        match &c.kind {
            ClaimKind::Edit {
                paths,
                at,
                status,
                input,
                result,
            } => {
                out.push(0);
                uint(out, paths.len() as u64);
                for p in paths.iter() {
                    bytes(out, &p.0);
                }
                int(out, at.0);
                out.push(match status {
                    ToolStatus::Completed => 0,
                    ToolStatus::Failed => 1,
                    ToolStatus::Denied => 2,
                    ToolStatus::Interrupted => 3,
                });
                content_ref(out, input);
                match result {
                    None => out.push(0),
                    Some(r) => {
                        out.push(1);
                        content_ref(out, r);
                    }
                }
            }
            ClaimKind::Command { from, to } => {
                out.push(1);
                int(out, from.0);
                int(out, to.0);
            }
        }
    }
}

pub(crate) fn decode(input: &[u8]) -> Option<Claims> {
    let mut r = Reader::new(input);
    let last_activity = Timestamp(read_int(&mut r)?);
    let adapter_version = r.u32()?;
    let n = r.len()?;
    let mut calls = Vec::with_capacity(n);
    for _ in 0..n {
        let turn = TurnId(text(&mut r)?);
        let ordinal = r.u32()?;
        let call = ToolCallId(text(&mut r)?);
        let kind = match r.byte()? {
            0 => {
                let m = r.len()?;
                let mut paths = Vec::with_capacity(m);
                for _ in 0..m {
                    let p = RepoPath::new(r.bytes()?)?;
                    paths.push(ChangePath(Arc::clone(p.shared())));
                }
                let at = Timestamp(read_int(&mut r)?);
                let status = match r.byte()? {
                    0 => ToolStatus::Completed,
                    1 => ToolStatus::Failed,
                    2 => ToolStatus::Denied,
                    3 => ToolStatus::Interrupted,
                    _ => return None,
                };
                let input = read_ref(&mut r)?;
                let result = match r.byte()? {
                    0 => None,
                    1 => Some(read_ref(&mut r)?),
                    _ => return None,
                };
                ClaimKind::Edit {
                    paths: paths.into(),
                    at,
                    status,
                    input,
                    result,
                }
            }
            1 => ClaimKind::Command {
                from: Timestamp(read_int(&mut r)?),
                to: Timestamp(read_int(&mut r)?),
            },
            _ => return None,
        };
        calls.push(ClaimCall {
            turn,
            ordinal,
            call,
            kind,
        });
    }
    r.done().then(|| Claims {
        last_activity,
        adapter_version,
        calls: calls.into(),
    })
}

fn int(out: &mut Vec<u8>, v: i64) {
    uint(out, ((v << 1) ^ (v >> 63)) as u64);
}

fn read_int(r: &mut Reader<'_>) -> Option<i64> {
    let v = r.uint()?;
    Some(((v >> 1) as i64) ^ -((v & 1) as i64))
}

fn content_ref(out: &mut Vec<u8>, at: &ContentRef) {
    uint(out, at.pos());
    uint(out, u64::from(at.line_len()));
    uint(out, u64::from(at.part()));
}

fn read_ref(r: &mut Reader<'_>) -> Option<ContentRef> {
    let pos = r.uint()?;
    let len = r.u32()?;
    let part = u16::try_from(r.uint()?).ok()?;
    Some(ContentRef::new(pos, len, part))
}

fn text(r: &mut Reader<'_>) -> Option<Arc<str>> {
    std::str::from_utf8(r.bytes()?).ok().map(Arc::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &str) -> ChangePath {
        ChangePath(Arc::from(p.as_bytes()))
    }

    fn sample() -> Claims {
        Claims {
            last_activity: Timestamp(1_760_000_123_456),
            adapter_version: 7,
            calls: vec![
                ClaimCall {
                    turn: TurnId("t1".into()),
                    ordinal: 1,
                    call: ToolCallId("c1".into()),
                    kind: ClaimKind::Edit {
                        paths: vec![path("src/a.rs"), path("src/b.rs")].into(),
                        at: Timestamp(1_760_000_000_000),
                        status: ToolStatus::Interrupted,
                        input: ContentRef::new(1234, 99, 2),
                        result: Some(ContentRef::new(u64::MAX, u32::MAX, u16::MAX)),
                    },
                },
                ClaimCall {
                    turn: TurnId("t2".into()),
                    ordinal: 2,
                    call: ToolCallId("c2".into()),
                    kind: ClaimKind::Command {
                        from: Timestamp(-5),
                        to: Timestamp(i64::MAX),
                    },
                },
            ]
            .into(),
        }
    }

    #[test]
    fn claims_value_round_trips() {
        let claims = sample();
        let mut out = Vec::new();
        encode(&claims, &mut out);
        assert_eq!(decode(&out), Some(claims));
        // Truncated or with trailing bytes: a miss.
        assert_eq!(decode(&out[..out.len() - 1]), None);
        out.push(0);
        assert_eq!(decode(&out), None);
    }

    #[test]
    fn claims_paths_relative_to_the_worktree() {
        let rel = Relative::new(Path::new("/private/tmp/w"), Path::new("/tmp/w/sub"), "sub");
        assert_eq!(
            rel.rel(Path::new("/private/tmp/w/src/a.rs")),
            Some(path("src/a.rs"))
        );
        assert_eq!(
            rel.rel(Path::new("/tmp/w/sub/x.rs")),
            Some(path("sub/x.rs"))
        );
        assert_eq!(rel.rel(Path::new("y.rs")), Some(path("sub/y.rs")));
        assert_eq!(rel.rel(Path::new("/elsewhere/a.rs")), None);
        assert_eq!(rel.rel(Path::new("/private/tmp/w")), None);
    }
}
