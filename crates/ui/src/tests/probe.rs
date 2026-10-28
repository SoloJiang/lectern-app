//! Step 4 (docs/design/ui.md「测试」): `semantic_records_bounds_and_parents`
//! and the other `semantic_*` tests of the recorder, the accessibility
//! mapping, and one snapshot per control of the nodes it declares.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, ElementId, IntoElement, ParentElement, Pixels, Render, Styled,
    TestAppContext, Toggled, VisualTestContext, Window, accesskit, div, point, px,
};
use lectern_i18n::{Locale, Strings, keys};

use crate::semantic::{
    Access, Label, NodeId, Record, Recorder, Role, Semantic, State, accesskit_role, semantic,
    semantic_interactive,
};
use crate::{
    ActionCard, Agent, AgentDot, Appearance, Button, ButtonKind, CircleCheck, Expander,
    GroupedList, Hint, Icon, MenuItem, PopUpButton, Progress, ProgressShape, Row, Segmented,
    SidebarRow, Text, init_fixed, strings,
};

/// A root view that renders whatever the test hands it.
type SceneFn = Box<dyn Fn(&mut Window, &mut App) -> AnyElement>;

struct Scene(SceneFn);

impl Render for Scene {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        (self.0)(window, cx)
    }
}

fn verbatim(text: &str) -> Text {
    Text::verbatim(Arc::<str>::from(text))
}

fn window_of(
    cx: &mut TestAppContext,
    scene: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
) -> &mut VisualTestContext {
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
    let (_, cx) = cx.add_window_view(|_, _| Scene(Box::new(scene)));
    cx.run_until_parked();
    cx
}

/// Draws one frame of the window with the recorder armed for it.
fn record(cx: &mut VisualTestContext) -> Vec<Record> {
    let id = cx.update(|window, _| window.window_handle().window_id());
    Recorder::begin(id);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    Recorder::finish()
}

fn record_scene(
    cx: &mut TestAppContext,
    scene: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
) -> Vec<Record> {
    let cx = window_of(cx, scene);
    record(cx)
}

#[gpui::test]
fn semantic_records_bounds_and_parents(cx: &mut TestAppContext) {
    let list = NodeId::new("probe", "list", None);
    let (a, b) = (list.item("a"), list.item("b"));
    let cx = window_of(cx, {
        let (list, a, b) = (list.clone(), a.clone(), b.clone());
        move |_, _| {
            // 20pt tall and clipped: the first row (44pt) shows, the second
            // starts below the clip.
            div()
                .w(px(200.0))
                .h(px(20.0))
                .overflow_hidden()
                .child(
                    GroupedList::new(list.clone())
                        .row(Row::new(a.clone()).child(verbatim("a").shared()))
                        .row(Row::new(b.clone()).child(verbatim("b").shared())),
                )
                .into_any_element()
        }
    });

    // Nothing is recorded while the recorder is disarmed.
    assert!(!Recorder::is_armed());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(Recorder::finish().is_empty());

    let records = record(cx);
    assert!(!Recorder::is_armed(), "finish disarms");
    let ids: Vec<_> = records
        .iter()
        .map(|r| r.semantic.id.as_str().to_owned())
        .collect();
    assert_eq!(ids, [list.as_str(), a.as_str(), b.as_str()]);
    assert_eq!(records[0].parent, None);
    assert_eq!(records[1].parent, Some(0));
    assert_eq!(records[2].parent, Some(0));

    let at = |x: f32, y: f32| point(px(x), px(y));
    let row_height = records[1].bounds.size.height;
    assert_eq!(records[0].bounds.origin, at(0.0, 0.0));
    assert_eq!(records[0].bounds.size.width, px(200.0));
    assert_eq!(records[1].bounds.origin, at(0.0, 0.0));
    assert_eq!(records[1].bounds.size.width, px(200.0));
    // The rows are 0.5pt apart: the separator between them.
    let separator = px(0.5);
    assert_eq!(
        records[2].bounds.origin,
        at(0.0, (row_height + separator).into())
    );
    assert_eq!(records[2].bounds.size, records[1].bounds.size);
    assert_eq!(
        records[0].bounds.size.height,
        row_height + separator + row_height
    );
    assert!(
        row_height > px(20.0),
        "the second row starts below the clip"
    );

    assert_eq!(
        records.iter().map(|r| r.visible).collect::<Vec<_>>(),
        [true, true, false]
    );
    assert_eq!(records[1].semantic.role, Role::ListItem);
    assert_eq!(records[0].semantic.role, Role::List);
}

#[gpui::test]
fn semantic_records_nothing_when_disarmed(cx: &mut TestAppContext) {
    let cx = window_of(cx, |_, _| {
        semantic(
            Semantic::new(NodeId::new("probe", "box", None), Role::Group),
            div().size(px(10.0)),
        )
        .into_any_element()
    });
    assert!(!Recorder::is_armed());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(Recorder::finish().is_empty());
    assert!(!Recorder::is_armed());
}

#[gpui::test]
fn semantic_nests_three_levels_and_records_the_call_site(cx: &mut TestAppContext) {
    let root = NodeId::new("probe", "outer", None);
    let records = record_scene(cx, {
        let root = root.clone();
        move |_, _| {
            semantic(
                Semantic::new(root.clone(), Role::Panel),
                div()
                    .child(semantic(
                        Semantic::new(root.child("middle"), Role::Group),
                        div().child(semantic(
                            Semantic::new(root.child("inner"), Role::Text),
                            div().size(px(4.0)),
                        )),
                    ))
                    .child(semantic(
                        Semantic::new(root.child("sibling"), Role::Text),
                        div().size(px(4.0)),
                    )),
            )
            .into_any_element()
        }
    });
    // Pre-order, and each parent is the nearest declared ancestor.
    let shape: Vec<_> = records
        .iter()
        .map(|r| (r.semantic.id.as_str().to_owned(), r.parent))
        .collect();
    assert_eq!(
        shape,
        [
            ("probe.outer".to_owned(), None),
            ("probe.outer.middle".to_owned(), Some(0)),
            ("probe.outer.inner".to_owned(), Some(1)),
            ("probe.outer.sibling".to_owned(), Some(0)),
        ]
    );
    // `declared_at` is a call of `semantic(...)` in this file, in the order
    // the closure above nests them (outermost first in source order).
    for record in &records {
        assert_eq!(record.declared_at.file(), file!());
    }
    let lines: Vec<u32> = records.iter().map(|r| r.declared_at.line()).collect();
    assert!(lines.windows(2).all(|w| w[0] != w[1]), "{lines:?}");
    assert_eq!(lines.len(), 4);
}

#[gpui::test]
fn semantic_declared_at_is_the_line_of_the_call(cx: &mut TestAppContext) {
    let expected = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let records = record_scene(cx, {
        let expected = expected.clone();
        move |_, _| {
            expected.set(line!() + 1);
            semantic(
                Semantic::new(NodeId::new("probe", "line", None), Role::Text),
                div().size(px(4.0)),
            )
            .into_any_element()
        }
    });
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].declared_at.file(), file!());
    assert_eq!(records[0].declared_at.line(), expected.get());
}

#[gpui::test]
fn semantic_begin_drops_unclaimed_records_and_other_windows(cx: &mut TestAppContext) {
    let cx = window_of(cx, |_, _| {
        semantic(
            Semantic::new(NodeId::new("probe", "box", None), Role::Group),
            div().size(px(10.0)),
        )
        .into_any_element()
    });
    let id = cx.update(|window, _| window.window_handle().window_id());
    Recorder::begin(id);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    // A second `begin` drops what the first frame recorded.
    Recorder::begin(id);
    assert!(Recorder::is_armed());
    assert!(Recorder::finish().is_empty());

    // Armed for another window: this window records nothing.
    let other = cx.update(|window, _| window.window_handle().window_id());
    let foreign = gpui::WindowId::from(other.as_u64() + 1);
    assert_ne!(other, foreign);
    Recorder::begin(foreign);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(Recorder::finish().is_empty());
}

#[test]
fn semantic_roles_map_to_accessibility_roles() {
    let all = [
        Role::Window,
        Role::Sidebar,
        Role::Toolbar,
        Role::Panel,
        Role::Group,
        Role::List,
        Role::ListItem,
        Role::Tree,
        Role::TreeItem,
        Role::TabBar,
        Role::Tab,
        Role::Button,
        Role::Toggle,
        Role::Text,
        Role::TextField,
        Role::SearchField,
        Role::Link,
        Role::Menu,
        Role::MenuItem,
        Role::Dialog,
        Role::Diff,
        Role::DiffHunk,
        Role::DiffLine,
        Role::Comment,
        Role::Badge,
        Role::Progress,
    ];
    for role in all {
        assert_ne!(
            accesskit_role(role),
            accesskit::Role::GenericContainer,
            "{role:?}: GPUI drops generic containers from the tree"
        );
        assert_ne!(accesskit_role(role), accesskit::Role::Unknown, "{role:?}");
    }
    assert_eq!(accesskit_role(Role::DiffHunk), accesskit::Role::Group);
    assert_eq!(accesskit_role(Role::Toggle), accesskit::Role::CheckBox);
    assert_eq!(accesskit_role(Role::TabBar), accesskit::Role::TabList);
    assert_eq!(accesskit_role(Role::ListItem), accesskit::Role::ListItem);
}

#[test]
fn semantic_access_writes_the_declaration() {
    let strings = Strings::of(Locale::En);
    let text = Text::key(&strings, keys::common::OK);
    let id = NodeId::new("probe", "ok", None);

    let access = Access::of(
        &Semantic::new(id.clone(), Role::Button)
            .label(&text)
            .state(State {
                disabled: true,
                ..State::default()
            }),
    );
    assert_eq!(access.id.as_ref(), "probe.ok");
    assert_eq!(access.role, accesskit::Role::Button);
    assert_eq!(access.label.as_deref(), Some(text.as_str()));
    // Not selectable, not selected, not expandable, not a toggle: nothing said.
    assert_eq!(
        (access.selected, access.expanded, access.toggled),
        (None, None, None)
    );

    let selected = Access::of(&Semantic::new(id.item("a"), Role::ListItem));
    assert_eq!(selected.selected, Some(false), "a list item says it");
    let selected = Access::of(&Semantic::new(id.item("a"), Role::ListItem).state(State {
        selected: true,
        ..State::default()
    }));
    assert_eq!(selected.selected, Some(true));

    let node = Access::of(&Semantic::new(id.clone(), Role::Group).state(State {
        expanded: Some(true),
        checked: Some(false),
        ..State::default()
    }));
    assert_eq!(node.expanded, Some(true));
    assert_eq!(node.toggled, Some(Toggled::False));
    assert_eq!(node.label, None, "a container with no label sets none");
    let checked = Access::of(&Semantic::new(id, Role::Toggle).state(State {
        checked: Some(true),
        ..State::default()
    }));
    assert_eq!(checked.toggled, Some(Toggled::True));
}

#[gpui::test]
fn semantic_interactive_declares_like_semantic(cx: &mut TestAppContext) {
    use gpui::InteractiveElement;
    let records = record_scene(cx, |_, _| {
        semantic_interactive(
            Semantic::new(NodeId::new("probe", "seen", None), Role::Toggle).state(State {
                checked: Some(true),
                ..State::default()
            }),
            div().id("seen").size(px(16.0)),
        )
        .into_any_element()
    });
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].semantic.role, Role::Toggle);
    assert_eq!(records[0].bounds.size.width, px(16.0));
}

#[gpui::test]
fn card_rows_are_16_by_14_and_standalone_rows_are_compact(cx: &mut TestAppContext) {
    let list = NodeId::new("probe", "metrics", None);
    let alone = NodeId::new("probe", "alone", None);
    let records = record_scene(cx, {
        let (list, alone) = (list.clone(), alone.clone());
        move |_, _| {
            div()
                .w(px(300.0))
                .child(
                    GroupedList::new(list.clone())
                        .row(Row::new(list.item("a")).child(verbatim("a").shared())),
                )
                .child(Row::new(alone.clone()).child(verbatim("b").shared()))
                .into_any_element()
        }
    });
    let height_of = |id: &NodeId| {
        records
            .iter()
            .find(|r| r.semantic.id.as_str() == id.as_str())
            .map(|r| r.bounds.size.height)
            .expect("recorded")
    };
    // 13pt text on a 16pt line plus 14pt above and below.
    assert_eq!(height_of(&list.item("a")), px(16.0 + 28.0));
    // A row outside a card is the inspector's: 30pt.
    assert_eq!(height_of(&alone), px(30.0));
}

#[gpui::test]
fn icon_only_button_is_a_28pt_square_with_a_label(cx: &mut TestAppContext) {
    let records = record_scene(cx, |_, cx| {
        let strings = strings(cx);
        div()
            .child(
                Button::icon_only(
                    root().child("back"),
                    Icon::Back,
                    Text::key(&strings, keys::common::OK),
                )
                .on_click(|_, _| {}),
            )
            .child(
                Button::icon_only(
                    root().child("inspector"),
                    Icon::Inspector,
                    verbatim("Inspector"),
                )
                .active(true),
            )
            .child(
                Button::icon_only(root().child("off"), Icon::Inspector, verbatim("Inspector"))
                    .active(false),
            )
            .into_any_element()
    });
    assert_eq!(records.len(), 3);
    for record in &records {
        assert_eq!(record.bounds.size.width, px(28.0));
        assert_eq!(record.bounds.size.height, px(28.0));
    }
    assert_eq!(records[0].semantic.role, Role::Button);
    assert_eq!(
        records[0].semantic.label.as_ref().and_then(|l| l.key),
        Some("common-ok")
    );
    assert_eq!(records[1].semantic.role, Role::Toggle);
    assert_eq!(records[1].semantic.state.checked, Some(true));
    // Off is still a toggle: a probe tells "toggle, off" from a button.
    assert_eq!(records[2].semantic.role, Role::Toggle);
    assert_eq!(records[2].semantic.state.checked, Some(false));
}

/// What a snapshot keeps of a record: no bounds (they depend on fonts) and
/// no call site (it is a line of this crate).
#[derive(Debug)]
#[allow(dead_code, reason = "read through Debug by the snapshots")]
struct Node {
    id: String,
    role: Role,
    label: Option<Label>,
    state: State,
    attrs: Vec<(&'static str, String)>,
    visible: bool,
    parent: Option<usize>,
}

fn nodes(records: Vec<Record>) -> Vec<Node> {
    records
        .into_iter()
        .map(|record| Node {
            id: record.semantic.id.as_str().to_owned(),
            role: record.semantic.role,
            label: record.semantic.label,
            state: record.semantic.state,
            attrs: record
                .semantic
                .attrs
                .into_iter()
                .map(|(key, value)| (key, value.to_string()))
                .collect(),
            visible: record.visible,
            parent: record.parent,
        })
        .collect()
}

fn snapshot_of(
    cx: &mut TestAppContext,
    scene: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
) -> Vec<Node> {
    nodes(record_scene(cx, scene))
}

/// The insta snapshot of a control's nodes, named after the control.
fn check(name: &'static str, nodes: Vec<Node>) {
    insta::with_settings!({ omit_expression => true }, {
        insta::assert_debug_snapshot!(name, nodes);
    });
}

fn root() -> NodeId {
    NodeId::new("snap", "root", None)
}

#[gpui::test]
fn semantic_snapshot_button(cx: &mut TestAppContext) {
    check(
        "button",
        snapshot_of(cx, |window, cx| {
            let strings = strings(cx);
            let _ = window;
            div()
                .child(
                    Button::new(
                        root().child("primary"),
                        Text::key(&strings, keys::common::OK),
                    )
                    .kind(ButtonKind::Primary)
                    .icon(Icon::Check),
                )
                .child(Button::new(root().child("plain"), verbatim("Back")).disabled(true))
                .into_any_element()
        }),
    );
}

#[gpui::test]
fn semantic_snapshot_agent_dot(cx: &mut TestAppContext) {
    check(
        "agent_dot",
        snapshot_of(cx, |_, _| {
            div()
                .child(
                    AgentDot::new(Agent::ClaudeCode)
                        .blink(ElementId::Name("round".into()))
                        .node(root().child("claude")),
                )
                .child(AgentDot::new(Agent::Codex).node(root().child("codex")))
                // Decorative: no node.
                .child(AgentDot::new(Agent::Codex))
                .into_any_element()
        }),
    );
}

#[gpui::test]
fn semantic_snapshot_circle_check(cx: &mut TestAppContext) {
    check(
        "circle_check",
        snapshot_of(cx, |_, _| {
            div()
                .child(CircleCheck::new(root().child("seen"), true))
                .child(CircleCheck::new(root().child("unseen"), false))
                .into_any_element()
        }),
    );
}

#[gpui::test]
fn semantic_snapshot_progress(cx: &mut TestAppContext) {
    check(
        "progress",
        snapshot_of(cx, |_, _| {
            div()
                .child(Progress::new(root().child("bar"), ProgressShape::Bar, 0.4))
                .child(Progress::new(
                    root().child("ring"),
                    ProgressShape::Ring,
                    1.0,
                ))
                .into_any_element()
        }),
    );
}

#[gpui::test]
fn semantic_snapshot_segmented(cx: &mut TestAppContext) {
    check(
        "segmented",
        snapshot_of(cx, |_, cx| {
            let strings = strings(cx);
            let items = vec![
                (root().child("files"), Text::key(&strings, keys::common::OK)),
                (root().child("conversation"), verbatim("Conversation")),
            ];
            Segmented::new(root().child("segments"), items, 1).into_any_element()
        }),
    );
}

#[gpui::test]
fn semantic_snapshot_grouped_list(cx: &mut TestAppContext) {
    check(
        "grouped_list",
        snapshot_of(cx, |_, _| {
            let list = root().child("sessions");
            GroupedList::new(list.clone())
                .header(verbatim("Today"))
                .row(
                    Row::new(list.item("a"))
                        .selected(true)
                        .child(AgentDot::new(Agent::Codex))
                        .child(verbatim("first").shared()),
                )
                .row(Row::new(list.item("b/c.rs")).child(verbatim("second").shared()))
                .into_any_element()
        }),
    );
}

#[gpui::test]
fn semantic_snapshot_expander(cx: &mut TestAppContext) {
    check(
        "expander",
        snapshot_of(cx, |_, _| {
            div()
                .child(Expander::new(
                    root().child("open"),
                    px(40.0),
                    1.0,
                    div().child(verbatim("comment").shared()),
                ))
                .child(Expander::new(
                    root().child("closed"),
                    Pixels::ZERO,
                    0.0,
                    div(),
                ))
                .into_any_element()
        }),
    );
}

#[gpui::test]
fn semantic_snapshot_hint(cx: &mut TestAppContext) {
    check(
        "hint",
        snapshot_of(cx, |_, _| {
            let shown = Instant::now();
            div()
                .child(Hint::new(root().child("hint"), verbatim("saved"), shown))
                // Past its time the hint draws nothing but still declares its node.
                .child(
                    Hint::new(
                        root().child("gone"),
                        verbatim("old"),
                        shown - Duration::from_secs(60),
                    )
                    .duration(Duration::from_millis(1)),
                )
                .into_any_element()
        }),
    );
}

gpui::actions!(lectern_ui_probe_tests, [PickLanguage]);

#[gpui::test]
fn semantic_snapshot_action_card(cx: &mut TestAppContext) {
    check(
        "action_card",
        snapshot_of(cx, |_, cx| {
            let strings = strings(cx);
            div()
                .w(px(320.0))
                .child(
                    ActionCard::new(
                        root().child("open"),
                        Icon::Folder,
                        Text::key(&strings, keys::common::OK),
                        verbatim("Review a project"),
                    )
                    .shortcut(verbatim("Cmd O"))
                    .on_click(|_, _| {}),
                )
                .into_any_element()
        }),
    );
}

#[gpui::test]
fn semantic_snapshot_popup_button(cx: &mut TestAppContext) {
    check(
        "popup_button",
        snapshot_of(cx, |_, cx| {
            let strings = strings(cx);
            let items = vec![
                MenuItem {
                    text: verbatim("English"),
                    action: Box::new(PickLanguage),
                    checked: true,
                },
                MenuItem {
                    text: verbatim("Chinese"),
                    action: Box::new(PickLanguage),
                    checked: false,
                },
            ];
            div()
                .child(
                    PopUpButton::new(
                        root().child("language"),
                        Text::key(&strings, keys::common::OK),
                        verbatim("English"),
                        items,
                    )
                    .value("en".into()),
                )
                .into_any_element()
        }),
    );
}

#[gpui::test]
fn semantic_snapshot_sidebar_row(cx: &mut TestAppContext) {
    check(
        "sidebar_row",
        snapshot_of(cx, |_, _| {
            div()
                .w(px(240.0))
                // Default.
                .child(
                    SidebarRow::new(root().child("recent").item("a"))
                        .child(verbatim("lectern").shared())
                        .child(verbatim("~/workspace").shared()),
                )
                // Selected on the accent.
                .child(
                    SidebarRow::new(root().child("recent").item("b"))
                        .selected(true)
                        .accent()
                        .child(verbatim("other").shared()),
                )
                // Compact with a count.
                .child(
                    SidebarRow::new(root().child("sidebar").item("worktrees"))
                        .compact()
                        .trailing(verbatim("11"))
                        .child(verbatim("Worktrees").shared()),
                )
                .into_any_element()
        }),
    );
}

#[gpui::test]
fn semantic_snapshot_segmented_fill(cx: &mut TestAppContext) {
    check(
        "segmented_fill",
        snapshot_of(cx, |_, _| {
            div()
                .w(px(240.0))
                .child(
                    Segmented::new(
                        root().child("segments"),
                        vec![
                            (root().child("files"), verbatim("Files")),
                            (root().child("conversation"), verbatim("Conversation")),
                        ],
                        0,
                    )
                    .fill(),
                )
                .into_any_element()
        }),
    );
}

/// The pop-up button is 24pt high; the filled segmented control takes the
/// parent's width; the compact sidebar row is 28pt high.
#[gpui::test]
fn second_batch_sizes_follow_the_design(cx: &mut TestAppContext) {
    let records = record_scene(cx, |_, _| {
        div()
            .w(px(240.0))
            .child(PopUpButton::new(
                root().child("language"),
                verbatim("Language"),
                verbatim("English"),
                Vec::new(),
            ))
            .child(
                SidebarRow::new(root().child("compact"))
                    .compact()
                    .child(verbatim("x").shared()),
            )
            .child(
                Segmented::new(
                    root().child("segments"),
                    vec![
                        (root().child("a"), verbatim("A")),
                        (root().child("b"), verbatim("B")),
                    ],
                    0,
                )
                .fill(),
            )
            .into_any_element()
    });
    let bounds_of = |id: &str| {
        records
            .iter()
            .find(|r| r.semantic.id.as_str() == id)
            .map(|r| r.bounds)
            .expect("recorded")
    };
    assert_eq!(bounds_of("snap.root.language").size.height, px(24.0));
    assert_eq!(bounds_of("snap.root.compact").size.height, px(28.0));
    assert_eq!(bounds_of("snap.root.segments").size.width, px(240.0));
    assert_eq!(bounds_of("snap.root.a").size.width, px(118.0));
    assert_eq!(bounds_of("snap.root.b").size.width, px(118.0));
}
