//! The Files tree's rows: the visible rows of a [`DirTable`] under an
//! expansion (docs/design/file_reader.md「界面」的 Files). No GPUI, no IO:
//! a pure function of the table and the set of expanded directories.
//!
//! A row is a directory or a file with its indent level. The root is not a
//! row; its children are the first rows. A directory with one subdirectory
//! and no file is folded with the chain below it into one row
//! (`src/main/java/com/example`), which stands for the chain's last
//! directory: the row is open when that directory is in the expanded set.
//! Opening a chain puts every directory of it in the set, so a regenerated
//! table in which the chain broke keeps the state of each part. Expanding
//! or collapsing a row inserts or removes the rows below it (a [`Splice`]),
//! the other rows stay where they are.

use std::collections::HashSet;
use std::sync::Arc;

use lectern_services::project::{DirId, DirTable};

/// What a row is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Node {
    /// A directory: the first directory of its folded chain, and the last.
    /// A directory that is not folded has `head == end`.
    Dir { head: DirId, end: DirId },
    /// An entry of the file list (a submodule is a file here, it does not
    /// open).
    File(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Line {
    pub(crate) node: Node,
    /// How many directories the row is inside.
    pub(crate) depth: u16,
}

/// Rows taken out and put in by one expand or collapse: `removed` rows at
/// `at` went, `inserted` rows came.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Splice {
    pub(crate) at: usize,
    pub(crate) removed: usize,
    pub(crate) inserted: usize,
}

pub(crate) struct Tree {
    table: Arc<DirTable>,
    /// Paths of the expanded directories. Directories that are not in the
    /// table (the file list changed) are dropped when the tree is built.
    expanded: HashSet<Box<str>>,
    rows: Vec<Line>,
}

impl Tree {
    /// The rows of `table` with `expanded` open. Work proportional to the
    /// visible rows.
    pub(crate) fn new(table: Arc<DirTable>, mut expanded: HashSet<Box<str>>) -> Tree {
        expanded.retain(|path| table.find(path).is_some());
        let mut tree = Tree {
            table,
            expanded,
            rows: Vec::new(),
        };
        tree.rows = tree.visible();
        tree
    }

    pub(crate) fn table(&self) -> &Arc<DirTable> {
        &self.table
    }

    pub(crate) fn rows(&self) -> &[Line] {
        &self.rows
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// The expanded directories, to build the next tree with.
    pub(crate) fn into_expanded(self) -> HashSet<Box<str>> {
        self.expanded
    }

    pub(crate) fn expanded(&self) -> &HashSet<Box<str>> {
        &self.expanded
    }

    /// The rows of the whole table under the expansion.
    fn visible(&self) -> Vec<Line> {
        let mut rows = Vec::new();
        self.children(self.table.root(), 0, &mut rows);
        rows
    }

    /// Pushes the rows of the children of `dir` (an open directory, or the
    /// root): subdirectories first, then files, each in Finder order, and
    /// below each open subdirectory its own.
    fn children(&self, dir: DirId, depth: u16, out: &mut Vec<Line>) {
        for &head in self.table.subdirs(dir) {
            let end = self.table.fold_end(head);
            out.push(Line {
                node: Node::Dir { head, end },
                depth,
            });
            if self.is_open(end) {
                self.children(end, depth.saturating_add(1), out);
            }
        }
        for &file in self.table.dir_files(dir) {
            out.push(Line {
                node: Node::File(file),
                depth,
            });
        }
    }

    fn is_open(&self, end: DirId) -> bool {
        self.expanded.contains(self.table.path(end))
    }

    /// Whether row `index` is a directory that is open; None for a file.
    pub(crate) fn open_at(&self, index: usize) -> Option<bool> {
        match self.rows.get(index)?.node {
            Node::Dir { end, .. } => Some(self.is_open(end)),
            Node::File(_) => None,
        }
    }

    /// The path the row stands for: a directory's is the last directory of
    /// its chain.
    pub(crate) fn path(&self, index: usize) -> Option<&str> {
        Some(match self.rows.get(index)?.node {
            Node::Dir { end, .. } => self.table.path(end),
            Node::File(file) => self.table.files().get(file as usize)?.0,
        })
    }

    /// What the row shows: the file's name, or the directory's name (a
    /// folded chain's names with their slashes).
    pub(crate) fn name(&self, index: usize) -> Option<&str> {
        let path = self.path(index)?;
        Some(match self.rows.get(index)?.node {
            Node::Dir { head, .. } => {
                let head_path = self.table.path(head);
                let start = head_path.len() - self.table.name(head).len();
                &path[start..]
            }
            Node::File(_) => path.rsplit_once('/').map_or(path, |(_, name)| name),
        })
    }

    /// Whether the row is a chain of several directories.
    pub(crate) fn folded(&self, index: usize) -> bool {
        matches!(
            self.rows.get(index).map(|line| line.node),
            Some(Node::Dir { head, end }) if head != end
        )
    }

    /// The index of the row for `path` (a directory's last directory, or a
    /// file), if it is visible.
    pub(crate) fn position(&self, path: &str) -> Option<usize> {
        (0..self.rows.len()).find(|&index| self.path(index) == Some(path))
    }

    /// The row of the directory that holds row `index`, if it is not at
    /// the top level.
    pub(crate) fn parent(&self, index: usize) -> Option<usize> {
        let depth = self.rows.get(index)?.depth;
        (0..index)
            .rev()
            .find(|&before| self.rows[before].depth < depth)
    }

    /// Opens the directory row `index`. None when it is not a closed
    /// directory.
    pub(crate) fn expand(&mut self, index: usize) -> Option<Splice> {
        let line = *self.rows.get(index)?;
        let Node::Dir { head, end } = line.node else {
            return None;
        };
        if self.is_open(end) {
            return None;
        }
        // Every directory of the chain, so that the parts keep their state
        // when the chain breaks.
        let mut dir = end;
        loop {
            self.expanded.insert(self.table.path(dir).into());
            if dir == head {
                break;
            }
            match self.table.parent(dir) {
                Some(parent) => dir = parent,
                None => break,
            }
        }
        let mut inserted = Vec::new();
        self.children(end, line.depth.saturating_add(1), &mut inserted);
        let count = inserted.len();
        self.rows.splice(index + 1..index + 1, inserted);
        Some(Splice {
            at: index + 1,
            removed: 0,
            inserted: count,
        })
    }

    /// Closes the directory row `index`. None when it is not an open
    /// directory. The directories below it stay in the expanded set: opened
    /// again, they open as they were.
    pub(crate) fn collapse(&mut self, index: usize) -> Option<Splice> {
        let line = *self.rows.get(index)?;
        let Node::Dir { head, end } = line.node else {
            return None;
        };
        if !self.is_open(end) {
            return None;
        }
        let mut dir = end;
        loop {
            self.expanded.remove(self.table.path(dir));
            if dir == head {
                break;
            }
            match self.table.parent(dir) {
                Some(parent) => dir = parent,
                None => break,
            }
        }
        let below = self.rows[index + 1..]
            .iter()
            .take_while(|below| below.depth > line.depth)
            .count();
        self.rows.drain(index + 1..index + 1 + below);
        Some(Splice {
            at: index + 1,
            removed: below,
            inserted: 0,
        })
    }

    /// Opens every directory above `path` and gives the file's row. None
    /// when the file list has no such path. The rows are rebuilt only when
    /// a directory had to be opened.
    pub(crate) fn reveal(&mut self, path: &str) -> Option<usize> {
        self.table.files().find(path)?;
        let mut changed = false;
        for (at, _) in path.match_indices('/') {
            let dir = &path[..at];
            if self.table.find(dir).is_some() && !self.expanded.contains(dir) {
                self.expanded.insert(dir.into());
                changed = true;
            }
        }
        if changed {
            self.rows = self.visible();
        }
        self.position(path)
    }
}
