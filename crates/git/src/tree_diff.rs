//! Walking two trees side by side: the entries that differ, without
//! descending into subtrees with the same OID (docs/design/git.md「改动集」).

use std::cmp::Ordering;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use crate::handle::Handle;
use crate::{GitError, HashKind, Mode, Oid};

/// One path that differs: either side may be missing. Never a tree.
pub(crate) struct RawEntry {
    pub path: Vec<u8>,
    pub old: Option<(Mode, Oid)>,
    pub new: Option<(Mode, Oid)>,
}

pub(crate) struct Walk {
    /// Sorted by path bytes.
    pub entries: Vec<RawEntry>,
    /// `.gitattributes` blobs in the new tree's directories that were
    /// visited, which are all the directories a changed path can be in.
    pub attributes: Vec<(Vec<u8>, Oid)>,
    /// Paths whose names break the `RepoPath` invariants: not walked into.
    pub invalid: Vec<Arc<[u8]>>,
}

struct Entry<'a> {
    name: &'a [u8],
    /// None for a subtree.
    mode: Option<Mode>,
    oid: Oid,
}

/// git's tree order: names compared as if a subtree's had a trailing `/`.
fn tree_order(a: &Entry<'_>, b: &Entry<'_>) -> Ordering {
    let n = a.name.len().min(b.name.len());
    a.name[..n].cmp(&b.name[..n]).then_with(|| {
        let end = |e: &Entry<'_>| {
            e.name
                .get(n)
                .copied()
                .unwrap_or(if e.mode.is_none() { b'/' } else { 0 })
        };
        end(a).cmp(&end(b))
    })
}

fn valid_name(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.contains(&b'/') && !name.contains(&0)
}

/// Every non-tree entry that differs between `old` and `new` (tree OIDs).
/// At most `max_entries` entries (`GitError::TooManyEntries` above it);
/// checks `cancel` before reading each pair of trees.
pub(crate) fn diff(
    handle: &Handle,
    old: Oid,
    new: Oid,
    max_entries: usize,
    cancel: &AtomicBool,
) -> Result<Walk, GitError> {
    let hash = old.kind();
    let mut walk = Walk {
        entries: Vec::new(),
        attributes: Vec::new(),
        invalid: Vec::new(),
    };
    let mut stack: Vec<(Vec<u8>, Option<Oid>, Option<Oid>)> = Vec::new();
    if old != new {
        stack.push((Vec::new(), Some(old), Some(new)));
    }
    while let Some((dir, old_tree, new_tree)) = stack.pop() {
        if cancel.load(AtomicOrdering::Relaxed) {
            return Err(GitError::Cancelled);
        }
        let old_data = read_tree(handle, old_tree)?;
        let new_data = read_tree(handle, new_tree)?;
        let olds = entries(&old_data, old_tree, hash)?;
        let news = entries(&new_data, new_tree, hash)?;
        let join = |name: &[u8]| {
            let mut path = Vec::with_capacity(dir.len() + 1 + name.len());
            if !dir.is_empty() {
                path.extend_from_slice(&dir);
                path.push(b'/');
            }
            path.extend_from_slice(name);
            path
        };
        for entry in &news {
            if entry.name == b".gitattributes"
                && matches!(entry.mode, Some(Mode::File | Mode::Executable))
            {
                walk.attributes.push((join(entry.name), entry.oid));
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < olds.len() || j < news.len() {
            let order = match (olds.get(i), news.get(j)) {
                (Some(a), Some(b)) => tree_order(a, b),
                (Some(_), None) => Ordering::Less,
                (None, _) => Ordering::Greater,
            };
            let (a, b) = match order {
                Ordering::Less => {
                    i += 1;
                    (Some(&olds[i - 1]), None)
                }
                Ordering::Greater => {
                    j += 1;
                    (None, Some(&news[j - 1]))
                }
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                    (Some(&olds[i - 1]), Some(&news[j - 1]))
                }
            };
            let name = a.or(b).map_or(&b""[..], |e| e.name);
            if a.zip(b)
                .is_some_and(|(a, b)| a.oid == b.oid && a.mode == b.mode)
            {
                continue;
            }
            let path = join(name);
            if !valid_name(name) {
                walk.invalid.push(path.into());
                continue;
            }
            let tree_of = |e: Option<&Entry<'_>>| e.filter(|e| e.mode.is_none()).map(|e| e.oid);
            let file_of = |e: Option<&Entry<'_>>| e.and_then(|e| Some((e.mode?, e.oid)));
            // Equal order means the same kind (both trees or both not).
            if tree_of(a).is_some() || tree_of(b).is_some() {
                stack.push((path, tree_of(a), tree_of(b)));
                continue;
            }
            if walk.entries.len() >= max_entries {
                return Err(GitError::TooManyEntries);
            }
            walk.entries.push(RawEntry {
                path,
                old: file_of(a),
                new: file_of(b),
            });
        }
    }
    walk.entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    walk.attributes.sort_unstable();
    walk.invalid.sort_unstable();
    Ok(walk)
}

fn read_tree(handle: &Handle, tree: Option<Oid>) -> Result<Vec<u8>, GitError> {
    let Some(id) = tree else {
        return Ok(Vec::new());
    };
    let object = handle
        .try_find_object(id.to_gix())
        .map_err(GitError::from_gix)?
        .ok_or(GitError::ObjectMissing(id))?;
    if object.kind != gix::objs::Kind::Tree {
        return Err(GitError::Corrupt(format!(
            "{id} is a {}, not a tree",
            object.kind
        )));
    }
    Ok(object.detach().data)
}

fn entries(data: &[u8], tree: Option<Oid>, hash: HashKind) -> Result<Vec<Entry<'_>>, GitError> {
    let Some(id) = tree else {
        return Ok(Vec::new());
    };
    let decoded = gix::objs::TreeRef::from_bytes(data, hash.to_gix())
        .map_err(|err| GitError::Corrupt(format!("tree {id}: {err}")))?;
    let mut out: Vec<Entry<'_>> = decoded
        .entries
        .iter()
        .map(|e| Entry {
            name: e.filename,
            mode: Mode::from_tree(e.mode),
            oid: Oid::from_gix(e.oid),
        })
        .collect();
    // Trees git wrote are sorted already; this keeps a corrupt one from
    // breaking the merge.
    out.sort_by(tree_order);
    Ok(out)
}
