//! Pure decisions of the session, from snapshots only: which directory a
//! target is read from, and which text `code_intel` is told is current
//! (docs/design/file_reader.md「数据流与线程」, the `code_intel` focus).

use std::path::Path;
use std::sync::Arc;

use lectern_services::code_intel::{TextOrigin, TextRef};
use lectern_services::project::{BlobId, FileList, ProjectKey, ProjectSnapshot, Scan};
use lectern_services::reader::ReadTarget;
use lectern_syntax::Canonical;

/// The root directory `target` is read from, in `snapshot` of its project:
/// the worktree's (not a prunable one) or the folder's. None when the
/// project has no such worktree or a folder project names one, so the
/// request cannot be placed.
pub(crate) fn root_of(snapshot: &ProjectSnapshot, target: &ReadTarget) -> Option<Arc<Path>> {
    match (&target.project, &target.worktree) {
        (ProjectKey::Git { .. }, Some(key)) => snapshot
            .worktrees
            .iter()
            .find(|w| &w.key == key && !w.prunable)
            .map(|w| w.root.clone()),
        (ProjectKey::Folder { root }, None) => Some(root.clone()),
        _ => None,
    }
}

/// The blob OID of `path` in the list (the index's, so the canonical
/// content's). Binary search over the sorted list: no scan.
pub(crate) fn index_oid(list: &FileList, path: &str) -> Option<BlobId> {
    let (mut lo, mut hi) = (0, list.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let (at, _) = list.get(mid)?;
        match at.cmp(path) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Equal => return list.oid(mid),
            std::cmp::Ordering::Greater => hi = mid,
        }
    }
    None
}

/// What `code_intel` is told about the document, once its OID is known.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Decision {
    /// This is the text that is current.
    Focus(TextRef),
    /// A workspace file of a git project whose raw OID is not the index's:
    /// the canonical OID is needed, and is not asked for yet.
    AskCanonical,
    /// No text is current (yet, or at all).
    None,
}

/// The text `code_intel` is told is current, once the document's OID is
/// known (`oid`, the snapshot's; None before the full read):
///
/// - a workspace file of a git project: the canonical content's OID. The
///   raw OID is it when it equals the index's (the active worktree's file
///   list); when it differs (the file is modified, converted by a git
///   attribute or not in the index) or the list is not there, it is the
///   `canonical` OID `syntax` found off the UI thread: `AskCanonical`
///   until it is asked for, no focus while it is pending and when there is
///   none (an external filter applies, the file is read in pages);
/// - a workspace file of a folder project: the raw OID (the SHA-1 of the
///   bytes);
/// - an object: `expect`;
/// - a dependency file: the raw OID.
pub(crate) fn decide(
    target: &ReadTarget,
    oid: Option<BlobId>,
    canonical: Canonical,
    project: Option<&ProjectSnapshot>,
) -> Decision {
    let Some(oid) = oid else {
        return Decision::None;
    };
    let oid = match &target.origin {
        TextOrigin::Object => match target.expect {
            Some(expect) => expect,
            None => return Decision::None,
        },
        TextOrigin::Dependency(_) => oid,
        TextOrigin::Worktree => match &target.project {
            ProjectKey::Folder { .. } => oid,
            ProjectKey::Git { .. } => {
                if in_index(target, oid, project) {
                    oid
                } else {
                    match canonical {
                        Canonical::Known(canonical) => canonical,
                        Canonical::NotAsked => return Decision::AskCanonical,
                        Canonical::Pending | Canonical::Unavailable => return Decision::None,
                    }
                }
            }
        },
    };
    Decision::Focus(TextRef {
        worktree: target.worktree.clone(),
        path: target.path.clone(),
        oid,
        origin: target.origin.clone(),
    })
}

/// Whether the file list of the active worktree names `oid` for the
/// target's path.
fn in_index(target: &ReadTarget, oid: BlobId, project: Option<&ProjectSnapshot>) -> bool {
    let Some(active) = project.and_then(|p| p.active.as_ref()) else {
        return false;
    };
    if active.worktree != target.worktree {
        return false;
    }
    let Scan::Ready(files) = &active.files else {
        return false;
    };
    index_oid(files, &target.path) == Some(oid)
}
