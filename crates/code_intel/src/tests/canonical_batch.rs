//! `canonical_batch_*`: 「规范内容 OID 的批量核对」. The overlay's re-read
//! and the batch tiers' batches convert worktree bytes with one
//! `canonical_oids` per at most 256 items and 2 MiB (`FakeGit` records each
//! call); the results are those of one `canonical_oid` per file; a failed
//! batch is tried one by one; an interactive read still converts its one
//! file with `canonical_oid`.

use std::sync::Arc;

use lectern_services::code_intel::{Focus, QueryKind, TextOrigin, TextRef};
use lectern_services::project::BlobId;

use crate::canonical::{CANONICAL_BATCH_BYTES, CANONICAL_BATCH_ITEMS, Canonical, Pending};
use crate::git_access::{GitAccess, Tree};
use crate::map::WorktreeState;
use crate::testing::FakeGit;

use super::support::{CHANGED, Harness, ts};

fn tree_of(git: &FakeGit) -> Tree {
    let common: Arc<std::path::Path> = Arc::from(git.common_dir());
    Tree {
        common_dir: common.clone(),
        git_dir: common,
        root: Arc::from(git.root("main").as_path()),
    }
}

/// Pushes `items` in order, checking each batch as the next item would
/// take it over its limits, as the re-read and the batch tiers do.
fn convert(git: &FakeGit, items: &[(String, Vec<u8>)]) -> Vec<(usize, Canonical)> {
    let tree = tree_of(git);
    let mut pending: Pending<usize, &[u8]> = Pending::new();
    let mut out = Vec::new();
    for (i, (path, bytes)) in items.iter().enumerate() {
        if pending.full_for(bytes.len()) {
            out.extend(pending.flush(git, &tree));
        }
        pending.push(i, path.as_str().into(), bytes.as_slice());
    }
    out.extend(pending.flush(git, &tree));
    out
}

fn crlf(text: &[u8]) -> Vec<u8> {
    String::from_utf8(text.to_vec())
        .expect("UTF-8")
        .replace('\n', "\r\n")
        .into_bytes()
}

/// Each tag once and in order, with what `canonical_oid` answers for it.
fn assert_per_file(git: &FakeGit, items: &[(String, Vec<u8>)], got: &[(usize, Canonical)]) {
    let tree = tree_of(git);
    assert_eq!(got.len(), items.len());
    for (n, ((i, canonical), (path, bytes))) in got.iter().zip(items).enumerate() {
        assert_eq!(*i, n, "in the order pushed");
        let one = match git.canonical_oid(&tree, path, bytes) {
            Ok(Some(oid)) => Canonical::Oid(oid),
            Ok(None) => Canonical::Filtered,
            Err(_) => Canonical::Failed,
        };
        assert_eq!(*canonical, one, "{path}");
    }
}

#[test]
fn canonical_batch_splits_at_256_items() {
    let git = FakeGit::new();
    let items: Vec<(String, Vec<u8>)> = (0..600)
        .map(|i| {
            let path = format!("src/f{i}.ts");
            git.eol(&path);
            (path, crlf(format!("export const v{i} = {i};\n").as_bytes()))
        })
        .collect();
    let got = convert(&git, &items);
    let sizes: Vec<usize> = git.canonical_batches().iter().map(|b| b.0).collect();
    assert_eq!(sizes, [CANONICAL_BATCH_ITEMS, CANONICAL_BATCH_ITEMS, 88]);
    assert_eq!(git.count("canonical_oid"), 0);
    assert_per_file(&git, &items, &got);
}

#[test]
fn canonical_batch_splits_at_2_mib() {
    const KIB: usize = 1 << 10;
    let git = FakeGit::new();
    const MIB: usize = 1 << 20;
    let lens = [
        // A third would pass 2 MiB.
        700 * KIB,
        700 * KIB,
        // Exactly 2 MiB with the next one: one batch.
        700 * KIB,
        CANONICAL_BATCH_BYTES - 700 * KIB,
        MIB,
        MIB,
        // Larger than a batch: a batch of its own.
        3 * MIB,
        10,
        10,
    ];
    let items: Vec<(String, Vec<u8>)> = lens
        .iter()
        .enumerate()
        .map(|(i, len)| (format!("f{i}.ts"), vec![b'x'; *len]))
        .collect();
    let got = convert(&git, &items);
    assert_eq!(
        git.canonical_batches(),
        [
            (2, 1400 * KIB),
            (2, CANONICAL_BATCH_BYTES),
            (2, CANONICAL_BATCH_BYTES),
            (1, 3 * MIB),
            (2, 20),
        ],
        "flushed before the item that would pass 2 MiB",
    );
    assert_per_file(&git, &items, &got);
}

#[test]
fn canonical_batch_reread_of_300_crlf_files_makes_two_calls() {
    let h = Harness::new();
    let mut files = Vec::new();
    for i in 0..300 {
        let path = format!("src/f{i}.ts");
        h.git.eol(&path);
        let source = ts(&format!("f{i}"));
        h.git.add("main", &[(&path, &crlf(&source))]);
        files.push((path, h.git.oid(&source)));
    }
    h.git.commit("main");
    // The batch tiers convert what they read too: kept out of the count.
    h.pause();
    h.open();
    let before = h.git.canonical_batches().len();
    let singles = h.git.count("canonical_oid");
    let events: Vec<(&str, _)> = files.iter().map(|(p, _)| (p.as_str(), CHANGED)).collect();
    h.emit(&events);
    h.settle();
    let sizes: Vec<usize> = h.git.canonical_batches()[before..]
        .iter()
        .map(|b| b.0)
        .collect();
    assert_eq!(sizes, [256, 44]);
    assert_eq!(h.git.count("canonical_oid"), singles, "no single call");
    // Every CRLF checkout is its index content: no overlay entry.
    let map = h.map();
    assert!(map.overlay.entries().is_empty());
    for (path, index) in &files {
        assert_eq!(map.effective(path).expect("in the map").oid, *index);
    }
}

/// The re-read's states, one batch, against one `canonical_oid` per file
/// under the rules of 「blob → 路径映射」.
#[test]
fn canonical_batch_reread_matches_per_file() {
    let h = Harness::new();
    let committed: Vec<(&str, Vec<u8>)> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|n| (*n, ts(n)))
        .collect();
    for (name, source) in &committed {
        h.git.add("main", &[(&format!("{name}.ts"), source)]);
    }
    h.git.commit("main");
    h.pause();
    h.open();
    // a: a CRLF checkout of its content. b: CRLF and edited. c: a clean
    // filter's pointer (no canonical OID). d: edited, no conversion. e:
    // unchanged.
    h.git.eol("a.ts");
    h.git.write("main", "a.ts", &crlf(&committed[0].1));
    h.git.eol("b.ts");
    h.git.write(
        "main",
        "b.ts",
        &crlf(&[&committed[1].1[..], b"// b\n"].concat()),
    );
    h.git.filter("c.ts");
    h.git.write(
        "main",
        "c.ts",
        b"version https://git-lfs.github.com/spec/v1\n",
    );
    h.git
        .write("main", "d.ts", &[&committed[3].1[..], b"// d\n"].concat());
    let paths = ["a.ts", "b.ts", "c.ts", "d.ts", "e.ts"];
    let before = h.git.canonical_batches().len();
    let singles = h.git.count("canonical_oid");
    let events: Vec<(&str, _)> = paths.iter().map(|p| (*p, CHANGED)).collect();
    h.emit(&events);
    h.settle();
    // e's raw OID is the file list's: not converted.
    let batches = &h.git.canonical_batches()[before..];
    assert_eq!(batches.iter().map(|b| b.0).collect::<Vec<_>>(), [4]);
    assert_eq!(h.git.count("canonical_oid"), singles);

    let tree = tree_of(&h.git);
    let root = h.git.root("main");
    let map = h.map();
    for (path, (_, source)) in paths.iter().zip(&committed) {
        let index = h.git.oid(source);
        let bytes = std::fs::read(root.join(path)).expect("on disk");
        let raw = h.git.oid(&bytes);
        let want = if raw == index {
            None
        } else {
            match h.git.canonical_oid(&tree, path, &bytes).expect("converts") {
                Some(oid) if oid == index => None,
                Some(_) => Some(WorktreeState::Content(raw)),
                None => Some(WorktreeState::Excluded),
            }
        };
        assert_eq!(map.overlay.get(path), want, "{path}");
    }
    assert!(map.overlay.get("b.ts").is_some() && map.overlay.get("a.ts").is_none());
}

/// A failed conversion fails its batch (git's rule): each item is tried
/// on its own, and only the failing one counts as unreadable.
#[test]
fn canonical_batch_failed_item_is_retried_one_by_one() {
    let h = Harness::new();
    let mut files = Vec::new();
    for i in 0..5 {
        let path = format!("f{i}.ts");
        h.git.eol(&path);
        let source = ts(&format!("f{i}"));
        h.git.add("main", &[(&path, &crlf(&source))]);
        files.push((path, h.git.oid(&source)));
    }
    h.git.commit("main");
    h.pause();
    h.open();
    h.git.fail_canonical("f2.ts");
    let before = h.git.canonical_batches().len();
    let singles = h.git.count("canonical_oid");
    let events: Vec<(&str, _)> = files.iter().map(|(p, _)| (p.as_str(), CHANGED)).collect();
    h.emit(&events);
    h.settle();
    // Every `ts("fN")` here has the same length.
    let len = crlf(&ts("f0")).len();
    assert_eq!(h.git.canonical_batches()[before..], [(5, 5 * len)]);
    assert_eq!(h.git.count("canonical_oid"), singles + 5);
    let map = h.map();
    for (path, index) in &files {
        if path == "f2.ts" {
            assert_eq!(map.overlay.get(path), Some(WorktreeState::Unindexable));
        } else {
            assert_eq!(map.overlay.get(path), None, "{path}");
            assert_eq!(map.effective(path).expect("in the map").oid, *index);
        }
    }
}

/// The batch tiers: the files read with another OID than queued are
/// converted in one call per batch (at most 64 files), never one by one,
/// and stored under the queued OID.
#[test]
fn canonical_batch_batch_tier_converts_once_per_batch() {
    let h = Harness::new();
    let mut files = Vec::new();
    for i in 0..100 {
        let path = format!("src/f{i}.ts");
        h.git.eol(&path);
        let source = ts(&format!("f{i}"));
        h.git.add("main", &[(&path, &crlf(&source))]);
        files.push((path, source));
    }
    h.git.commit("main");
    h.open();
    // The comparison found the checkouts equal to the index: the re-read
    // converted nothing; all of it was the batch tiers'.
    assert_eq!(h.git.count("canonical_oid"), 0);
    let batches = h.git.canonical_batches();
    assert!(batches.len() >= 2, "{batches:?}");
    assert!(
        batches
            .iter()
            .all(|(n, _)| *n <= crate::index::batch::BATCH_FILES)
    );
    assert_eq!(batches.iter().map(|b| b.0).sum::<usize>(), files.len());
    let extracted: usize = h.batches().iter().sum();
    assert_eq!(extracted, files.len());
    for (path, source) in &files {
        assert!(
            h.stored(h.git.oid(source), "ts"),
            "{path} under its index OID"
        );
        assert!(!h.stored(h.git.oid(&crlf(source)), "ts"), "{path}: not raw");
        assert!(h.hinted(path), "{path}");
    }
}

/// In the batch tiers a failed batch is tried one by one: the failing file
/// is not written, the others are, under the queued OID.
#[test]
fn canonical_batch_batch_tier_failure_skips_only_that_file() {
    let h = Harness::new();
    let mut files = Vec::new();
    for i in 0..5 {
        let path = format!("f{i}.ts");
        h.git.eol(&path);
        let source = ts(&format!("f{i}"));
        h.git.add("main", &[(&path, &crlf(&source))]);
        files.push((path, source));
    }
    h.git.commit("main");
    h.pause();
    h.open();
    assert!(h.git.canonical_batches().is_empty());
    h.git.fail_canonical("f2.ts");
    h.resume();
    h.settle();
    assert_eq!(h.git.canonical_batches().len(), 1);
    assert_eq!(h.git.count("canonical_oid"), 5);
    for (path, source) in &files {
        let stored = h.stored(h.git.oid(source), "ts");
        assert_eq!(stored, path != "f2.ts", "{path}");
        assert!(!h.stored(h.git.oid(&crlf(source)), "ts"), "{path}: not raw");
    }
}

/// The re-read stops between batches once the session ended: a session
/// ended during the first call makes no other.
#[test]
fn canonical_batch_reread_stops_between_batches_when_the_session_ends() {
    let h = Harness::new();
    let mut paths = Vec::new();
    for i in 0..600 {
        let path = format!("src/f{i}.ts");
        h.git.eol(&path);
        h.git.add("main", &[(&path, &crlf(&ts(&format!("f{i}"))))]);
        paths.push(path);
    }
    h.git.commit("main");
    h.pause();
    h.open();
    let project = h.indexer.inner().project(&h.key()).expect("open");
    let before = h.git.canonical_batches().len();
    h.git.before_canonical_oids(move || {
        crate::index::lock(&project.state).end();
    });
    let events: Vec<(&str, _)> = paths.iter().map(|p| (p.as_str(), CHANGED)).collect();
    h.emit(&events);
    h.settle();
    assert_eq!(h.git.canonical_batches().len(), before + 1);
}

/// A jump's current text is one file on the interactive path: one
/// `canonical_oid`, no batch.
#[test]
fn canonical_batch_interactive_path_converts_one_file() {
    let h = Harness::new();
    let text = b"export function run() {\n  const limit = 1;\n  return limit;\n}\n";
    h.git.eol("a.ts");
    h.git.add("main", &[("a.ts", &crlf(text))]);
    h.git.commit("main");
    h.pause();
    h.open();
    let index = h.git.oid(text);
    let at = TextRef {
        worktree: Some(h.wt()),
        path: "a.ts".into(),
        oid: index,
        origin: TextOrigin::Worktree,
    };
    let batches = h.git.canonical_batches().len();
    let singles = h.git.count("canonical_oid");
    let position = h.pos("a.ts", "limit", 1);
    let results = h.ask(QueryKind::Definition { at, position });
    assert!(!results.candidates.is_empty(), "{:?}", results.state);
    assert!(h.git.count("canonical_oid") > singles);
    assert_eq!(h.git.canonical_batches().len(), batches);
}

/// A query's extractions are written as read: their key is the content
/// the query verified, so the write converts nothing.
#[test]
fn canonical_batch_query_writes_convert_nothing() {
    let h = Harness::new();
    let source = ts("helper");
    h.git.eol("lib.ts");
    h.git.add("main", &[("lib.ts", &crlf(&source))]);
    h.git.commit("main");
    h.pause();
    h.open();
    let project = h.indexer.inner().project(&h.key()).expect("open");
    let session = project.state.lock().unwrap().session;
    let table = crate::extract::LanguageTable::core();
    let lang = table.get("ts").expect("ts").clone();
    let extraction = crate::extract::extract(
        &lang,
        &crlf(&source),
        &crate::extract::Deadline::background(&h.indexer.inner().watchdog),
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let calls = h.git.calls().len();
    let index: BlobId = h.git.oid(&source);
    h.indexer.inner().commit_scanned(
        &project,
        session,
        vec![crate::index::batch::Scanned {
            path: "lib.ts".into(),
            oid: index,
            lang,
            extraction,
        }],
    );
    assert_eq!(h.git.calls().len(), calls, "no git call");
    assert!(h.stored(index, "ts"));
}

/// The production adapter: one `canonical_oids` call answers as one
/// `canonical_oid` per path, a path git cannot name included.
#[test]
fn canonical_batch_lectern_git_matches_per_file() {
    use lectern_git::testing::{TestHead, TestRepo};

    use crate::git_access::{LecternGit, hash_blob};

    let repo = TestRepo::new();
    repo.append_config("[core]\n\tautocrlf = true\n");
    let lf: &[u8] = b"export const a = 1;\nexport const b = 2;\n";
    let crlf_bytes: &[u8] = b"export const a = 1;\r\nexport const b = 2;\r\n";
    repo.write_file("a.ts", lf);
    repo.write_file("src/b.ts", lf);
    repo.backdate("a.ts");
    repo.backdate("src/b.ts");
    let entries = [repo.add("a.ts"), repo.add("src/b.ts")];
    repo.write_index(repo.common_dir(), &entries);
    let tree = repo.tree(&[("a.ts", lf), ("src/b.ts", lf)]);
    let commit = repo.commit(tree, &[], "init");
    repo.set_ref("refs/heads/main", commit);
    repo.set_head(TestHead::Branch("refs/heads/main"));

    let git = LecternGit::with_discover(lectern_git::testing::discover);
    let common = repo.common_dir().canonicalize().expect("canonical");
    let root = repo.root().canonicalize().expect("canonical");
    let tree = Tree {
        common_dir: Arc::from(common.as_path()),
        git_dir: Arc::from(common.as_path()),
        root: Arc::from(root.as_path()),
    };
    let items: [(&str, &[u8]); 5] = [
        ("a.ts", crlf_bytes),
        ("src/b.ts", lf),
        ("new.ts", crlf_bytes),
        ("../outside.ts", crlf_bytes),
        ("src/b.ts", b"edited\r\n"),
    ];
    let batch = git.canonical_oids(&tree, &items).expect("batch");
    let one: Vec<Option<BlobId>> = items
        .iter()
        .map(|(path, raw)| git.canonical_oid(&tree, path, raw).expect("one"))
        .collect();
    assert_eq!(batch, one);
    let kind = repo.hash();
    assert_eq!(batch[0], Some(hash_blob(kind, lf)), "CRLF converts to LF");
    assert_eq!(batch[3], None, "a path git cannot name");
    assert_eq!(git.canonical_oids(&tree, &[]).expect("empty"), Vec::new());
}

/// The current text is a CRLF checkout of the committed content, made
/// current: tier 1 converts that one file on its own.
#[test]
fn canonical_batch_tier1_converts_its_one_file() {
    let h = Harness::new();
    let source = ts("open");
    h.git.eol("a.ts");
    h.git.add("main", &[("a.ts", &crlf(&source))]);
    h.git.commit("main");
    h.pause();
    h.open();
    let batches = h.git.canonical_batches().len();
    let index = h.git.oid(&source);
    h.set_focus(Focus::Current(Some(TextRef {
        worktree: Some(h.wt()),
        path: "a.ts".into(),
        oid: index,
        origin: TextOrigin::Worktree,
    })));
    h.settle();
    assert!(h.stored(index, "ts"));
    assert_eq!(h.git.canonical_batches().len(), batches);
    assert!(h.git.count("canonical_oid") >= 1);
}
