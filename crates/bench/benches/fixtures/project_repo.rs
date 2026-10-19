//! Repositories for `project/*` (docs/design/project.md「性能」), shared by
//! the `time` and `memory` targets: a `lectern_project::testing::Shape`
//! written with `lectern_git::testing::TestRepo`. The index lists every
//! path of the shape with its blob OID; only the manifests are written to
//! the worktrees, as the project model reads nothing else of a git
//! worktree (its file list comes from the index). Every linked worktree has
//! the main worktree's index and manifests (`<base>/wtNN`), as worktrees of
//! one repository mostly do.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

use lectern_bench::BenchError;
use lectern_git::testing::{IndexEntry, TestHead, TestRepo};
use lectern_git::{Mode, Oid};
use lectern_project::testing::Shape;

pub struct ProjectRepo {
    pub t: TestRepo,
    pub shape: Shape,
    /// The main worktree's index, and the same index with one source
    /// file's blob OID changed (`git add` of an edited file).
    index: [PathBuf; 2],
    flipped: Cell<bool>,
}

/// `shape` with `worktrees` worktrees, the main one included.
pub fn build(shape: Shape, worktrees: usize) -> Result<ProjectRepo, BenchError> {
    let t = TestRepo::new();
    let hash = t.hash();
    let commit = t.commit(t.tree(&[("README.md", b"# repo\n")]), &[], "base");
    t.set_ref("refs/heads/main", commit);
    let mut entries: Vec<IndexEntry> = Vec::with_capacity(shape.paths.len());
    let mut manifests = shape.manifests.iter().peekable();
    for path in &shape.paths {
        let oid = match manifests.peek() {
            Some((p, content)) if p == path => {
                manifests.next();
                Oid::for_blob(hash, content)
            }
            // A stand-in for the content: distinct per path, never read.
            _ => Oid::for_blob(hash, path.as_bytes()),
        };
        entries.push(IndexEntry::new(path, Mode::File, oid));
    }
    if manifests.next().is_some() {
        return Err(BenchError::new("a manifest is not among the paths"));
    }
    write_manifests(t.root(), &shape)?;
    t.write_index(t.common_dir(), &entries);

    // The second index: the first file that is not a manifest, edited.
    let edited = entries
        .iter_mut()
        .find(|e| !shape.manifests.iter().any(|(p, _)| p.as_bytes() == e.path))
        .ok_or_else(|| BenchError::new("no source file"))?;
    edited.oid = Oid::for_blob(hash, b"edited\n");
    let other = t.base().join("index-b");
    fs::create_dir_all(&other)?;
    t.write_index(&other, &entries);
    let original = t.base().join("index-a");
    fs::create_dir_all(&original)?;
    fs::copy(t.common_dir().join("index"), original.join("index"))?;

    for i in 0..worktrees.saturating_sub(1) {
        let name = format!("wt{i:02}");
        let branch = format!("refs/heads/{name}");
        t.set_ref(&branch, commit);
        let root = t.base().join(&name);
        let admin = t.add_worktree(&name, &root, TestHead::Branch(&branch));
        write_manifests(&root, &shape)?;
        fs::copy(original.join("index"), admin.join("index"))?;
    }
    Ok(ProjectRepo {
        t,
        shape,
        index: [original.join("index"), other.join("index")],
        flipped: Cell::new(false),
    })
}

fn write_manifests(root: &Path, shape: &Shape) -> Result<(), BenchError> {
    for (path, content) in &shape.manifests {
        let full = root.join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(full, content)?;
    }
    Ok(())
}

impl ProjectRepo {
    pub fn root(&self) -> &Path {
        self.t.root()
    }

    pub fn common_dir(&self) -> &Path {
        self.t.common_dir()
    }

    /// Replaces the main worktree's index with the other of its two
    /// versions: the checksum and one entry's blob OID change, the paths
    /// do not.
    pub fn flip_index(&self) -> std::io::Result<()> {
        let next = !self.flipped.get();
        self.flipped.set(next);
        fs::copy(
            &self.index[usize::from(next)],
            self.common_dir().join("index"),
        )
        .map(drop)
    }
}
