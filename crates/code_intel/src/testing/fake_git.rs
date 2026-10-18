//! `FakeGit`: git metadata in memory, worktree files as real files in a
//! temporary directory. The index, HEAD and the object database are maps;
//! the worktree comparison reads the files from disk and compares them with
//! the index and HEAD as `git::Worktree::changes(Base::Head)` would. Every
//! call is recorded.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};

use lectern_fs_watch::WorktreeSource;
use lectern_fs_watch::testing::FakeSource;
use lectern_git::{GitError, HashKind};
use lectern_services::project::{
    ActiveTree, BlobId, FileKind, FileList, Head, Package, ProjectKey, ProjectSnapshot, RelPath,
    Scan, Worktree, WorktreeKey,
};

use crate::git_access::{ChangedPaths, GitAccess, Tree, hash_blob};

type Hook = Box<dyn FnOnce() + Send>;

struct FakeTree {
    name: String,
    root: PathBuf,
    git_dir: PathBuf,
    index: BTreeMap<String, BlobId>,
    head: Option<BlobId>,
    head_tree: BTreeMap<String, BlobId>,
    source: Arc<FakeSource>,
    prunable: bool,
}

#[derive(Default)]
struct State {
    trees: Vec<FakeTree>,
    blobs: HashMap<BlobId, Arc<[u8]>>,
    ignored: Vec<String>,
    filtered: HashSet<String>,
    eol: HashSet<String>,
    untracked_cap: Option<usize>,
    commits: u64,
    calls: Vec<String>,
    before_compare: Vec<Hook>,
    fail_compare: bool,
    unchanged: bool,
}

/// An in-memory git over real worktree files.
pub struct FakeGit {
    _dir: tempfile::TempDir,
    common_dir: PathBuf,
    hash: HashKind,
    state: Mutex<State>,
}

impl Default for FakeGit {
    fn default() -> Self {
        FakeGit::new()
    }
}

impl FakeGit {
    /// A SHA-1 repository with its main worktree, no files.
    pub fn new() -> FakeGit {
        FakeGit::with_hash(HashKind::Sha1)
    }

    pub fn with_hash(hash: HashKind) -> FakeGit {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = dir.path().canonicalize().expect("canonical temp dir");
        let root = base.join("repo");
        let common_dir = root.join(".git");
        std::fs::create_dir_all(&common_dir).expect("create the git dir");
        let state = State {
            trees: vec![FakeTree {
                name: "main".into(),
                root,
                git_dir: common_dir.clone(),
                index: BTreeMap::new(),
                head: None,
                head_tree: BTreeMap::new(),
                source: FakeSource::new(),
                prunable: false,
            }],
            ..State::default()
        };
        FakeGit {
            _dir: dir,
            common_dir,
            hash,
            state: Mutex::new(state),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn hash(&self) -> HashKind {
        self.hash
    }

    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    pub fn key(&self) -> ProjectKey {
        ProjectKey::Git {
            common_dir: Arc::from(self.common_dir.as_path()),
        }
    }

    /// A worktree's root ("main" or a linked worktree's name).
    pub fn root(&self, wt: &str) -> PathBuf {
        self.with_tree(wt, |t| t.root.clone())
    }

    pub fn worktree_key(&self, wt: &str) -> WorktreeKey {
        WorktreeKey(Arc::from(
            self.with_tree(wt, |t| t.git_dir.clone()).as_path(),
        ))
    }

    fn with_tree<R>(&self, wt: &str, f: impl FnOnce(&mut FakeTree) -> R) -> R {
        let mut state = self.lock();
        let tree = state
            .trees
            .iter_mut()
            .find(|t| t.name == wt)
            .unwrap_or_else(|| panic!("no worktree {wt}"));
        f(tree)
    }

    /// Adds a linked worktree (its root and git directory are created).
    pub fn add_worktree(&self, name: &str) -> PathBuf {
        let base = self
            .common_dir
            .parent()
            .and_then(Path::parent)
            .expect("base");
        let root = base.join(format!("wt-{name}"));
        let git_dir = self.common_dir.join("worktrees").join(name);
        std::fs::create_dir_all(&root).expect("create the worktree");
        std::fs::create_dir_all(&git_dir).expect("create its git dir");
        self.lock().trees.push(FakeTree {
            name: name.into(),
            root: root.clone(),
            git_dir,
            index: BTreeMap::new(),
            head: None,
            head_tree: BTreeMap::new(),
            source: FakeSource::new(),
            prunable: false,
        });
        root
    }

    pub fn set_prunable(&self, wt: &str, prunable: bool) {
        self.with_tree(wt, |t| t.prunable = prunable);
    }

    /// Writes a worktree file.
    pub fn write(&self, wt: &str, path: &str, content: &[u8]) {
        let file = self.root(wt).join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("create the parent");
        }
        std::fs::write(&file, content).expect("write the file");
    }

    /// Removes a worktree file.
    pub fn delete(&self, wt: &str, path: &str) {
        let _ = std::fs::remove_file(self.root(wt).join(path));
    }

    /// Writes and stages files.
    pub fn add(&self, wt: &str, files: &[(&str, &[u8])]) {
        for (path, content) in files {
            self.write(wt, path, content);
            self.stage(wt, path);
        }
    }

    /// The OID of content in this repository's hash.
    pub fn oid(&self, content: &[u8]) -> BlobId {
        hash_blob(self.hash, content)
    }

    /// Stages a path as it is on disk (removed from the index when gone).
    pub fn stage(&self, wt: &str, path: &str) {
        let file = self.root(wt).join(path);
        match std::fs::read(&file) {
            Ok(bytes) => {
                let canonical = self.canonical(path, &bytes);
                let oid = self.oid(&canonical);
                let mut state = self.lock();
                state.blobs.insert(oid, canonical.into());
                let tree = state
                    .trees
                    .iter_mut()
                    .find(|t| t.name == wt)
                    .expect("worktree");
                tree.index.insert(path.into(), oid);
                tree.source.track(&[path]);
            }
            Err(_) => self.with_tree(wt, |t| {
                t.index.remove(path);
                t.source.untrack(&[path]);
            }),
        }
    }

    /// Sets the index entry of a path to `content` without touching the
    /// worktree file.
    pub fn stage_content(&self, wt: &str, path: &str, content: &[u8]) {
        let oid = self.oid(content);
        let mut state = self.lock();
        state.blobs.insert(oid, content.into());
        let tree = state
            .trees
            .iter_mut()
            .find(|t| t.name == wt)
            .expect("worktree");
        tree.index.insert(path.into(), oid);
        tree.source.track(&[path]);
    }

    /// Sets the index entries of `entries` to their OIDs with no content
    /// behind them: no blob, no worktree file (the benchmarks' virtual
    /// repositories, with [`FakeGit::assume_unchanged`]).
    pub fn stage_oids(&self, wt: &str, entries: impl IntoIterator<Item = (String, BlobId)>) {
        let mut state = self.lock();
        let tree = state
            .trees
            .iter_mut()
            .find(|t| t.name == wt)
            .expect("worktree");
        let mut paths = Vec::new();
        for (path, oid) in entries {
            tree.index.insert(path.clone(), oid);
            paths.push(path);
        }
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        tree.source.track(&refs);
    }

    /// The worktree comparison lists nothing: the worktree is taken to
    /// equal the index (virtual repositories whose files do not exist).
    pub fn assume_unchanged(&self, unchanged: bool) {
        self.lock().unchanged = unchanged;
    }

    /// Commits the index: HEAD becomes a new commit with the index's tree.
    pub fn commit(&self, wt: &str) -> BlobId {
        let mut state = self.lock();
        state.commits += 1;
        let n = state.commits;
        let commit = hash_blob(self.hash, format!("commit {n}").as_bytes());
        let tree = state
            .trees
            .iter_mut()
            .find(|t| t.name == wt)
            .expect("worktree");
        tree.head = Some(commit);
        tree.head_tree = tree.index.clone();
        commit
    }

    /// Moves HEAD to a new commit with the same tree (an amend that changes
    /// nothing, a checkout of an equal branch).
    pub fn move_head(&self, wt: &str) {
        let mut state = self.lock();
        state.commits += 1;
        let n = state.commits;
        let commit = hash_blob(self.hash, format!("commit {n}").as_bytes());
        let tree = state
            .trees
            .iter_mut()
            .find(|t| t.name == wt)
            .expect("worktree");
        tree.head = Some(commit);
    }

    /// Ignore patterns for untracked files: a path, a `dir/` prefix or
    /// `*.ext`.
    pub fn ignore(&self, pattern: &str) {
        self.lock().ignored.push(pattern.into());
        for t in &self.lock().trees {
            t.source.ignore(&[pattern]);
        }
    }

    /// A clean filter applies to the path (Git LFS): no canonical OID.
    pub fn filter(&self, path: &str) {
        self.lock().filtered.insert(path.into());
    }

    /// The path is checked out with CRLF: its canonical form has LF.
    pub fn eol(&self, path: &str) {
        self.lock().eol.insert(path.into());
    }

    /// At most `n` untracked files are listed.
    pub fn cap_untracked(&self, n: Option<usize>) {
        self.lock().untracked_cap = n;
    }

    /// The next worktree comparisons fail.
    pub fn fail_compare(&self, fail: bool) {
        self.lock().fail_compare = fail;
    }

    /// Runs `hook` at the start of the next worktree comparison.
    pub fn before_compare(&self, hook: impl FnOnce() + Send + 'static) {
        self.lock().before_compare.push(Box::new(hook));
    }

    /// The names of the calls so far.
    pub fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }

    pub fn count(&self, call: &str) -> usize {
        self.lock().calls.iter().filter(|c| *c == call).count()
    }

    /// The worktree's index as a file list with OIDs.
    pub fn file_list(&self, wt: &str) -> Arc<FileList> {
        self.with_tree(wt, |t| {
            let mut b = FileList::builder();
            for (path, oid) in &t.index {
                b.push_with_oid(path, FileKind::File, *oid);
            }
            Arc::new(b.build())
        })
    }

    /// A snapshot of the project with `active` as the current worktree, its
    /// file list from the index.
    pub fn snapshot(
        &self,
        generation: u64,
        active: &str,
        packages: Vec<Package>,
    ) -> ProjectSnapshot {
        let files = self.file_list(active);
        let state = self.lock();
        let worktrees: Vec<Worktree> = state
            .trees
            .iter()
            .map(|t| Worktree {
                key: WorktreeKey(Arc::from(t.git_dir.as_path())),
                root: Arc::from(t.root.as_path()),
                is_main: t.name == "main",
                head: Head::Unknown,
                locked: false,
                prunable: t.prunable,
            })
            .collect();
        let tree = state
            .trees
            .iter()
            .find(|t| t.name == active)
            .expect("worktree");
        ProjectSnapshot {
            key: self.key(),
            generation,
            name: "repo".into(),
            worktrees: worktrees.into(),
            active: Some(ActiveTree {
                root: Arc::from(tree.root.as_path()),
                worktree: Some(WorktreeKey(Arc::from(tree.git_dir.as_path()))),
                opened_at: "".into(),
                files: Scan::Ready(files),
                packages: Scan::Ready(packages.into()),
            }),
        }
    }

    fn canonical(&self, path: &str, bytes: &[u8]) -> Vec<u8> {
        if self.lock().eol.contains(path) {
            let mut out = Vec::with_capacity(bytes.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            out
        } else {
            bytes.to_vec()
        }
    }

    fn record(&self, call: &str) {
        self.lock().calls.push(call.into());
    }

    fn tree_of(&self, tree: &Tree) -> Result<TreeCopy, GitError> {
        let state = self.lock();
        let t = state
            .trees
            .iter()
            .find(|t| *t.git_dir == *tree.git_dir)
            .ok_or_else(|| GitError::Corrupt(format!("no worktree {}", tree.git_dir.display())))?;
        Ok(TreeCopy {
            root: t.root.clone(),
            index: t.index.clone(),
            head_tree: t.head_tree.clone(),
            head: t.head,
        })
    }

    fn ignored(&self, path: &str) -> bool {
        let state = self.lock();
        state.ignored.iter().any(|p| {
            if let Some(ext) = p.strip_prefix("*.") {
                path.ends_with(&format!(".{ext}"))
            } else if let Some(dir) = p.strip_suffix('/') {
                path.starts_with(&format!("{dir}/"))
            } else {
                path == p
            }
        })
    }
}

/// One worktree's state, copied out of the lock.
struct TreeCopy {
    root: PathBuf,
    index: BTreeMap<String, BlobId>,
    head_tree: BTreeMap<String, BlobId>,
    head: Option<BlobId>,
}

/// Every file under `root`, relative, skipping `.git`. A directory holding
/// a `.git` (a nested repository) is one entry, its own path, as git lists
/// it.
fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let nested = kind.is_dir() && path.join(".git").exists();
        if kind.is_dir() && !nested {
            walk(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root)
            && let Some(rel) = rel.to_str()
        {
            out.insert(rel.replace('\\', "/"));
        }
    }
}

impl GitAccess for FakeGit {
    fn changed_paths(&self, tree: &Tree, _cancel: &AtomicBool) -> Result<ChangedPaths, GitError> {
        self.record("changed_paths");
        let hooks = std::mem::take(&mut self.lock().before_compare);
        for hook in hooks {
            hook();
        }
        if self.lock().fail_compare {
            return Err(GitError::Corrupt("comparison failed".into()));
        }
        if self.lock().unchanged {
            return Ok(ChangedPaths::default());
        }
        let TreeCopy {
            root,
            index,
            head_tree,
            ..
        } = self.tree_of(tree)?;
        let mut disk = BTreeSet::new();
        walk(&root, &root, &mut disk);
        let cap = self.lock().untracked_cap;
        let mut universe: BTreeSet<String> = index.keys().cloned().collect();
        universe.extend(head_tree.keys().cloned());
        let mut untracked = 0;
        let mut capped = false;
        for path in &disk {
            if index.contains_key(path) || head_tree.contains_key(path) {
                continue;
            }
            if self.ignored(path) {
                continue;
            }
            if cap.is_some_and(|c| untracked >= c) {
                capped = true;
                continue;
            }
            untracked += 1;
            universe.insert(path.clone());
        }
        let filtered = self.lock().filtered.clone();
        let mut out = ChangedPaths {
            untracked_capped: capped,
            ..ChangedPaths::default()
        };
        for path in universe {
            let worktree = match std::fs::read(root.join(&path)) {
                Ok(bytes) if filtered.contains(&path) && index.contains_key(&path) => {
                    // A filter's pointer never equals the index: reported.
                    Some(hash_blob(
                        self.hash,
                        &[bytes.as_slice(), b"\0filtered"].concat(),
                    ))
                }
                Ok(bytes) => Some(self.oid(&self.canonical(&path, &bytes))),
                Err(_) => None,
            };
            let i = index.get(&path).copied();
            let h = head_tree.get(&path).copied();
            // An untracked entry is listed whether or not it reads as a file
            // (a nested repository is a directory).
            let untracked = i.is_none() && h.is_none() && disk.contains(&path);
            if worktree != h || i != h || untracked {
                let rel: RelPath = path.as_str().into();
                out.head.push((rel.clone(), h));
                out.paths.push(rel);
            }
        }
        Ok(out)
    }

    fn tracked(
        &self,
        tree: &Tree,
        _cancel: &AtomicBool,
    ) -> Result<Vec<(RelPath, BlobId)>, GitError> {
        self.record("tracked");
        let index = self.tree_of(tree)?.index;
        Ok(index
            .into_iter()
            .map(|(p, o)| (p.as_str().into(), o))
            .collect())
    }

    fn blob(&self, _tree: &Tree, oid: BlobId, max: u64) -> Result<Arc<[u8]>, GitError> {
        self.record("blob");
        let state = self.lock();
        match state.blobs.get(&oid) {
            Some(b) if b.len() as u64 > max => Err(GitError::TooLarge {
                size: b.len() as u64,
            }),
            Some(b) => Ok(b.clone()),
            None => Err(GitError::ObjectMissing(crate::git_access::git_oid(oid))),
        }
    }

    fn head(&self, tree: &Tree) -> Result<Option<BlobId>, GitError> {
        self.record("head");
        Ok(self.tree_of(tree)?.head)
    }

    fn index_checksum(&self, tree: &Tree) -> Result<Option<BlobId>, GitError> {
        self.record("index_checksum");
        let index = self.tree_of(tree)?.index;
        let mut text = String::new();
        for (path, oid) in &index {
            text.push_str(path);
            text.push(' ');
            text.push_str(&oid.to_string());
            text.push('\n');
        }
        Ok(Some(hash_blob(self.hash, text.as_bytes())))
    }

    fn canonical_oid(
        &self,
        _tree: &Tree,
        path: &str,
        raw: &[u8],
    ) -> Result<Option<BlobId>, GitError> {
        self.record("canonical_oid");
        if self.lock().filtered.contains(path) {
            return Ok(None);
        }
        Ok(Some(self.oid(&self.canonical(path, raw))))
    }

    fn watch_source(&self, tree: &Tree) -> Arc<dyn WorktreeSource> {
        self.record("watch_source");
        let state = self.lock();
        match state.trees.iter().find(|t| *t.git_dir == *tree.git_dir) {
            Some(t) => t.source.clone(),
            None => FakeSource::new(),
        }
    }
}
