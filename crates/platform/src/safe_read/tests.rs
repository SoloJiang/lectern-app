//! Behaviour tests run once for every implementation available on this
//! platform: `Openat2` and `Walk` on Linux; `NofollowAny`, `NofollowAnyParent`
//! and `Walk` on macOS. `NofollowAnyParent` is what macOS 11 and 12 use; forcing
//! it keeps that path tested on newer CI machines.

use std::fs;
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use rustix::fs::{Mode, OFlags};
use tempfile::TempDir;

use super::*;

fn backends() -> Vec<Backend> {
    vec![
        #[cfg(target_os = "linux")]
        Backend::Openat2,
        #[cfg(target_os = "macos")]
        Backend::NofollowAny,
        #[cfg(target_os = "macos")]
        Backend::NofollowAnyParent,
        Backend::Walk,
    ]
}

/// One root per implementation.
fn roots(dir: &Path) -> Vec<SafeRoot> {
    backends()
        .into_iter()
        .map(|backend| SafeRoot::open_with(dir, backend).unwrap())
        .collect()
}

fn rel(path: &str) -> RelPath<'_> {
    RelPath::new(path.as_bytes()).unwrap_or_else(|| panic!("{path:?} is a valid RelPath"))
}

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn file_bytes(read: Result<Read, SafeError>) -> Vec<u8> {
    match read {
        Ok(Read::File { bytes, .. }) => bytes,
        other => panic!("expected a file, got {other:?}"),
    }
}

/// A comparable summary of a result (`SafeError` holds an `io::Error`).
fn outcome<T>(result: Result<T, SafeError>) -> Result<T, String> {
    result.map_err(|error| match error {
        SafeError::NotFound => "NotFound".to_owned(),
        SafeError::PathThroughSymlink => "PathThroughSymlink".to_owned(),
        SafeError::Io(error) => format!("Io({:?})", error.kind()),
    })
}

fn is_not_found<T>(result: Result<T, SafeError>) -> bool {
    matches!(result, Err(SafeError::NotFound))
}

fn is_through_symlink<T>(result: Result<T, SafeError>) -> bool {
    matches!(result, Err(SafeError::PathThroughSymlink))
}

#[cfg(target_os = "linux")]
fn mkfifo(path: &Path) {
    rustix::fs::mknodat(
        rustix::fs::CWD,
        path,
        FileType::Fifo,
        Mode::from_bits_truncate(0o644),
        0,
    )
    .unwrap();
}

#[cfg(target_os = "macos")]
fn mkfifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` is a valid NUL-terminated string that outlives the call.
    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o644) };
    assert_eq!(result, 0, "mkfifo: {}", std::io::Error::last_os_error());
}

// relpath_*

#[test]
fn relpath_rejects_invalid_paths() {
    let invalid: [&[u8]; 11] = [
        b"", b"/a", b"a/", b"a//b", b".", b"..", b"a/../b", b"a/./b", b"../a", b"a\0b", b"a/\0",
    ];
    for path in invalid {
        assert_eq!(RelPath::new(path), None, "{path:?}");
    }
    assert_eq!(RelPath::new(b"/"), None);
    assert_eq!(RelPath::new(b"a/.."), None);
}

#[test]
fn relpath_accepts_valid_paths() {
    let valid: [&[u8]; 8] = [
        b"a",
        b"a/b",
        b".git",
        b"..a",
        b"a..",
        b".hidden/x",
        b"\xff\xfe/x",
        b"dir/\xc3\x28",
    ];
    for path in valid {
        let rel = RelPath::new(path).unwrap_or_else(|| panic!("{path:?}"));
        assert_eq!(rel.as_bytes(), path);
    }
}

// stat_*

fn assert_meta_matches(meta: &Meta, path: &Path) {
    let md = fs::symlink_metadata(path).unwrap();
    assert_eq!(meta.size, md.size(), "size of {path:?}");
    assert_eq!(meta.mode, md.mode(), "mode of {path:?}");
    assert_eq!(
        meta.mtime,
        Timestamp {
            secs: md.mtime(),
            nanos: md.mtime_nsec().try_into().unwrap(),
        },
        "mtime of {path:?}"
    );
    assert_eq!(
        meta.ctime,
        Timestamp {
            secs: md.ctime(),
            nanos: md.ctime_nsec().try_into().unwrap(),
        },
        "ctime of {path:?}"
    );
    assert_eq!(meta.ino, md.ino(), "ino of {path:?}");
    #[cfg(target_vendor = "apple")]
    let dev = u64::from(md.dev() as u32);
    #[cfg(not(target_vendor = "apple"))]
    let dev = md.dev();
    assert_eq!(meta.dev, dev, "dev of {path:?}");
    assert_eq!(meta.uid, md.uid(), "uid of {path:?}");
    assert_eq!(meta.gid, md.gid(), "gid of {path:?}");
}

#[test]
fn stat_matches_symlink_metadata() {
    let tmp = TempDir::new().unwrap();
    let top = tmp.path().join("top.txt");
    let deep = tmp.path().join("a/b/c/deep.txt");
    write(&top, b"hello");
    write(&deep, b"deeper contents");
    // Nanoseconds that a coarser conversion would lose.
    let mtime = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
    fs::File::options()
        .write(true)
        .open(&deep)
        .unwrap()
        .set_modified(mtime)
        .unwrap();

    for root in roots(tmp.path()) {
        let meta = root.stat(rel("top.txt")).unwrap();
        assert_eq!(meta.kind, FileKind::File);
        assert_eq!(meta.size, 5);
        assert_meta_matches(&meta, &top);

        let meta = root.stat(rel("a/b/c/deep.txt")).unwrap();
        assert_eq!(meta.kind, FileKind::File);
        assert_eq!(
            meta.mtime,
            Timestamp {
                secs: 1_700_000_000,
                nanos: 123_456_789,
            }
        );
        assert_eq!(meta.mtime.as_nanos(), 1_700_000_000_123_456_789);
        assert_meta_matches(&meta, &deep);

        let meta = root.stat(rel("a/b")).unwrap();
        assert_eq!(meta.kind, FileKind::Dir);
        assert_meta_matches(&meta, &tmp.path().join("a/b"));
    }
}

#[test]
fn stat_executable_bit() {
    let tmp = TempDir::new().unwrap();
    for (name, mode) in [("run.sh", 0o755), ("owner.sh", 0o744), ("plain.txt", 0o644)] {
        let path = tmp.path().join("bin").join(name);
        write(&path, b"#!/bin/sh\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }
    fs::create_dir(tmp.path().join("bin/dir")).unwrap();
    fs::set_permissions(
        tmp.path().join("bin/dir"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    for root in roots(tmp.path()) {
        assert!(root.stat(rel("bin/run.sh")).unwrap().is_executable());
        assert!(root.stat(rel("bin/owner.sh")).unwrap().is_executable());
        assert!(!root.stat(rel("bin/plain.txt")).unwrap().is_executable());
        // Only regular files are executable in git's sense.
        assert!(!root.stat(rel("bin/dir")).unwrap().is_executable());
        assert_eq!(root.stat(rel("bin/run.sh")).unwrap().mode & 0o777, 0o755);
    }
}

#[test]
fn stat_missing_and_through_a_file_are_not_found() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a/file.txt"), b"x");

    for root in roots(tmp.path()) {
        for path in [
            "missing",
            "a/missing",
            "missing/x",
            "a/file.txt/x",
            "a/file.txt/x/y",
        ] {
            assert!(is_not_found(root.stat(rel(path))), "stat {path}");
            assert!(is_not_found(root.read(rel(path), 1024)), "read {path}");
        }
    }
}

#[test]
fn stat_after_the_root_is_deleted_is_not_found() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("root");
    write(&dir.join("a/f.txt"), b"x");
    let roots = roots(&dir);
    fs::remove_dir_all(&dir).unwrap();
    for root in roots {
        for path in ["a", "a/f.txt"] {
            assert!(is_not_found(root.stat(rel(path))), "stat {path}");
            assert!(is_not_found(root.read(rel(path), 1024)), "read {path}");
        }
    }
}

#[test]
fn stat_overlong_name_is_io() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join("a")).unwrap();
    let long = "n".repeat(300);
    for root in roots(tmp.path()) {
        for path in [long.clone(), format!("{long}/x"), format!("a/{long}")] {
            assert!(
                matches!(root.stat(rel(&path)), Err(SafeError::Io(_))),
                "stat {}",
                &path[..8]
            );
        }
    }
}

// symlink_*

/// A root with `a/b.txt`, links in the middle of paths to directories inside
/// and outside the root, and links in the last component.
struct LinkTree {
    tmp: TempDir,
    _outside: TempDir,
    /// Where the tree is built: the temporary directory, or a subdirectory
    /// of it for roots opened with `open_dir`.
    base: PathBuf,
}

impl LinkTree {
    fn new() -> LinkTree {
        LinkTree::build(None)
    }

    /// The tree in the subdirectory `dir` of the temporary directory, whose
    /// other entries (`secret`, `sibling/b.txt`) are outside the tree but
    /// inside the temporary directory.
    fn beneath(dir: &str) -> LinkTree {
        LinkTree::build(Some(dir))
    }

    fn build(dir: Option<&str>) -> LinkTree {
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let base = match dir {
            Some(dir) => {
                write(&tmp.path().join("secret"), b"parent");
                write(&tmp.path().join("sibling/b.txt"), b"sibling");
                tmp.path().join(dir)
            }
            None => tmp.path().to_owned(),
        };
        let root = base.as_path();
        write(&root.join("a/b.txt"), b"inside");
        write(&outside.path().join("b.txt"), b"outside");
        symlink("a", root.join("in_link")).unwrap();
        symlink(outside.path(), root.join("out_link")).unwrap();
        symlink("..", root.join("a/up")).unwrap();
        symlink("../a", root.join("a/self")).unwrap();
        symlink("b.txt", root.join("a/to_file")).unwrap();
        symlink("nowhere", root.join("dangling")).unwrap();
        symlink("/etc/passwd", root.join("passwd")).unwrap();
        symlink(outside.path().join("b.txt"), root.join("a/out_file")).unwrap();
        LinkTree {
            tmp,
            _outside: outside,
            base,
        }
    }

    fn path(&self) -> &Path {
        &self.base
    }

    /// The temporary directory the tree is in.
    fn top(&self) -> &Path {
        self.tmp.path()
    }
}

fn check_intermediate_symlinks(backend: Backend) {
    let tree = LinkTree::new();
    let root = SafeRoot::open_with(tree.path(), backend).unwrap();
    check_intermediate_symlinks_in(&root, backend);
}

fn check_intermediate_symlinks_in(root: &SafeRoot, backend: Backend) {
    for path in [
        "in_link/b.txt",
        "out_link/b.txt",
        "a/up/a/b.txt",
        "a/self/b.txt",
        "in_link/missing",
        "out_link/missing/deeper",
        "dangling/x",
    ] {
        assert!(
            is_through_symlink(root.stat(rel(path))),
            "{backend:?} stat {path}"
        );
        assert!(
            is_through_symlink(root.read(rel(path), 1024)),
            "{backend:?} read {path}"
        );
    }
}

fn check_last_symlinks(backend: Backend) {
    let tree = LinkTree::new();
    let root = SafeRoot::open_with(tree.path(), backend).unwrap();
    check_last_symlinks_in(&tree, &root, backend);
}

fn check_last_symlinks_in(tree: &LinkTree, root: &SafeRoot, backend: Backend) {
    for path in [
        "in_link",
        "out_link",
        "a/to_file",
        "dangling",
        "passwd",
        "a/out_file",
        "a/up",
    ] {
        let full = tree.path().join(path);
        let target = fs::read_link(&full).unwrap();
        let target = target.as_os_str().as_encoded_bytes();

        let meta = root.stat(rel(path)).unwrap();
        assert_eq!(meta.kind, FileKind::Symlink, "{backend:?} stat {path}");
        assert_eq!(meta.size, target.len() as u64, "{backend:?} stat {path}");
        assert_meta_matches(&meta, &full);

        match root.read(rel(path), 1024).unwrap() {
            Read::Symlink {
                meta: read_meta,
                target: read_target,
            } => {
                assert_eq!(read_target, target, "{backend:?} read {path}");
                assert_eq!(read_meta, meta, "{backend:?} read {path}");
            }
            other => panic!("{backend:?} read {path}: {other:?}"),
        }
        // The target is returned in full whatever `max` is; it is never read.
        assert!(
            matches!(root.read(rel(path), 0), Ok(Read::Symlink { .. })),
            "{backend:?} read {path} with max 0"
        );
    }
}

#[test]
fn symlink_in_the_middle_is_path_through_symlink() {
    for backend in backends() {
        check_intermediate_symlinks(backend);
    }
}

#[test]
fn symlink_in_the_last_component_is_returned_not_followed() {
    for backend in backends() {
        check_last_symlinks(backend);
    }
}

// read_*

#[test]
fn read_regular_files_up_to_max() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("small.txt"), b"small file\n");
    write(&tmp.path().join("empty"), b"");
    write(&tmp.path().join("d/exact"), &[7; 4096]);
    write(&tmp.path().join("d/over"), &[7; 4097]);
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    write(&tmp.path().join("big.bin"), &big);

    for root in roots(tmp.path()) {
        match root.read(rel("small.txt"), 4096).unwrap() {
            Read::File { meta, bytes } => {
                assert_eq!(bytes, b"small file\n");
                assert_eq!(meta, root.stat(rel("small.txt")).unwrap());
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(file_bytes(root.read(rel("empty"), 4096)), b"");
        assert_eq!(file_bytes(root.read(rel("d/exact"), 4096)), vec![7; 4096]);
        match root.read(rel("d/over"), 4096).unwrap() {
            Read::TooLarge { meta } => assert_eq!(meta.size, 4097),
            other => panic!("{other:?}"),
        }
        assert_eq!(file_bytes(root.read(rel("big.bin"), 300_000)), big);
        assert!(matches!(
            root.read(rel("big.bin"), 299_999),
            Ok(Read::TooLarge { .. })
        ));
    }
}

#[test]
fn read_with_max_zero() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("empty"), b"");
    write(&tmp.path().join("one"), b"1");
    for root in roots(tmp.path()) {
        assert_eq!(file_bytes(root.read(rel("empty"), 0)), b"");
        assert!(matches!(
            root.read(rel("one"), 0),
            Ok(Read::TooLarge { .. })
        ));
    }
}

#[test]
fn read_sparse_terabyte_file_is_too_large_at_once() {
    let tmp = TempDir::new().unwrap();
    let file = fs::File::create(tmp.path().join("sparse")).unwrap();
    file.set_len(1 << 40).unwrap();
    for root in roots(tmp.path()) {
        let start = Instant::now();
        match root.read(rel("sparse"), 64 << 20).unwrap() {
            Read::TooLarge { meta } => assert_eq!(meta.size, 1 << 40),
            other => panic!("{other:?}"),
        }
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}

#[test]
fn read_directory_is_unsupported() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("a/b")).unwrap();
    for root in roots(tmp.path()) {
        for path in ["a", "a/b"] {
            match root.read(rel(path), 1024).unwrap() {
                Read::Unsupported { meta } => assert_eq!(meta.kind, FileKind::Dir),
                other => panic!("{path}: {other:?}"),
            }
        }
    }
}

/// A file that reports size 0 but has content (procfs) is read to EOF, still
/// bounded by `max`.
#[cfg(target_os = "linux")]
#[test]
fn read_procfs_file_with_size_zero_to_eof() {
    for root in roots(Path::new("/proc/self")) {
        assert_eq!(root.stat(rel("status")).unwrap().size, 0);
        let bytes = file_bytes(root.read(rel("status"), 1 << 20));
        assert!(bytes.starts_with(b"Name:"), "{:?}", root.backend);
        assert!(matches!(
            root.read(rel("status"), 4),
            Ok(Read::TooLarge { .. })
        ));
    }
}

// fifo_*

#[test]
fn fifo_read_returns_unsupported_without_blocking() {
    let tmp = TempDir::new().unwrap();
    mkfifo(&tmp.path().join("pipe"));
    fs::create_dir(tmp.path().join("d")).unwrap();
    mkfifo(&tmp.path().join("d/pipe"));

    for backend in backends() {
        let root = SafeRoot::open_with(tmp.path(), backend).unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let results = [
                outcome(root.read(rel("pipe"), 1024)),
                outcome(root.read(rel("d/pipe"), 1024)),
            ];
            let _ = tx.send(results);
        });
        let results = rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|_| panic!("{backend:?}: reading a FIFO blocked"));
        for result in results {
            match result {
                Ok(Read::Unsupported { meta }) => assert_eq!(meta.kind, FileKind::Fifo),
                other => panic!("{backend:?}: {other:?}"),
            }
        }
    }
    for root in roots(tmp.path()) {
        assert_eq!(root.stat(rel("pipe")).unwrap().kind, FileKind::Fifo);
        assert_eq!(root.stat(rel("d/pipe")).unwrap().kind, FileKind::Fifo);
    }
}

// socket_*

#[test]
fn socket_read_is_unsupported() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join("d")).unwrap();
    let _top = UnixListener::bind(tmp.path().join("s")).unwrap();
    let _nested = UnixListener::bind(tmp.path().join("d/s")).unwrap();
    for root in roots(tmp.path()) {
        for path in ["s", "d/s"] {
            assert_eq!(root.stat(rel(path)).unwrap().kind, FileKind::Socket);
            match root.read(rel(path), 1024).unwrap() {
                Read::Unsupported { meta } => assert_eq!(meta.kind, FileKind::Socket),
                other => panic!("{path}: {other:?}"),
            }
        }
    }
}

// device_*

#[test]
fn device_null_is_unsupported() {
    for root in roots(Path::new("/dev")) {
        assert_eq!(root.stat(rel("null")).unwrap().kind, FileKind::CharDevice);
        match root.read(rel("null"), 1024).unwrap() {
            Read::Unsupported { meta } => assert_eq!(meta.kind, FileKind::CharDevice),
            other => panic!("{other:?}"),
        }
    }
}

// race_*

fn check_swap_between_calls(backend: Backend) {
    let tmp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    write(&tmp.path().join("a/b.txt"), b"inside");
    write(&outside.path().join("b.txt"), b"outside");
    let root = SafeRoot::open_with(tmp.path(), backend).unwrap();

    assert_eq!(root.stat(rel("a/b.txt")).unwrap().kind, FileKind::File);
    fs::rename(tmp.path().join("a"), tmp.path().join("moved")).unwrap();
    symlink(outside.path(), tmp.path().join("a")).unwrap();
    assert!(is_through_symlink(root.stat(rel("a/b.txt"))), "{backend:?}");
    assert!(
        is_through_symlink(root.read(rel("a/b.txt"), 1024)),
        "{backend:?}"
    );

    fs::remove_file(tmp.path().join("a")).unwrap();
    fs::rename(tmp.path().join("moved"), tmp.path().join("a")).unwrap();
    assert_eq!(root.stat(rel("a/b.txt")).unwrap().kind, FileKind::File);
    assert_eq!(file_bytes(root.read(rel("a/b.txt"), 1024)), b"inside");
}

fn check_swap_never_escapes(backend: Backend) {
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
                // Directory `a` -> symbolic link `a` -> directory `a`.
                fs::rename(&a, &held).unwrap();
                fs::rename(&link, &a).unwrap();
                fs::rename(&a, &link).unwrap();
                fs::rename(&held, &a).unwrap();
                swaps += 1;
            }
            swaps
        })
    };

    let (mut inside, mut missing, mut through) = (0, 0, 0);
    for i in 0..10_000 {
        let result = if i % 4 == 3 {
            root.stat(rel("a/secret")).map(|meta| {
                assert_eq!(meta.size, 6, "{backend:?}: stat saw the outside file");
                Read::Unsupported { meta }
            })
        } else {
            root.read(rel("a/secret"), 1024)
        };
        match result {
            Ok(Read::File { bytes, .. }) if bytes == b"inside" => inside += 1,
            Ok(Read::Unsupported { .. }) => inside += 1,
            Err(SafeError::NotFound) => missing += 1,
            Err(SafeError::PathThroughSymlink) => through += 1,
            other => panic!("{backend:?}: unexpected {other:?}"),
        }
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = swapper.join().unwrap();
    assert!(swaps > 0, "{backend:?}: the swapper never ran");
    eprintln!(
        "{backend:?}: {inside} inside, {missing} missing, {through} through a link, {swaps} swaps"
    );
}

// list_*

fn names(entries: Vec<DirEntry>) -> Vec<(String, FileKind)> {
    let mut out: Vec<(String, FileKind)> = entries
        .into_iter()
        .map(|e| (String::from_utf8(e.name).unwrap(), e.kind))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn list_root_and_subdirectories_with_kinds() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("f"), b"f");
    write(&tmp.path().join("d/inner"), b"i");
    symlink("/etc", tmp.path().join("link")).unwrap();
    let _socket = UnixListener::bind(tmp.path().join("sock")).unwrap();
    mkfifo(&tmp.path().join("fifo"));
    for root in roots(tmp.path()) {
        assert_eq!(
            names(root.list(None).unwrap()),
            [
                ("d".to_owned(), FileKind::Dir),
                ("f".to_owned(), FileKind::File),
                ("fifo".to_owned(), FileKind::Fifo),
                ("link".to_owned(), FileKind::Symlink),
                ("sock".to_owned(), FileKind::Socket),
            ],
            "{root:?}"
        );
        assert_eq!(
            names(root.list(Some(rel("d"))).unwrap()),
            [("inner".to_owned(), FileKind::File)]
        );
        // Listing the root twice gives the same answer: no shared offset.
        assert_eq!(root.list(None).unwrap().len(), 5);
    }
}

#[test]
fn list_never_follows_symlinks() {
    let tmp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    write(&outside.path().join("secret/x"), b"x");
    write(&tmp.path().join("real/y"), b"y");
    symlink(outside.path(), tmp.path().join("out")).unwrap();
    symlink(tmp.path().join("real"), tmp.path().join("alias")).unwrap();
    write(&tmp.path().join("file"), b"f");
    for root in roots(tmp.path()) {
        assert!(is_through_symlink(root.list(Some(rel("out")))), "{root:?}");
        assert!(
            is_through_symlink(root.list(Some(rel("out/secret")))),
            "{root:?}"
        );
        assert!(
            is_through_symlink(root.list(Some(rel("alias")))),
            "{root:?}"
        );
        assert!(is_not_found(root.list(Some(rel("missing")))), "{root:?}");
        assert_eq!(
            outcome(root.list(Some(rel("file")))).unwrap_err(),
            "Io(NotADirectory)",
            "{root:?}"
        );
    }
}

fn check_list_swap_never_escapes(backend: Backend) {
    let tmp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let root_path = tmp.path().to_owned();
    write(&root_path.join("a/inside"), b"i");
    write(&outside.path().join("OUTSIDE"), b"o");
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
    for _ in 0..5_000 {
        match root.list(Some(rel("a"))) {
            Ok(entries) => {
                assert!(
                    entries.iter().all(|e| e.name == b"inside"),
                    "{backend:?}: listed {entries:?}"
                );
            }
            Err(SafeError::NotFound | SafeError::PathThroughSymlink) => {}
            Err(other) => panic!("{backend:?}: unexpected {other:?}"),
        }
    }
    stop.store(true, Ordering::Relaxed);
    assert!(
        swapper.join().unwrap() > 0,
        "{backend:?}: the swapper never ran"
    );
}

#[test]
fn race_list_swap_never_escapes() {
    for backend in backends() {
        check_list_swap_never_escapes(backend);
    }
}

#[test]
fn race_swap_between_calls() {
    for backend in backends() {
        check_swap_between_calls(backend);
    }
}

#[test]
fn race_swap_never_escapes() {
    for backend in backends() {
        check_swap_never_escapes(backend);
    }
}

// open_dir_*

#[test]
fn open_dir_opens_a_subdirectory() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("node_modules/pkg/index.d.ts"), b"types");
    write(&tmp.path().join("node_modules/pkg/lib/deep.d.ts"), b"deep");
    write(&tmp.path().join("node_modules/top.json"), b"{}");
    write(&tmp.path().join("outside_deps"), b"workspace");
    for root in roots(tmp.path()) {
        let deps = root.open_dir(rel("node_modules")).unwrap();
        assert_eq!(deps.backend, root.backend);
        assert_eq!(
            file_bytes(deps.read(rel("pkg/index.d.ts"), 1024)),
            b"types",
            "{root:?}"
        );
        assert_eq!(deps.stat(rel("pkg")).unwrap().kind, FileKind::Dir);
        assert_meta_matches(
            &deps.stat(rel("pkg/lib/deep.d.ts")).unwrap(),
            &tmp.path().join("node_modules/pkg/lib/deep.d.ts"),
        );
        assert_eq!(
            names(deps.list(None).unwrap()),
            [
                ("pkg".to_owned(), FileKind::Dir),
                ("top.json".to_owned(), FileKind::File),
            ],
            "{root:?}"
        );
        // The parent root's other entries are not beneath the new one.
        assert!(is_not_found(deps.read(rel("outside_deps"), 1024)));
        assert!(is_not_found(deps.stat(rel("node_modules"))));

        // Several levels at once, and a root opened from an opened root.
        let lib = root.open_dir(rel("node_modules/pkg/lib")).unwrap();
        assert_eq!(file_bytes(lib.read(rel("deep.d.ts"), 1024)), b"deep");
        let pkg = deps.open_dir(rel("pkg")).unwrap();
        assert_eq!(file_bytes(pkg.read(rel("lib/deep.d.ts"), 1024)), b"deep");
        assert_eq!(
            names(pkg.list(Some(rel("lib"))).unwrap()),
            [("deep.d.ts".to_owned(), FileKind::File)]
        );
    }
}

#[test]
fn open_dir_through_a_symlink_is_path_through_symlink() {
    let tree = LinkTree::new();
    fs::create_dir(tree.path().join("a/c")).unwrap();
    for root in roots(tree.path()) {
        for path in [
            // The directory itself is a link.
            "in_link",
            "out_link",
            "a/up",
            "a/self",
            "a/to_file",
            "dangling",
            "passwd",
            // An intermediate component is a link.
            "in_link/c",
            "out_link/x",
            "a/up/a",
            "dangling/x",
        ] {
            assert!(
                is_through_symlink(root.open_dir(rel(path))),
                "{root:?} {path}"
            );
        }
        // The directory the links name opens by its own path.
        assert!(root.open_dir(rel("a/c")).is_ok(), "{root:?}");
    }
}

#[test]
fn open_dir_of_a_non_directory_fails() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("file"), b"f");
    write(&tmp.path().join("d/inner"), b"i");
    mkfifo(&tmp.path().join("fifo"));
    let _socket = UnixListener::bind(tmp.path().join("sock")).unwrap();
    for root in roots(tmp.path()) {
        for path in ["file", "d/inner", "fifo", "sock"] {
            assert_eq!(
                outcome(root.open_dir(rel(path))).unwrap_err(),
                "Io(NotADirectory)",
                "{root:?} {path}"
            );
        }
        for path in ["missing", "d/missing", "missing/x", "file/x"] {
            assert!(is_not_found(root.open_dir(rel(path))), "{root:?} {path}");
        }
    }
}

#[test]
fn open_dir_stays_bound_when_the_path_is_replaced() {
    let tmp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    write(&tmp.path().join("deps/secret"), b"inside");
    write(&tmp.path().join("deps/pkg/x"), b"x");
    write(&outside.path().join("secret"), b"OUTSIDE");
    write(&outside.path().join("pkg/x"), b"OUTSIDE");
    let roots = roots(tmp.path());
    let opened: Vec<SafeRoot> = roots
        .iter()
        .map(|root| root.open_dir(rel("deps")).unwrap())
        .collect();

    // The path now names a link to a directory outside the root.
    fs::rename(tmp.path().join("deps"), tmp.path().join("held")).unwrap();
    symlink(outside.path(), tmp.path().join("deps")).unwrap();
    for (root, deps) in roots.iter().zip(&opened) {
        assert_eq!(
            file_bytes(deps.read(rel("secret"), 1024)),
            b"inside",
            "{deps:?}"
        );
        assert_eq!(deps.stat(rel("secret")).unwrap().size, 6, "{deps:?}");
        assert_eq!(
            names(deps.list(None).unwrap()),
            [
                ("pkg".to_owned(), FileKind::Dir),
                ("secret".to_owned(), FileKind::File),
            ]
        );
        assert_eq!(file_bytes(deps.read(rel("pkg/x"), 1024)), b"x");
        let pkg = deps.open_dir(rel("pkg")).unwrap();
        assert_eq!(file_bytes(pkg.read(rel("x"), 1024)), b"x");
        // Opening by the path again sees the link.
        assert!(is_through_symlink(root.open_dir(rel("deps"))), "{root:?}");
    }

    // Once the directory is deleted, the bound root finds nothing.
    fs::remove_dir_all(tmp.path().join("held")).unwrap();
    for deps in &opened {
        assert!(is_not_found(deps.read(rel("secret"), 1024)), "{deps:?}");
        assert!(is_not_found(deps.stat(rel("pkg"))), "{deps:?}");
    }
}

#[test]
fn open_dir_root_follows_the_same_rules_beneath_it() {
    let tree = LinkTree::beneath("deps");
    for root in roots(tree.top()) {
        let deps = root.open_dir(rel("deps")).unwrap();
        let backend = deps.backend;
        // Links in the middle are refused and links in the last component
        // are returned unfollowed, as beneath the parent root.
        check_intermediate_symlinks_in(&deps, backend);
        check_last_symlinks_in(&tree, &deps, backend);
        for path in ["in_link", "out_link", "a/up", "dangling"] {
            assert!(is_through_symlink(deps.list(Some(rel(path)))), "{path}");
            assert!(is_through_symlink(deps.open_dir(rel(path))), "{path}");
        }
        // `a/up` points at the new root itself: its parent stays unreachable.
        assert!(is_through_symlink(deps.read(rel("a/up/secret"), 1024)));

        // Paths that would leave the new root are not `RelPath`s.
        for path in ["../secret", "/etc/passwd", "a/../../secret", ".."] {
            assert_eq!(RelPath::new(path.as_bytes()), None, "{path}");
            // Bypassing `RelPath::new`, the check before every system call
            // refuses them without opening anything.
            let bypass = RelPath(path.as_bytes());
            for result in [
                outcome(deps.stat(bypass).map(drop)),
                outcome(deps.read(bypass, 1024).map(drop)),
                outcome(deps.list(Some(bypass)).map(drop)),
                outcome(deps.open_dir(bypass).map(drop)),
            ] {
                assert_eq!(result.unwrap_err(), "Io(InvalidInput)", "{path}");
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn open_dir_openat2_rejects_escape() {
    let tree = LinkTree::beneath("deps");
    let root = SafeRoot::open_with(tree.top(), Backend::Openat2).unwrap();
    let deps = root.open_dir(rel("deps")).unwrap();
    for path in [
        "../secret",
        "../sibling/b.txt",
        "/etc/passwd",
        "a/../../secret",
    ] {
        for target in [Target::Entry, Target::Dir] {
            match linux::open_beneath(deps.fd.as_fd(), path.as_bytes(), target) {
                Err(rustix::io::Errno::XDEV) => {}
                other => panic!("{path} {target:?}: {other:?}"),
            }
        }
    }
}

/// One thread swaps directory `a` with a link to a directory outside the
/// root while this one opens `a` and reads through the result, and reads
/// through a root opened from `a` before the swapping began. Every read
/// sees the inside file, `NotFound` or `PathThroughSymlink`.
fn check_open_dir_swap_never_escapes(backend: Backend) {
    let tmp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let root_path = tmp.path().to_owned();
    write(&root_path.join("a/secret"), b"inside");
    write(&outside.path().join("secret"), b"OUTSIDE");
    symlink(outside.path(), root_path.join("link")).unwrap();
    let root = SafeRoot::open_with(&root_path, backend).unwrap();
    let before = root.open_dir(rel("a")).unwrap();

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

    let (mut opened, mut refused) = (0, 0);
    for _ in 0..5_000 {
        // Bound before the swapping began: always the original directory.
        assert_eq!(
            file_bytes(before.read(rel("secret"), 1024)),
            b"inside",
            "{backend:?}"
        );
        match root.open_dir(rel("a")) {
            Ok(a) => {
                opened += 1;
                // Bound to whatever directory the open found, which is `a`
                // under its own name or under `held`, never the outside one.
                assert_eq!(
                    file_bytes(a.read(rel("secret"), 1024)),
                    b"inside",
                    "{backend:?}"
                );
                assert!(
                    a.list(None).unwrap().iter().all(|e| e.name == b"secret"),
                    "{backend:?}"
                );
            }
            Err(SafeError::NotFound | SafeError::PathThroughSymlink) => refused += 1,
            Err(other) => panic!("{backend:?}: unexpected {other:?}"),
        }
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = swapper.join().unwrap();
    assert!(swaps > 0, "{backend:?}: the swapper never ran");
    eprintln!("{backend:?}: {opened} opened, {refused} refused, {swaps} swaps");
}

#[test]
fn race_open_dir_swap_never_escapes() {
    for backend in backends() {
        check_open_dir_swap_never_escapes(backend);
    }
}

// fd_*

fn open_fds() -> usize {
    let dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    fs::read_dir(dir).unwrap().count()
}

#[test]
fn fd_count_is_unchanged_after_many_calls() {
    let tree = LinkTree::new();
    let root_path = tree.path();
    write(&root_path.join("a/big"), &[1; 10_000]);
    mkfifo(&root_path.join("a/pipe"));
    let _socket = UnixListener::bind(root_path.join("a/s")).unwrap();
    let paths = [
        "a/b.txt",
        "a/big",
        "a/pipe",
        "a/s",
        "a",
        "in_link/b.txt",
        "out_link/b.txt",
        "a/to_file",
        "dangling",
        "missing",
        "a/b.txt/x",
        "a/missing/x",
    ];
    let roots = roots(root_path);
    // Warm up anything that is allocated once per process.
    for root in &roots {
        for path in paths {
            let _ = root.stat(rel(path));
            let _ = root.read(rel(path), 100);
            let _ = root.open_dir(rel(path));
        }
    }

    const ROUNDS: usize = 200;
    let before = open_fds();
    for _ in 0..ROUNDS {
        for root in &roots {
            for path in paths {
                let _ = root.stat(rel(path));
                let _ = root.read(rel(path), 100);
                let _ = root.read(rel(path), 1 << 20);
                // Dropping an opened root closes its descriptor.
                let _ = root.open_dir(rel(path));
            }
        }
    }
    let after = open_fds();
    if std::env::var_os("NEXTEST").is_some() {
        // `just test`: nextest runs each test in its own process.
        assert_eq!(after, before);
    } else {
        // `cargo test` runs the other tests on threads of this process, and
        // they open descriptors of their own. A leak on any path above would
        // leave at least one descriptor per round.
        assert!(
            after < before + ROUNDS / 2,
            "{before} -> {after} descriptors"
        );
    }
}

#[test]
fn fd_root_is_close_on_exec() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join("sub")).unwrap();
    let mut all = roots(tmp.path());
    all.push(SafeRoot::open(tmp.path()).unwrap());
    let opened: Vec<SafeRoot> = all
        .iter()
        .map(|root| root.open_dir(rel("sub")).unwrap())
        .collect();
    all.extend(opened);
    for root in all {
        let flags = rustix::io::fcntl_getfd(&root.fd).unwrap();
        assert!(
            flags.contains(rustix::io::FdFlags::CLOEXEC),
            "{:?}",
            root.backend
        );
    }
}

// send_sync

#[test]
fn send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SafeRoot>();
    assert_send_sync::<RelPath<'static>>();
    assert_send_sync::<Meta>();
    assert_send_sync::<Read>();
    assert_send_sync::<SafeError>();

    let tree = LinkTree::new();
    write(&tree.path().join("a/c/d.txt"), b"nested");
    const PATHS: [&str; 8] = [
        "a/b.txt",
        "a/c/d.txt",
        "a",
        "a/c",
        "in_link/b.txt",
        "a/to_file",
        "missing",
        "a/b.txt/x",
    ];
    let summarize = |root: &SafeRoot| {
        PATHS
            .iter()
            .map(|path| {
                (
                    outcome(root.stat(rel(path))),
                    outcome(root.read(rel(path), 1024)),
                )
            })
            .collect::<Vec<_>>()
    };
    for backend in backends() {
        let root = Arc::new(SafeRoot::open_with(tree.path(), backend).unwrap());
        let expected = summarize(&root);
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let root = Arc::clone(&root);
                thread::spawn(move || (0..200).map(|_| summarize(&root)).collect::<Vec<_>>())
            })
            .collect();
        for thread in threads {
            for got in thread.join().unwrap() {
                assert_eq!(got, expected, "{backend:?}");
            }
        }
    }
}

// walk_*

/// Builds a path longer than PATH_MAX (4096 on Linux, 1024 on macOS) with
/// `*at` calls, since absolute paths that long cannot be used. Returns the
/// relative path of the file at the bottom.
fn deep_tree(root: &Path) -> String {
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut dir = rustix::fs::open(root, dir_flags, Mode::empty()).unwrap();
    let mut rel = String::new();
    for level in 0..25 {
        let name = format!("{level:02}{}", "x".repeat(198));
        rustix::fs::mkdirat(&dir, name.as_str(), Mode::from_bits_truncate(0o755)).unwrap();
        dir = rustix::fs::openat(&dir, name.as_str(), dir_flags, Mode::empty()).unwrap();
        rel.push_str(&name);
        rel.push('/');
    }
    let file_flags = OFlags::WRONLY | OFlags::CREATE | OFlags::CLOEXEC;
    let file = rustix::fs::openat(
        &dir,
        "leaf.txt",
        file_flags,
        Mode::from_bits_truncate(0o644),
    )
    .unwrap();
    assert_eq!(rustix::io::write(&file, b"deep").unwrap(), 4);
    rel.push_str("leaf.txt");
    assert!(rel.len() > 4096);
    rel
}

fn check_walk_cases(backend: Backend) {
    let tree = LinkTree::new();
    let root_path = tree.path();
    let _socket = UnixListener::bind(root_path.join("a/s")).unwrap();
    let deep = deep_tree(root_path);
    let root = SafeRoot::open_with(root_path, backend).unwrap();

    // Intermediate symbolic link.
    assert!(
        is_through_symlink(root.stat(rel("out_link/b.txt"))),
        "{backend:?}"
    );
    assert!(
        is_through_symlink(root.read(rel("out_link/b.txt"), 9)),
        "{backend:?}"
    );
    // Intermediate regular file.
    assert!(is_not_found(root.stat(rel("a/b.txt/c"))), "{backend:?}");
    assert!(is_not_found(root.read(rel("a/b.txt/c"), 9)), "{backend:?}");
    // Symbolic link in the last component.
    assert_eq!(root.stat(rel("a/to_file")).unwrap().kind, FileKind::Symlink);
    assert!(matches!(
        root.read(rel("a/to_file"), 9),
        Ok(Read::Symlink { target, .. }) if target == b"b.txt"
    ));
    // Socket.
    assert!(matches!(
        root.read(rel("a/s"), 9),
        Ok(Read::Unsupported { meta }) if meta.kind == FileKind::Socket
    ));
    // Longer than PATH_MAX: the one-step open fails, the walk succeeds.
    let meta = root.stat(rel(&deep)).unwrap();
    assert_eq!((meta.kind, meta.size), (FileKind::File, 4), "{backend:?}");
    assert_eq!(file_bytes(root.read(rel(&deep), 9)), b"deep", "{backend:?}");
    let deep_dir = &deep[..deep.len() - "/leaf.txt".len()];
    assert_eq!(root.stat(rel(deep_dir)).unwrap().kind, FileKind::Dir);
}

#[test]
fn walk_matches_the_table_when_the_one_step_open_fails() {
    for backend in backends() {
        check_walk_cases(backend);
    }
}

#[test]
fn walk_directly_classifies_each_failure() {
    let tree = LinkTree::new();
    let _socket = UnixListener::bind(tree.path().join("a/s")).unwrap();
    let root = SafeRoot::open_with(tree.path(), Backend::Walk).unwrap();
    let fd = root.fd.as_fd();
    let some = |s: &'static str| Some(s.as_bytes());

    assert!(is_through_symlink(walk::stat(
        fd,
        some("in_link"),
        b"b.txt"
    )));
    assert!(is_through_symlink(walk::read(
        fd,
        some("a/up/a"),
        b"b.txt",
        9
    )));
    assert!(is_not_found(walk::stat(fd, some("a/b.txt"), b"x")));
    assert!(is_not_found(walk::read(fd, some("missing"), b"x", 9)));
    assert!(matches!(
        walk::read(fd, some("a"), b"to_file", 9),
        Ok(Read::Symlink { .. })
    ));
    assert!(matches!(
        walk::read(fd, None, b"dangling", 9),
        Ok(Read::Symlink { target, .. }) if target == b"nowhere"
    ));
    assert!(matches!(
        walk::read(fd, some("a"), b"s", 9),
        Ok(Read::Unsupported { meta }) if meta.kind == FileKind::Socket
    ));
    // The walk re-checks every component and opens nothing for a bad one.
    for parent in ["..", "a/..", ".", "a//b", ""] {
        assert!(
            matches!(
                walk::stat(fd, some(parent), b"b.txt"),
                Err(SafeError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidInput
            ),
            "{parent:?}"
        );
    }
}

#[test]
fn walk_permission_denied_is_io() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("closed/f.txt"), b"x");
    write(&tmp.path().join("unreadable.txt"), b"x");
    fs::set_permissions(tmp.path().join("closed"), fs::Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(
        tmp.path().join("unreadable.txt"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    // Root ignores permissions; nothing to check then.
    let is_root = fs::read(tmp.path().join("unreadable.txt")).is_ok();
    for root in roots(tmp.path()) {
        if is_root {
            break;
        }
        for result in [
            outcome(root.read(rel("closed/f.txt"), 9)),
            outcome(root.read(rel("unreadable.txt"), 9)),
            outcome(
                root.stat(rel("closed/f.txt"))
                    .map(|meta| Read::Unsupported { meta }),
            ),
        ] {
            assert_eq!(
                result,
                Err("Io(PermissionDenied)".to_owned()),
                "{:?}",
                root.backend
            );
        }
    }
    fs::set_permissions(tmp.path().join("closed"), fs::Permissions::from_mode(0o755)).unwrap();
}

// Errors

#[test]
fn errors_convert_to_io_errors() {
    let not_found = std::io::Error::from(SafeError::NotFound);
    assert_eq!(not_found.kind(), std::io::ErrorKind::NotFound);

    let through = std::io::Error::from(SafeError::PathThroughSymlink);
    assert_eq!(through.kind(), std::io::ErrorKind::Other);
    let inner = through.get_ref().unwrap().downcast_ref::<SafeError>();
    assert!(matches!(inner, Some(SafeError::PathThroughSymlink)));

    let io = std::io::Error::from(SafeError::Io(std::io::ErrorKind::PermissionDenied.into()));
    assert_eq!(io.kind(), std::io::ErrorKind::PermissionDenied);

    use std::error::Error as _;
    assert!(SafeError::NotFound.source().is_none());
    assert!(
        SafeError::Io(std::io::ErrorKind::Other.into())
            .source()
            .is_some()
    );
    assert_eq!(
        SafeError::PathThroughSymlink.to_string(),
        "a directory in the path is a symbolic link"
    );
}

#[test]
fn timestamp_as_nanos_handles_times_before_1970() {
    let before = Timestamp {
        secs: -2,
        nanos: 500_000_000,
    };
    assert_eq!(before.as_nanos(), -1_500_000_000);
    assert!(before < Timestamp { secs: -1, nanos: 0 });
}

#[test]
fn file_kind_follows_the_mode() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("f"), b"");
    for root in roots(tmp.path()) {
        let meta = root.stat(rel("f")).unwrap();
        assert_eq!(meta.mode & 0o170000, 0o100000);
        assert_eq!(meta.kind, FileKind::File);
    }
}

// Linux only

#[cfg(target_os = "linux")]
#[test]
fn openat2_rejects_escape() {
    let tmp = TempDir::new().unwrap();
    let root_path = tmp.path().join("root");
    fs::create_dir_all(root_path.join("a")).unwrap();
    write(&tmp.path().join("x"), b"outside");
    let root = SafeRoot::open_with(&root_path, Backend::Openat2).unwrap();
    for path in ["../x", "/etc/passwd", "a/../../x", "a/../.."] {
        for target in [Target::Entry, Target::Dir] {
            match linux::open_beneath(root.fd.as_fd(), path.as_bytes(), target) {
                Err(rustix::io::Errno::XDEV) => {}
                other => panic!("{path} {target:?}: {other:?}"),
            }
        }
    }
    // A `..` that stays beneath the root is the kernel's business, not an
    // escape; `RelPath` already refuses it before any system call.
    assert!(linux::open_beneath(root.fd.as_fd(), b"a/..", Target::Dir).is_ok());
    assert_eq!(RelPath::new(b"a/.."), None);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_backend_choice() {
    use rustix::io::Errno;
    for errno in [Errno::NOSYS, Errno::PERM, Errno::INVAL, Errno::TOOBIG] {
        assert_eq!(
            linux::backend_for_probe(Err(errno)),
            Backend::Walk,
            "{errno:?}"
        );
    }
    for probe in [
        Ok(()),
        Err(Errno::ACCESS),
        Err(Errno::NOENT),
        Err(Errno::AGAIN),
    ] {
        assert_eq!(
            linux::backend_for_probe(probe),
            Backend::Openat2,
            "{probe:?}"
        );
    }
    let tmp = TempDir::new().unwrap();
    assert_eq!(
        SafeRoot::open(tmp.path()).unwrap().backend,
        Backend::Openat2
    );
}

// macOS only

/// The major version `sw_vers` reports, such as 14 for 14.6.1.
#[cfg(target_os = "macos")]
fn macos_major_version() -> u32 {
    let out = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .unwrap();
    let version = String::from_utf8(out.stdout).unwrap();
    let major = version.trim().split('.').next().unwrap();
    major
        .parse()
        .unwrap_or_else(|_| panic!("sw_vers: {version:?}"))
}

#[cfg(target_os = "macos")]
#[test]
fn nofollow_any_backend_choice() {
    assert_eq!(
        macos::backend_for_probe(Err(Errno::INVAL)),
        Backend::NofollowAnyParent
    );
    for probe in [
        Ok(()),
        Err(Errno::NOENT),
        Err(Errno::ACCESS),
        Err(Errno::IO),
    ] {
        assert_eq!(
            macos::backend_for_probe(probe),
            Backend::NofollowAny,
            "{probe:?}"
        );
    }

    let tmp = TempDir::new().unwrap();
    let root = SafeRoot::open(tmp.path()).unwrap();
    let probe = macos::probe(root.fd.as_fd());
    let major = macos_major_version();
    eprintln!("macOS {major}: probe {probe:?}, backend {:?}", root.backend);
    if major >= 13 {
        assert_eq!(probe, Ok(()));
        assert_eq!(root.backend, Backend::NofollowAny);
    } else {
        assert_eq!(probe, Err(Errno::INVAL));
        assert_eq!(root.backend, Backend::NofollowAnyParent);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn nofollow_any_one_step_open_refuses_every_symlink() {
    let tree = LinkTree::new();
    let root = SafeRoot::open_with(tree.path(), Backend::NofollowAny).unwrap();
    let fd = root.fd.as_fd();
    for path in [
        "in_link/b.txt",
        "out_link/b.txt",
        "in_link",
        "a/self",
        "a/to_file",
        "dangling",
        "passwd",
    ] {
        assert!(
            macos::open_nofollow_any(fd, path.as_bytes()).is_err(),
            "{path}"
        );
    }
    assert!(macos::open_nofollow_any(fd, b"a/b.txt").is_ok());
    assert!(macos::open_nofollow_any(fd, b"a").is_ok());

    // The parent open of `NofollowAnyParent`.
    for path in [
        "in_link",
        "out_link",
        "a/self",
        "a/up",
        "in_link/x",
        "dangling",
    ] {
        assert!(
            macos::open_dir_nofollow_any(fd, path.as_bytes()).is_err(),
            "{path}"
        );
    }
    assert!(macos::open_dir_nofollow_any(fd, b"a").is_ok());
    assert!(macos::open_dir_nofollow_any(fd, b"a/b.txt").is_err());
}

/// The one-step `fstatat` itself, without the walk behind it: it answers a
/// regular path in one call (otherwise every `stat` would silently take the
/// slower walk), refuses a link before the last component with something other
/// than `ENOENT` (which would end the call as `NotFound` without the walk),
/// and never reports what a link points to.
#[cfg(target_os = "macos")]
#[test]
fn nofollow_any_one_step_stat_follows_no_symlink() {
    let tree = LinkTree::new();
    write(&tree.path().join("a/b/c/d.txt"), b"deep");
    let root = SafeRoot::open_with(tree.path(), Backend::NofollowAny).unwrap();
    let fd = root.fd.as_fd();
    if macos::probe(fd) == Err(Errno::INVAL) {
        // Before macOS 13; `nofollow_any_backend_choice` checks that this is
        // only the case there.
        eprintln!("fstatat has no AT_SYMLINK_NOFOLLOW_ANY here; nothing to check");
        return;
    }

    for (path, kind) in [
        ("a", FileKind::Dir),
        ("a/b.txt", FileKind::File),
        ("a/b", FileKind::Dir),
        ("a/b/c/d.txt", FileKind::File),
    ] {
        let meta = macos::stat_nofollow_any(fd, path.as_bytes())
            .unwrap_or_else(|error| panic!("{path}: {error:?}"));
        assert_eq!(meta.kind, kind, "{path}");
        assert_meta_matches(&meta, &tree.path().join(path));
    }
    assert_eq!(
        macos::stat_nofollow_any(fd, b"a/missing").map(|_| ()),
        Err(Errno::NOENT)
    );

    for path in [
        "in_link/b.txt",
        "out_link/b.txt",
        "a/up/a/b.txt",
        "a/self/b.txt",
        "in_link/missing",
        "out_link/missing/deeper",
        "dangling/x",
    ] {
        match macos::stat_nofollow_any(fd, path.as_bytes()) {
            Ok(meta) => panic!("{path}: followed a link to {meta:?}"),
            Err(error) => assert_ne!(error, Errno::NOENT, "{path}"),
        }
    }

    for path in [
        "in_link",
        "out_link",
        "a/to_file",
        "dangling",
        "passwd",
        "a/out_file",
        "a/up",
    ] {
        match macos::stat_nofollow_any(fd, path.as_bytes()) {
            Ok(meta) => {
                assert_eq!(meta.kind, FileKind::Symlink, "{path}");
                assert_meta_matches(&meta, &tree.path().join(path));
            }
            // `SafeRoot::stat` then asks the walk, which reports the link.
            Err(error) => assert_ne!(error, Errno::NOENT, "{path}"),
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn nofollow_any_symlinks() {
    for backend in [Backend::NofollowAny, Backend::NofollowAnyParent] {
        check_intermediate_symlinks(backend);
        check_last_symlinks(backend);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn nofollow_any_races() {
    for backend in [Backend::NofollowAny, Backend::NofollowAnyParent] {
        check_swap_between_calls(backend);
        check_swap_never_escapes(backend);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn nofollow_any_walk_cases() {
    for backend in [Backend::NofollowAny, Backend::NofollowAnyParent] {
        check_walk_cases(backend);
    }
}
