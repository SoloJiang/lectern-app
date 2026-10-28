//! `items::build` and its helpers (docs/design/review_view.md「测试」:
//! `items_*`): pure, no window.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lectern_services::changes::ChangePath;
use lectern_services::review::{
    Degraded, FileKey, FileRows, GroupKind, NarrativeItem, NoiseKind, ReviewSnapshot, RowsKey,
    StepId,
};

use crate::brief::file_text;
use crate::items::{self, BuildOptions, DEFAULT_CONTEXT, DiffMode, Item, ItemKey, Notice};
use crate::testing::{file_key, snapshot_fixture};

pub fn rows_map(rows: Vec<(RowsKey, FileRows)>) -> HashMap<RowsKey, Arc<FileRows>> {
    rows.into_iter().map(|(k, r)| (k, Arc::new(r))).collect()
}

pub fn build(
    snapshot: &ReviewSnapshot,
    rows: &HashMap<RowsKey, Arc<FileRows>>,
    collapsed: bool,
    noise: bool,
) -> Vec<Item> {
    let context = HashMap::new();
    let known = HashMap::new();
    items::build(
        snapshot,
        rows,
        &BuildOptions {
            mode: DiffMode::Unified,
            earlier_collapsed: collapsed,
            noise_expanded: noise,
            context: &context,
            known_lines: &known,
        },
    )
}

/// One line per item, for the snapshot and the order tests.
pub fn dump(items: &[Item], snapshot: &ReviewSnapshot) -> String {
    let rows_name = |rows: &RowsKey| match rows {
        RowsKey::Final { file, context } => format!("final:{}/{context}", file_text(file)),
        RowsKey::Step { step, context } => format!("step:{}/{context}", step.0),
    };
    let mut out = String::new();
    for item in items {
        let line = match item {
            Item::Header => "header".to_owned(),
            Item::Brief => "brief".to_owned(),
            Item::Notice(notice) => format!("notice {notice:?}"),
            Item::EarlierTurns { count, expanded } => {
                format!("earlier {count} expanded={expanded}")
            }
            Item::Turn { at, ix, collapsed } => {
                let folded = if *collapsed { " folded" } else { "" };
                format!("turn {}:{} (#{ix}){folded}", at.session, at.ordinal)
            }
            Item::Prompt { at, .. } => format!("prompt {}:{}", at.session, at.ordinal),
            Item::Text { at, .. } => format!("text @{}", at.item),
            Item::Injected { at, .. } => format!("injected @{}", at.item),
            Item::Step { id, at } => {
                let file = items::step_entry(snapshot, *at).map(|s| file_text(&s.file));
                format!("step {} {}", id.0, file.unwrap_or_default())
            }
            Item::File { key, ix } => format!("file {} (#{ix})", file_text(key)),
            Item::Group { kind, files } => format!("group {kind:?} {files}"),
            Item::Noise { count, expanded } => format!("noise {count} expanded={expanded}"),
            Item::Hunk {
                rows,
                hunk,
                old,
                new,
            } => {
                format!("  hunk {} #{hunk} {old}:{new}", rows_name(rows))
            }
            Item::Row { rows, ix, old, new } => {
                format!("    row {} #{ix} {old:?}:{new:?}", rows_name(rows))
            }
            Item::Fold { rows, ix, old } => format!("    fold {} #{ix} {old}", rows_name(rows)),
            Item::Placeholder { rows, lines } => {
                format!("  placeholder {} {lines}", rows_name(rows))
            }
            Item::Incomplete { rows, kind } => format!("  incomplete {} {kind:?}", rows_name(rows)),
            Item::Command { call, .. } => format!("command {}", call.call.0),
            Item::Subagent { session, call, .. } => format!("subagent {session} {}", call.0),
            Item::Tool { call, .. } => format!("tool {}", call.call.0),
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[test]
fn items_session_order() {
    let (snapshot, rows) = snapshot_fixture(3, 2, 8);
    let items = build(&snapshot, &rows_map(rows), true, false);
    let kinds: Vec<&str> = items
        .iter()
        .map(|item| match item {
            Item::Header => "header",
            Item::Brief => "brief",
            Item::Turn { .. } => "turn",
            Item::Prompt { .. } => "prompt",
            Item::Text { .. } => "text",
            Item::Step { .. } => "step",
            Item::Hunk { .. } => "hunk",
            Item::Row { .. } => "row",
            Item::Tool { .. } => "tool",
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    // The title block and the brief, then turn 1 (files 0 and 2), turn 2
    // (file 1): prompt, text, the steps each with their hunk and rows, the
    // tool call last.
    assert_eq!(&kinds[..2], ["header", "brief"]);
    assert_eq!(&kinds[2..5], ["turn", "prompt", "text"]);
    assert_eq!(kinds[5], "step");
    assert_eq!(kinds[6], "hunk");
    assert!(kinds[7..15].iter().all(|k| *k == "row"));
    let steps: Vec<u32> = items
        .iter()
        .filter_map(|item| match item {
            Item::Step { id, .. } => Some(id.0),
            _ => None,
        })
        .collect();
    assert_eq!(steps, [0, 2, 1], "the narrative order: turn 1 then turn 2");
    assert_eq!(kinds.last(), Some(&"tool"));
    let turns = items
        .iter()
        .filter(|item| matches!(item, Item::Turn { .. }))
        .count();
    assert_eq!(turns, 2);
}

#[test]
fn items_keys_are_unique_and_found_again() {
    let (snapshot, rows) = snapshot_fixture(4, 2, 30);
    let items = build(&snapshot, &rows_map(rows), true, false);
    let keys: HashSet<ItemKey> = items.iter().map(Item::key).collect();
    assert_eq!(keys.len(), items.len(), "every item has its own key");
    for (ix, item) in items.iter().enumerate() {
        assert_eq!(crate::anchor::find(&items, &item.key()), Some(ix));
    }
}

#[test]
fn items_unprepared_rows_are_placeholders_with_an_estimate() {
    let (snapshot, _) = snapshot_fixture(2, 1, 20);
    let items = build(&snapshot, &HashMap::new(), true, false);
    let lines: Vec<u32> = items
        .iter()
        .filter_map(|item| match item {
            Item::Placeholder { lines, .. } => Some(*lines),
            _ => None,
        })
        .collect();
    // A step: added + removed + 7 (10 + 5 + 7).
    assert_eq!(lines, [22, 22]);
    // A rows key that was prepared once keeps the height it had.
    let key = RowsKey::Step {
        step: StepId(0),
        context: DEFAULT_CONTEXT,
    };
    let mut known = HashMap::new();
    known.insert(key, 41);
    let context = HashMap::new();
    let items = items::build(
        &snapshot,
        &HashMap::new(),
        &BuildOptions {
            mode: DiffMode::Unified,
            earlier_collapsed: true,
            noise_expanded: false,
            context: &context,
            known_lines: &known,
        },
    );
    let first = items.iter().find_map(|item| match item {
        Item::Placeholder { lines, .. } => Some(*lines),
        _ => None,
    });
    assert_eq!(first, Some(41));
}

#[test]
fn items_context_changes_the_rows_key() {
    let (snapshot, _) = snapshot_fixture(1, 1, 8);
    let mut context = HashMap::new();
    context.insert(file_key(0), 9);
    let known = HashMap::new();
    let items = items::build(
        &snapshot,
        &HashMap::new(),
        &BuildOptions {
            mode: DiffMode::Unified,
            earlier_collapsed: true,
            noise_expanded: false,
            context: &context,
            known_lines: &known,
        },
    );
    assert!(items.iter().any(|item| matches!(
        item,
        Item::Placeholder {
            rows: RowsKey::Step { context: 9, .. },
            ..
        }
    )));
}

#[test]
fn items_noise_is_folded_to_one_line_at_the_end() {
    let (mut snapshot, rows) = snapshot_fixture(3, 1, 4);
    let mut files = snapshot.files.to_vec();
    files[1].noise = Some(NoiseKind::Lockfile);
    snapshot.files = Arc::from(files);
    let rows = rows_map(rows);
    let folded = build(&snapshot, &rows, true, false);
    assert!(
        !folded
            .iter()
            .any(|item| matches!(item, Item::Step { id, .. } if id.0 == 1)),
        "the noise file's step is not in the narrative"
    );
    assert_eq!(
        folded.last(),
        Some(&Item::Noise {
            count: 1,
            expanded: false
        })
    );
    let shown = build(&snapshot, &rows, true, true);
    assert!(
        shown
            .iter()
            .any(|item| matches!(item, Item::Step { id, .. } if id.0 == 1)),
        "expanded, the step comes back in its turn"
    );
    assert_eq!(
        shown.last(),
        Some(&Item::Noise {
            count: 1,
            expanded: true
        })
    );
}

#[test]
fn items_notices_come_after_the_brief() {
    let (mut snapshot, _) = snapshot_fixture(1, 1, 4);
    snapshot.unclaimed = 3;
    snapshot.base = Some(lectern_services::review::BaseInfo {
        commit: None,
        how: lectern_services::review::BaseHow::CurrentHead,
    });
    snapshot.totals.invalid_paths = Arc::from(vec![Arc::<[u8]>::from(&b"\xff"[..])]);
    let items = build(&snapshot, &HashMap::new(), true, false);
    assert_eq!(items[0], Item::Header);
    assert_eq!(items[1], Item::Brief);
    assert_eq!(items[2], Item::Notice(Notice::BaseCurrentHead));
    assert_eq!(items[3], Item::Notice(Notice::Unclaimed(3)));
    assert_eq!(items[4], Item::Notice(Notice::InvalidPaths(1)));
}

#[test]
fn items_without_turns_are_grouped_by_rule() {
    let (mut snapshot, rows) = snapshot_fixture(3, 1, 4);
    snapshot.turns = None;
    snapshot.degraded = Some(Degraded::UnrecognizedSession);
    let items = build(&snapshot, &rows_map(rows), true, false);
    let kinds: Vec<String> = items
        .iter()
        .map(|item| match item {
            Item::Header => "header".to_owned(),
            Item::Brief => "brief".to_owned(),
            Item::Notice(_) => "notice".to_owned(),
            Item::Group { kind, .. } => format!("group {kind:?}"),
            Item::File { .. } => "file".to_owned(),
            Item::Hunk { .. } => "hunk".to_owned(),
            Item::Row { .. } => "row".to_owned(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        &kinds[..5],
        ["header", "brief", "notice", "group Code", "file"]
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(item, Item::File { .. }))
            .count(),
        3
    );
    // The rows of a file are its final diff.
    assert!(items.iter().all(|item| match item {
        Item::Row { rows, .. } | Item::Hunk { rows, .. } => matches!(rows, RowsKey::Final { .. }),
        _ => true,
    }));
}

#[test]
fn items_grouped_noise_folds_and_expands() {
    let (mut snapshot, _) = snapshot_fixture(2, 1, 4);
    snapshot.turns = None;
    let mut files = snapshot.files.to_vec();
    files[1].noise = Some(NoiseKind::Generated);
    files[1].group = GroupKind::Noise(NoiseKind::Generated);
    snapshot.files = Arc::from(files);
    snapshot.groups = Arc::from(vec![
        lectern_services::review::Group {
            kind: GroupKind::Code,
            files: Arc::from(vec![0]),
        },
        lectern_services::review::Group {
            kind: GroupKind::Noise(NoiseKind::Generated),
            files: Arc::from(vec![1]),
        },
    ]);
    let folded = build(&snapshot, &HashMap::new(), true, false);
    assert_eq!(
        folded
            .iter()
            .filter(|item| matches!(item, Item::File { .. }))
            .count(),
        1
    );
    assert_eq!(
        folded.last(),
        Some(&Item::Noise {
            count: 1,
            expanded: false
        })
    );
    let shown = build(&snapshot, &HashMap::new(), true, true);
    assert_eq!(
        shown
            .iter()
            .filter(|item| matches!(item, Item::File { .. }))
            .count(),
        2
    );
}

#[test]
fn items_earlier_turns_fold_only_when_something_is_new() {
    let (mut snapshot, rows) = snapshot_fixture(3, 3, 4);
    let flags = |snapshot: &mut ReviewSnapshot, flags: [bool; 3]| {
        let turns: Vec<_> = snapshot
            .turns
            .as_deref()
            .unwrap()
            .iter()
            .zip(flags)
            .map(|(turn, flag)| {
                let mut turn = turn.clone();
                turn.after_last_seen = flag;
                turn
            })
            .collect();
        snapshot.turns = Some(Arc::from(turns));
    };
    let rows = rows_map(rows);
    flags(&mut snapshot, [false, false, true]);
    assert_eq!(items::earlier_count(&snapshot), 2);
    let items_folded = build(&snapshot, &rows, true, false);
    assert_eq!(
        items_folded[2],
        Item::EarlierTurns {
            count: 2,
            expanded: false
        }
    );
    assert_eq!(
        items_folded
            .iter()
            .filter(|item| matches!(item, Item::Turn { .. }))
            .count(),
        1
    );
    let items_open = build(&snapshot, &rows, false, false);
    assert_eq!(
        items_open[2],
        Item::EarlierTurns {
            count: 2,
            expanded: true
        }
    );
    assert_eq!(
        items_open
            .iter()
            .filter(|item| matches!(item, Item::Turn { .. }))
            .count(),
        3
    );
    flags(&mut snapshot, [true, true, true]);
    assert_eq!(items::earlier_count(&snapshot), 0, "no seen mark: no fold");
    flags(&mut snapshot, [false, false, false]);
    assert_eq!(items::earlier_count(&snapshot), 0, "nothing new: no fold");
}

#[test]
fn items_a_file_without_a_full_diff_has_a_note() {
    let (mut snapshot, rows) = snapshot_fixture(2, 1, 4);
    let mut files = snapshot.files.to_vec();
    files[1].change.detail = lectern_services::changes::ChangeDetail::Binary {
        old_size: None,
        new_size: Some(9),
    };
    snapshot.files = Arc::from(files);
    snapshot.turns = None;
    let items = build(&snapshot, &rows_map(rows), true, false);
    assert!(items.iter().any(|item| matches!(
        item,
        Item::Incomplete {
            kind: lectern_services::review::IncompleteKind::Binary,
            ..
        }
    )));
}

#[test]
fn items_file_and_rows_helpers() {
    let (snapshot, _) = snapshot_fixture(2, 1, 4);
    let key = RowsKey::Step {
        step: StepId(1),
        context: 3,
    };
    assert_eq!(items::rows_file(&snapshot, &key), Some(file_key(1)));
    let missing = FileKey {
        old: None,
        new: Some(ChangePath(Arc::from(&b"nope"[..]))),
    };
    assert_ne!(items::rows_file(&snapshot, &key), Some(missing));
    let steps = snapshot.turns.as_deref().unwrap()[0]
        .items
        .iter()
        .filter(|item| matches!(item, NarrativeItem::Step(_)))
        .count();
    assert_eq!(steps, 2);
    assert!(!items::step_unaligned(&snapshot, StepId(0)));
}

#[test]
fn items_snapshot() {
    let (snapshot, rows) = snapshot_fixture(3, 2, 20);
    let items = build(&snapshot, &rows_map(rows), true, false);
    // The rows are many; the snapshot keeps the structure and the first and
    // last row of each diff.
    let text = dump(&items, &snapshot);
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| {
            !line.trim_start().starts_with("row ") || line.contains("#0 ") || line.contains("#19 ")
        })
        .collect();
    insta::assert_snapshot!("items_snapshot", kept.join("\n"));
}
