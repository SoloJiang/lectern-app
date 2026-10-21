//! `doc_*`: real files in a temporary worktree, the manual executor: the
//! four anchors, the tiers, a full read that sees another version than the
//! first screen, `Byte` anchors whose OID differs, the entry types the
//! first screen does not read, tier C's pages and scan, fs_watch events,
//! and what dropping a document releases.

use std::sync::Arc;

use lectern_git::{HashKind, Oid};
use lectern_services::reader::Anchor;

use super::rig::{Rig, VIEW, created, modified, open_fds, removed, texts};
use crate::doc::content::Content;
use crate::git_access::blob_id;
use crate::limits::{PAGE_LRU, TIER_B_MAX};
use crate::text::Text;
use crate::{
    DocState, Editable, Highlighting, LineCount, NotEditable, ReadError, Row, RowPos, Tier, testing,
};

/// Numbered lines: `line <n>` padded to `width` bytes.
fn numbered(lines: usize, width: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(lines * (width + 1));
    for n in 0..lines {
        let line = format!("line {n}");
        out.extend_from_slice(line.as_bytes());
        out.extend(std::iter::repeat_n(b' ', width.saturating_sub(line.len())));
        out.push(b'\n');
    }
    out
}

fn first_text(s: &crate::DocSnapshot, at: RowPos) -> String {
    texts(s, at, 1, 120).remove(0).trim_end().to_owned()
}

#[test]
fn doc_opens_at_each_anchor() {
    let rig = Rig::new();
    let small = numbered(20_000, 30);
    let large = numbered(100_000, 30);
    rig.write("small.txt", &small);
    rig.write("large.txt", &large);
    let (_s, start) = rig.open_settled(rig.target("small.txt", Anchor::Start));
    assert_eq!(start.state, DocState::Ready);
    assert_eq!(start.anchor, RowPos::default());
    assert_eq!(first_text(&start, start.anchor), "line 0");
    // A byte in the middle of line 700.
    let byte = 700 * 31 + 5;
    let (_b, at_byte) = rig.open_settled(rig.target(
        "small.txt",
        Anchor::Byte {
            byte,
            line: 700,
            column: 5,
        },
    ));
    assert_eq!(first_text(&at_byte, at_byte.anchor), "line 700");
    // A line of a file up to 1 MiB: found on the first screen.
    let line = Anchor::Line {
        line: 12_345,
        column: 0,
    };
    let first = rig
        .engine
        .first_screen(rig.target("small.txt", line), rig.root.clone(), VIEW);
    assert_eq!(first_text(&first, first.anchor), "line 12345");
    // A line of a larger file without checkpoints: after the full read.
    let (_l, at_line) = rig.open_settled(rig.target(
        "large.txt",
        Anchor::Line {
            line: 90_000,
            column: 0,
        },
    ));
    assert_eq!(first_text(&at_line, at_line.anchor), "line 90000");
    assert_eq!(at_line.tier, Some(Tier::B));
    assert_eq!(at_line.lines, LineCount::Exact(100_001));
    assert!(at_line.generation >= 2, "moved after the first screen");
}

#[test]
fn doc_tier_a_highlights_the_whole_file_and_can_be_edited() {
    let rig = Rig::new();
    let bytes = testing::ts(200 << 10, 0);
    rig.write("a.ts", &bytes);
    let (_d, s) = rig.open_settled(rig.target("a.ts", Anchor::Start));
    assert_eq!(s.tier, Some(Tier::A));
    assert_eq!(s.highlight, Highlighting::On);
    assert_eq!(s.oid, Some(blob_id(&Oid::for_blob(HashKind::Sha1, &bytes))));
    assert_eq!(s.editable, Editable::Yes);
    assert!(s.spans[0].exact());
    let text = Text::from_bytes(bytes.clone());
    assert_eq!(s.lines, LineCount::Exact(text.index().line_count()));
    // Rows deep in the file have tokens from the whole file's spans.
    let deep = s.pos_of_byte(150_000, 120);
    let rows = s.rows(deep, 40, 120);
    let tokens: usize = rows
        .iter()
        .map(|r| match r {
            Row::Text { tokens, .. } => tokens.len(),
            Row::Pending { .. } => 0,
        })
        .sum();
    assert!(tokens > 40);
    assert!(rig.notified.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    assert_eq!(rig.engine.tasks_in_flight(), 0);
}

#[test]
fn doc_byte_with_another_oid_moves_by_its_line() {
    let rig = Rig::new();
    let bytes = numbered(5_000, 20);
    rig.write("a.txt", &bytes);
    // The caller saw other content (say, the canonical one): its byte means
    // nothing here, its line does.
    let mut target = rig.target(
        "a.txt",
        Anchor::Byte {
            byte: 10,
            line: 4_000,
            column: 0,
        },
    );
    target.expect = Some(blob_id(&Oid::for_blob(HashKind::Sha1, b"other")));
    let (_d, s) = rig.open_settled(target);
    assert_eq!(first_text(&s, s.anchor), "line 4000");
    // The same OID: the byte stands.
    let mut same = rig.target(
        "a.txt",
        Anchor::Byte {
            byte: 21 * 7,
            line: 4_000,
            column: 0,
        },
    );
    same.expect = s.oid;
    let (_e, t) = rig.open_settled(same);
    assert_eq!(first_text(&t, t.anchor), "line 7");
}

#[test]
fn doc_full_read_of_another_version_drops_the_first_screen() {
    let rig = Rig::new();
    rig.write("a.ts", &testing::ts(100 << 10, 0));
    let path = rig.path("a.ts");
    let replacement = numbered(3_000, 10);
    let swap = replacement.clone();
    *crate::util::lock(&rig.engine.inner().after_first) = Some(Box::new(move || {
        // Between the first screen and the full read.
        std::fs::write(&path, &swap).unwrap();
    }));
    let (_d, s) = rig.open_settled(rig.target(
        "a.ts",
        Anchor::Line {
            line: 2_000,
            column: 0,
        },
    ));
    // Truncated in place: the same descriptor reads the new bytes.
    assert_eq!(s.size, replacement.len() as u64);
    assert_eq!(first_text(&s, s.anchor), "line 2000");
}

#[test]
fn doc_entries_that_are_not_read() {
    let rig = Rig::new();
    rig.write("real.txt", b"hello\n");
    std::os::unix::fs::symlink("real.txt", rig.path("link.txt")).unwrap();
    std::fs::create_dir_all(rig.path("dir/sub")).unwrap();
    std::os::unix::fs::symlink("dir", rig.path("via")).unwrap();
    std::fs::write(rig.path("dir/sub/x.txt"), b"x").unwrap();
    let state = |rel: &str| {
        rig.open_settled(rig.target(rel, Anchor::Start))
            .1
            .state
            .clone()
    };
    assert_eq!(
        state("link.txt"),
        DocState::Symlink {
            target: Arc::from(&b"real.txt"[..])
        }
    );
    assert_eq!(state("dir"), DocState::Unsupported);
    assert_eq!(state("missing.txt"), DocState::Missing);
    assert_eq!(
        state("via/sub/x.txt"),
        DocState::Unreadable(ReadError::PathThroughSymlink)
    );
    assert_eq!(state("../x"), DocState::Unreadable(ReadError::InvalidPath));
}

#[test]
fn doc_reopens_after_an_outside_change_at_the_same_line() {
    let rig = Rig::new();
    rig.write("a.txt", &numbered(2_000, 20));
    let (doc, s) = rig.open_settled(rig.target("a.txt", Anchor::Start));
    let at = s.pos_of_byte(21 * 300, 120);
    doc.scrolled(at);
    rig.settle();
    let generation = doc.snapshot().generation;
    // Another version: a line inserted at the top.
    let mut changed = b"inserted\n".to_vec();
    changed.extend(numbered(2_000, 20));
    std::fs::write(rig.path("a.txt"), &changed).unwrap();
    rig.touch("a.txt", modified());
    let after = doc.snapshot();
    assert_eq!(after.size, changed.len() as u64);
    assert!(after.generation > generation);
    assert_eq!(
        after.oid,
        Some(blob_id(&Oid::for_blob(HashKind::Sha1, &changed)))
    );
    // The same line number: now one line earlier in the numbering.
    assert_eq!(first_text(&after, after.anchor), "line 299");
    // An event that changes nothing reopens nothing.
    let quiet = after.generation;
    rig.touch("a.txt", modified());
    assert_eq!(doc.snapshot().generation, quiet);
    // Events on other paths are ignored.
    rig.write("b.txt", b"b");
    rig.touch("b.txt", modified());
    assert_eq!(doc.snapshot().generation, quiet);
}

#[test]
fn doc_shows_a_deleted_file_missing_and_opens_it_when_it_is_back() {
    let rig = Rig::new();
    rig.write("a.txt", b"one\ntwo\n");
    let (doc, _) = rig.open_settled(rig.target("a.txt", Anchor::Start));
    std::fs::remove_file(rig.path("a.txt")).unwrap();
    rig.touch("a.txt", removed());
    assert_eq!(doc.snapshot().state, DocState::Missing);
    std::fs::write(rig.path("a.txt"), b"three\n").unwrap();
    rig.touch("a.txt", created());
    let back = doc.snapshot();
    assert_eq!(back.state, DocState::Ready);
    assert_eq!(first_text(&back, back.anchor), "three");
}

/// Over 16 MiB: pages, the streaming scan, a page LRU of at most 8 MiB.
fn tier_c(rig: &Rig) -> Vec<u8> {
    let bytes = numbered((TIER_B_MAX as usize + (1 << 20)) / 31, 30);
    rig.write("huge.txt", &bytes);
    bytes
}

#[test]
fn doc_tier_c_scans_and_reads_pages_within_the_limit() {
    let rig = Rig::new();
    let bytes = tier_c(&rig);
    let (doc, s) = rig.open_settled(rig.target("huge.txt", Anchor::Start));
    assert_eq!(s.tier, Some(Tier::C));
    assert_eq!(s.oid, Some(blob_id(&Oid::for_blob(HashKind::Sha1, &bytes))));
    let lines = memchr::memchr_iter(b'\n', &bytes).count() as u64 + 1;
    assert_eq!(s.lines, LineCount::Exact(lines));
    assert_eq!(s.editable, Editable::No(NotEditable::TooLarge));
    let checkpoints = rig
        .engine
        .inner()
        .checkpoints(&rig.project(), &s.oid.unwrap())
        .expect("written by the scan");
    assert_eq!(checkpoints.len, bytes.len() as u64);
    // Scroll across the file: every position reads its pages.
    for step in 0..80u64 {
        let byte = step * (bytes.len() as u64 / 80);
        let pos = doc.snapshot().pos_of_byte(byte, 120);
        doc.scrolled(pos);
        rig.settle();
        let now = doc.snapshot();
        let top = now.pos_of_byte(byte, 120);
        let line = (top.line_start / 31) as usize;
        assert_eq!(first_text(&now, top), format!("line {line}"), "step {step}");
        let Content::Paged(paged) = &now.content else {
            panic!("tier C is paged");
        };
        assert!(paged.pages.bytes() as u64 <= PAGE_LRU);
    }
}

#[test]
fn doc_drop_cancels_its_tasks_and_closes_its_file() {
    let rig = Rig::new();
    tier_c(&rig);
    // Warm the root and the cache's database, which stay open.
    let (warm, _) = rig.open_settled(rig.target("huge.txt", Anchor::Start));
    drop(warm);
    rig.settle();
    let base = open_fds();
    let (doc, _) = rig.open_settled(rig.target("huge.txt", Anchor::Start));
    assert_eq!(open_fds(), base + 1, "tier C keeps its file open");
    drop(doc);
    rig.settle();
    assert_eq!(open_fds(), base);
    // Dropped before any task ran: they run and do nothing.
    let doc = rig.open(rig.target("huge.txt", Anchor::Start));
    drop(doc);
    rig.settle();
    assert_eq!(open_fds(), base);
    assert_eq!(rig.engine.tasks_in_flight(), 0);
}

#[test]
fn doc_tier_b_highlights_the_window_it_scrolls_to() {
    let rig = Rig::new();
    let bytes = testing::ts(3 << 20, 0);
    rig.write("big.ts", &bytes);
    let (doc, s) = rig.open_settled(rig.target("big.ts", Anchor::Start));
    assert_eq!(s.tier, Some(Tier::B));
    assert_eq!(s.highlight, Highlighting::On);
    let tokens = |s: &crate::DocSnapshot, at: RowPos| -> usize {
        s.rows(at, 40, 120)
            .iter()
            .map(|r| match r {
                Row::Text { tokens, .. } => tokens.len(),
                Row::Pending { .. } => 0,
            })
            .sum()
    };
    let middle = s.pos_of_byte(2 << 20, 120);
    assert_eq!(tokens(&s, middle), 0, "not highlighted there yet");
    doc.scrolled(middle);
    rig.settle();
    let after = doc.snapshot();
    assert!(tokens(&after, middle) > 40);
    // Scrolling within the window computes nothing more.
    let windows = after.spans.len();
    doc.scrolled(after.advance(middle, 5, 120));
    rig.settle();
    assert_eq!(doc.snapshot().spans.len(), windows);
}

#[test]
fn doc_folder_projects_open_without_watching() {
    let rig = Rig::new();
    rig.write("a.txt", b"folder\n");
    let mut target = rig.target("a.txt", Anchor::Start);
    target.project = lectern_services::project::ProjectKey::Folder {
        root: rig.root.clone(),
    };
    target.worktree = None;
    let (doc, s) = rig.open_settled(target);
    assert_eq!(first_text(&s, s.anchor), "folder");
    assert!(!doc.is_watched());
    rig.write("b.txt", b"git\n");
    let (watched, _) = rig.open_settled(rig.target("b.txt", Anchor::Start));
    assert!(watched.is_watched());
}

#[test]
fn doc_long_lines_fold_after_the_full_read() {
    let rig = Rig::new();
    let bytes = testing::minified_js(200 << 10, 0);
    rig.write("app.min.js", &bytes);
    let byte = 100_000;
    let (_d, s) = rig.open_settled(rig.target(
        "app.min.js",
        Anchor::Byte {
            byte,
            line: 0,
            column: byte as u32,
        },
    ));
    // One line over 4 KiB: tier B, folded at the viewport's columns.
    assert_eq!(s.tier, Some(Tier::B));
    assert_eq!(s.editable, Editable::No(NotEditable::LongLine));
    assert_eq!(s.anchor.line_start, 0);
    assert_eq!(u64::from(s.anchor.sub_row), byte / u64::from(VIEW.cols));
    let rows = s.rows(s.anchor, 3, VIEW.cols);
    for row in &rows {
        let Row::Text { bytes: range, .. } = row else {
            panic!("in memory");
        };
        assert_eq!(range.end - range.start, u64::from(VIEW.cols));
    }
    let Row::Text { bytes: range, .. } = &rows[0] else {
        unreachable!()
    };
    assert!(range.contains(&byte));
    assert_eq!(
        s.total_rows(VIEW.cols),
        (bytes.len() as u64).div_ceil(u64::from(VIEW.cols))
    );
}
