//! `FakeGit`: git metadata in memory over real directories (docs/design/
//! project.md「测试」). Worktree roots and their files are real, in a
//! temporary directory, so paths canonicalize and `lstat` works; the
//! repositories, worktree lists, HEADs and indexes are in memory, and every
//! call is recorded. Never runs the git CLI. Every method panics on
//! failure: it is test code.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lectern_git::{GitError, HashKind, Oid};
use lectern_services::project::{FileKind, FileList, Head};

use crate::git_access::{Discovered, GitAccess, IndexFiles, TreeAccess, WorktreeInfo};

/// One call `FakeGit` received.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Call {
    Discover(PathBuf),
    Worktrees(PathBuf),
    OpenWorktree(PathBuf),
    IndexFiles(PathBuf),
    IndexChecksum(PathBuf),
    Read { git_dir: PathBuf, rel: String },
}

pub struct FakeGit {
    _dir: tempfile::TempDir,
    base: PathBuf,
    repos: Mutex<Vec<Repo>>,
    calls: Arc<Mutex<Vec<Call>>>,
}

struct Repo {
    common_dir: PathBuf,
    bare: bool,
    hash: HashKind,
    unreadable: Option<String>,
    trees: Vec<Tree>,
}

#[derive(Clone)]
struct Tree {
    git_dir: PathBuf,
    root: PathBuf,
    is_main: bool,
    head: Head,
    locked: bool,
    index: BTreeMap<String, FileKind>,
    index_error: Option<String>,
}

impl FakeGit {
    pub fn new() -> FakeGit {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        FakeGit {
            _dir: dir,
            base,
            repos: Mutex::new(Vec::new()),
            calls: Arc::default(),
        }
    }

    /// The temporary directory everything lives in (canonical).
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// A SHA-1 repository with its main worktree at `<base>/<root>` on
    /// `refs/heads/main`. Returns the common directory, `<root>/.git`.
    pub fn init(&self, root: &str) -> PathBuf {
        self.init_with(root, HashKind::Sha1)
    }

    pub fn init_with(&self, root: &str, hash: HashKind) -> PathBuf {
        let root = self.base.join(root);
        let common_dir = root.join(".git");
        std::fs::create_dir_all(&common_dir).unwrap();
        self.lock().push(Repo {
            common_dir: common_dir.clone(),
            bare: false,
            hash,
            unreadable: None,
            trees: vec![Tree {
                git_dir: common_dir.clone(),
                root,
                is_main: true,
                head: Head::Branch("refs/heads/main".into()),
                locked: false,
                index: BTreeMap::new(),
                index_error: None,
            }],
        });
        common_dir
    }

    /// A bare repository at `<base>/<dir>`; returns it (the common
    /// directory).
    pub fn init_bare(&self, dir: &str) -> PathBuf {
        let common_dir = self.base.join(dir);
        std::fs::create_dir_all(&common_dir).unwrap();
        self.lock().push(Repo {
            common_dir: common_dir.clone(),
            bare: true,
            hash: HashKind::Sha1,
            unreadable: None,
            trees: Vec::new(),
        });
        common_dir
    }

    /// A linked worktree `<common>/worktrees/<name>` at `<base>/<root>` on
    /// `refs/heads/<name>`. Returns its git directory.
    pub fn add_worktree(&self, common_dir: &Path, name: &str, root: &str) -> PathBuf {
        let root = self.base.join(root);
        let git_dir = common_dir.join("worktrees").join(name);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&git_dir).unwrap();
        let mut repos = self.lock();
        let repo = repos
            .iter_mut()
            .find(|r| r.common_dir == common_dir)
            .expect("no such repository");
        repo.trees.push(Tree {
            git_dir: git_dir.clone(),
            root,
            is_main: false,
            head: Head::Branch(format!("refs/heads/{name}").into()),
            locked: false,
            index: BTreeMap::new(),
            index_error: None,
        });
        git_dir
    }

    /// The root of the worktree whose git directory is `git_dir`.
    pub fn root(&self, git_dir: &Path) -> PathBuf {
        self.with_tree(git_dir, |t| t.root.clone())
    }

    pub fn set_head(&self, git_dir: &Path, head: Head) {
        self.with_tree(git_dir, |t| t.head = head);
    }

    pub fn lock_worktree(&self, git_dir: &Path) {
        self.with_tree(git_dir, |t| t.locked = true);
    }

    /// Deletes the worktree root: the worktree becomes prunable.
    pub fn delete_root(&self, git_dir: &Path) {
        std::fs::remove_dir_all(self.root(git_dir)).unwrap();
    }

    /// Writes a file into the worktree and stages it as a regular file.
    pub fn write(&self, git_dir: &Path, rel: &str, content: &[u8]) {
        self.write_unstaged(git_dir, rel, content);
        self.track(git_dir, rel, FileKind::File);
    }

    /// Adds an index entry without touching the worktree.
    pub fn track(&self, git_dir: &Path, rel: &str, kind: FileKind) {
        self.with_tree(git_dir, |t| {
            t.index.insert(rel.to_owned(), kind);
        });
    }

    pub fn untrack(&self, git_dir: &Path, rel: &str) {
        self.with_tree(git_dir, |t| {
            t.index.remove(rel);
        });
    }

    /// Changes a worktree file without staging it.
    pub fn write_unstaged(&self, git_dir: &Path, rel: &str, content: &[u8]) {
        let path = self.root(git_dir).join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// Deletes a worktree file, leaving the index as it is.
    pub fn remove_unstaged(&self, git_dir: &Path, rel: &str) {
        std::fs::remove_file(self.root(git_dir).join(rel)).unwrap();
    }

    /// Reading this worktree's index fails with `reason`.
    pub fn fail_index(&self, git_dir: &Path, reason: &str) {
        self.with_tree(git_dir, |t| t.index_error = Some(reason.to_owned()));
    }

    /// Discovering this repository fails with `reason`.
    pub fn fail_repo(&self, common_dir: &Path, reason: &str) {
        let mut repos = self.lock();
        let repo = repos
            .iter_mut()
            .find(|r| r.common_dir == common_dir)
            .expect("no such repository");
        repo.unreadable = Some(reason.to_owned());
    }

    pub fn calls(&self) -> Vec<Call> {
        lock(&self.calls).clone()
    }

    pub fn take_calls(&self) -> Vec<Call> {
        std::mem::take(&mut *lock(&self.calls))
    }

    /// The worktree files read since the last `take_calls`.
    pub fn reads(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Read { rel, .. } => Some(rel),
                _ => None,
            })
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Repo>> {
        lock(&self.repos)
    }

    fn record(&self, call: Call) {
        lock(&self.calls).push(call);
    }

    fn with_tree<T>(&self, git_dir: &Path, f: impl FnOnce(&mut Tree) -> T) -> T {
        let mut repos = self.lock();
        let tree = repos
            .iter_mut()
            .flat_map(|r| r.trees.iter_mut())
            .find(|t| t.git_dir == git_dir)
            .expect("no such worktree");
        f(tree)
    }
}

impl Default for FakeGit {
    fn default() -> Self {
        FakeGit::new()
    }
}

impl GitAccess for FakeGit {
    fn discover(&self, path: &Path) -> Result<Option<Discovered>, GitError> {
        self.record(Call::Discover(path.to_path_buf()));
        let repos = self.lock();
        // (depth, repository, worktree root, git directory)
        let mut best: Option<(usize, &Repo, Option<PathBuf>, PathBuf)> = None;
        let mut consider = |dir: &Path, repo, root: Option<PathBuf>, git_dir: &Path| {
            if !path.starts_with(dir) {
                return;
            }
            let depth = dir.components().count();
            if best.as_ref().is_none_or(|(d, ..)| depth > *d) {
                best = Some((depth, repo, root, git_dir.to_path_buf()));
            }
        };
        for repo in repos.iter() {
            consider(&repo.common_dir, repo, None, &repo.common_dir);
            for tree in &repo.trees {
                if tree.root.exists() {
                    consider(&tree.root, repo, Some(tree.root.clone()), &tree.git_dir);
                }
                consider(&tree.git_dir, repo, None, &tree.git_dir);
            }
        }
        let Some((_, repo, worktree_root, git_dir)) = best else {
            return Ok(None);
        };
        if let Some(reason) = &repo.unreadable {
            return Err(GitError::Corrupt(reason.clone()));
        }
        Ok(Some(Discovered {
            worktree_root,
            git_dir,
            common_dir: repo.common_dir.clone(),
            bare: repo.bare,
            hash: repo.hash,
        }))
    }

    fn worktrees(&self, common_dir: &Path) -> Result<Vec<WorktreeInfo>, GitError> {
        self.record(Call::Worktrees(common_dir.to_path_buf()));
        let repos = self.lock();
        let repo = repos
            .iter()
            .find(|r| r.common_dir == common_dir)
            .ok_or_else(|| not_found(common_dir))?;
        Ok(repo
            .trees
            .iter()
            .map(|t| WorktreeInfo {
                root: t.root.clone(),
                git_dir: t.git_dir.clone(),
                is_main: t.is_main,
                head: t.head.clone(),
                locked: t.locked,
                prunable: !t.root.exists(),
            })
            .collect())
    }

    fn open_worktree(
        &self,
        common_dir: &Path,
        git_dir: &Path,
    ) -> Result<Box<dyn TreeAccess>, GitError> {
        self.record(Call::OpenWorktree(git_dir.to_path_buf()));
        let repos = self.lock();
        let repo = repos
            .iter()
            .find(|r| r.common_dir == common_dir)
            .ok_or_else(|| not_found(common_dir))?;
        let tree = repo
            .trees
            .iter()
            .find(|t| t.git_dir == git_dir)
            .ok_or_else(|| not_found(git_dir))?;
        if !tree.root.exists() {
            return Err(not_found(&tree.root));
        }
        Ok(Box::new(FakeTree {
            tree: tree.clone(),
            hash: repo.hash,
            calls: self.calls.clone(),
        }))
    }

    fn hash_blob(&self, kind: HashKind, bytes: &[u8]) -> Oid {
        Oid::for_blob(kind, bytes)
    }
}

/// One opened worktree: the index as it was when opened.
struct FakeTree {
    tree: Tree,
    hash: HashKind,
    calls: Arc<Mutex<Vec<Call>>>,
}

impl FakeTree {
    fn record(&self, call: Call) {
        lock(&self.calls).push(call);
    }

    fn checksum(&self) -> Oid {
        let mut text = String::new();
        for (path, kind) in &self.tree.index {
            text.push_str(&format!("{kind:?} {path}\n"));
        }
        Oid::for_blob(self.hash, text.as_bytes())
    }
}

impl TreeAccess for FakeTree {
    fn index_files(&self, _cancel: &AtomicBool) -> Result<IndexFiles, GitError> {
        self.record(Call::IndexFiles(self.tree.git_dir.clone()));
        if let Some(reason) = &self.tree.index_error {
            return Err(GitError::Corrupt(reason.clone()));
        }
        let mut files = FileList::builder();
        for (path, kind) in &self.tree.index {
            files.push(path, *kind);
        }
        Ok(IndexFiles {
            checksum: Some(self.checksum()),
            files: files.build(),
        })
    }

    fn index_checksum(&self) -> Result<Option<Oid>, GitError> {
        self.record(Call::IndexChecksum(self.tree.git_dir.clone()));
        if let Some(reason) = &self.tree.index_error {
            return Err(GitError::Corrupt(reason.clone()));
        }
        Ok(Some(self.checksum()))
    }

    fn read_file(&self, rel: &str, limit: u64) -> Result<Option<Vec<u8>>, GitError> {
        self.record(Call::Read {
            git_dir: self.tree.git_dir.clone(),
            rel: rel.to_owned(),
        });
        let path = self.tree.root.join(rel);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(GitError::Io(err)),
        };
        if !meta.is_file() {
            return Err(GitError::Corrupt(format!("{rel} is not a regular file")));
        }
        if meta.len() > limit {
            return Err(GitError::TooLarge { size: meta.len() });
        }
        Ok(Some(std::fs::read(path)?))
    }
}

fn not_found(path: &Path) -> GitError {
    GitError::Io(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} not found", path.display()),
    ))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
