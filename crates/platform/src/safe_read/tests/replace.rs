//! `replace_file_*`, `race_replace_never_escapes` and `fd_replace_file_*`
//! (「原子替换」 in docs/design/platform.md), once per implementation like the
//! read tests.

use std::io::Write as _;
use std::os::unix::fs::FileTypeExt as _;

use super::*;

use crate::safe_read::replace::TEMP_ATTEMPTS;

/// The names in `dir`, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Asserts that no temporary file of `replace_file` is left in `dir`.
fn assert_no_temp_files(dir: &Path) {
    let left: Vec<_> = entries(dir)
        .into_iter()
        .filter(|name| name.starts_with(".lectern-save-"))
        .collect();
    assert!(left.is_empty(), "left in {dir:?}: {left:?}");
}

fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

fn identity_of(root: &SafeRoot, path: &str) -> FileIdentity {
    root.stat(rel(path)).unwrap().identity()
}

/// A comparable summary of a result (`ReplaceError` holds an `io::Error`).
fn replace_outcome(result: Result<FileIdentity, ReplaceError>) -> String {
    match result {
        Ok(_) => "Ok".to_owned(),
        Err(ReplaceError::Changed { .. }) => "Changed".to_owned(),
        Err(ReplaceError::NotFound) => "NotFound".to_owned(),
        Err(ReplaceError::PathThroughSymlink) => "PathThroughSymlink".to_owned(),
        Err(ReplaceError::Unsupported) => "Unsupported".to_owned(),
        Err(ReplaceError::Io(error)) => format!("Io({:?})", error.kind()),
    }
}

#[test]
fn replace_file_replaces_content_and_keeps_permissions() {
    for backend in backends() {
        let tmp = TempDir::new().unwrap();
        let cases = [("top.txt", 0o640), ("a/b/c.txt", 0o751), ("a/b/d", 0o604)];
        for (path, mode) in cases {
            let full = tmp.path().join(path);
            write(&full, b"old content");
            fs::set_permissions(&full, fs::Permissions::from_mode(mode)).unwrap();
        }
        let root = SafeRoot::open_with(tmp.path(), backend).unwrap();
        let big = random_bytes(3 << 20, 7);

        for (path, mode) in cases {
            let full = tmp.path().join(path);
            let before = identity_of(&root, path);

            let saved = root
                .replace_file(rel(path), b"new", Some(before))
                .unwrap_or_else(|error| panic!("{backend:?} {path}: {error}"));
            assert_eq!(fs::read(&full).unwrap(), b"new", "{backend:?} {path}");
            assert_eq!(mode_of(&full), mode, "{backend:?} {path}");
            // A new file took the old one's place; the identity returned is
            // the one the next save expects.
            assert_ne!(saved.ino, before.ino, "{backend:?} {path}");
            assert_eq!(saved, identity_of(&root, path), "{backend:?} {path}");

            // Saving again with that identity, a larger and an empty content.
            let saved = root.replace_file(rel(path), &big, Some(saved)).unwrap();
            assert_eq!(fs::read(&full).unwrap(), big, "{backend:?} {path}");
            assert_eq!(saved.size, big.len() as u64);
            let saved = root.replace_file(rel(path), b"", Some(saved)).unwrap();
            assert_eq!(fs::read(&full).unwrap(), b"", "{backend:?} {path}");
            assert_eq!(saved, identity_of(&root, path));
            // Without an identity to compare, it replaces whatever is there.
            root.replace_file(rel(path), b"forced", None).unwrap();
            assert_eq!(fs::read(&full).unwrap(), b"forced", "{backend:?} {path}");
            assert_eq!(mode_of(&full), mode, "{backend:?} {path}");
        }
        assert_eq!(entries(tmp.path()), ["a", "top.txt"]);
        assert_eq!(entries(&tmp.path().join("a/b")), ["c.txt", "d"]);
    }
}

#[test]
fn replace_file_changed_on_disk_is_not_replaced() {
    for backend in backends() {
        let tmp = TempDir::new().unwrap();
        let full = tmp.path().join("a/f.txt");
        write(&full, b"version 1");
        let root = SafeRoot::open_with(tmp.path(), backend).unwrap();
        let path = "a/f.txt";
        let assert_changed = |expect: FileIdentity, disk: &[u8], what: &str| {
            match root.replace_file(rel(path), b"mine", Some(expect)) {
                Err(ReplaceError::Changed { now }) => {
                    assert_eq!(now, identity_of(&root, path), "{backend:?} {what}");
                    assert_ne!(now, expect, "{backend:?} {what}");
                }
                other => panic!("{backend:?} {what}: {:?}", replace_outcome(other)),
            }
            // The other writer's content stays; nothing is left behind.
            assert_eq!(fs::read(&full).unwrap(), disk, "{backend:?} {what}");
            assert_no_temp_files(&tmp.path().join("a"));
        };

        // Appended to.
        let read = identity_of(&root, path);
        fs::OpenOptions::new()
            .append(true)
            .open(&full)
            .unwrap()
            .write_all(b" and more")
            .unwrap();
        assert_changed(read, b"version 1 and more", "append");

        // Rewritten in place with the same size, in a later clock tick.
        let read = identity_of(&root, path);
        wait_for_a_later_ctime(tmp.path(), read.ctime);
        fs::write(&full, b"version 2 and more").unwrap();
        assert_changed(read, b"version 2 and more", "same-size rewrite");

        // Only its mode changed (`ctime`).
        let read = identity_of(&root, path);
        wait_for_a_later_ctime(tmp.path(), read.ctime);
        fs::set_permissions(&full, fs::Permissions::from_mode(0o600)).unwrap();
        assert_changed(read, b"version 2 and more", "chmod");

        // Replaced by another file (an atomic save by another program).
        let read = identity_of(&root, path);
        write(&tmp.path().join("a/other"), b"theirs");
        fs::rename(tmp.path().join("a/other"), &full).unwrap();
        assert_changed(read, b"theirs", "renamed over");

        // Deleted: the save does not bring it back.
        let read = identity_of(&root, path);
        fs::remove_file(&full).unwrap();
        let result = root.replace_file(rel(path), b"mine", Some(read));
        assert_eq!(replace_outcome(result), "NotFound", "{backend:?}");
        assert!(!full.exists(), "{backend:?}");
        assert_no_temp_files(&tmp.path().join("a"));
    }
}

#[test]
fn replace_file_through_a_symlink_is_refused() {
    let tree = LinkTree::new();
    let root_path = tree.path();
    let outside = root_path.join("out_link");
    let links = [
        "in_link",
        "out_link",
        "a/up",
        "a/self",
        "a/to_file",
        "dangling",
        "passwd",
        "a/out_file",
    ];
    let targets: Vec<_> = links
        .iter()
        .map(|link| fs::read_link(root_path.join(link)).unwrap())
        .collect();
    let paths = [
        // The last component is a link: to a file inside, outside, nowhere.
        "a/to_file",
        "a/out_file",
        "dangling",
        "passwd",
        // An intermediate component is a link: inside, outside, to the root.
        "in_link/b.txt",
        "out_link/b.txt",
        "a/self/b.txt",
        "a/up/a/b.txt",
        // Directories through links, even to themselves.
        "in_link",
        "out_link",
    ];
    for root in roots(root_path) {
        let any = identity_of(&root, "a/b.txt");
        for path in paths {
            for expect in [None, Some(any)] {
                let result = root.replace_file(rel(path), b"written", expect);
                assert_eq!(
                    replace_outcome(result),
                    "PathThroughSymlink",
                    "{:?} {path}",
                    root.backend
                );
            }
        }
    }
    // Nothing was written, inside or outside the root, and no link changed.
    assert_eq!(fs::read(root_path.join("a/b.txt")).unwrap(), b"inside");
    assert_eq!(fs::read(outside.join("b.txt")).unwrap(), b"outside");
    assert_eq!(entries(&outside), ["b.txt"]);
    for (link, target) in links.iter().zip(&targets) {
        assert_eq!(&fs::read_link(root_path.join(link)).unwrap(), target);
    }
    assert_no_temp_files(root_path);
    assert_no_temp_files(&root_path.join("a"));
}

#[test]
fn replace_file_of_other_types_and_missing_paths() {
    let tree = LinkTree::new();
    let root_path = tree.path();
    mkfifo(&root_path.join("a/pipe"));
    let _socket = UnixListener::bind(root_path.join("a/s")).unwrap();
    for root in roots(root_path) {
        let backend = root.backend;
        for path in ["a", "a/pipe", "a/s"] {
            let result = root.replace_file(rel(path), b"x", None);
            assert_eq!(replace_outcome(result), "Unsupported", "{backend:?} {path}");
        }
        // No file is created, and a regular file in the middle of the path
        // means there is no such file.
        for path in ["missing", "a/missing", "missing/x", "a/b.txt/x", "a/pipe/x"] {
            let result = root.replace_file(rel(path), b"x", None);
            assert_eq!(replace_outcome(result), "NotFound", "{backend:?} {path}");
        }
    }
    assert!(!root_path.join("missing").exists());
    assert!(!root_path.join("a/missing").exists());
    let pipe = fs::symlink_metadata(root_path.join("a/pipe")).unwrap();
    assert!(pipe.file_type().is_fifo());
    assert_no_temp_files(root_path);
    assert_no_temp_files(&root_path.join("a"));
}

#[test]
fn replace_file_temp_name_collision() {
    let tree = LinkTree::new();
    let root_path = tree.path();
    let a = root_path.join("a");
    let outside = root_path.join("out_link");
    write(&a.join("taken-file"), b"keep");
    symlink(outside.join("b.txt"), a.join("taken-link")).unwrap();
    // Following it would create a file outside the root.
    symlink(outside.join("created"), a.join("taken-dangling")).unwrap();
    let before = entries(&a);

    for root in roots(root_path) {
        let backend = root.backend;
        // Taken names, whether files or links, are skipped, never opened.
        let mut queue = [
            "taken-link",
            "taken-dangling",
            "taken-file",
            ".lectern-save-free",
        ]
        .into_iter()
        .map(|name| name.as_bytes().to_vec());
        let mut asked = 0;
        let mut names = || {
            asked += 1;
            queue.next().unwrap()
        };
        let saved = root.replace_with_names(rel("a/b.txt"), b"saved", None, &mut names);
        assert_eq!(replace_outcome(saved), "Ok", "{backend:?}");
        assert_eq!(asked, 4, "{backend:?}");
        assert_eq!(fs::read(a.join("b.txt")).unwrap(), b"saved", "{backend:?}");

        // Every name taken: an error after `TEMP_ATTEMPTS` names, and the
        // file is left as it was.
        let mut asked = 0;
        let mut taken = || {
            asked += 1;
            b"taken-file".to_vec()
        };
        let result = root.replace_with_names(rel("a/b.txt"), b"lost", None, &mut taken);
        assert_eq!(replace_outcome(result), "Io(AlreadyExists)", "{backend:?}");
        assert_eq!(asked, TEMP_ATTEMPTS, "{backend:?}");
        assert_eq!(fs::read(a.join("b.txt")).unwrap(), b"saved", "{backend:?}");

        assert_eq!(entries(&a), before, "{backend:?}");
        assert_eq!(fs::read(a.join("taken-file")).unwrap(), b"keep");
        assert_eq!(fs::read(outside.join("b.txt")).unwrap(), b"outside");
        assert_eq!(entries(&outside), ["b.txt"]);
        fs::write(a.join("b.txt"), b"inside").unwrap();
    }
}

#[test]
fn replace_file_parent_outside_the_root() {
    let tree = LinkTree::beneath("deps");
    let top = tree.top();
    let outside = fs::read_link(tree.path().join("out_link")).unwrap();
    for root in roots(top) {
        let backend = root.backend;
        let deps = root.open_dir(rel("deps")).unwrap();
        // Paths that would leave the root are not `RelPath`s; bypassing
        // `RelPath::new`, they are refused before any system call.
        for path in [
            "../secret",
            "a/../../secret",
            "/tmp/escaped",
            "..",
            "a//b.txt",
            "a/./b.txt",
        ] {
            assert_eq!(RelPath::new(path.as_bytes()), None, "{path}");
            let bypass = RelPath(path.as_bytes());
            let result = deps.replace_file(bypass, b"escaped", None);
            assert_eq!(
                replace_outcome(result),
                "Io(InvalidInput)",
                "{backend:?} {path}"
            );
        }
        // The link to the parent root is a link like any other.
        let result = deps.replace_file(rel("a/up/secret"), b"escaped", None);
        assert_eq!(replace_outcome(result), "PathThroughSymlink", "{backend:?}");
        assert_eq!(fs::read(top.join("secret")).unwrap(), b"parent");
        assert_eq!(entries(top), ["deps", "secret", "sibling"]);

        // A root from `open_dir` stays bound to its directory: once the path
        // points outside, the save still lands in the original directory.
        fs::rename(top.join("deps"), top.join("moved")).unwrap();
        symlink(&outside, top.join("deps")).unwrap();
        deps.replace_file(rel("a/b.txt"), b"saved", None).unwrap();
        assert_eq!(fs::read(top.join("moved/a/b.txt")).unwrap(), b"saved");
        assert_eq!(fs::read(top.join("deps/b.txt")).unwrap(), b"outside");
        assert!(!top.join("deps/a").exists(), "{backend:?}");
        fs::remove_file(top.join("deps")).unwrap();
        fs::rename(top.join("moved"), top.join("deps")).unwrap();
        fs::write(top.join("deps/a/b.txt"), b"inside").unwrap();
    }
}

/// One thread swaps directory `a` with a link to a directory outside the
/// root while this one saves `a/secret`. Every save lands inside the root or
/// is refused (`NotFound`, `PathThroughSymlink`); the outside file is never
/// written and no temporary file appears outside.
fn check_replace_swap_never_escapes(backend: Backend) {
    let tmp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let root_path = tmp.path().to_owned();
    write(&root_path.join("a/secret"), b"inside");
    write(&outside.path().join("secret"), b"OUTSIDE");
    symlink(outside.path(), root_path.join("link")).unwrap();
    let root = SafeRoot::open_with(&root_path, backend).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let stop = Arc::clone(&stop);
        let path = |name: &str| root_path.join(name);
        let (a, held, link) = (path("a"), path("held"), path("link"));
        thread::spawn(move || {
            let mut swaps = 0u64;
            while !stop.load(Ordering::Relaxed) {
                fs::rename(&a, &held).unwrap();
                fs::rename(&link, &a).unwrap();
                fs::rename(&a, &link).unwrap();
                fs::rename(&held, &a).unwrap();
                swaps += 1;
            }
            swaps
        })
    };

    let (mut saved, mut missing, mut through) = (0, 0, 0);
    for _ in 0..500 {
        match root.replace_file(rel("a/secret"), b"saved", None) {
            Ok(_) => saved += 1,
            Err(ReplaceError::NotFound) => missing += 1,
            Err(ReplaceError::PathThroughSymlink) => through += 1,
            other => panic!("{backend:?}: unexpected {:?}", replace_outcome(other)),
        }
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = swapper.join().unwrap();
    assert!(swaps > 0, "{backend:?}: the swapper never ran");
    assert_eq!(fs::read(outside.path().join("secret")).unwrap(), b"OUTSIDE");
    assert_eq!(entries(outside.path()), ["secret"], "{backend:?}");
    assert_eq!(entries(&root_path.join("a")), ["secret"], "{backend:?}");
    eprintln!(
        "{backend:?}: {saved} saved, {missing} missing, {through} through a link, {swaps} swaps"
    );
}

#[test]
fn race_replace_never_escapes() {
    for backend in backends() {
        check_replace_swap_never_escapes(backend);
    }
}

#[test]
fn fd_replace_file_keeps_no_descriptor() {
    let tree = LinkTree::new();
    let root_path = tree.path();
    mkfifo(&root_path.join("a/pipe"));
    let paths = [
        "a/b.txt",
        "a",
        "a/pipe",
        "a/to_file",
        "in_link/b.txt",
        "out_link/b.txt",
        "missing",
        "a/b.txt/x",
        "a/missing/x",
    ];
    let roots = roots(root_path);
    let stale = identity_of(&roots[0], "a");
    let exercise = |root: &SafeRoot| {
        for path in paths {
            let _ = root.replace_file(rel(path), b"x", None);
            // `Changed` after the temporary file was written.
            let _ = root.replace_file(rel(path), b"x", Some(stale));
        }
        let _ = root.replace_with_names(rel("a/b.txt"), b"x", None, &mut || b"pipe".to_vec());
    };
    // Warm up anything that is allocated once per process.
    for root in &roots {
        exercise(root);
    }

    const ROUNDS: usize = 30;
    let before = open_fds();
    for _ in 0..ROUNDS {
        for root in &roots {
            exercise(root);
        }
    }
    let after = open_fds();
    if std::env::var_os("NEXTEST").is_some() {
        assert_eq!(after, before);
    } else {
        // As in `fd_count_is_unchanged_after_many_calls`.
        assert!(
            after < before + ROUNDS / 2,
            "{before} -> {after} descriptors"
        );
    }
    assert_no_temp_files(&root_path.join("a"));
}

#[test]
fn replace_error_is_send_sync_and_displays() {
    fn assert_send_sync<T: Send + Sync + std::error::Error>() {}
    assert_send_sync::<ReplaceError>();
    let identity = FileIdentity {
        dev: 1,
        ino: 2,
        size: 3,
        mtime: Timestamp { secs: 4, nanos: 5 },
        ctime: Timestamp { secs: 6, nanos: 7 },
    };
    let errors = [
        ReplaceError::Changed { now: identity },
        ReplaceError::NotFound,
        ReplaceError::PathThroughSymlink,
        ReplaceError::Unsupported,
        ReplaceError::Io(std::io::ErrorKind::StorageFull.into()),
    ];
    for error in &errors {
        assert!(!error.to_string().is_empty(), "{error:?}");
    }
    use std::error::Error as _;
    assert!(errors[4].source().is_some());
    assert!(errors[0].source().is_none());
}
