//! The dependency roots of one session's worktree (「依赖根的读取限制」,
//! ADR 0033, 0040): the one place dependency files are opened.
//!
//! - A root is a `node_modules` on a sub-project's Node lookup chain,
//!   opened from the worktree root's `SafeRoot` with `open_dir` (no level
//!   may be a symbolic link), registered by its directory relative to the
//!   worktree root with the device and inode it had. At most
//!   [`MAX_ROOTS`] per worktree.
//! - A package directory is a directory in a root, or a symbolic link
//!   there whose target, resolved lexically against the link's directory,
//!   lies inside some root registered here (ADR 0040), followed at most
//!   [`MAX_LINKS`] times; the last level must be a directory reached from
//!   that root without any symbolic link.
//! - A file ([`DepFile`]) is built only here: a path inside its package
//!   directory (lexically), with an extension the ecosystem allows
//!   ([`super::allowed`]). Every read goes through [`Roots::read`] on the
//!   file's root's `SafeRoot`, which follows no symbolic link either.
//! - A root whose directory was replaced (`npm ci`) is opened again when
//!   [`Roots::revalidate`] sees another device or inode, and on a read
//!   that finds nothing (`NotFound`: revalidated, read once more).

use std::sync::{Arc, Mutex};

use lectern_platform::{FileKind, Meta, Read, RelPath as SafeRel, SafeError, SafeRoot};
use lectern_services::code_intel::Ecosystem;
use lectern_services::project::RelPath;

use super::npm::join;

/// Roots opened at most per worktree.
pub(crate) const MAX_ROOTS: usize = 8;

/// Symbolic links followed at most to a package directory.
pub(crate) const MAX_LINKS: usize = 4;

/// One registered root.
struct Root {
    /// Relative to the worktree root, e.g. `node_modules` or
    /// `packages/app/node_modules`.
    dir: RelPath,
    safe: Arc<SafeRoot>,
    dev: u64,
    ino: u64,
}

/// The dependency roots of one session (its worktree). `Send + Sync`.
pub(crate) struct Roots {
    worktree: Arc<SafeRoot>,
    roots: Mutex<Vec<Root>>,
    /// Every path this opened (`open`), stat'ed (`stat`, `lookup`, `link`)
    /// or read (`read`), relative to the worktree root (tests: nothing
    /// outside the roots, nothing read when an identity holds).
    #[cfg(test)]
    pub(crate) log: Mutex<Vec<(&'static str, String)>>,
}

/// A package directory found in a root.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct PkgDir {
    /// The root it is in (its directory relative to the worktree root).
    pub root: RelPath,
    /// Relative to the root: `react`, `.pnpm/react@19.0.0/node_modules/react`.
    rel: String,
    /// Where the lookup found it, relative to the worktree root (the link's
    /// path for pnpm): the key of npm's lockfile.
    pub lookup: String,
}

impl PkgDir {
    /// The real directory relative to the worktree root.
    pub(crate) fn path(&self) -> String {
        format!("{}/{}", self.root, self.rel)
    }
}

/// A file inside a dependency root, inside its package directory, with an
/// allowed extension: built only by [`Roots`].
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct DepFile {
    root: RelPath,
    /// Relative to the root.
    rel: Box<str>,
}

impl DepFile {
    /// The file's path relative to the worktree root (`Location::path`).
    pub(crate) fn path(&self) -> String {
        format!("{}/{}", self.root, self.rel)
    }
}

/// Why [`Roots::read`] gave no bytes.
#[derive(Debug)]
pub(crate) enum DepRead {
    /// Larger than the limit.
    TooLarge,
    /// Not a regular file there (gone, a link, a directory), or its root
    /// is gone.
    Gone,
    Failed(String),
}

impl Roots {
    pub(crate) fn new(worktree: Arc<SafeRoot>) -> Roots {
        Roots {
            worktree,
            roots: Mutex::new(Vec::new()),
            #[cfg(test)]
            log: Mutex::new(Vec::new()),
        }
    }

    fn note(&self, _op: &'static str, _path: &str) {
        #[cfg(test)]
        super::lock(&self.log).push((_op, _path.to_owned()));
    }

    /// The roots of a sub-project's lookup chain (`dir` relative to the
    /// worktree root, "" for the root), nearest first: the `node_modules`
    /// of `dir` and of each directory above it, opened when not yet (up to
    /// [`MAX_ROOTS`] per worktree). A level whose `node_modules` is missing,
    /// is a link or lies under one is skipped.
    pub(crate) fn chain(&self, dir: &str) -> Vec<RelPath> {
        let mut out = Vec::new();
        let mut at = Some(dir.trim_matches('/').to_owned());
        while let Some(d) = at {
            let candidate: RelPath = if d.is_empty() {
                Arc::from("node_modules")
            } else {
                Arc::from(format!("{d}/node_modules"))
            };
            // `node_modules/x/node_modules` is a package's own, not a
            // sub-project's: the chain starts above it.
            if !d.split('/').any(|p| p == "node_modules") && self.open(&candidate) {
                out.push(candidate);
            }
            at = if d.is_empty() {
                None
            } else {
                Some(d.rfind('/').map_or(String::new(), |i| d[..i].to_owned()))
            };
        }
        out
    }

    /// Opens `dir` as a root unless registered; whether it is one now.
    fn open(&self, dir: &RelPath) -> bool {
        if super::lock(&self.roots).iter().any(|r| r.dir == *dir) {
            return true;
        }
        if super::lock(&self.roots).len() >= MAX_ROOTS {
            return false;
        }
        let Some(root) = self.open_root(dir) else {
            return false;
        };
        let mut roots = super::lock(&self.roots);
        if roots.iter().any(|r| r.dir == *dir) {
            return true;
        }
        if roots.len() >= MAX_ROOTS {
            return false;
        }
        roots.push(root);
        true
    }

    /// `open_dir` with the directory's device and inode: a `stat` before and
    /// after the open must agree, so the descriptor is the directory
    /// recorded.
    fn open_root(&self, dir: &RelPath) -> Option<Root> {
        self.note("open", dir);
        let rel = SafeRel::new(dir.as_bytes())?;
        let before = self.worktree.stat(rel).ok()?;
        if before.kind != FileKind::Dir {
            return None;
        }
        let safe = self.worktree.open_dir(rel).ok()?;
        let after = self.worktree.stat(rel).ok()?;
        if (before.dev, before.ino) != (after.dev, after.ino) || after.kind != FileKind::Dir {
            return None;
        }
        Some(Root {
            dir: dir.clone(),
            safe: Arc::new(safe),
            dev: after.dev,
            ino: after.ino,
        })
    }

    /// Checks every root against its directory now: another device or inode
    /// (deleted and installed again) opens it again; gone, it is dropped.
    pub(crate) fn revalidate(&self) {
        let dirs: Vec<(RelPath, u64, u64)> = super::lock(&self.roots)
            .iter()
            .map(|r| (r.dir.clone(), r.dev, r.ino))
            .collect();
        for (dir, dev, ino) in dirs {
            self.note("revalidate", &dir);
            let now = SafeRel::new(dir.as_bytes()).and_then(|rel| self.worktree.stat(rel).ok());
            let same = now
                .as_ref()
                .is_some_and(|m| m.kind == FileKind::Dir && (m.dev, m.ino) == (dev, ino));
            if same {
                continue;
            }
            let reopened = now.and_then(|_| self.open_root(&dir));
            let mut roots = super::lock(&self.roots);
            let Some(at) = roots.iter().position(|r| r.dir == dir) else {
                continue;
            };
            match reopened {
                Some(root) => roots[at] = root,
                None => {
                    roots.remove(at);
                }
            }
        }
    }

    fn safe(&self, dir: &str) -> Option<Arc<SafeRoot>> {
        super::lock(&self.roots)
            .iter()
            .find(|r| &*r.dir == dir)
            .map(|r| r.safe.clone())
    }

    /// The registered root containing `path` (relative to the worktree
    /// root): its directory and the rest of the path; None when no root has
    /// it, or it is a root itself.
    fn root_of(&self, path: &str) -> Option<(RelPath, String)> {
        let roots = super::lock(&self.roots);
        // The deepest: a root may lie inside another's tree only by being
        // listed under it (`a/node_modules/b/node_modules` is never a root),
        // but take the longest match anyway.
        roots
            .iter()
            .filter_map(|r| {
                let rest = path.strip_prefix(&*r.dir)?.strip_prefix('/')?;
                (!rest.is_empty()).then(|| (r.dir.clone(), rest.to_owned()))
            })
            .max_by_key(|(d, _)| d.len())
    }

    /// The package `name` from `chain` (nearest root first): the first root
    /// with a directory or a link (resolved inside the roots) by that name.
    pub(crate) fn find_package(&self, chain: &[RelPath], name: &str) -> Option<PkgDir> {
        for dir in chain {
            let Some(safe) = self.safe(dir) else {
                continue;
            };
            let lookup = format!("{dir}/{name}");
            self.note("lookup", &lookup);
            let Some(rel) = SafeRel::new(name.as_bytes()) else {
                continue;
            };
            match safe.stat(rel) {
                Ok(m) if m.kind == FileKind::Dir => {
                    return Some(PkgDir {
                        root: dir.clone(),
                        rel: name.to_owned(),
                        lookup,
                    });
                }
                Ok(m) if m.kind == FileKind::Symlink => {
                    if let Some(found) = self.follow(&safe, dir, name, lookup, 0) {
                        return Some(found);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Resolves the link `rel` in the root `dir` (its `SafeRoot` `safe`):
    /// its target, joined lexically with the link's directory relative to
    /// the worktree root, must lie inside a registered root (ADR 0040); a
    /// directory reached there without a link is the package, another link
    /// is followed, up to [`MAX_LINKS`] in all.
    fn follow(
        &self,
        safe: &SafeRoot,
        dir: &str,
        rel: &str,
        lookup: String,
        depth: usize,
    ) -> Option<PkgDir> {
        if depth >= MAX_LINKS {
            return None;
        }
        let target = match safe.read(SafeRel::new(rel.as_bytes())?, 0).ok()? {
            Read::Symlink { target, .. } => String::from_utf8(target).ok()?,
            _ => return None,
        };
        let link = format!("{dir}/{rel}");
        let parent = link.rfind('/').map_or("", |i| &link[..i]);
        let resolved = join(parent, &target)?;
        let (root, rest) = self.root_of(&resolved)?;
        let next = self.safe(&root)?;
        self.note("link", &resolved);
        match next.stat(SafeRel::new(rest.as_bytes())?) {
            Ok(m) if m.kind == FileKind::Dir => Some(PkgDir {
                root,
                rel: rest,
                lookup,
            }),
            Ok(m) if m.kind == FileKind::Symlink => {
                self.follow(&next, &root, &rest, lookup, depth + 1)
            }
            _ => None,
        }
    }

    /// The file `path` (relative to `pkg`'s directory) of the package: None
    /// when it leaves the package directory or its extension is not one
    /// `eco` reads.
    pub(crate) fn file(&self, pkg: &PkgDir, path: &str, eco: Ecosystem) -> Option<DepFile> {
        let inside = join("", path).filter(|p| !p.is_empty())?;
        if !super::allowed(eco, &inside) {
            return None;
        }
        Some(DepFile {
            root: pkg.root.clone(),
            rel: format!("{}/{inside}", pkg.rel).into(),
        })
    }

    /// The file at `path` relative to the worktree root, as stored or as a
    /// view's `TextRef` gives it: inside a registered root (`root`, when
    /// given, must be that root), with an allowed extension, no `.` or `..`
    /// component. None otherwise: nothing outside the roots is ever read.
    pub(crate) fn file_at(
        &self,
        path: &str,
        root: Option<&str>,
        eco: Ecosystem,
    ) -> Option<DepFile> {
        SafeRel::new(path.as_bytes())?;
        if !super::allowed(eco, path) {
            return None;
        }
        let (dir, rest) = self.root_of(path)?;
        if root.is_some_and(|r| r != &*dir) {
            return None;
        }
        Some(DepFile {
            root: dir,
            rel: rest.into(),
        })
    }

    /// `lstat` of a file through its root (a missing file is the usual
    /// answer for a specifier's candidates: no revalidation).
    pub(crate) fn stat(&self, file: &DepFile) -> Result<Meta, SafeError> {
        let safe = self.safe(&file.root).ok_or(SafeError::NotFound)?;
        self.note("stat", &file.path());
        let rel = SafeRel::new(file.rel.as_bytes()).ok_or(SafeError::NotFound)?;
        safe.stat(rel)
    }

    /// A regular file's bytes (at most `max`) through its root, with the
    /// `fstat` of the read; a `NotFound` (the root deleted and installed
    /// again) revalidates the roots and reads once more.
    pub(crate) fn read(&self, file: &DepFile, max: u64) -> Result<(Vec<u8>, Meta), DepRead> {
        let once = || {
            let safe = self.safe(&file.root).ok_or(None)?;
            self.note("read", &file.path());
            let rel = SafeRel::new(file.rel.as_bytes()).ok_or(Some(DepRead::Gone))?;
            match safe.read(rel, max) {
                Ok(Read::File { bytes, meta }) => Ok((bytes, meta)),
                Ok(Read::TooLarge { .. }) => Err(Some(DepRead::TooLarge)),
                Ok(Read::Symlink { .. } | Read::Unsupported { .. }) => Err(Some(DepRead::Gone)),
                Err(SafeError::NotFound) => Err(None),
                Err(error) => Err(Some(DepRead::Failed(error.to_string()))),
            }
        };
        match once() {
            Ok(read) => Ok(read),
            Err(Some(why)) => Err(why),
            Err(None) => {
                self.revalidate();
                once().map_err(|why| why.unwrap_or(DepRead::Gone))
            }
        }
    }

    /// The registered roots' directories (tests).
    #[cfg(test)]
    pub(crate) fn dirs(&self) -> Vec<String> {
        super::lock(&self.roots)
            .iter()
            .map(|r| r.dir.to_string())
            .collect()
    }
}
