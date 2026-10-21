//! The benchmarks' virtual repository written once and opened by many rigs
//! in turn ([`VirtualTemplate`]), kept on disk between runs when asked
//! ([`VirtualTemplate::cached`]).

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_cache::{Budgets, Cache, Config};
use lectern_git::HashKind;

use super::{FakeGit, synthetic, virtual_repo_sharing};
use crate::extract::LanguageTable;
use crate::store::{NAMESPACE, SCHEMA, Store, Version};

/// Bumped when what a template holds, or how it is laid out on disk,
/// changes in a way [`key`] does not see.
const TEMPLATE_VERSION: u32 = 1;

/// Written last into a kept template: its key, files and units.
const MANIFEST: &str = "manifest";

/// A [`virtual_repo_sharing`] of the main worktree written once and opened
/// by many rigs in turn (the benchmarks' shared fixture, ADR 0036). After it
/// is written nothing opens its cache again: each [`open`](Self::open)
/// copies the files and stages the entries in a new `FakeGit`, so what a
/// rig writes (units, access times, evictions) stays in its own copy.
pub struct VirtualTemplate {
    /// `git`: where each opened repository lives (its common directory names
    /// the cache partition, so every copy must see the same one); `cache`:
    /// the written cache, only ever copied.
    dir: PathBuf,
    /// Removes `dir` when the template is not kept.
    _temp: Option<tempfile::TempDir>,
    n: usize,
    units: usize,
}

impl VirtualTemplate {
    /// Writes the units of `n` files (for each `(name, every)` of `common`,
    /// every `every`th file also defining `name`) into a cache with
    /// `budgets` in a temporary directory, removed with the template.
    pub fn build(
        n: usize,
        common: &[(&str, usize)],
        budgets: Budgets,
    ) -> io::Result<VirtualTemplate> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().canonicalize()?;
        let units = write(&dir, n, common, budgets)?;
        Ok(VirtualTemplate {
            dir,
            _temp: Some(temp),
            n,
            units,
        })
    }

    /// The same template kept under `root` between runs: in `root/<key>`,
    /// where the key ([`key`]) covers everything its content depends on
    /// (this module's version, the files and common names, the budgets, the
    /// namespace and its tables, the current version of every core language,
    /// a sample of the synthetic generator's output and the directory, which
    /// names the cache partition). A kept one is used when its manifest
    /// (written last) has the key and a copy of it opens with every unit the
    /// manifest records (so a changed cache format, which `lectern-cache`
    /// would drop on open, is caught); otherwise the templates of this size
    /// under `root` are removed (those of other sizes, such as smoke mode's,
    /// stay) and the template written again. One benchmark run at a time
    /// per `root`.
    pub fn cached(
        root: &Path,
        n: usize,
        common: &[(&str, usize)],
        budgets: Budgets,
    ) -> io::Result<VirtualTemplate> {
        std::fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        let key = key(&root, n, common, &budgets);
        let dir = root.join(&key);
        if let Some(units) = manifest_units(&dir, &key) {
            // A repository left by a run that stopped while a copy was open.
            let _ = std::fs::remove_dir_all(dir.join("git"));
            let kept = VirtualTemplate {
                dir: dir.clone(),
                _temp: None,
                n,
                units,
            };
            if kept.verify(&root.join(format!("{key}.verify")), &budgets)? {
                return Ok(kept);
            }
        }
        // Every template of this size but the current key goes (stale ones,
        // a verification left behind); other sizes (smoke mode's) stay.
        let size = format!("{n}-");
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            if !entry.file_name().to_string_lossy().starts_with(&size) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
        }
        std::fs::create_dir(&dir)?;
        let units = write(&dir, n, common, budgets)?;
        std::fs::write(
            dir.join(MANIFEST),
            format!("key={key}\nfiles={n}\nunits={units}\n"),
        )?;
        Ok(VirtualTemplate {
            dir,
            _temp: None,
            n,
            units,
        })
    }

    /// The number of files.
    pub fn files(&self) -> usize {
        self.n
    }

    /// The number of units stored.
    pub fn units(&self) -> usize {
        self.units
    }

    /// A repository of its own with the entries staged in the main worktree
    /// (the comparison taken as clean), and a cache with `budgets` rooted at
    /// `root` (which must not exist) over a copy of the written one. Fails
    /// while the repository of an earlier `open` is still alive.
    pub fn open(&self, root: &Path, budgets: Budgets) -> io::Result<(Arc<FakeGit>, Cache)> {
        let git = FakeGit::at(&self.dir.join("git"))?;
        git.assume_unchanged(true);
        git.stage_oids(
            "main",
            (0..self.n).map(|i| (synthetic::path(i), synthetic::oid(i))),
        );
        copy_tree(&self.dir.join("cache"), root)?;
        let cache = Cache::new(Config {
            root: root.to_path_buf(),
            budgets,
            ui_thread: None,
        });
        Ok((Arc::new(git), cache))
    }

    /// Whether a copy opened at `at` (removed after) holds the template's
    /// units in the current versions.
    fn verify(&self, at: &Path, budgets: &Budgets) -> io::Result<bool> {
        let _ = std::fs::remove_dir_all(at);
        let found = {
            let (git, cache) = self.open(at, budgets.clone())?;
            stored_units(&git, &cache)
        };
        std::fs::remove_dir_all(at)?;
        Ok(found == Some(self.units))
    }
}

/// Writes a template into `dir` (`git`, `cache`) and returns the units its
/// store holds once written (after any eviction).
fn write(dir: &Path, n: usize, common: &[(&str, usize)], budgets: Budgets) -> io::Result<usize> {
    let git = FakeGit::at(&dir.join("git"))?;
    let cache = Cache::new(Config {
        root: dir.join("cache"),
        budgets,
        ui_thread: None,
    });
    virtual_repo_sharing(&git, &cache, n, &["main"], common);
    let units = stored_units(&git, &cache)
        .ok_or_else(|| io::Error::other("the template's store cannot be read"))?;
    // Closes the partition before anything copies it.
    drop(cache);
    drop(git);
    Ok(units)
}

/// The `Done` units of the current versions in `git`'s partition of `cache`.
fn stored_units(git: &FakeGit, cache: &Cache) -> Option<usize> {
    let versions: Vec<Version> = LanguageTable::core().iter().map(Version::of).collect();
    Store::new(&cache.repo(git.common_dir()))
        .units(&versions)
        .map(|u| u.len())
}

/// The units a kept template's manifest records, if it has `key`.
fn manifest_units(dir: &Path, key: &str) -> Option<usize> {
    let text = std::fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let mut lines = text.lines();
    (lines.next()? == format!("key={key}")).then_some(())?;
    lines.next()?.strip_prefix("files=")?;
    lines.next()?.strip_prefix("units=")?.parse().ok()
}

/// The key of a template kept under `root`, its directory's name: the file
/// count, then a hash of everything its content depends on (see
/// [`VirtualTemplate::cached`]).
pub fn key(root: &Path, n: usize, common: &[(&str, usize)], budgets: &Budgets) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "template {TEMPLATE_VERSION}");
    let _ = writeln!(text, "root {}", root.display());
    let _ = writeln!(text, "files {n} common {common:?} budgets {budgets:?}");
    let _ = writeln!(text, "namespace {} {}", NAMESPACE.name, NAMESPACE.version);
    for table in SCHEMA {
        let _ = writeln!(text, "{table}");
    }
    for version in LanguageTable::core().iter().map(Version::of) {
        let _ = writeln!(text, "{version:?}");
    }
    // The generator: its output for the first files and a spread of others.
    for i in (0..64).chain((0..64).map(|k| k * 1567 + 64)) {
        let _ = writeln!(
            text,
            "{} {:?} {:?}",
            synthetic::path(i),
            synthetic::oid(i),
            synthetic::extraction(i)
        );
    }
    let hash = crate::git_access::hash_blob(HashKind::Sha1, text.as_bytes());
    hash.as_bytes()[..10]
        .iter()
        .fold(format!("{n}-"), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Copies the directory `from` to the new directory `to`, leaving out
/// SQLite's shared-memory files (rebuilt on open).
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if !entry.file_name().to_string_lossy().ends_with("-shm") {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
