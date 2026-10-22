//! A worktree document's fs_watch subscription (docs/design/file_reader.md
//! 「数据流与线程」「状态与异常」): the repository and the document's
//! worktree, sharing the worktree state with `project` and `code_intel`.
//! A batch naming the document's path (or a directory above it), or a
//! rescan, makes a `Background` task compare the file's identity
//! (`SafeRoot::stat`): a change reopens the document at the line at its
//! top, a deleted file shows as missing, and a file that comes back is
//! opened again. Folder projects are not watched. While an edit session
//! lives, the session checks the file instead (`edit`).

use std::path::Path;
use std::sync::{Arc, Weak};

use lectern_executor::Priority;
use lectern_fs_watch::{
    Coalesce, Health, RepoBatch, RepoSpec, RepoWatch, Sink, WorktreeBatch, WorktreeName,
    WorktreeSpec, WorktreeWatch,
};
use lectern_kernel::BusyGuard;
use lectern_platform::{RelPath, SafeError};
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::ProjectKey;

use crate::git_access::Tree;
use crate::util::log;

use super::content::Content;
use super::engine::{Doc, EngineInner};
use super::{DocState, load};

/// The handles; dropping them unsubscribes.
pub(crate) struct Watches {
    _worktree: WorktreeWatch,
    _repo: RepoWatch,
}

/// The fs_watch name of the worktree whose git directory is `git_dir`.
fn worktree_name(common_dir: &Path, git_dir: &Path) -> Option<WorktreeName> {
    if git_dir == common_dir {
        return Some(WorktreeName::Main);
    }
    let name = git_dir.file_name()?.to_str()?;
    Some(WorktreeName::Linked(name.into()))
}

/// Subscribes a worktree document of a git project, once. The source is
/// opened in a `Background` task (it reads the worktree's git directory).
pub(crate) fn ensure(engine: &Arc<EngineInner>, doc: &Arc<Doc>) {
    let target = &doc.target;
    let (ProjectKey::Git { common_dir }, Some(worktree), TextOrigin::Worktree) =
        (&target.project, &target.worktree, &target.origin)
    else {
        return;
    };
    {
        let mut st = doc.lock();
        if st.watch_asked || doc.is_closed() {
            return;
        }
        st.watch_asked = true;
    }
    let tree = Tree {
        common_dir: common_dir.clone(),
        git_dir: worktree.0.clone(),
        root: doc.root.clone(),
    };
    let (engine2, doc2) = (engine.clone(), doc.clone());
    let _ = engine.spawn(Priority::Background, async move {
        subscribe(&engine2, &doc2, &tree);
    });
}

fn subscribe(engine: &Arc<EngineInner>, doc: &Arc<Doc>, tree: &Tree) {
    let Some(name) = worktree_name(&tree.common_dir, &tree.git_dir) else {
        return;
    };
    let source = match engine.git.watch_source(tree) {
        Ok(source) => source,
        Err(e) => {
            log(format_args!("no watch for {}: {e}", doc.target.path));
            return;
        }
    };
    let repo = engine.watcher.watch_repo(
        RepoSpec {
            common_dir: tree.common_dir.to_path_buf(),
            coalesce: Coalesce::REPO,
        },
        Arc::new(RepoSink {
            engine: Arc::downgrade(engine),
        }),
    );
    let worktree = repo.watch_worktree(
        WorktreeSpec {
            name,
            root: tree.root.to_path_buf(),
            source,
            coalesce: Coalesce::WORKTREE,
        },
        Arc::new(DocSink {
            engine: Arc::downgrade(engine),
            doc: Arc::downgrade(doc),
        }),
    );
    let watches = Watches {
        _worktree: worktree,
        _repo: repo,
    };
    let mut st = doc.lock();
    if doc.is_closed() {
        drop(st);
        drop(watches);
        return;
    }
    st.watch = Some(watches);
}

/// The repository's batches are not needed: only the worktree's paths.
struct RepoSink {
    engine: Weak<EngineInner>,
}

impl Sink<RepoBatch> for RepoSink {
    fn deliver(&self, _batch: RepoBatch, _busy: Option<BusyGuard>) {}

    fn busy(&self) -> Option<BusyGuard> {
        self.engine.upgrade()?.busy()
    }
}

struct DocSink {
    engine: Weak<EngineInner>,
    doc: Weak<Doc>,
}

impl Sink<WorktreeBatch> for DocSink {
    fn deliver(&self, batch: WorktreeBatch, busy: Option<BusyGuard>) {
        let (Some(engine), Some(doc)) = (self.engine.upgrade(), self.doc.upgrade()) else {
            return;
        };
        if doc.is_closed() {
            return;
        }
        if let Some(Health::Failed(_)) = batch.health {
            let watches = doc.lock().watch.take();
            let _ = engine.spawn(Priority::Background, async move { drop(watches) });
            return;
        }
        let path = doc.target.path.as_bytes();
        let touched = batch.rescan
            || batch.paths.iter().any(|p| {
                let p = p.as_bytes();
                path.starts_with(p) && (path.len() == p.len() || path[p.len()] == b'/')
            });
        if !touched {
            return;
        }
        let (engine2, doc2) = (engine.clone(), doc.clone());
        let _ = engine.spawn_with(Priority::Background, busy, async move {
            check(&engine2, &doc2);
        });
    }

    fn busy(&self) -> Option<BusyGuard> {
        self.engine.upgrade()?.busy()
    }
}

/// Compares the file's identity with the one read. While an edit session
/// lives, the session checks the file instead: the document is not reloaded
/// under it (docs/design/file_reader.md「编辑」).
pub(crate) fn check(engine: &Arc<EngineInner>, doc: &Arc<Doc>) {
    if doc.is_closed() {
        return;
    }
    if let Some(session) = doc.editing() {
        session.disk_event();
        return;
    }
    let target = &doc.target;
    let Some(rel) = RelPath::new(target.path.as_bytes()) else {
        return;
    };
    let stat = match engine.root(&target.project, &doc.root) {
        Ok(safe) => safe.stat(rel),
        Err(e) => {
            log(format_args!("watch check of {}: {e}", target.path));
            return;
        }
    };
    let mut st = doc.lock();
    match stat {
        Ok(meta) => {
            if st.state == DocState::Ready && st.identity == Some(meta.identity()) {
                return;
            }
            drop(st);
            let at = doc.current_line();
            load::reload(engine, doc, at);
        }
        Err(SafeError::NotFound) => {
            if st.state == DocState::Missing {
                return;
            }
            st.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            st.load += 1;
            st.state = DocState::Missing;
            st.content = Content::Nothing;
            st.file = None;
            st.pages = None;
            st.whole = None;
            st.windows.clear();
            st.identity = None;
            st.size = 0;
            st.find
                .cancel
                .store(true, std::sync::atomic::Ordering::SeqCst);
            st.find.generation += 1;
            st.find.results = None;
            doc.commit(st);
        }
        Err(_) => {
            drop(st);
            let at = doc.current_line();
            load::reload(engine, doc, at);
        }
    }
}
