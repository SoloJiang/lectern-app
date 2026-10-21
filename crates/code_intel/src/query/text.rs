//! Reading what a query answers about (「跳转到定义」 step 1): the text the
//! user sees, verified against its OID, and the files queries read on
//! demand, keyed by the bytes actually read (「存储表」).

use std::sync::Arc;

use lectern_platform::{Read, RelPath as SafeRel, SafeError, SafeRoot};
use lectern_services::code_intel::{DepOrigin, DepRoot, QueryError, TextOrigin, TextRef};

use crate::deps::roots::DepRead;
use lectern_services::project::BlobId;

use super::Snap;
use super::account::Reason;
use crate::extract::INTERACTIVE_MAX_LEN;
use crate::git_access::{hash_blob, hash_kind};
use crate::index::Inner;
use crate::map::Effective;

/// Bytes read as a blob's content, and whether they are that content as
/// they are (`canonical`: their raw OID is the blob's, or they come from
/// the object database) rather than through `canonical_oid`'s built-in
/// conversions (a CRLF worktree file): only then do byte offsets in them
/// go to views (「字节偏移」).
pub(crate) struct Content<B> {
    pub bytes: B,
    pub canonical: bool,
}

impl Inner {
    /// The bytes of `text`, only when they are provably that content: the
    /// raw bytes' OID is `text.oid`, or the built-in conversions turn them
    /// into it (`canonical_oid`); otherwise `Stale`, whatever the map still
    /// records. An object's bytes are read by OID and need no check.
    pub(crate) fn read_text(
        &self,
        snap: &Snap,
        text: &TextRef,
    ) -> Result<Content<Arc<[u8]>>, QueryError> {
        match &text.origin {
            TextOrigin::Object => {
                let tree = snap
                    .tree
                    .as_ref()
                    .ok_or_else(|| QueryError::Unreadable("no repository".into()))?;
                match self.git.blob(tree, text.oid, INTERACTIVE_MAX_LEN as u64) {
                    Ok(bytes) => Ok(Content {
                        bytes,
                        canonical: true,
                    }),
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
                match self.content_kind(snap, &text.path, &bytes, text.oid) {
                    Some(canonical) => Ok(Content {
                        bytes: bytes.into(),
                        canonical,
                    }),
                    None => Err(QueryError::Stale),
                }
            }
            TextOrigin::Dependency(origin) => self.read_dependency_text(snap, text, origin),
        }
    }

    /// A dependency file's bytes (「依赖层」「读取核对」): read only through
    /// the session's dependency roots, inside the root its origin names,
    /// with an allowed extension; its raw bytes' OID must be the text's
    /// (dependency files are not normalized), otherwise `Stale`, as is a
    /// path outside the roots.
    pub(crate) fn read_dependency_text(
        &self,
        snap: &Snap,
        text: &TextRef,
        origin: &DepOrigin,
    ) -> Result<Content<Arc<[u8]>>, QueryError> {
        let DepRoot::Worktree { dir } = &origin.root else {
            return Err(QueryError::Stale);
        };
        let roots = snap.dep_roots.as_ref().ok_or(QueryError::Stale)?;
        let file = roots
            .file_at(&text.path, Some(dir), origin.ecosystem)
            .ok_or(QueryError::Stale)?;
        crate::phases::count("file reads", 1);
        let bytes = match roots.read(&file, crate::deps::FILE_MAX) {
            Ok((bytes, _)) => bytes,
            Err(DepRead::TooLarge) => return Err(QueryError::TooLarge),
            Err(DepRead::Gone) => return Err(QueryError::Stale),
            Err(DepRead::Failed(why)) => return Err(QueryError::Unreadable(why.into())),
        };
        if hash_blob(hash_kind(&text.oid), &bytes) != text.oid {
            return Err(QueryError::Stale);
        }
        Ok(Content {
            bytes: bytes.into(),
            canonical: true,
        })
    }

    /// Whether worktree bytes read at `path` are the blob `oid`: by their
    /// raw OID (`Some(true)`), or after the path's built-in conversions
    /// (`Some(false)`); None when they are not. A folder project (no
    /// repository) has raw OIDs only; a path with an external filter never
    /// matches when the raw OID differs.
    pub(crate) fn content_kind(
        &self,
        snap: &Snap,
        path: &str,
        bytes: &[u8],
        oid: BlobId,
    ) -> Option<bool> {
        crate::phases::count("hashed bytes", bytes.len() as u64);
        if hash_blob(hash_kind(&oid), bytes) == oid {
            return Some(true);
        }
        let tree = snap.tree.as_ref()?;
        matches!(self.git.canonical_oid(tree, path, bytes), Ok(Some(c)) if c == oid)
            .then_some(false)
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
    ///
    /// `max` is the limit the file's unit was extracted under
    /// (`Snap::read_limit`). Otherwise why it cannot be searched: over
    /// `max`, gone or not the snapshot's content (`Stale`), or unreadable;
    /// the caller records it in the query's account.
    pub(crate) fn read_content(
        &self,
        snap: &Snap,
        eff: &Effective,
        max: usize,
    ) -> Result<Content<Vec<u8>>, Reason> {
        self.step_hook(super::Step::Read, snap);
        let Some(root) = snap.root_of(None) else {
            return Err(Reason::Unreadable);
        };
        match read(&root, &eff.path, max) {
            Ok(Some(bytes)) => match self.content_kind(snap, &eff.path, &bytes, eff.oid) {
                Some(canonical) => Ok(Content { bytes, canonical }),
                None => Err(Reason::Stale),
            },
            Err(Reading::Gone) => Err(Reason::Stale),
            Ok(None) => Err(Reason::TooLarge),
            Err(Reading::Failed(_)) => Err(Reason::Unreadable),
        }
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
