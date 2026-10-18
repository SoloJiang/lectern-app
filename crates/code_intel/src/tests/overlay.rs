//! `overlay_*`: `FakeGit` builds the cases of git's「工作区比较」table; after
//! the reconcile the overlay agrees with the worktree's actual content.

use std::collections::BTreeMap;

use crate::map::WorktreeState;
use crate::testing::FakeGit;

use super::support::{Harness, ts};

/// What the map must say about a path: its effective OID is this file's
/// content (canonical when `eol`), or the path is not in the map.
fn check_against_disk(h: &Harness) {
    let map = h.map();
    let root = h.git.root("main");
    for (path, state) in map.overlay.entries() {
        let on_disk = std::fs::read(root.join(&**path));
        match state {
            WorktreeState::Content(oid) => {
                let bytes = on_disk.unwrap_or_else(|_| panic!("{path} is not on disk"));
                assert_eq!(*oid, h.git.oid(&bytes), "{path}");
            }
            WorktreeState::Absent => assert!(on_disk.is_err(), "{path} is on disk"),
            WorktreeState::Unindexable => {}
        }
    }
}

struct Case {
    name: &'static str,
    /// Run after the first commit of `a.ts`, `b.ts` and `big.ts`.
    change: fn(&FakeGit),
    overlay: &'static [(&'static str, Expect)],
}

#[derive(Clone, Copy, Debug)]
enum Expect {
    /// Content of these bytes.
    Content(&'static str),
    Absent,
    Unindexable,
}

const CASES: &[Case] = &[
    Case {
        name: "clean",
        change: |_| {},
        overlay: &[],
    },
    Case {
        name: "unstaged modification",
        change: |g| g.write("main", "a.ts", b"export const a = 2;\n"),
        overlay: &[("a.ts", Expect::Content("export const a = 2;\n"))],
    },
    Case {
        name: "staged and unchanged since",
        change: |g| g.add("main", &[("a.ts", b"export const a = 3;\n")]),
        overlay: &[],
    },
    Case {
        name: "StagedOnly: the worktree equals HEAD, the index does not",
        change: |g| {
            g.stage_content("main", "a.ts", b"export const a = 4;\n");
        },
        overlay: &[("a.ts", Expect::Content("export const a = 1;\n"))],
    },
    Case {
        name: "unstaged deletion",
        change: |g| g.delete("main", "b.ts"),
        overlay: &[("b.ts", Expect::Absent)],
    },
    Case {
        name: "untracked file",
        change: |g| g.write("main", "src/new.ts", b"export const n = 1;\n"),
        overlay: &[("src/new.ts", Expect::Content("export const n = 1;\n"))],
    },
    Case {
        name: "ignored untracked file",
        change: |g| {
            g.ignore("gen/");
            g.write("main", "gen/out.ts", b"export const g = 1;\n");
        },
        overlay: &[],
    },
    Case {
        name: "untracked file of no supported language",
        change: |g| g.write("main", "notes.md", b"# notes\n"),
        overlay: &[],
    },
    Case {
        name: "rename not staged",
        change: |g| {
            g.delete("main", "b.ts");
            g.write("main", "c.ts", b"export const b = 1;\n");
        },
        overlay: &[
            ("b.ts", Expect::Absent),
            ("c.ts", Expect::Content("export const b = 1;\n")),
        ],
    },
    Case {
        name: "Filtered (a clean filter's pointer)",
        change: |g| {
            g.filter("a.ts");
            g.write("main", "a.ts", b"version https://git-lfs.github.com/spec/v1\n");
        },
        overlay: &[("a.ts", Expect::Unindexable)],
    },
    Case {
        name: "over 1 MiB",
        change: |g| g.write("main", "big.ts", &vec![b' '; (1 << 20) + 1]),
        overlay: &[("big.ts", Expect::Unindexable)],
    },
    Case {
        name: "CRLF checkout of an LF blob",
        change: |g| {
            g.eol("a.ts");
            g.write("main", "a.ts", b"export const a = 1;\r\n");
        },
        overlay: &[],
    },
];

#[test]
fn overlay_follows_the_worktree() {
    for case in CASES {
        let h = Harness::new();
        h.git.add(
            "main",
            &[
                ("a.ts", b"export const a = 1;\n"),
                ("b.ts", b"export const b = 1;\n"),
                ("big.ts", &ts("big")),
            ],
        );
        h.git.commit("main");
        (case.change)(&h.git);
        h.open();
        let got: BTreeMap<String, WorktreeState> = h
            .map()
            .overlay
            .entries()
            .iter()
            .map(|(p, s)| (p.to_string(), *s))
            .collect();
        let want: BTreeMap<String, WorktreeState> = case
            .overlay
            .iter()
            .map(|(p, e)| {
                let state = match e {
                    Expect::Content(text) => WorktreeState::Content(h.git.oid(text.as_bytes())),
                    Expect::Absent => WorktreeState::Absent,
                    Expect::Unindexable => WorktreeState::Unindexable,
                };
                (p.to_string(), state)
            })
            .collect();
        assert_eq!(got, want, "{}", case.name);
        check_against_disk(&h);
    }
}

#[test]
fn overlay_failed_comparison_keeps_the_overlay() {
    let h = Harness::new();
    h.git.add("main", &[("a.ts", b"export const a = 1;\n")]);
    h.git.commit("main");
    h.git.write("main", "a.ts", b"export const a = 2;\n");
    h.open();
    assert_eq!(h.map().overlay.len(), 1);
    h.git.fail_compare(true);
    h.git.move_head("main");
    h.emit_git(&[("HEAD", super::support::CHANGED)]);
    h.settle();
    // The comparison failed: the overlay is the previous one.
    assert_eq!(
        h.map().overlay.get("a.ts"),
        Some(WorktreeState::Content(h.git.oid(b"export const a = 2;\n")))
    );
}
