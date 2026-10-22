//! The turn-history digest of a path (docs/design/review.md「归因的缓存」):
//! part of the `review.attribution` key, so that two sessions that leave a
//! file with the same content still have their own narratives
//! (`crates/review/AGENTS.md`「边界」, architecture rule 5).
//!
//! Only turn-index data goes in, never loaded content: computing the key
//! loads nothing, so a cache hit reads no session text. The positions
//! (`ContentRef`) stand for the content, which is valid because session
//! files are append-only while `last_activity` stays the same.

use lectern_services::changes::ChangePath;
use lectern_services::session::{ContentRef, SessionKey, ToolCallId, ToolStatus, TurnId};
use sha2::{Digest, Sha256};

/// One change step of the path's history, as the turn index has it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HistoryStep<'a> {
    pub session: &'a SessionKey,
    /// `TurnIndex::cursor`'s adapter version for the session's index.
    pub adapter_version: u32,
    pub turn: &'a TurnId,
    pub call: &'a ToolCallId,
    pub status: ToolStatus,
    pub input: ContentRef,
    pub result: Option<ContentRef>,
}

/// Bumped when what goes into the digest changes (the namespace version of
/// `review.attribution` covers the chain's algorithm and value format).
const DOMAIN: &[u8] = b"lectern-review-history\0v1\0";

/// SHA-256 over, in order, each step's source ID, session ID, adapter
/// version, turn ID, call ID, call status and input and result positions,
/// then the paths of the rename chain (`paths`: those the steps were taken
/// on, the base side's path first). Every field is length-prefixed or
/// fixed-size, so no two histories encode the same.
pub(crate) fn digest(steps: &[HistoryStep<'_>], paths: &[ChangePath]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update((steps.len() as u64).to_le_bytes());
    for s in steps {
        bytes(&mut h, s.session.source.0.as_bytes());
        bytes(&mut h, s.session.id.as_bytes());
        h.update(s.adapter_version.to_le_bytes());
        bytes(&mut h, s.turn.0.as_bytes());
        bytes(&mut h, s.call.0.as_bytes());
        h.update([match s.status {
            ToolStatus::Completed => 0u8,
            ToolStatus::Failed => 1,
            ToolStatus::Denied => 2,
            ToolStatus::Interrupted => 3,
        }]);
        content_ref(&mut h, Some(s.input));
        content_ref(&mut h, s.result);
    }
    h.update((paths.len() as u64).to_le_bytes());
    for p in paths {
        bytes(&mut h, &p.0);
    }
    h.finalize().into()
}

fn bytes(h: &mut Sha256, b: &[u8]) {
    h.update((b.len() as u64).to_le_bytes());
    h.update(b);
}

fn content_ref(h: &mut Sha256, at: Option<ContentRef>) {
    match at {
        None => h.update([0u8]),
        Some(at) => {
            h.update([1u8]);
            h.update(at.pos().to_le_bytes());
            h.update(at.line_len().to_le_bytes());
            h.update(at.part().to_le_bytes());
        }
    }
}
