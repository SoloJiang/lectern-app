//! The Files tree's state and what it does (docs/design/file_reader.md
//! 「界面」的 Files): it reads the project's snapshot (the active worktree's
//! directory table), keeps the expansion and the selection, takes the
//! arrows and ↵, and asks the reader to show a file. It reads snapshots and
//! sends commands: no IO, no parsing, no git (architecture rule 4). The
//! drawing is in `render.rs`; the rows are `tree.rs`'s.
//!
//! `workspace` drops the sidebar's view whenever the sidebar is hidden, and
//! the reading view hides it. So the expansion and the selection go to
//! [`Shared`] when the view goes and come back with the next view, and the
//! file the reader showed last (`Session::last_target`) is revealed when
//! the tree appears.

use std::cell::Cell;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{Context, FocusHandle, ScrollStrategy, Task, UniformListScrollHandle};
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::{DirTable, ProjectKey, Scan, WorktreeKey};
use lectern_services::reader::{Anchor, ReadTarget, Reader};
use lectern_services::workspace::WorkspaceCommand;

use super::tree::{Node, Tree};
use crate::shared::{FilesMemory, Shared};

/// What the panel shows.
pub(crate) enum Content {
    /// The directory table is not there yet (since when).
    Loading {
        since: Instant,
    },
    /// The index cannot be read.
    Unavailable,
    /// No tracked files, or no worktree to list them of.
    Empty,
    Tree(Tree),
}

pub struct FilesView {
    pub(crate) shared: Arc<Shared>,
    id: u64,
    pub(crate) focus: FocusHandle,
    pub(crate) scroll: UniformListScrollHandle,
    pub(crate) project: Option<ProjectKey>,
    pub(crate) worktree: Option<WorktreeKey>,
    /// The project and worktree the tree's rows were built for: rows of
    /// another one are not shown while its table is built.
    tree_for: Option<(ProjectKey, Option<WorktreeKey>)>,
    pub(crate) content: Content,
    /// The expanded directories and the selected path while there is no
    /// tree (the next tree starts from them).
    kept: HashSet<Box<str>>,
    kept_selected: Option<Box<str>>,
    pub(crate) selected: Option<usize>,
    /// The reader's last file is not revealed yet (no tree to do it in).
    want_reveal: bool,
    /// How many rows the last frame built.
    pub(crate) built: Cell<usize>,
    _listen: Task<()>,
}

impl FilesView {
    pub(crate) fn new(shared: Arc<Shared>, cx: &mut Context<FilesView>) -> FilesView {
        // `ProjectEvent::Changed` wakes this from any thread; the foreground
        // task reads the snapshot. One message waits at most.
        let (tx, mut rx) = mpsc::unbounded::<()>();
        let queued = Arc::new(AtomicBool::new(false));
        let flag = queued.clone();
        let id = shared.attach_files(Arc::new(move || {
            if !flag.swap(true, Ordering::SeqCst) {
                let _ = tx.unbounded_send(());
            }
        }));
        let listen = cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                queued.store(false, Ordering::SeqCst);
                if this.update(cx, |view, cx| view.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        let project = shared.session.open_project();
        let memory = shared.take_files_memory(project.as_ref());
        let mut view = FilesView {
            shared,
            id,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            project,
            worktree: None,
            tree_for: None,
            content: Content::Loading {
                since: Instant::now(),
            },
            kept: memory.expanded,
            kept_selected: memory.selected,
            selected: None,
            want_reveal: true,
            built: Cell::new(0),
            _listen: listen,
        };
        view.refresh(cx);
        view
    }

    // ----- the snapshot ---------------------------------------------------

    /// Reads the project's snapshot again: the directory table of the
    /// active worktree, as `project` published it. A table that is the one
    /// the tree is built from changes nothing; a new one rebuilds the rows
    /// with the expanded directories and the selected row kept by path.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let session = &self.shared.session;
        let project = session.open_project();
        let snapshot = project
            .as_ref()
            .and_then(|project| session.project_snapshot(project));
        self.project = project;
        let active = snapshot.as_deref().and_then(|s| s.active.as_ref());
        match active {
            None => {
                self.worktree = None;
                self.settle(Content::Empty);
            }
            Some(active) => {
                self.worktree = active.worktree.clone();
                // Another worktree (or project) has another list: the old
                // rows would open files of the wrong tree. They wait in
                // `kept` (by path) for the new table.
                let tree_for = self.project.clone().map(|p| (p, active.worktree.clone()));
                if matches!(self.content, Content::Tree(_)) && self.tree_for != tree_for {
                    self.settle(Content::Loading {
                        since: Instant::now(),
                    });
                }
                match &active.dirs {
                    Scan::Ready(table) => {
                        let current = matches!(
                            &self.content,
                            Content::Tree(tree) if Arc::ptr_eq(tree.table(), table)
                        );
                        if !current {
                            if table.files().is_empty() {
                                self.settle(Content::Empty);
                            } else {
                                self.rebuild(table.clone());
                            }
                        }
                    }
                    // A new file list comes with its table pending for a
                    // moment: the old rows stay until the new table is there.
                    Scan::Pending => {
                        if !matches!(self.content, Content::Tree(_) | Content::Loading { .. }) {
                            self.settle(Content::Loading {
                                since: Instant::now(),
                            });
                        }
                    }
                    Scan::Unavailable(_) => self.settle(Content::Unavailable),
                }
            }
        }
        if self.want_reveal && !matches!(self.content, Content::Loading { .. }) {
            self.want_reveal = false;
            self.reveal_current(cx);
        }
        cx.notify();
    }

    /// The expanded directories of the tree about to be replaced, or the
    /// ones kept while there was none.
    /// A tree that is taken leaves `Empty` behind: the caller puts a
    /// content in.
    fn take_expanded(&mut self) -> HashSet<Box<str>> {
        match std::mem::replace(&mut self.content, Content::Empty) {
            Content::Tree(tree) => tree.into_expanded(),
            other => {
                self.content = other;
                std::mem::take(&mut self.kept)
            }
        }
    }

    /// The path of the selected row, or the one kept while there is no
    /// tree.
    fn selected_path(&self) -> Option<Box<str>> {
        match (&self.content, self.selected) {
            (Content::Tree(tree), Some(index)) => tree.path(index).map(Box::from),
            (Content::Tree(_), None) => None,
            _ => self.kept_selected.clone(),
        }
    }

    /// Puts the rows of `table` in, with the expanded directories and the
    /// selected row of the tree that was there kept by path.
    fn rebuild(&mut self, table: Arc<DirTable>) {
        let selected = self.selected_path();
        self.kept_selected = None;
        let tree = Tree::new(table, self.take_expanded());
        self.selected = selected.and_then(|path| tree.position(&path));
        self.tree_for = self.project.clone().map(|p| (p, self.worktree.clone()));
        self.content = Content::Tree(tree);
    }

    /// A state without rows: what the tree had is kept for the next one.
    fn settle(&mut self, content: Content) {
        if matches!(content, Content::Loading { .. })
            && matches!(self.content, Content::Loading { .. })
        {
            return;
        }
        if let Content::Tree(_) = self.content {
            self.kept_selected = self.selected_path();
            self.kept = self.take_expanded();
            self.selected = None;
        }
        self.content = content;
    }

    // ----- moving ---------------------------------------------------------

    fn tree(&self) -> Option<&Tree> {
        match &self.content {
            Content::Tree(tree) => Some(tree),
            _ => None,
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = Some(index);
        self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        cx.notify();
    }

    pub(crate) fn select_next(&mut self, cx: &mut Context<Self>) {
        let Some(len) = self.tree().map(Tree::len).filter(|len| *len > 0) else {
            return;
        };
        let next = self.selected.map_or(0, |index| (index + 1).min(len - 1));
        self.select(next, cx);
    }

    pub(crate) fn select_previous(&mut self, cx: &mut Context<Self>) {
        let Some(len) = self.tree().map(Tree::len).filter(|len| *len > 0) else {
            return;
        };
        let previous = self.selected.map_or(0, |index| index.saturating_sub(1));
        self.select(previous.min(len - 1), cx);
    }

    /// →: opens the directory; open already, selects its first row.
    pub(crate) fn expand(&mut self, cx: &mut Context<Self>) {
        let (Some(index), Content::Tree(tree)) = (self.selected, &mut self.content) else {
            return;
        };
        match tree.open_at(index) {
            Some(false) => {
                tree.expand(index);
                cx.notify();
            }
            Some(true) => {
                let child = index + 1;
                if tree.rows().get(child).is_some_and(|line| {
                    tree.rows()
                        .get(index)
                        .is_some_and(|parent| line.depth > parent.depth)
                }) {
                    self.select(child, cx);
                }
            }
            None => {}
        }
    }

    /// ←: closes the directory; closed already, or a file, selects the
    /// directory that holds the row.
    pub(crate) fn collapse(&mut self, cx: &mut Context<Self>) {
        let (Some(index), Content::Tree(tree)) = (self.selected, &mut self.content) else {
            return;
        };
        if tree.open_at(index) == Some(true) {
            tree.collapse(index);
            cx.notify();
        } else if let Some(parent) = tree.parent(index) {
            self.select(parent, cx);
        }
    }

    /// ↵: a file is shown in the reading view; a directory opens or closes.
    pub(crate) fn activate(&mut self, cx: &mut Context<Self>) {
        if let Some(index) = self.selected {
            self.activate_row(index, cx);
        }
    }

    /// A click on row `index`: the same as moving to it and ↵.
    pub(crate) fn click(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.tree().is_none_or(|tree| index >= tree.len()) {
            return;
        }
        self.selected = Some(index);
        self.activate_row(index, cx);
    }

    fn activate_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Content::Tree(tree) = &mut self.content else {
            return;
        };
        match tree.rows().get(index).map(|line| line.node) {
            Some(Node::Dir { .. }) => {
                if tree.open_at(index) == Some(true) {
                    tree.collapse(index);
                } else {
                    tree.expand(index);
                }
                self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
                cx.notify();
            }
            Some(Node::File(_)) => self.open(index, cx),
            None => {}
        }
    }

    /// Asks the reader to show the file of row `index`, at its start, as
    /// every other open does (`Reader::open`). The panel goes first: the
    /// reading view hides the sidebar, and `workspace`'s Esc closes a
    /// sidebar panel before it pops a screen, so a panel left open would
    /// take the first Esc of the reader without any visible change. The
    /// tree comes back as it was (expansion, selection, the file just
    /// opened) when it is shown again.
    fn open(&mut self, index: usize, cx: &mut Context<Self>) {
        let (Some(project), Some(path)) = (
            self.project.clone(),
            self.tree()
                .and_then(|tree| tree.path(index))
                .map(Arc::<str>::from),
        ) else {
            return;
        };
        let target = ReadTarget {
            project,
            worktree: self.worktree.clone(),
            path,
            origin: TextOrigin::Worktree,
            expect: None,
            at: Anchor::Start,
        };
        self.shared
            .dispatch(WorkspaceCommand::ShowSidebarPanel(None));
        self.shared.session.open(target);
        cx.notify();
    }

    /// The panel goes: back to the project sidebar (the back arrow).
    pub(crate) fn close(&mut self, _cx: &mut Context<Self>) {
        self.shared
            .dispatch(WorkspaceCommand::ShowSidebarPanel(None));
    }

    /// Opens the directories above the file the reader showed last and
    /// selects it. Nothing when it is not a file of this worktree's list.
    /// With no tree yet it waits for one.
    pub(crate) fn reveal_current(&mut self, cx: &mut Context<Self>) {
        if matches!(self.content, Content::Loading { .. }) {
            self.want_reveal = true;
            return;
        }
        let Some(target) = self.shared.session.last_target() else {
            return;
        };
        if matches!(target.origin, TextOrigin::Dependency(_))
            || Some(&target.project) != self.project.as_ref()
            || target.worktree != self.worktree
        {
            return;
        }
        let Content::Tree(tree) = &mut self.content else {
            return;
        };
        if let Some(index) = tree.reveal(&target.path) {
            self.selected = Some(index);
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
            cx.notify();
        }
    }
}

impl Drop for FilesView {
    /// The view goes with the sidebar: what it kept goes to `Shared`.
    fn drop(&mut self) {
        let selected = self.selected_path();
        let expanded = self.take_expanded();
        self.shared.keep_files_memory(FilesMemory {
            project: self.project.clone(),
            expanded,
            selected,
        });
        self.shared.detach_files(self.id);
    }
}
