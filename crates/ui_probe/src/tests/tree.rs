//! `Tree::from_records` and the ID grammar, on made-up records.

use gpui::{Bounds, point, px, size};
use lectern_ui::semantic::{Label, NodeId, Record, Role, Semantic, State};

use crate::tree::{Size, Tree, WindowInfo, check_id, warnings};

pub(super) fn window() -> WindowInfo {
    WindowInfo {
        id: 1,
        title: "Lectern".into(),
        size: Size { w: 820.0, h: 520.0 },
        scale: 2.0,
        key: true,
    }
}

#[track_caller]
pub(super) fn record(
    semantic: Semantic,
    parent: Option<usize>,
    visible: bool,
    at: (f32, f32, f32, f32),
) -> Record {
    Record {
        semantic,
        bounds: Bounds {
            origin: point(px(at.0), px(at.1)),
            size: size(px(at.2), px(at.3)),
        },
        visible,
        parent,
        declared_at: std::panic::Location::caller(),
    }
}

fn files() -> NodeId {
    NodeId::new("review", "files", None)
}

/// A list with two items, the first selected and carrying an attribute, and
/// a sibling panel after it.
fn sample() -> Vec<Record> {
    let item = |key: &str, selected: bool| {
        Semantic::new(files().item(key), Role::ListItem)
            .label(&label_text(key))
            .state(State {
                selected,
                ..State::default()
            })
    };
    vec![
        record(
            Semantic::new(files(), Role::List),
            None,
            true,
            (12.0, 40.0, 240.0, 400.0),
        ),
        record(
            item("src/main.rs", true).attr("seen", "true"),
            Some(0),
            true,
            (12.0, 48.0, 240.0, 22.0),
        ),
        record(
            item("src/lib.rs", false),
            Some(0),
            false,
            (12.0, 480.0, 240.0, 22.0),
        ),
        record(
            Semantic::new(NodeId::new("review", "diff", None), Role::Diff),
            None,
            true,
            (260.0, 40.0, 548.0, 400.0),
        ),
    ]
}

fn label_text(text: &str) -> lectern_ui::Text {
    lectern_ui::Text::verbatim(std::sync::Arc::<str>::from(text))
}

#[test]
fn tree_numbers_in_pre_order_and_links_children() {
    let tree = Tree::from_records(window(), sample());
    let shape: Vec<_> = tree
        .nodes
        .iter()
        .map(|n| (n.n, n.parent, n.children.clone()))
        .collect();
    assert_eq!(
        shape,
        [
            (0, None, vec![1, 4]),
            (1, Some(0), vec![2, 3]),
            (2, Some(1), vec![]),
            (3, Some(1), vec![]),
            (4, Some(0), vec![]),
        ]
    );
    assert_eq!(tree.nodes[2].id, "review.files.item[src/main.rs]");
    assert_eq!(tree.nodes[4].id, "review.diff");
}

#[test]
fn tree_renumbers_records_that_are_not_in_pre_order() {
    // Records: a, b, a's child. Pre-order is a, a's child, b.
    let a = NodeId::new("t", "a", None);
    let b = NodeId::new("t", "b", None);
    let records = vec![
        record(
            Semantic::new(a.clone(), Role::Group),
            None,
            true,
            (0.0, 0.0, 1.0, 1.0),
        ),
        record(
            Semantic::new(b.clone(), Role::Group),
            None,
            true,
            (0.0, 0.0, 1.0, 1.0),
        ),
        record(
            Semantic::new(a.child("c"), Role::Text),
            Some(0),
            true,
            (0.0, 0.0, 1.0, 1.0),
        ),
    ];
    let tree = Tree::from_records(window(), records);
    let ids: Vec<_> = tree.nodes.iter().map(|n| (n.n, n.id.as_str())).collect();
    assert_eq!(
        ids,
        [(0, "window[1]"), (1, "t.a"), (2, "t.a.c"), (3, "t.b")]
    );
    assert_eq!(tree.nodes[0].children, [1, 3]);
    assert_eq!(tree.nodes[1].children, [2]);
    assert_eq!(tree.nodes[2].parent, Some(1));
}

#[test]
fn tree_hangs_a_record_with_an_unknown_parent_under_the_window() {
    let records = vec![record(
        Semantic::new(NodeId::new("t", "a", None), Role::Group),
        Some(7),
        true,
        (0.0, 0.0, 1.0, 1.0),
    )];
    let tree = Tree::from_records(window(), records);
    assert_eq!(tree.nodes[1].parent, Some(0));
    assert_eq!(tree.nodes[0].children, [1]);
}

#[test]
fn tree_synthesises_the_window_root() {
    let tree = Tree::from_records(window(), Vec::new());
    assert_eq!(tree.schema, "ui-tree/1");
    assert_eq!(tree.nodes.len(), 1);
    let root = &tree.nodes[0];
    assert_eq!(
        (root.n, root.id.as_str(), root.role),
        (0, "window[1]", "window")
    );
    assert_eq!(
        root.label.as_ref().map(|l| l.text.as_str()),
        Some("Lectern")
    );
    assert_eq!(
        (root.bounds.x, root.bounds.y, root.bounds.w, root.bounds.h),
        (0.0, 0.0, 820.0, 520.0)
    );
    assert!(root.visible && root.parent.is_none() && root.children.is_empty());
}

#[test]
fn tree_carries_visible_state_label_and_attrs() {
    let tree = Tree::from_records(window(), sample());
    let first = &tree.nodes[2];
    assert!(first.visible && first.state.selected);
    assert_eq!(first.attrs.get("seen").map(String::as_str), Some("true"));
    assert_eq!(
        first.label.as_ref().map(|l| l.text.as_str()),
        Some("src/main.rs")
    );
    assert_eq!(first.label.as_ref().and_then(|l| l.key), None);
    // A container has no label; a scrolled-out row is not visible.
    assert!(tree.nodes[1].label.is_none());
    assert!(!tree.nodes[3].visible);
    assert_eq!(tree.nodes[3].bounds.y, 480.0);
}

#[test]
fn tree_carries_the_fluent_key() {
    let label = Label {
        key: Some("common-ok"),
        text: "OK".into(),
    };
    let mut semantic = Semantic::new(NodeId::new("t", "ok", None), Role::Button);
    semantic.label = Some(label);
    let tree = Tree::from_records(
        window(),
        vec![record(semantic, None, true, (0.0, 0.0, 1.0, 1.0))],
    );
    let label = tree.nodes[1].label.as_ref().unwrap();
    assert_eq!((label.key, label.text.as_str()), (Some("common-ok"), "OK"));
}

#[test]
fn tree_rounds_lengths_to_two_decimals() {
    let tree = Tree::from_records(
        window(),
        vec![record(
            Semantic::new(NodeId::new("t", "a", None), Role::Group),
            None,
            true,
            (0.1 + 0.2, 1.0 / 3.0, 10.0, 10.0),
        )],
    );
    let b = &tree.nodes[1].bounds;
    assert_eq!((b.x, b.y), (0.3, 0.33));
}

#[test]
fn tree_basic() {
    insta::assert_snapshot!(Tree::from_records(window(), sample()).to_json());
}

#[test]
fn tree_warnings() {
    let a = NodeId::new("t", "a", None);
    let records = vec![
        record(
            Semantic::new(a.clone(), Role::Group),
            None,
            true,
            (0.0, 0.0, 1.0, 1.0),
        ),
        record(
            Semantic::new(a.clone(), Role::Group),
            Some(0),
            true,
            (0.0, 0.0, 1.0, 1.0),
        ),
        record(
            Semantic::new(a, Role::Group),
            Some(0),
            true,
            (0.0, 0.0, 1.0, 1.0),
        ),
    ];
    let tree = Tree::from_records(window(), records);
    // Three nodes share an ID: one warning, and all three are still output.
    assert_eq!(tree.nodes.len(), 4);
    insta::assert_snapshot!(tree.to_json());
}

#[test]
fn check_id_accepts_the_grammar() {
    for id in [
        "window[1]",
        "review",
        "review.files",
        "review.files.item[src/main.rs]",
        "a1-b.c2",
        "list.item[]",
        "list.item[a b]",
        "list.item[a[b]]",
        "list.item[a]].x",
        "list.item[a.b].child",
        "list.item[ünï/文件.rs]",
        "x.item[a].item[b]",
    ] {
        assert!(check_id(id), "{id}");
    }
}

#[test]
fn check_id_rejects_what_breaks_it() {
    for id in [
        "",
        ".",
        "a.",
        ".a",
        "a..b",
        "A",
        "a_b",
        "a b",
        "a[",
        "a[b",
        "a[b]c",
        "a[b]x.c",
        "a[b\nc]",
        "a[b]]x",
        "a.item[x]]y.c",
        "[x]",
    ] {
        assert!(!check_id(id), "{id:?}");
    }
}

#[test]
fn check_id_splits_a_key_at_the_first_bracket_and_dot() {
    // `a[x].b[y]` is two segments; `a[x].b]` is a key `x` then a name `b]`,
    // which is not a name.
    assert!(check_id("a[x].b[y]"));
    assert!(!check_id("a[x].b]"));
}

#[test]
fn warnings_report_bad_and_duplicate_ids_in_order() {
    let found = warnings(["ok", "Bad", "ok", "ok", "Bad", "x"]);
    let found: Vec<_> = found.iter().map(|w| (w.kind, w.id.as_str())).collect();
    assert_eq!(
        found,
        [
            ("bad-id", "Bad"),
            ("duplicate-id", "ok"),
            ("duplicate-id", "Bad"),
        ]
    );
}
