//! Pure decisions of the session, from snapshots only: which directory a
//! target is read from, and which text `code_intel` is told is current
//! (docs/design/file_reader.md「数据流与线程」, the `code_intel` focus).

use std::path::Path;
use std::sync::Arc;

use lectern_services::code_intel::{TextOrigin, TextRef};
use lectern_services::project::{BlobId, FileList, ProjectKey, ProjectSnapshot, Scan};
use lectern_services::reader::ReadTarget;

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

/// The text `code_intel` is told is current, once the document's OID is
/// known (`oid`, the snapshot's; None before the full read):
///
/// - a workspace file of a git project: the canonical content's OID. The
///   raw OID is it when it equals the index's (the active worktree's file
///   list); when it differs the file is modified or converted by a filter,
///   and the canonical OID needs git (`Worktree::canonical_oid`), which
///   this crate does not call: no focus then (docs/design/file_reader.md
///   「与设计的差异」);
/// - a workspace file of a folder project: the raw OID (the SHA-1 of the
///   bytes);
/// - an object: `expect`;
/// - a dependency file: the raw OID.
pub(crate) fn text_ref(
    target: &ReadTarget,
    oid: Option<BlobId>,
    project: Option<&ProjectSnapshot>,
) -> Option<TextRef> {
    let oid = oid?;
    let oid = match &target.origin {
        TextOrigin::Object => target.expect?,
        TextOrigin::Dependency(_) => oid,
        TextOrigin::Worktree => match &target.project {
            ProjectKey::Folder { .. } => oid,
            ProjectKey::Git { .. } => {
                let active = project?.active.as_ref()?;
                if active.worktree != target.worktree {
                    return None;
                }
                let Scan::Ready(files) = &active.files else {
                    return None;
                };
                (index_oid(files, &target.path)? == oid).then_some(oid)?
            }
        },
    };
    Some(TextRef {
        worktree: target.worktree.clone(),
        path: target.path.clone(),
        oid,
        origin: target.origin.clone(),
    })
}
