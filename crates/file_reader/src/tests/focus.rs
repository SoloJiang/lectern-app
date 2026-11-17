//! `focus_*`: which text `code_intel` is told is current, and which root a
//! target is read from; from snapshots alone.

use std::path::Path;
use std::sync::Arc;

use lectern_services::code_intel::{DepOrigin, DepRoot, Ecosystem, TextOrigin};
use lectern_services::project::{BlobId, FileKind, FileList, ProjectKey, WorktreeKey};
use lectern_services::reader::{Anchor, ReadTarget};

use super::rig::{folder_snapshot, git_snapshot};
use crate::focus::{index_oid, root_of, text_ref};

fn oid(n: u8) -> BlobId {
    BlobId::Sha1([n; 20])
}

fn git() -> (ProjectKey, WorktreeKey, Arc<Path>) {
    let common: Arc<Path> = Arc::from(Path::new("/repo/.git"));
    (
        ProjectKey::Git {
            common_dir: common.clone(),
        },
        WorktreeKey(common),
        Arc::from(Path::new("/repo")),
    )
}

fn target(project: &ProjectKey, worktree: Option<&WorktreeKey>, path: &str) -> ReadTarget {
    ReadTarget {
        project: project.clone(),
        worktree: worktree.cloned(),
        path: path.into(),
        origin: TextOrigin::Worktree,
        expect: None,
        at: Anchor::Start,
    }
}

#[test]
fn focus_root_of_a_worktree_a_folder_and_what_is_unknown() {
    let (key, wt, root) = git();
    let snapshot = git_snapshot(&key, &wt, &root, &[]);
    assert_eq!(
        root_of(&snapshot, &target(&key, Some(&wt), "a.rs")),
        Some(root)
    );
    // A worktree the project does not have; none at all for a git project.
    let other = WorktreeKey(Arc::from(Path::new("/other/.git")));
    assert_eq!(
        root_of(&snapshot, &target(&key, Some(&other), "a.rs")),
        None
    );
    assert_eq!(root_of(&snapshot, &target(&key, None, "a.rs")), None);

    let folder_root: Arc<Path> = Arc::from(Path::new("/folder"));
    let folder = ProjectKey::Folder {
        root: folder_root.clone(),
    };
    let snapshot = folder_snapshot(&folder, &folder_root);
    assert_eq!(
        root_of(&snapshot, &target(&folder, None, "a.rs")),
        Some(folder_root)
    );
    // A folder project has no worktrees.
    assert_eq!(
        root_of(&snapshot, &target(&folder, Some(&wt), "a.rs")),
        None
    );
}

#[test]
fn focus_root_of_a_prunable_worktree_is_none() {
    let (key, wt, root) = git();
    let mut snapshot = git_snapshot(&key, &wt, &root, &[]);
    let mut worktrees = snapshot.worktrees.to_vec();
    worktrees[0].prunable = true;
    snapshot.worktrees = worktrees.into();
    assert_eq!(root_of(&snapshot, &target(&key, Some(&wt), "a.rs")), None);
}

#[test]
fn focus_index_oid_finds_paths_by_binary_search() {
    let mut list = FileList::builder();
    for (i, path) in ["Cargo.toml", "a/b.rs", "a/c.rs", "src/lib.rs", "z"]
        .iter()
        .enumerate()
    {
        list.push_with_oid(path, FileKind::File, oid(i as u8 + 1));
    }
    let list = list.build();
    assert_eq!(index_oid(&list, "Cargo.toml"), Some(oid(1)));
    assert_eq!(index_oid(&list, "a/c.rs"), Some(oid(3)));
    assert_eq!(index_oid(&list, "z"), Some(oid(5)));
    assert_eq!(index_oid(&list, "a"), None);
    assert_eq!(index_oid(&list, "a/b.rs/x"), None);
    assert_eq!(index_oid(&list, "zz"), None);
    assert_eq!(index_oid(&FileList::builder().build(), "a"), None);
}

#[test]
fn focus_none_until_the_oid_is_known() {
    let (key, wt, root) = git();
    let snapshot = git_snapshot(&key, &wt, &root, &[("a.rs", oid(1))]);
    assert_eq!(
        text_ref(&target(&key, Some(&wt), "a.rs"), None, Some(&snapshot)),
        None
    );
}

#[test]
fn focus_a_workspace_file_needs_the_index_oid_to_equal_the_raw_one() {
    let (key, wt, root) = git();
    let snapshot = git_snapshot(&key, &wt, &root, &[("a.rs", oid(1)), ("b.rs", oid(2))]);
    let a = target(&key, Some(&wt), "a.rs");
    let text = text_ref(&a, Some(oid(1)), Some(&snapshot)).expect("unchanged");
    assert_eq!(text.oid, oid(1));
    assert_eq!(&*text.path, "a.rs");
    assert_eq!(text.worktree, Some(wt.clone()));
    assert_eq!(text.origin, TextOrigin::Worktree);
    // Modified (or converted by a filter): the canonical OID needs git.
    assert_eq!(text_ref(&a, Some(oid(9)), Some(&snapshot)), None);
    // Not in the index, or no snapshot, or not the active worktree.
    let c = target(&key, Some(&wt), "c.rs");
    assert_eq!(text_ref(&c, Some(oid(1)), Some(&snapshot)), None);
    assert_eq!(text_ref(&a, Some(oid(1)), None), None);
    let other = WorktreeKey(Arc::from(Path::new("/other/.git")));
    let elsewhere = target(&key, Some(&other), "a.rs");
    assert_eq!(text_ref(&elsewhere, Some(oid(1)), Some(&snapshot)), None);
}

#[test]
fn focus_a_folder_file_uses_the_raw_oid() {
    let root: Arc<Path> = Arc::from(Path::new("/folder"));
    let key = ProjectKey::Folder { root: root.clone() };
    let snapshot = folder_snapshot(&key, &root);
    let text =
        text_ref(&target(&key, None, "a.rs"), Some(oid(7)), Some(&snapshot)).expect("folder file");
    assert_eq!(text.oid, oid(7));
    assert_eq!(text.worktree, None);
}

#[test]
fn focus_an_object_uses_expect_and_a_dependency_the_raw_oid() {
    let (key, wt, _) = git();
    let mut object = target(&key, Some(&wt), "old.rs");
    object.origin = TextOrigin::Object;
    assert_eq!(text_ref(&object, Some(oid(3)), None), None, "no expect");
    object.expect = Some(oid(3));
    let text = text_ref(&object, Some(oid(3)), None).expect("object");
    assert_eq!((text.oid, text.origin), (oid(3), TextOrigin::Object));
    assert_eq!(text_ref(&object, None, None), None, "OID not known");

    let mut dep = target(&key, Some(&wt), "node_modules/x/index.js");
    dep.origin = TextOrigin::Dependency(DepOrigin {
        ecosystem: Ecosystem::Npm,
        package: "x".into(),
        version: None,
        root: DepRoot::Worktree {
            dir: "node_modules/x".into(),
        },
    });
    let text = text_ref(&dep, Some(oid(5)), None).expect("dependency");
    assert_eq!(text.oid, oid(5));
}
