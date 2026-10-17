//! `TestRepo`: builds repositories in a temporary directory with gix's
//! writing API and the standard library, never the git CLI
//! (docs/design/git.md「测试」). For the tests of this crate and of
//! `project`, `review` and `bench`. Every method panics on failure: it is
//! test code.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use crate::handle::ConfigScope;
use crate::{Discovered, GitError, HashKind, Mode, Oid, Repo};

/// [`Repo::discover`], reading only the repository's own configuration:
/// no system, XDG or `~/.gitconfig`, so the machine's configuration can't
/// change a test's result.
pub fn discover(path: &Path, ceilings: &[PathBuf]) -> Result<Option<Discovered>, GitError> {
    Repo::discover_scoped(path, ceilings, ConfigScope::RepositoryOnly)
}

/// What a worktree's HEAD should be.
#[derive(Clone, Copy, Debug)]
pub enum TestHead<'a> {
    /// Symbolic, to this full reference name (it need not exist: unborn).
    Branch(&'a str),
    Detached(Oid),
}

/// One index entry for [`TestRepo::write_index`].
#[derive(Clone, Debug)]
pub struct IndexEntry {
    /// Raw path bytes, written as given (invalid paths included).
    pub path: Vec<u8>,
    pub mode: Mode,
    pub oid: Oid,
    /// 0 unconflicted, 1 base, 2 ours, 3 theirs.
    pub stage: u8,
    pub skip_worktree: bool,
    /// The stat `git add` records; all zeros when None.
    pub stat: Option<IndexStat>,
    /// `git add -N`.
    pub intent_to_add: bool,
}

/// A file's `lstat` as an index entry records it.
#[derive(Clone, Copy, Debug)]
pub struct IndexStat(gix::index::entry::Stat);

impl IndexStat {
    /// The current `lstat` of `path` (not following a symbolic link).
    pub fn of(path: &Path) -> IndexStat {
        let meta = gix::index::fs::Metadata::from_path_no_follow(path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        IndexStat(gix::index::entry::Stat::from_fs(&meta).unwrap())
    }
}

impl IndexEntry {
    pub fn new(path: &str, mode: Mode, oid: Oid) -> IndexEntry {
        IndexEntry {
            path: path.as_bytes().to_vec(),
            mode,
            oid,
            stage: 0,
            skip_worktree: false,
            stat: None,
            intent_to_add: false,
        }
    }
}

/// How [`TestRepo::write_index_with`] writes the index.
#[derive(Clone, Debug, Default)]
pub struct IndexOptions<'a> {
    /// Sparse directory entries: `(directory without trailing /, tree)`.
    /// The index is then written with the `sdir` extension, as
    /// `index.sparse = true` makes git write it.
    pub sparse_dirs: &'a [(&'a str, Oid)],
    /// A null trailing checksum, as `index.skipHash = true` writes it.
    pub skip_hash: bool,
}

pub struct TestRepo {
    _dir: tempfile::TempDir,
    base: PathBuf,
    root: Option<PathBuf>,
    common_dir: PathBuf,
    hash: HashKind,
    clock: Cell<i64>,
}

impl TestRepo {
    /// A SHA-1 repository with a worktree at `<base>/repo`; HEAD is
    /// `refs/heads/main`, unborn.
    pub fn new() -> TestRepo {
        TestRepo::init(HashKind::Sha1, false)
    }

    /// The same with `extensions.objectFormat = sha256`.
    pub fn sha256() -> TestRepo {
        TestRepo::init(HashKind::Sha256, false)
    }

    /// A bare SHA-1 repository at `<base>/repo.git`.
    pub fn bare() -> TestRepo {
        TestRepo::init(HashKind::Sha1, true)
    }

    /// A repository laid out the way `git init --ref-format=reftable`
    /// (git 2.45 and later) leaves it: format version 1 with
    /// `extensions.refStorage = reftable`, HEAD pointing at
    /// `refs/heads/.invalid`, `refs/heads` a plain file, and
    /// `reftable/tables.list` naming one table. The table holds only a
    /// reftable header; nothing in this crate reads it.
    pub fn reftable() -> TestRepo {
        let repo = TestRepo::init(HashKind::Sha1, false);
        let git = repo.common_dir.clone();
        write(&git.join("HEAD"), b"ref: refs/heads/.invalid\n");
        std::fs::remove_dir_all(git.join("refs")).unwrap();
        std::fs::create_dir(git.join("refs")).unwrap();
        write(
            &git.join("refs/heads"),
            b"this repository uses the reftable format\n",
        );
        let table = "0x000000000001-0x000000000001-3b4e7f1a.ref";
        write(
            &git.join("reftable/tables.list"),
            format!("{table}\n").as_bytes(),
        );
        // "REFT", version 1, block size 4096, min and max update index 1.
        let mut header = b"REFT\x01\x00\x10\x00".to_vec();
        header.extend_from_slice(&1u64.to_be_bytes());
        header.extend_from_slice(&1u64.to_be_bytes());
        write(&git.join("reftable").join(table), &header);
        repo.append_config(
            "[core]\n\trepositoryformatversion = 1\n[extensions]\n\trefstorage = reftable\n",
        );
        repo
    }

    fn init(hash: HashKind, bare: bool) -> TestRepo {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let (path, kind) = if bare {
            (base.join("repo.git"), gix::create::Kind::Bare)
        } else {
            (base.join("repo"), gix::create::Kind::WithWorktree)
        };
        let options = gix::create::Options {
            object_hash: Some(hash.to_gix()),
            ..Default::default()
        };
        let repo = gix::ThreadSafeRepository::init_opts(
            &path,
            kind,
            options,
            gix::open::Options::isolated(),
        )
        .unwrap()
        .to_thread_local();
        let common_dir = std::fs::canonicalize(repo.git_dir()).unwrap();
        TestRepo {
            _dir: dir,
            base,
            root: (!bare).then_some(path),
            common_dir,
            hash,
            clock: Cell::new(1_700_000_000),
        }
    }

    /// The temporary directory holding the repository; a place for linked
    /// worktrees and other directories outside it.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// The main worktree root. Panics for a bare repository.
    pub fn root(&self) -> &Path {
        self.root
            .as_deref()
            .expect("a bare TestRepo has no worktree")
    }

    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    pub fn hash(&self) -> HashKind {
        self.hash
    }

    /// The repository, discovered from its common directory with
    /// [`discover`].
    pub fn repo(&self) -> Repo {
        discover(&self.common_dir, &[]).unwrap().unwrap().repo
    }

    fn gix(&self) -> gix::Repository {
        gix::open_opts(
            &self.common_dir,
            gix::open::Options::isolated().open_path_as_is(true),
        )
        .unwrap()
    }

    pub fn blob(&self, content: &[u8]) -> Oid {
        Oid::from_gix(&self.gix().write_blob(content).unwrap())
    }

    /// A tree of regular files; `/` in a path makes subtrees.
    pub fn tree(&self, files: &[(&str, &[u8])]) -> Oid {
        let entries: Vec<(&str, Mode, Oid)> = files
            .iter()
            .map(|(path, content)| (*path, Mode::File, self.blob(content)))
            .collect();
        self.tree_entries(&entries)
    }

    /// A tree of entries with any mode, pointing at existing objects.
    pub fn tree_entries(&self, entries: &[(&str, Mode, Oid)]) -> Oid {
        let repo = self.gix();
        let entries: Vec<TreeItem> = entries
            .iter()
            .map(|(p, m, o)| (p.as_bytes().to_vec(), *m, *o))
            .collect();
        write_tree(&repo, &entries)
    }

    /// A commit by "Test <test@example.com>"; each one is a minute later
    /// than the previous.
    pub fn commit(&self, tree: Oid, parents: &[Oid], message: &str) -> Oid {
        let seconds = self.clock.get() + 60;
        self.clock.set(seconds);
        self.commit_at(tree, parents, message, seconds)
    }

    pub fn commit_at(&self, tree: Oid, parents: &[Oid], message: &str, seconds: i64) -> Oid {
        let signature = gix::actor::Signature {
            name: "Test".into(),
            email: "test@example.com".into(),
            time: gix::date::Time {
                seconds,
                offset: 3600,
            },
        };
        let commit = gix::objs::Commit {
            tree: tree.to_gix(),
            parents: parents.iter().map(|p| p.to_gix()).collect(),
            author: signature.clone(),
            committer: signature,
            encoding: None,
            message: message.into(),
            extra_headers: Vec::new(),
        };
        Oid::from_gix(&self.gix().write_object(&commit).unwrap())
    }

    /// Writes the loose reference `name` (e.g. `refs/heads/main`).
    pub fn set_ref(&self, name: &str, target: Oid) {
        write(
            &self.common_dir.join(name),
            format!("{}\n", target.to_hex()).as_bytes(),
        );
    }

    pub fn set_symbolic_ref(&self, name: &str, target: &str) {
        write(
            &self.common_dir.join(name),
            format!("ref: {target}\n").as_bytes(),
        );
    }

    /// Writes `packed-refs` with these entries (sorted as git sorts them).
    pub fn set_packed_refs(&self, refs: &[(&str, Oid)]) {
        let mut refs = refs.to_vec();
        refs.sort_by(|a, b| a.0.cmp(b.0));
        let mut text = String::from("# pack-refs with: peeled fully-peeled sorted \n");
        for (name, oid) in refs {
            text.push_str(&format!("{} {name}\n", oid.to_hex()));
        }
        write(&self.common_dir.join("packed-refs"), text.as_bytes());
    }

    /// Sets the main worktree's (or a bare repository's) HEAD.
    pub fn set_head(&self, head: TestHead<'_>) {
        write_head(&self.common_dir, head);
    }

    /// Appends raw text to the common directory's `config`.
    pub fn append_config(&self, text: &str) {
        let path = self.common_dir.join("config");
        let mut config = std::fs::read(&path).unwrap_or_default();
        config.extend_from_slice(text.as_bytes());
        write(&path, &config);
    }

    /// Adds a linked worktree the way `git worktree add` lays it out:
    /// `<common>/worktrees/<name>/{HEAD,commondir,gitdir}` and a `.git` file
    /// in `root`, which is created. Returns the worktree's git directory.
    pub fn add_worktree(&self, name: &str, root: &Path, head: TestHead<'_>) -> PathBuf {
        std::fs::create_dir_all(root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let admin = self.common_dir.join("worktrees").join(name);
        std::fs::create_dir_all(&admin).unwrap();
        write_head(&admin, head);
        write(&admin.join("commondir"), b"../..\n");
        let mut gitdir = root.join(".git").into_os_string().into_encoded_bytes();
        gitdir.push(b'\n');
        write(&admin.join("gitdir"), &gitdir);
        let mut dot_git = b"gitdir: ".to_vec();
        dot_git.extend_from_slice(admin.as_os_str().as_encoded_bytes());
        dot_git.push(b'\n');
        write(&root.join(".git"), &dot_git);
        admin
    }

    /// Writes `<common>/worktrees/<name>/locked` with `reason`.
    pub fn lock_worktree(&self, name: &str, reason: &str) {
        write(
            &self.common_dir.join("worktrees").join(name).join("locked"),
            reason.as_bytes(),
        );
    }

    /// Writes the index of the worktree whose git directory is `git_dir`.
    pub fn write_index(&self, git_dir: &Path, entries: &[IndexEntry]) {
        self.write_index_with(git_dir, entries, &IndexOptions::default());
    }

    pub fn write_index_with(
        &self,
        git_dir: &Path,
        entries: &[IndexEntry],
        options: &IndexOptions<'_>,
    ) {
        use gix::index::entry::{Flags, Mode as M};

        let mut state = gix::index::State::new(self.hash.to_gix());
        for entry in entries {
            let mode = match entry.mode {
                Mode::File => M::FILE,
                Mode::Executable => M::FILE_EXECUTABLE,
                Mode::Symlink => M::SYMLINK,
                Mode::Gitlink => M::COMMIT,
                Mode::Special => panic!("the index can't hold Mode::Special"),
            };
            let mut flags = Flags::from_bits_retain(u32::from(entry.stage) << 12);
            if entry.skip_worktree {
                flags |= Flags::SKIP_WORKTREE | Flags::EXTENDED;
            }
            if entry.intent_to_add {
                flags |= Flags::INTENT_TO_ADD | Flags::EXTENDED;
            }
            let stat = entry.stat.map_or_else(Default::default, |s| s.0);
            state.dangerously_push_entry(
                stat,
                entry.oid.to_gix(),
                flags,
                mode,
                entry.path[..].into(),
            );
        }
        for (dir, tree) in options.sparse_dirs {
            let path = format!("{dir}/");
            let flags = Flags::SKIP_WORKTREE | Flags::EXTENDED;
            state.dangerously_push_entry(
                Default::default(),
                tree.to_gix(),
                flags,
                M::DIR,
                path.as_str().into(),
            );
        }
        state.sort_entries();
        let path = git_dir.join("index");
        let write_options = gix::index::write::Options {
            skip_hash: options.skip_hash,
            ..Default::default()
        };
        gix::index::File::from_state(state, &path)
            .write(write_options)
            .unwrap();
        if !options.sparse_dirs.is_empty() {
            // Read back, the state is sparse now; writing it again adds `sdir`.
            let mut file =
                gix::index::File::at(&path, self.hash.to_gix(), true, Default::default()).unwrap();
            assert!(file.is_sparse());
            file.write(write_options).unwrap();
        }
    }

    /// Writes a file under the main worktree, creating directories.
    pub fn write_file(&self, rel: &str, content: &[u8]) {
        write(&self.root().join(rel), content);
    }

    /// The index entry `git add` would record for the main worktree's file
    /// or symbolic link at `rel`, without conversions: its blob (written to
    /// the object database), mode and stat.
    pub fn add(&self, rel: &str) -> IndexEntry {
        let path = self.root().join(rel);
        let meta = std::fs::symlink_metadata(&path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let (mode, content) = if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&path).unwrap();
            (Mode::Symlink, target.into_os_string().into_encoded_bytes())
        } else {
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o100 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            let mode = if executable {
                Mode::Executable
            } else {
                Mode::File
            };
            (mode, std::fs::read(&path).unwrap())
        };
        IndexEntry {
            stat: Some(IndexStat::of(&path)),
            ..IndexEntry::new(rel, mode, self.blob(&content))
        }
    }

    /// Sets the modification time of the main worktree's file at `rel`.
    pub fn set_mtime(&self, rel: &str, time: std::time::SystemTime) {
        let file = std::fs::File::options()
            .write(true)
            .open(self.root().join(rel))
            .unwrap();
        file.set_modified(time).unwrap();
    }

    /// Moves the file's modification time an hour back, so that an index
    /// written afterwards doesn't treat its entry as racy.
    pub fn backdate(&self, rel: &str) {
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        self.set_mtime(rel, hour_ago);
    }

    /// Removes a file, symbolic link or directory under the main worktree.
    pub fn remove(&self, rel: &str) {
        let path = self.root().join(rel);
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            std::fs::remove_dir_all(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }

    /// Writes the `files/` of a worktree fixture (`tests/fixtures/README.md`)
    /// into the main worktree. Returns their paths, sorted.
    pub fn import_worktree_fixture(&self, dir: &Path) -> Vec<String> {
        fn visit(base: &Path, dir: &Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(base, &path, out);
                } else {
                    let rel = path.strip_prefix(base).unwrap();
                    out.push(rel.to_str().unwrap().replace('\\', "/"));
                }
            }
        }
        let files = dir.join("files");
        let mut paths = Vec::new();
        visit(&files, &files, &mut paths);
        paths.sort();
        let mut out = Vec::with_capacity(paths.len());
        for rel in &paths {
            // `.git*` files are stored as `dot-git*`, where they don't apply
            // to the fixture itself in this repository.
            let (dir, base) = rel
                .rsplit_once('/')
                .map_or(("", rel.as_str()), |(d, b)| (d, b));
            let base = base
                .strip_prefix("dot-git")
                .map_or_else(|| base.to_owned(), |rest| format!(".git{rest}"));
            let name = if dir.is_empty() {
                base
            } else {
                format!("{dir}/{base}")
            };
            self.write_file(&name, &std::fs::read(files.join(rel)).unwrap());
            out.push(name);
        }
        out.sort();
        out
    }

    /// Imports a directory fixture made by `tests/fixtures/scripts/*.sh`
    /// (format in `tests/fixtures/README.md`): writes its blobs, rebuilds
    /// both trees from the recorded `git ls-tree` listings and checks that
    /// every OID is the one git recorded. Returns the old and new tree.
    pub fn import_fixture(&self, dir: &Path) -> (Oid, Oid) {
        let text = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .unwrap_or_else(|err| panic!("{}: {err}", dir.join(name).display()))
        };
        let oid =
            |hex: &str| Oid::from_hex(self.hash, hex).unwrap_or_else(|| panic!("bad OID {hex:?}"));
        let trees = text("trees");
        let recorded = |side: &str| {
            trees
                .lines()
                .find_map(|l| l.strip_prefix(side)?.strip_prefix(' '))
                .map(oid)
                .unwrap_or_else(|| panic!("no {side} tree in {}", dir.display()))
        };
        let build = |listing: &str| -> Oid {
            let mut entries: Vec<(String, Mode, Oid)> = Vec::new();
            for line in text(listing).lines() {
                let (meta, path) = line.split_once('\t').expect("<mode> <type> <oid>\\t<path>");
                let fields: Vec<&str> = meta.split(' ').collect();
                let [mode, kind, hex] = fields[..] else {
                    panic!("bad ls-tree line {line:?}");
                };
                let mode = match mode {
                    "100644" => Mode::File,
                    "100755" => Mode::Executable,
                    "120000" => Mode::Symlink,
                    "160000" => Mode::Gitlink,
                    other => panic!("mode {other} in {line:?}"),
                };
                let id = oid(hex);
                if kind == "blob" {
                    let content = std::fs::read(dir.join("blobs").join(hex)).unwrap();
                    assert_eq!(self.blob(&content), id, "blob {hex}");
                }
                entries.push((path.to_owned(), mode, id));
            }
            let entries: Vec<(&str, Mode, Oid)> = entries
                .iter()
                .map(|(p, m, o)| (p.as_str(), *m, *o))
                .collect();
            self.tree_entries(&entries)
        };
        let (old, new) = (build("old.tree"), build("new.tree"));
        assert_eq!(old, recorded("old"), "old tree of {}", dir.display());
        assert_eq!(new, recorded("new"), "new tree of {}", dir.display());
        (old, new)
    }

    #[cfg(unix)]
    pub fn symlink(&self, rel: &str, target: &str) {
        let path = self.root().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::os::unix::fs::symlink(target, path).unwrap();
    }
}

impl Default for TestRepo {
    fn default() -> Self {
        TestRepo::new()
    }
}

fn write(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn write_head(git_dir: &Path, head: TestHead<'_>) {
    let text = match head {
        TestHead::Branch(name) => format!("ref: {name}\n"),
        TestHead::Detached(oid) => format!("{}\n", oid.to_hex()),
    };
    write(&git_dir.join("HEAD"), text.as_bytes());
}

/// A tree entry to write: path relative to the tree, mode, object.
type TreeItem = (Vec<u8>, Mode, Oid);

fn write_tree(repo: &gix::Repository, entries: &[TreeItem]) -> Oid {
    use gix::objs::tree::{Entry, EntryKind};
    use std::collections::BTreeMap;

    let mut files = Vec::new();
    let mut dirs: BTreeMap<Vec<u8>, Vec<TreeItem>> = BTreeMap::new();
    for (path, mode, oid) in entries {
        match path.iter().position(|&b| b == b'/') {
            Some(slash) => dirs.entry(path[..slash].to_vec()).or_default().push((
                path[slash + 1..].to_vec(),
                *mode,
                *oid,
            )),
            None => files.push((path.clone(), *mode, *oid)),
        }
    }
    let mut tree = gix::objs::Tree::empty();
    for (name, mode, oid) in files {
        let kind = match mode {
            Mode::File => EntryKind::Blob,
            Mode::Executable => EntryKind::BlobExecutable,
            Mode::Symlink => EntryKind::Link,
            Mode::Gitlink => EntryKind::Commit,
            Mode::Special => panic!("a tree can't hold Mode::Special"),
        };
        tree.entries.push(Entry {
            mode: kind.into(),
            filename: name.into(),
            oid: oid.to_gix(),
        });
    }
    for (name, children) in dirs {
        let oid = write_tree(repo, &children);
        tree.entries.push(Entry {
            mode: EntryKind::Tree.into(),
            filename: name.into(),
            oid: oid.to_gix(),
        });
    }
    tree.entries.sort();
    Oid::from_gix(&repo.write_object(&tree).unwrap())
}
