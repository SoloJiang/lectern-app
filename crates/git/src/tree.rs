//! Walking a tree's file entries, for sparse-index expansion and
//! `Repo::tree_paths`.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::handle::Handle;
use crate::{GitError, HashKind, Mode, Oid};

/// One non-tree entry: its full path (`prefix/…`, raw bytes), mode and OID.
pub(crate) struct FileEntry {
    pub path: Vec<u8>,
    pub mode: Mode,
    pub oid: Oid,
}

/// Visits every non-tree entry under `tree` in depth-first tree order (the
/// index's path order), with paths prefixed by `prefix` (no trailing `/`;
/// empty for the root). Checks `cancel` before reading each tree object.
/// `visit` returns an error to stop the walk (e.g. too many entries).
pub(crate) fn walk(
    handle: &Handle,
    tree: Oid,
    prefix: &[u8],
    cancel: &AtomicBool,
    visit: &mut dyn FnMut(FileEntry) -> Result<(), GitError>,
) -> Result<(), GitError> {
    #[cfg(test)]
    hook::expanded();
    enum Item {
        Tree(Oid, Vec<u8>),
        File(FileEntry),
    }
    let hash: HashKind = tree.kind();
    let mut stack = vec![Item::Tree(tree, prefix.to_vec())];
    let mut children = Vec::new();
    while let Some(item) = stack.pop() {
        let (id, dir) = match item {
            Item::File(entry) => {
                visit(entry)?;
                continue;
            }
            Item::Tree(id, dir) => (id, dir),
        };
        if cancel.load(Ordering::Relaxed) {
            return Err(GitError::Cancelled);
        }
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
        let decoded = gix::objs::TreeRef::from_bytes(&object.data, hash.to_gix())
            .map_err(|err| GitError::Corrupt(format!("tree {id}: {err}")))?;
        children.clear();
        for entry in &decoded.entries {
            let mut path = Vec::with_capacity(dir.len() + 1 + entry.filename.len());
            if !dir.is_empty() {
                path.extend_from_slice(&dir);
                path.push(b'/');
            }
            path.extend_from_slice(entry.filename);
            let oid = Oid::from_gix(entry.oid);
            children.push(match Mode::from_tree(entry.mode) {
                None => Item::Tree(oid, path),
                Some(mode) => Item::File(FileEntry { path, mode, oid }),
            });
        }
        stack.extend(children.drain(..).rev());
    }
    Ok(())
}

/// Counting hook: how many tree walks (sparse expansions, `tree_paths`)
/// this thread started.
#[cfg(test)]
pub(crate) mod hook {
    use std::cell::Cell;

    thread_local! {
        static EXPANDED: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn expanded() {
        EXPANDED.with(|c| c.set(c.get() + 1));
    }

    pub(crate) fn expanded_count() -> usize {
        EXPANDED.with(Cell::get)
    }
}
