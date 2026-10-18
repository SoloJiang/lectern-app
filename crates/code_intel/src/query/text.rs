//! Reading what a query answers about (「跳转到定义」 step 1): the text the
//! user sees, verified against its OID, and the files queries read on
//! demand, keyed by the bytes actually read (「存储表」).

use std::sync::Arc;

use lectern_platform::{Read, RelPath as SafeRel, SafeError, SafeRoot};
use lectern_services::code_intel::{QueryError, TextOrigin, TextRef};
use lectern_services::project::BlobId;

use super::Snap;
use crate::extract::{BACKGROUND_MAX_LEN, INTERACTIVE_MAX_LEN};
use crate::git_access::{hash_blob, hash_kind};
use crate::index::Inner;
use crate::map::Effective;

impl Inner {
    /// The bytes of `text`, only when they are provably that content: the
    /// raw bytes' OID is `text.oid`, or the built-in conversions turn them
    /// into it (`canonical_oid`); otherwise `Stale`, whatever the map still
    /// records. An object's bytes are read by OID and need no check.
    pub(crate) fn read_text(&self, snap: &Snap, text: &TextRef) -> Result<Arc<[u8]>, QueryError> {
        match text.origin {
            TextOrigin::Object => {
                let tree = snap
                    .tree
                    .as_ref()
                    .ok_or_else(|| QueryError::Unreadable("no repository".into()))?;
                match self.git.blob(tree, text.oid, INTERACTIVE_MAX_LEN as u64) {
                    Ok(bytes) => Ok(bytes),
                    Err(lectern_git::GitError::TooLarge { .. }) => Err(QueryError::TooLarge),
                    Err(error) => Err(QueryError::Unreadable(error.to_string().into())),
                }
            }
            TextOrigin::Worktree => {
                let root = snap
                    .root_of(text.worktree.as_ref())
                    .ok_or_else(|| QueryError::Unreadable("cannot open the worktree".into()))?;
                let bytes = match read(&root, &text.path, INTERACTIVE_MAX_LEN) {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => return Err(QueryError::TooLarge),
                    // Gone, or not a file any more: not that content.
                    Err(Reading::Gone) => return Err(QueryError::Stale),
                    Err(Reading::Failed(why)) => return Err(QueryError::Unreadable(why.into())),
                };
                if self.is_content(snap, &text.path, &bytes, text.oid) {
                    Ok(bytes.into())
                } else {
                    Err(QueryError::Stale)
                }
            }
        }
    }

    /// Whether worktree bytes read at `path` are the blob `oid`: by their
    /// raw OID, or after the path's built-in conversions. A folder project
    /// (no repository) has raw OIDs only; a path with an external filter
    /// never matches when the raw OID differs.
    pub(crate) fn is_content(&self, snap: &Snap, path: &str, bytes: &[u8], oid: BlobId) -> bool {
        crate::phases::count("hashed bytes", bytes.len() as u64);
        if hash_blob(hash_kind(&oid), bytes) == oid {
            return true;
        }
        let Some(tree) = &snap.tree else {
            return false;
        };
        matches!(self.git.canonical_oid(tree, path, bytes), Ok(Some(c)) if c == oid)
    }

    /// A file of the snapshot's map, read for a query: the only way a query
    /// reads a file other than its own text (step 2's import targets, the
    /// indexed files of find references, the time-boxed search; a prefetch
    /// runs the same steps). The bytes count only when they are the content
    /// the snapshot's map has for the path, `eff.oid` (raw, or after the
    /// built-in conversions), as [`read_text`](Self::read_text) checks the
    /// text: otherwise the file changed after the snapshot and is `Stale`,
    /// never searched or extracted under the new bytes' OID. A path without
    /// an index entry (untracked, or a folder project) has the OID its
    /// overlay entry recorded when the worktree was read, and the same rule.
    pub(crate) fn read_content(&self, snap: &Snap, eff: &Effective) -> FileRead {
        self.step_hook(super::Step::Read, snap);
        let Some(root) = snap.root_of(None) else {
            return FileRead::Stale;
        };
        match read(&root, &eff.path, BACKGROUND_MAX_LEN) {
            Ok(Some(bytes)) if self.is_content(snap, &eff.path, &bytes, eff.oid) => {
                FileRead::Content(bytes)
            }
            Ok(None) => FileRead::TooLarge,
            Ok(Some(_)) | Err(_) => FileRead::Stale,
        }
    }
}

/// What [`Inner::read_content`] read.
pub(crate) enum FileRead {
    /// The snapshot's content of the path.
    Content(Vec<u8>),
    /// Over the background limit (1 MiB): no query reads it, as the batch
    /// tiers store it `TooLarge`.
    TooLarge,
    /// Gone, unreadable, or not the snapshot's content any more: nothing
    /// from it is used, and it counts as unsearched.
    Stale,
}

pub(crate) enum Reading {
    /// No file there (or a directory, a link).
    Gone,
    Failed(String),
}

/// A regular file's bytes; `Ok(None)` when larger than `max`.
pub(crate) fn read(root: &SafeRoot, path: &str, max: usize) -> Result<Option<Vec<u8>>, Reading> {
    crate::phases::count("file reads", 1);
    let rel = SafeRel::new(path.as_bytes()).ok_or(Reading::Gone)?;
    match root.read(rel, max as u64) {
        Ok(Read::File { bytes, .. }) => Ok(Some(bytes)),
        Ok(Read::TooLarge { .. }) => Ok(None),
        Ok(Read::Symlink { .. } | Read::Unsupported { .. }) => Err(Reading::Gone),
        Err(SafeError::NotFound) => Err(Reading::Gone),
        Err(error) => Err(Reading::Failed(error.to_string())),
    }
}
