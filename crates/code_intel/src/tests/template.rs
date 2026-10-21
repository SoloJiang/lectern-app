//! `template_*`: the benchmarks' virtual repository kept between runs
//! (`testing::VirtualTemplate::cached`): reused while its key holds and it
//! matches what the current writer stores, written again otherwise.

use std::path::{Path, PathBuf};

use lectern_cache::{Budgets, Cache, Config, Written};

use crate::extract::LanguageTable;
use crate::store::{Store, Version};
use crate::testing::{FakeGit, VirtualTemplate, synthetic, template_key, virtual_unit};

const N: usize = 200;
const NAMES: [(&str, usize); 1] = [("shared", 3)];

/// The kept template's directory under `root`, with a sentinel file that
/// only a reuse leaves in place.
fn kept(root: &Path) -> PathBuf {
    let root = root.canonicalize().expect("root");
    root.join(template_key(&root, N, &NAMES, &Budgets::default()))
}

fn mark(dir: &Path) {
    std::fs::write(dir.join("sentinel"), b"").expect("sentinel");
}

fn reused(dir: &Path) -> bool {
    dir.join("sentinel").exists()
}

/// The units a copy of `t` opens with, in the current versions.
fn opened_units(t: &VirtualTemplate) -> usize {
    let dir = tempfile::tempdir().expect("temp dir");
    let (git, cache) = t
        .open(&dir.path().join("cache"), Budgets::default())
        .expect("a copy");
    let versions: Vec<Version> = LanguageTable::core().iter().map(Version::of).collect();
    Store::new(&cache.repo(git.common_dir()))
        .units(&versions)
        .expect("read")
        .len()
}

/// Written once, then reused as it is; a copy holds every unit.
#[test]
fn template_kept_is_reused() {
    let root = tempfile::tempdir().expect("temp dir");
    let first = VirtualTemplate::cached(root.path(), N, &NAMES, Budgets::default()).expect("built");
    assert!(first.units() > 0);
    assert_eq!(opened_units(&first), first.units());
    drop(first);
    let dir = kept(root.path());
    mark(&dir);
    let again = VirtualTemplate::cached(root.path(), N, &NAMES, Budgets::default()).expect("kept");
    assert!(reused(&dir));
    assert_eq!(opened_units(&again), again.units());
}

/// A manifest of another key, or a store that lost units (another cache
/// format drops them on open, an interrupted write leaves fewer): written
/// again, never used.
#[test]
fn template_stale_is_written_again() {
    let root = tempfile::tempdir().expect("temp dir");
    drop(VirtualTemplate::cached(root.path(), N, &NAMES, Budgets::default()).expect("built"));
    let dir = kept(root.path());
    // The manifest of another key.
    mark(&dir);
    let manifest = dir.join("manifest");
    let text = std::fs::read_to_string(&manifest).expect("manifest");
    std::fs::write(&manifest, text.replacen("key=", "key=0", 1)).expect("write");
    let t = VirtualTemplate::cached(root.path(), N, &NAMES, Budgets::default()).expect("rebuilt");
    assert!(!reused(&dir));
    assert_eq!(opened_units(&t), t.units());
    drop(t);
    // The store without its units: the partition file removed.
    mark(&dir);
    let partitions = dir.join("cache").join("repo");
    for entry in std::fs::read_dir(&partitions).expect("partitions") {
        std::fs::remove_file(entry.expect("entry").path()).expect("remove");
    }
    let t = VirtualTemplate::cached(root.path(), N, &NAMES, Budgets::default()).expect("rebuilt");
    assert!(!reused(&dir));
    assert_eq!(opened_units(&t), t.units());
}

/// A template an older writer left (here: file 0's common definition one
/// line off) under the current key and with a matching manifest and unit
/// count: the reuse check regenerates the sample's units, sees the
/// difference and writes the template again.
#[test]
fn template_changed_writer_is_written_again() {
    let root = tempfile::tempdir().expect("temp dir");
    drop(VirtualTemplate::cached(root.path(), N, &NAMES, Budgets::default()).expect("built"));
    let dir = kept(root.path());
    {
        let git = FakeGit::at(&dir.join("git")).expect("repository");
        let cache = Cache::new(Config {
            root: dir.join("cache"),
            budgets: Budgets::default(),
            ui_thread: None,
        });
        let (version, mut extraction) =
            virtual_unit(&LanguageTable::core(), 0, &NAMES).expect("file 0 is a source file");
        let def = extraction.defs.last_mut().expect("the common definition");
        assert_eq!(&*def.name, NAMES[0].0);
        def.line += 1;
        let store = Store::new(&cache.repo(git.common_dir()));
        let mut write = store.write();
        write.add(synthetic::oid(0), &version, &extraction);
        assert!(matches!(write.commit(), Written::Committed));
    }
    mark(&dir);
    let t = VirtualTemplate::cached(root.path(), N, &NAMES, Budgets::default()).expect("rebuilt");
    assert!(
        !reused(&dir),
        "a template the current writer would not write is not used"
    );
    assert_eq!(opened_units(&t), t.units());
    drop(t);
    // Written again by the current writer: reused from now on.
    mark(&dir);
    drop(VirtualTemplate::cached(root.path(), N, &NAMES, Budgets::default()).expect("kept"));
    assert!(reused(&dir));
}

/// Other common names for the same size are another key: the old template
/// is removed, not kept beside the new one. A template of another size
/// (smoke mode's beside the measured one) stays.
#[test]
fn template_other_key_replaces_the_old() {
    let root = tempfile::tempdir().expect("temp dir");
    drop(VirtualTemplate::cached(root.path(), N, &NAMES, Budgets::default()).expect("built"));
    let old = kept(root.path());
    let other: [(&str, usize); 1] = [("shared", 4)];
    let t = VirtualTemplate::cached(root.path(), N, &other, Budgets::default()).expect("built");
    assert_eq!(t.files(), N);
    drop(t);
    assert!(!old.exists());
    let entries = std::fs::read_dir(root.path()).expect("root").count();
    assert_eq!(entries, 1, "only the current template of this size is kept");
    // Another size: kept beside it.
    drop(VirtualTemplate::cached(root.path(), N + 1, &other, Budgets::default()).expect("built"));
    let entries = std::fs::read_dir(root.path()).expect("root").count();
    assert_eq!(entries, 2, "each size keeps its own template");
}
