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

    /// The key of worktree bytes read at `path`, queued as `expected`: the
    /// expected OID when the bytes are that content, else their raw OID.
    pub(crate) fn key_of(&self, snap: &Snap, path: &str, bytes: &[u8], expected: BlobId) -> BlobId {
        if self.is_content(snap, path, bytes, expected) {
            expected
        } else {
            hash_blob(hash_kind(&expected), bytes)
        }
    }

    /// A worktree file of the current worktree, read up to the background
    /// limit (1 MiB) for a query: None when it cannot be read as a file.
    pub(crate) fn read_file(&self, snap: &Snap, path: &str) -> Option<Vec<u8>> {
        let root = snap.root_of(None)?;
        read(&root, path, BACKGROUND_MAX_LEN).ok().flatten()
    }
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
