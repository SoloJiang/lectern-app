//! What the runtime log keeps of this crate's values (docs/design/review.md
//! 「日志与诊断」, docs/design/logging.md): the correlation IDs of a review
//! and a session, and the kinds of errors. Never their text: error messages
//! can quote paths' contents, session lines or commit messages.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use lectern_git::GitError;
use lectern_logging::{Ctx, Value};
use lectern_services::changes::{ChangeError, ChangePath, RefreshHint};
use lectern_services::review::{BaseHow, ReviewError, ReviewTarget};
use lectern_services::session::{SessionError, SessionKey};
use sha2::{Digest, Sha256};

use crate::engine::round::Kind;
use crate::seen::TargetKey;

/// The 128-bit tag of a session ID for the `session` scope and fields: the
/// UUID itself when the ID is one (Claude Code and Codex IDs are), a hash
/// otherwise (a Claude Code subagent's `<parent>/agent-<id>`). The same
/// derivation as `usage`'s, so that both crates' records of one session
/// carry the same value.
pub(crate) fn session_tag(id: &str) -> [u8; 16] {
    let hex: Vec<u8> = id.bytes().filter(|b| *b != b'-').collect();
    if hex.len() == 32 && hex.iter().all(u8::is_ascii_hexdigit) {
        let mut out = [0u8; 16];
        for (i, pair) in hex.chunks(2).enumerate() {
            let digit = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
            out[i] = digit(pair[0]) << 4 | digit(pair[1]);
        }
        return out;
    }
    use std::hash::{Hash, Hasher};
    let mut out = [0u8; 16];
    for (i, half) in out.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (i, id).hash(&mut h);
        half.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

/// A session key as a field value: its tag (the source ID is a runtime
/// string and is not written).
pub(crate) fn session(key: &SessionKey) -> Value<'static> {
    Value::uuid(session_tag(&key.id))
}

/// The scope a review's tasks start with, before its repository is known:
/// the target session's tag.
pub(crate) fn target_ctx(target: &ReviewTarget) -> Ctx {
    Ctx {
        session: match target {
            ReviewTarget::Session(key) => Some(session_tag(&key.id)),
            ReviewTarget::Outside { .. } | ReviewTarget::Range { .. } => None,
        },
        ..Ctx::default()
    }
}

/// The scope of a review once its repository is known: `review` is the
/// first 8 bytes of SHA-256(the repository's hash `h`, the annotation log's
/// encoding of the target), the same across runs; `session` the target
/// session's tag.
pub(crate) fn review_ctx(target: &ReviewTarget, common_dir: &Path, key: &TargetKey) -> Ctx {
    let mut hasher = Sha256::new();
    hasher.update(crate::log::repo_hash(common_dir));
    hasher.update(crate::log::target_bytes(key));
    let digest = hasher.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    Ctx {
        review: Some(u64::from_be_bytes(first)),
        ..target_ctx(target)
    }
}

/// The scope of a project's overview: `review` is the first 8 bytes of
/// SHA-256(the repository's hash, "overview"), so the records of one
/// repository's overview can be told from its reviews'.
pub(crate) fn overview_ctx(common_dir: &Path) -> Ctx {
    let mut hasher = Sha256::new();
    hasher.update(crate::log::repo_hash(common_dir));
    hasher.update(b"overview");
    let digest = hasher.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    Ctx {
        review: Some(u64::from_be_bytes(first)),
        ..Ctx::default()
    }
}

/// A change path as a path field (bytes that are not UTF-8 are replaced
/// when written).
pub(crate) fn path(p: &ChangePath) -> Value<'_> {
    Value::path(Path::new(OsStr::from_bytes(&p.0)))
}

pub(crate) fn base_how(how: BaseHow) -> Value<'static> {
    Value::text(match how {
        BaseHow::Recorded => "recorded",
        BaseHow::Reflog => "reflog",
        BaseHow::BeforeLog => "before_log",
        BaseHow::CurrentHead => "current_head",
        BaseHow::Spec => "spec",
    })
}

/// A `ChangeError`'s kind; never its text.
pub(crate) fn change_error(e: &ChangeError) -> Value<'static> {
    Value::text(match e {
        ChangeError::Unsupported(_) => "unsupported",
        ChangeError::NoMergeBase => "no_merge_base",
        ChangeError::TooManyEntries => "too_many_entries",
        ChangeError::Io(_) => "io",
        ChangeError::Cancelled => "cancelled",
    })
}

/// A `ReviewError`'s kind (a change set error by its own kind).
pub(crate) fn review_error(e: &ReviewError) -> Value<'static> {
    match e {
        ReviewError::Missing => Value::text("missing"),
        ReviewError::NotInOpenProject => Value::text("not_in_open_project"),
        ReviewError::Changes(e) => change_error(e),
    }
}

/// How a round got its change set.
pub(crate) fn kind(kind: &Kind) -> Value<'static> {
    Value::text(match kind {
        Kind::Fetch => "fetch",
        Kind::Refresh(RefreshHint::Full) => "refresh_full",
        Kind::Refresh(_) => "refresh_paths",
        Kind::Reuse => "reuse",
    })
}

/// A `SessionError`'s kind; never its text.
pub(crate) fn session_error(e: &SessionError) -> Value<'static> {
    Value::text(match e {
        SessionError::NotFound => "not_found",
        SessionError::Io(_) => "io",
        SessionError::Unrecognized => "unrecognized",
        SessionError::TooLarge(_) => "too_large",
    })
}

/// A `GitError`'s kind, and its `io::Error` when it has one; never the
/// text of `Corrupt`.
pub(crate) fn git_error(e: &GitError) -> (Value<'static>, Option<Value<'static>>) {
    let kind = match e {
        GitError::Unsupported(_) => "unsupported",
        GitError::NoMergeBase => "no_merge_base",
        GitError::ObjectMissing(_) => "object_missing",
        GitError::TooLarge { .. } => "too_large",
        GitError::TooManyEntries => "too_many_entries",
        GitError::Corrupt(_) => "corrupt",
        GitError::Io(_) => "io",
        GitError::Cancelled => "cancelled",
    };
    let io = match e {
        GitError::Io(io) => Some(Value::io(io)),
        _ => None,
    };
    (Value::text(kind), io)
}
