//! `brief::build` on hand-made inputs: the rules, the order, the cap and
//! the reconciliation of the providers' items. The engine's side (the
//! inputs from a review, the providers' requests) is in
//! `engine::tests::brief`.

use std::sync::Arc;

use lectern_services::changes::{
    ChangeDetail, ChangePath, ChangeStatus, DiffHunk, FileChange, FileMode, FileSide, SideIdentity,
    UnavailableKind,
};
use lectern_services::project::BlobId;
use lectern_services::review::{
    AnalysisState, Brief, BriefRule, DiffSide, FileKey, IncompleteKind, LineAnchor, NoiseKind,
    ProvidedItem, SeenState, SensitiveKind,
};
use lectern_services::session::Timestamp;

use super::{FileInput, MAX_ITEMS, MAX_PROVIDED, Provided, build};

fn path(p: &str) -> ChangePath {
    ChangePath(Arc::from(p.as_bytes()))
}

fn key(p: &str) -> FileKey {
    FileKey {
        old: Some(path(p)),
        new: Some(path(p)),
    }
}

fn blob(n: u8) -> FileSide {
    FileSide {
        mode: FileMode::File,
        content: SideIdentity::Blob(BlobId::from_bytes(&[n; 20]).unwrap()),
    }
}

/// A modified text file with one hunk: old lines 0..1, new lines 0..2.
fn modified(p: &str) -> FileChange {
    FileChange {
        status: ChangeStatus::Modified,
        path: path(p),
        old: Some(blob(1)),
        new: Some(blob(2)),
        detail: ChangeDetail::Text {
            hunks: Arc::from(vec![DiffHunk {
                old: 0..1,
                new: 0..2,
                old_byte: 0,
                new_byte: 0,
            }]),
            added: 1,
            removed: 0,
        },
        generated: false,
        untracked: false,
        staged: None,
    }
}

fn deleted(p: &str, untracked: bool) -> FileChange {
    FileChange {
        status: ChangeStatus::Deleted,
        old: Some(blob(1)),
        new: None,
        detail: ChangeDetail::NoDiff,
        untracked,
        ..modified(p)
    }
}

struct File {
    key: FileKey,
    change: FileChange,
    noise: Option<NoiseKind>,
    seen: SeenState,
    drifted: bool,
    drifted_at: Option<LineAnchor>,
    edit_failed: bool,
    command_only: Vec<(u32, u32)>,
}

impl File {
    fn new(change: FileChange) -> File {
        File {
            key: FileKey {
                old: change.old.as_ref().map(|_| change.path.clone()),
                new: change.new.as_ref().map(|_| change.path.clone()),
            },
            change,
            noise: None,
            seen: SeenState::Unseen,
            drifted: false,
            drifted_at: None,
            edit_failed: false,
            command_only: Vec::new(),
        }
    }

    fn input(&self) -> FileInput<'_> {
        FileInput {
            key: &self.key,
            change: &self.change,
            noise: self.noise,
            seen: self.seen,
            drifted: self.drifted,
            drifted_at: self.drifted_at,
            edit_failed: self.edit_failed,
            command_only: &self.command_only,
        }
    }
}

fn brief(files: &[File]) -> Brief {
    brief_with(files, &[])
}

fn brief_with(files: &[File], provided: &[(&str, Vec<ProvidedItem>)]) -> Brief {
    let inputs: Vec<FileInput<'_>> = files.iter().map(File::input).collect();
    let provided: Vec<Provided<'_>> = provided
        .iter()
        .map(|(provider, items)| Provided { provider, items })
        .collect();
    build(
        &inputs,
        &provided,
        0,
        AnalysisState::Unavailable(Arc::from("unavailable")),
    )
}

fn rules(b: &Brief) -> Vec<String> {
    b.items.iter().map(|i| format!("{:?}", i.rule)).collect()
}

const SINCE: SeenState = SeenState::ChangedSinceSeen {
    since: Timestamp(1),
};

#[test]
fn brief_changed_since_seen_is_one_item_per_file() {
    let mut a = File::new(modified("a.rs"));
    a.seen = SINCE;
    let mut b = File::new(modified("b.rs"));
    b.seen = SeenState::Seen;
    let brief = brief(&[a, b]);
    assert_eq!(rules(&brief), ["ChangedSinceSeen"]);
    let item = &brief.items[0];
    assert_eq!(item.file, Some(key("a.rs")));
    assert!(!item.critical);
    assert_eq!(item.count, 1);
    assert_eq!(brief.omitted, 0);
}

#[test]
fn brief_drifted_carries_the_first_drifted_hunk_line() {
    let mut a = File::new(modified("a.rs"));
    a.drifted = true;
    a.drifted_at = Some(LineAnchor {
        side: DiffSide::New,
        line: 1,
    });
    let brief = brief(&[a]);
    assert_eq!(rules(&brief), ["Drifted"]);
    assert_eq!(
        brief.items[0].line,
        Some(LineAnchor {
            side: DiffSide::New,
            line: 1
        })
    );
}

#[test]
fn brief_edit_failed_is_one_item_per_file() {
    let mut a = File::new(modified("a.rs"));
    a.edit_failed = true;
    let brief = brief(&[a]);
    assert_eq!(rules(&brief), ["EditFailed"]);
    assert_eq!(brief.items[0].file, Some(key("a.rs")));
}

#[test]
fn brief_command_only_is_one_item_per_call_with_the_file_count() {
    let mut a = File::new(modified("a.rs"));
    a.command_only = vec![(0, 3)];
    let mut b = File::new(modified("b.rs"));
    b.command_only = vec![(0, 3), (0, 5)];
    let brief = brief(&[a, b]);
    assert_eq!(rules(&brief), ["CommandOnly", "CommandOnly"]);
    // Call 3 claimed both, anchored on the first; call 5 only `b.rs`.
    assert_eq!(brief.items[0].file, Some(key("a.rs")));
    assert_eq!(brief.items[0].count, 2);
    assert_eq!(brief.items[1].file, Some(key("b.rs")));
    assert_eq!(brief.items[1].count, 1);
}

#[test]
fn brief_sensitive_kinds_are_critical_except_executable_and_submodule() {
    let ci = File::new(modified(".github/workflows/ci.yml"));
    let mut exe = File::new(modified("run.sh"));
    exe.change.new = Some(FileSide {
        mode: FileMode::Executable,
        content: SideIdentity::Blob(BlobId::from_bytes(&[2; 20]).unwrap()),
    });
    let brief = brief(&[exe, ci]);
    // Critical first, so the CI file comes before the executable.
    assert_eq!(rules(&brief), ["Sensitive(Ci)", "Sensitive(Executable)"]);
    assert!(brief.items[0].critical);
    assert!(!brief.items[1].critical);
    assert_eq!(brief.items[0].rule, BriefRule::Sensitive(SensitiveKind::Ci));
}

#[test]
fn brief_deleted_counts_tracked_non_noise_files_in_one_critical_item() {
    let tracked = File::new(deleted("a.rs", false));
    let also = File::new(deleted("b.rs", false));
    let untracked = File::new(deleted("c.rs", true));
    let mut lock = File::new(deleted("Cargo.lock", false));
    lock.noise = Some(NoiseKind::Lockfile);
    let brief = brief(&[tracked, also, untracked, lock]);
    assert_eq!(rules(&brief), ["Deleted", "Noise(Lockfile)"]);
    assert!(brief.items[0].critical);
    assert_eq!(brief.items[0].count, 2);
    assert_eq!(brief.items[0].file, None);
}

#[test]
fn brief_incomplete_counts_each_kind_once_per_file() {
    let mut a = File::new(modified("a.rs"));
    a.change.detail = ChangeDetail::Truncated {
        added: 1,
        removed: 0,
    };
    a.change.new = Some(FileSide {
        mode: FileMode::File,
        content: SideIdentity::NotRetained(BlobId::from_bytes(&[2; 20]).unwrap()),
    });
    let mut b = File::new(modified("b.bin"));
    b.change.detail = ChangeDetail::Binary {
        old_size: Some(1),
        new_size: Some(2),
    };
    let mut c = File::new(modified("c.rs"));
    c.change.old = Some(FileSide {
        mode: FileMode::File,
        content: SideIdentity::Unavailable(UnavailableKind::Unreadable),
    });
    c.change.new = Some(FileSide {
        mode: FileMode::File,
        content: SideIdentity::Unavailable(UnavailableKind::Unreadable),
    });
    let brief = brief(&[a, b, c]);
    assert_eq!(
        rules(&brief),
        [
            "Incomplete(Truncated)",
            "Incomplete(Binary)",
            "Incomplete(NotRetained)",
            "Incomplete(Unavailable)",
        ]
    );
    // Both sides of `c.rs` are unavailable: still one.
    assert!(brief.items.iter().all(|i| i.count == 1));
    assert_eq!(
        brief.items[3].rule,
        BriefRule::Incomplete(IncompleteKind::Unavailable)
    );
}

#[test]
fn brief_noise_files_only_count_in_their_noise_item() {
    let mut lock = File::new(modified("Cargo.lock"));
    lock.noise = Some(NoiseKind::Lockfile);
    lock.seen = SINCE;
    lock.drifted = true;
    lock.edit_failed = true;
    let mut generated = File::new(modified("gen/a.rs"));
    generated.noise = Some(NoiseKind::Generated);
    let mut other = File::new(modified("gen/b.rs"));
    other.noise = Some(NoiseKind::Generated);
    let brief = brief(&[lock, generated, other]);
    assert_eq!(rules(&brief), ["Noise(Lockfile)", "Noise(Generated)"]);
    assert_eq!(brief.items[1].count, 2);
}

#[test]
fn brief_orders_critical_first_then_the_table_then_the_files() {
    let mut second = File::new(modified("b.rs"));
    second.seen = SINCE;
    let mut first = File::new(modified("a.rs"));
    first.seen = SINCE;
    first.drifted = true;
    let ci = File::new(modified(".github/workflows/ci.yml"));
    let gone = File::new(deleted("z.rs", false));
    // Progress order: b, a, ci, z.
    let brief = brief(&[second, first, ci, gone]);
    assert_eq!(
        rules(&brief),
        [
            "Sensitive(Ci)",
            "Deleted",
            "ChangedSinceSeen",
            "ChangedSinceSeen",
            "Drifted",
        ]
    );
    // Within a rule, the files' order.
    assert_eq!(brief.items[2].file, Some(key("b.rs")));
    assert_eq!(brief.items[3].file, Some(key("a.rs")));
}

#[test]
fn brief_keeps_twelve_items_and_counts_the_rest() {
    let files: Vec<File> = (0..15)
        .map(|n| {
            let mut f = File::new(modified(&format!("f{n}.rs")));
            f.seen = SINCE;
            f
        })
        .collect();
    let brief = brief(&files);
    assert_eq!(brief.items.len(), MAX_ITEMS);
    assert_eq!(brief.omitted, 3);
    assert_eq!(brief.items[11].file, Some(key("f11.rs")));
}

fn provided(rule: &str, file: Option<&str>, line: Option<LineAnchor>) -> ProvidedItem {
    ProvidedItem {
        rule: Arc::from(rule),
        critical: false,
        file: file.map(key),
        line,
        args: Arc::from(vec![(Arc::from("name"), Arc::from("run"))]),
    }
}

#[test]
fn brief_reconciles_the_providers_items() {
    let a = File::new(modified("a.rs"));
    let shown = LineAnchor {
        side: DiffSide::New,
        line: 1,
    };
    let hidden = LineAnchor {
        side: DiffSide::New,
        line: 7,
    };
    let items = vec![
        // A line of the shown hunk: kept.
        provided("public-api", Some("a.rs"), Some(shown)),
        // A line outside it: a file note.
        provided("callers", Some("a.rs"), Some(hidden)),
        // A file not in the snapshot: dropped.
        provided("callers", Some("zz.rs"), Some(shown)),
        // No file: a general note.
        provided("summary", None, None),
    ];
    let brief = brief_with(&[a], &[("code-intel", items)]);
    assert_eq!(brief.items.len(), 3);
    let BriefRule::Provided {
        provider,
        rule,
        args,
    } = &brief.items[0].rule
    else {
        panic!("{:?}", brief.items[0].rule);
    };
    assert_eq!(&**provider, "code-intel");
    assert_eq!(&**rule, "public-api");
    assert_eq!(&*args[0].0, "name");
    assert_eq!(brief.items[0].line, Some(shown));
    assert_eq!(brief.items[1].line, None);
    assert_eq!(brief.items[1].file, Some(key("a.rs")));
    assert_eq!(brief.items[2].file, None);
}

#[test]
fn brief_keeps_eight_items_per_provider() {
    let a = File::new(modified("a.rs"));
    let many: Vec<ProvidedItem> = (0..10)
        .map(|n| provided(&format!("r{n}"), Some("a.rs"), None))
        .collect();
    let brief = brief_with(&[a], &[("p", many)]);
    assert_eq!(brief.items.len(), MAX_PROVIDED);
    assert_eq!(brief.omitted, 0);
}

#[test]
fn brief_critical_provided_items_come_first() {
    let a = File::new(modified("a.rs"));
    let mut critical = provided("secret", Some("a.rs"), None);
    critical.critical = true;
    let mut b = File::new(modified("b.rs"));
    b.seen = SINCE;
    let brief = brief_with(&[a, b], &[("p", vec![critical])]);
    assert!(matches!(brief.items[0].rule, BriefRule::Provided { .. }));
    assert_eq!(brief.items[1].rule, BriefRule::ChangedSinceSeen);
}
