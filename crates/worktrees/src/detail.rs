//! `DetailTab`: the inspector's Worktree page (docs/design/worktrees.md
//! 「界面」): the selected worktree's usage, its changes against the base, the
//! files it shares with other worktrees and its recent sessions. It reads the
//! shared [`WorktreesModel`] from the GPUI global `Current` and follows it;
//! it has no keyboard focus, so its clicks call the model's methods directly
//! (the screen handles the same commands as actions).

use std::collections::HashMap;
use std::time::Instant;

use gpui::{
    AnyElement, App, Context, Div, EntityId, InteractiveElement, IntoElement, ParentElement,
    Pixels, Render, StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window, div, px,
};
use lectern_i18n::{Args, Strings, count_compact, keys};
use lectern_logging::debug;
use lectern_services::project::WorktreeKey;
use lectern_services::session::Timestamp;
use lectern_ui::motion::Transition;
use lectern_ui::semantic::{Role, Semantic, semantic};
use lectern_ui::{
    AgentDot, Button, ButtonKind, Disclosure, Expander, LoadingPhase, Row, Skeleton, Text,
    TextRole, TextRoleExt, strings,
};

use crate::model::{self, WorktreesModel};
use crate::nodes;
use crate::present;
use crate::screen::flag;
use crate::snapshot::{
    BranchUsage, MrItem, OverlapRow, Overlaps, RowUsage, SessionRow, WorktreeRow, WorktreesSnapshot,
};
use lectern_services::review::ChangeCount;

/// The page's padding and the gap between its sections.
const PADDING: Pixels = px(12.0);
const SECTION_GAP: Pixels = px(18.0);
/// The gap between a heading and its lines, and between lines.
const HEADING_GAP: Pixels = px(8.0);
const LINE_GAP: Pixels = px(4.0);
/// The gap between the pieces of one line.
const PIECE_GAP: Pixels = px(6.0);
/// One listed path of an open overlap item.
const PATH_HEIGHT: Pixels = px(18.0);
/// The inset of the listed paths from the row's left edge.
const PATH_INSET: Pixels = px(18.0);
const BLOCK_WIDE: Pixels = px(120.0);
const BLOCK_HEIGHT: Pixels = px(10.0);
const DOT_SLOT: Pixels = px(10.0);

/// The page: `InspectorTab("worktree")`'s factory builds it; hiding the
/// inspector or switching the page drops it.
pub struct DetailTab {
    model: Option<WeakEntity<WorktreesModel>>,
    /// The model whose changes this page redraws for.
    observing: Option<(EntityId, Subscription)>,
    /// The height and the opacity of each overlap item's paths, by the other
    /// worktree (`Expander::go`).
    open: HashMap<WorktreeKey, (Transition, Transition)>,
}

impl DetailTab {
    pub(crate) fn new(cx: &mut Context<DetailTab>) -> DetailTab {
        debug!("worktrees view attached"; view = "detail");
        DetailTab {
            model: None,
            observing: None,
            open: HashMap::new(),
        }
        .attached(cx)
    }

    fn attached(mut self, cx: &mut Context<DetailTab>) -> DetailTab {
        self.attach(cx);
        self
    }

    /// Follows the screen on top: a different model, or none.
    fn attach(&mut self, cx: &mut Context<DetailTab>) {
        let current = model::current(cx);
        let id = current.as_ref().map(|model| model.entity_id());
        if self.observing.as_ref().map(|(seen, _)| *seen) == id {
            return;
        }
        self.observing = current
            .as_ref()
            .map(|model| (model.entity_id(), cx.observe(model, |_, _, cx| cx.notify())));
        self.model = current.map(|model| model.downgrade());
        self.open.clear();
    }

    /// The paths of an overlap item: the height and the opacity the
    /// transitions have now, moved towards the model's `expanded`.
    fn expansion(
        &mut self,
        other: &WorktreeKey,
        lines: usize,
        expanded: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> (Pixels, f32) {
        let target = if expanded {
            PATH_HEIGHT * lines as f32
        } else {
            px(0.0)
        };
        let now = Instant::now();
        let (height, fade) = self.open.entry(other.clone()).or_insert_with(|| {
            // An item that is open when the page first sees it appears open.
            let open = if expanded { 1.0 } else { 0.0 };
            (Transition::new(target.as_f32()), Transition::new(open))
        });
        Expander::go(height, fade, target, now, cx);
        (px(height.tick(now, window)), fade.tick(now, window))
    }
}

impl Drop for DetailTab {
    fn drop(&mut self) {
        debug!("worktrees view detached"; view = "detail");
    }
}

impl Render for DetailTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.attach(cx);
        let strings = strings(cx);
        let model = self.model.as_ref().and_then(|model| model.upgrade());
        let shown = model.as_ref().and_then(|model| {
            let model = model.read(cx);
            let snapshot = model.snapshot.clone()?;
            let row = model.selected_row()?.clone();
            let expanded = model.expanded.clone();
            Some((snapshot, row, expanded))
        });
        let Some((snapshot, row, expanded)) = shown else {
            let text = Text::key(&strings, keys::worktrees::DETAIL_EMPTY);
            return semantic(
                Semantic::new(nodes::detail(), Role::Panel).attr("selected", flag(false)),
                div()
                    .id("worktrees-detail")
                    .size_full()
                    .p(PADDING)
                    .child(semantic(
                        Semantic::new(nodes::detail().child("empty"), Role::Text).label(&text),
                        div()
                            .text_role(TextRole::Secondary, window, cx)
                            .child(text.shared()),
                    )),
            )
            .into_any_element();
        };
        let model = model.expect("a model shows a row");
        let phase = model.read(cx).phase(cx);

        let mut column = div()
            .flex()
            .flex_col()
            .gap(SECTION_GAP)
            .child(title(&strings, &row, window, cx));
        if let Some(section) = usage(&strings, &snapshot, &row, phase, window, cx) {
            column = column.child(section);
        }
        column = column.child(compare(
            &strings, &snapshot, &row, phase, &model, window, cx,
        ));
        if !row.prunable {
            let section = self.overlaps(&strings, &row, phase, &expanded, &model, window, cx);
            column = column.child(section);
        }
        column = column.child(sessions(&strings, &snapshot, &row, &model, window, cx));
        if !row.is_active && !row.prunable {
            let weak = model.downgrade();
            column = column.child(
                div().child(
                    Button::new(
                        nodes::detail_switch(),
                        Text::key(&strings, keys::worktrees::SWITCH),
                    )
                    .kind(ButtonKind::Primary)
                    .on_click(move |_, cx| {
                        let _ = weak.update(cx, |model, cx| model.switch_to(cx));
                    }),
                ),
            );
        }
        semantic(
            Semantic::new(nodes::detail(), Role::Panel).attr("selected", flag(true)),
            div()
                .id("worktrees-detail")
                .size_full()
                .overflow_y_scroll()
                .p(PADDING)
                .child(column),
        )
        .into_any_element()
    }
}

// ------------------------------------------------------------------ sections

/// A heading over a section.
fn heading(text: Text, window: &mut Window, cx: &mut App) -> Div {
    div()
        .text_role(TextRole::Headline, window, cx)
        .child(text.shared())
}

/// A section: its heading, then its lines.
fn section(heading: Div) -> Div {
    div().flex().flex_col().gap(HEADING_GAP).child(heading)
}

/// A small note under a section's lines.
fn note(text: Text, window: &mut Window, cx: &mut App) -> Div {
    div()
        .text_role(TextRole::Caption, window, cx)
        .child(text.shared())
}

/// A line with a name on the left and a figure on the right.
fn figure(name: Text, value: String, window: &mut Window, cx: &mut App) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(PIECE_GAP)
        .text_role(TextRole::Body, window, cx)
        .child(div().min_w_0().truncate().child(name.shared()))
        .child(div().flex_shrink_0().child(value))
}

/// The branch label with its marks, and the root under it.
fn title(strings: &Strings, row: &WorktreeRow, window: &mut Window, cx: &mut App) -> AnyElement {
    let (label, suffix) = present::head(strings, &row.head);
    let mut first = div()
        .flex()
        .items_center()
        .gap(PIECE_GAP)
        .text_role(TextRole::Body, window, cx)
        .child(div().min_w_0().truncate().child(label.shared()));
    if let Some(suffix) = suffix {
        first = first.child(
            div()
                .flex_shrink_0()
                .text_role(TextRole::Callout, window, cx)
                .child(suffix.shared()),
        );
    }
    for (_, text) in present::badges(strings, row) {
        first = first.child(
            div()
                .flex_shrink_0()
                .text_role(TextRole::Caption, window, cx)
                .child(text.shared()),
        );
    }
    let root = Text::verbatim(row.root.to_string_lossy().into_owned());
    let root_line = div()
        .truncate()
        .text_role(TextRole::Secondary, window, cx)
        .child(root.shared());
    semantic(
        Semantic::new(nodes::detail().child("title"), Role::Group).label(&label),
        div()
            .flex()
            .flex_col()
            .gap(LINE_GAP)
            .child(first)
            .child(root_line),
    )
    .into_any_element()
}

/// The usage of the branch or of its merge requests. None when there is no
/// `usage` plugin.
fn usage(
    strings: &Strings,
    snapshot: &WorktreesSnapshot,
    row: &WorktreeRow,
    phase: LoadingPhase,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    let node = nodes::detail_usage();
    let kind = |kind: &'static str| Semantic::new(node.clone(), Role::Group).attr("kind", kind);
    match &row.usage {
        RowUsage::Absent => None,
        RowUsage::Pending => {
            let mut lines = section(heading(
                Text::key(strings, keys::worktrees::USAGE_BRANCH),
                window,
                cx,
            ));
            if phase != LoadingPhase::Nothing {
                lines = lines.child(Skeleton::new(BLOCK_WIDE, BLOCK_HEIGHT));
            }
            Some(semantic(kind("pending"), lines).into_any_element())
        }
        RowUsage::NotCounted => {
            let text = Text::key(strings, keys::worktrees::USAGE_NOT_COUNTED);
            Some(
                semantic(
                    kind("not-counted").label(&text),
                    div()
                        .text_role(TextRole::Secondary, window, cx)
                        .child(text.shared()),
                )
                .into_any_element(),
            )
        }
        RowUsage::Branch(usage) => Some(branch_usage(
            strings,
            kind("branch"),
            usage,
            snapshot.built_at,
            window,
            cx,
        )),
        RowUsage::Mrs(items) => {
            let mut lines = section(heading(
                Text::key(strings, keys::worktrees::USAGE_MR),
                window,
                cx,
            ));
            for item in items.iter() {
                lines = lines.child(mr_item(strings, item, phase, window, cx));
            }
            Some(semantic(kind("mr"), lines).into_any_element())
        }
    }
}

fn branch_usage(
    strings: &Strings,
    spec: Semantic,
    usage: &BranchUsage,
    now: Timestamp,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let total = usage.cumulative.total();
    let mut lines = section(heading(
        Text::key(strings, keys::worktrees::USAGE_BRANCH),
        window,
        cx,
    ))
    .child(
        div()
            .text_role(TextRole::Title3, window, cx)
            .child(present::tokens(strings, total)),
    );
    let tokens = &usage.cumulative;
    for (name, value) in [
        (keys::worktrees::TOKENS_INPUT, tokens.input),
        (keys::worktrees::TOKENS_OUTPUT, tokens.output),
        (keys::worktrees::TOKENS_CACHE_READ, tokens.cache_read),
        (keys::worktrees::TOKENS_CACHE_WRITE, tokens.cache_write),
        (keys::worktrees::TOKENS_REVIEW, usage.review.total()),
    ] {
        lines = lines.child(figure(
            Text::key(strings, name),
            count_compact(value),
            window,
            cx,
        ));
    }
    if let Some(since) = usage.since {
        // Counted back from the snapshot's time: the page has no clock of
        // its own, and the text only changes when the snapshot does.
        let text = Text::fmt(
            strings,
            keys::worktrees::USAGE_SINCE,
            &Args::new().str("time", &present::relative(strings, now, since)),
        );
        lines = lines.child(note(text, window, cx));
    }
    if usage.may_include_inherited {
        lines = lines.child(note(
            Text::key(strings, keys::worktrees::USAGE_INHERITED),
            window,
            cx,
        ));
    }
    if usage.backfilling {
        lines = lines.child(note(
            Text::key(strings, keys::worktrees::USAGE_BACKFILLING),
            window,
            cx,
        ));
    }
    semantic(spec.attr("tokens", total.to_string()), lines).into_any_element()
}

fn mr_item(
    strings: &Strings,
    item: &MrItem,
    phase: LoadingPhase,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let title = Text::verbatim(item.label.title.clone());
    let tokens = item.tokens.map(|tokens| tokens.total());
    let mut spec = Semantic::new(nodes::detail_mr(item.label.mr.number), Role::ListItem)
        .label(&title)
        .attr("shared", item.shared_with.to_string());
    if let Some(tokens) = tokens {
        spec = spec.attr("tokens", tokens.to_string());
    }
    let mut line = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(PIECE_GAP)
        .text_role(TextRole::Body, window, cx)
        .child(div().min_w_0().truncate().child(title.shared()));
    line = match tokens {
        Some(total) => line.child(div().flex_shrink_0().child(present::tokens(strings, total))),
        None if phase != LoadingPhase::Nothing => {
            line.child(Skeleton::new(BLOCK_WIDE, BLOCK_HEIGHT))
        }
        None => line,
    };
    let mut item_column = div().flex().flex_col().gap(LINE_GAP).child(line);
    if item.shared_with > 0 {
        item_column = item_column.child(note(
            Text::fmt(
                strings,
                keys::worktrees::MR_SHARED,
                &Args::new().num("count", item.shared_with),
            ),
            window,
            cx,
        ));
    }
    semantic(spec, item_column).into_any_element()
}

/// The changes against the base (or the uncommitted ones when there is no
/// base), and the button that reviews them.
fn compare(
    strings: &Strings,
    snapshot: &WorktreesSnapshot,
    row: &WorktreeRow,
    phase: LoadingPhase,
    model: &gpui::Entity<WorktreesModel>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let node = nodes::detail_compare();
    if row.prunable {
        let text = Text::key(strings, keys::worktrees::MISSING_DETAIL);
        return semantic(
            Semantic::new(node, Role::Group).label(&text),
            div()
                .text_role(TextRole::Secondary, window, cx)
                .child(text.shared()),
        )
        .into_any_element();
    }
    let head = match &snapshot.base {
        Some(base) => Text::fmt(
            strings,
            keys::worktrees::COMPARE_TITLE,
            &Args::new().str("base", &base.short),
        ),
        None => Text::key(strings, keys::worktrees::COMPARE_HEAD),
    };
    let mut lines = section(heading(head, window, cx));
    let changes = &row.changes;
    let mut rows = Vec::new();
    if snapshot.base.is_some() {
        rows.push((keys::worktrees::COMMITTED, changes.committed));
    }
    rows.push((keys::worktrees::UNCOMMITTED_FILES, changes.uncommitted));
    if snapshot.base.is_some() {
        rows.push((keys::worktrees::TOTAL, changes.vs_base));
    }
    for (key, count) in rows {
        match count {
            ChangeCount::Files(files) => {
                lines = lines.child(
                    div()
                        .text_role(TextRole::Body, window, cx)
                        .child(strings.format(key, &Args::new().num("count", files))),
                );
            }
            ChangeCount::Capped => {
                lines = lines.child(div().text_role(TextRole::Body, window, cx).child(
                    strings.format(keys::worktrees::CAPPED, &Args::new().num("limit", 4000u32)),
                ));
            }
            ChangeCount::Pending if phase != LoadingPhase::Nothing => {
                lines = lines.child(Skeleton::new(BLOCK_WIDE, BLOCK_HEIGHT));
            }
            ChangeCount::Pending | ChangeCount::Unavailable => {}
        }
    }
    if !row.is_active
        && let Some(at) = changes.counted_at
    {
        lines = lines.child(note(
            Text::fmt(
                strings,
                keys::worktrees::COUNTED_AT,
                &Args::new().str("time", &present::relative(strings, snapshot.built_at, at)),
            ),
            window,
            cx,
        ));
    }
    let weak = model.downgrade();
    lines = lines.child(
        div().child(
            Button::new(
                nodes::detail_open(),
                Text::key(strings, keys::worktrees::REVIEW_CHANGES),
            )
            .kind(ButtonKind::Secondary)
            .on_click(move |_, cx| {
                let _ = weak.update(cx, |model, cx| model.review_changes(cx));
            }),
        ),
    );
    let mut spec = Semantic::new(node, Role::Group)
        .attr("uncommitted", present::count_attr(changes.uncommitted));
    if snapshot.base.is_some() {
        spec = spec
            .attr("committed", present::count_attr(changes.committed))
            .attr("total", present::count_attr(changes.vs_base));
    }
    semantic(spec, lines).into_any_element()
}

impl DetailTab {
    /// The files the worktree shares with the others: one item per other
    /// worktree, which opens to list the paths.
    #[allow(
        clippy::too_many_arguments,
        reason = "a render helper: the strings, the row, the phase, the open set, the model and the two contexts are all it needs"
    )]
    fn overlaps(
        &mut self,
        strings: &Strings,
        row: &WorktreeRow,
        phase: LoadingPhase,
        expanded: &std::collections::HashSet<WorktreeKey>,
        model: &gpui::Entity<WorktreesModel>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let node = nodes::detail_overlaps();
        let mut lines = section(heading(
            Text::key(strings, keys::worktrees::OVERLAPS_TITLE),
            window,
            cx,
        ));
        let spec = Semantic::new(node, Role::List);
        match &row.overlaps {
            Overlaps::Pending => {
                if phase != LoadingPhase::Nothing {
                    lines = lines.child(Skeleton::new(BLOCK_WIDE, BLOCK_HEIGHT));
                }
                self.open.clear();
                semantic(spec.attr("pending", flag(true)), lines).into_any_element()
            }
            Overlaps::Unknown => {
                lines = lines.child(
                    div()
                        .text_role(TextRole::Secondary, window, cx)
                        .child(strings.get(keys::worktrees::OVERLAP_UNKNOWN)),
                );
                self.open.clear();
                semantic(spec.attr("unknown", flag(true)), lines).into_any_element()
            }
            Overlaps::List(items) if items.is_empty() => {
                lines = lines.child(
                    div()
                        .text_role(TextRole::Secondary, window, cx)
                        .child(strings.get(keys::worktrees::NO_OVERLAP)),
                );
                self.open.clear();
                semantic(spec.attr("count", "0"), lines).into_any_element()
            }
            Overlaps::List(items) => {
                self.open
                    .retain(|key, _| items.iter().any(|item| item.other == *key));
                for item in items.iter() {
                    let open = expanded.contains(&item.other);
                    lines = lines.child(self.overlap_item(strings, item, open, model, window, cx));
                }
                semantic(spec.attr("count", items.len().to_string()), lines).into_any_element()
            }
        }
    }

    fn overlap_item(
        &mut self,
        strings: &Strings,
        item: &OverlapRow,
        open: bool,
        model: &gpui::Entity<WorktreesModel>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let more = item.files as usize > item.paths.len();
        let lines = item.paths.len() + usize::from(more);
        let (height, fade) = self.expansion(&item.other, lines, open, window, cx);
        let (label, _) = present::head(strings, &item.other_head);
        let files = strings.format(
            keys::worktrees::OVERLAP_FILES,
            &Args::new().num("count", item.files),
        );
        let facts = Semantic::new(nodes::facts(&item.node), Role::Group)
            .label(&label)
            .attr("files", item.files.to_string())
            .attr("expanded", flag(open));

        let mut paths = div().flex().flex_col().pl(PATH_INSET);
        for path in item.paths.iter() {
            paths = paths.child(
                div()
                    .h(PATH_HEIGHT)
                    .truncate()
                    .text_role(TextRole::Code, window, cx)
                    .child(Text::verbatim(path.to_string()).shared()),
            );
        }
        if more {
            let rest = item.files as usize - item.paths.len();
            paths = paths.child(
                div()
                    .h(PATH_HEIGHT)
                    .text_role(TextRole::Callout, window, cx)
                    .child(strings.format(
                        keys::worktrees::OVERLAP_MORE,
                        &Args::new().num("count", rest as f64),
                    )),
            );
        }

        let weak = model.downgrade();
        let key = item.other.clone();
        let header = Row::new(item.node.clone())
            .on_click(move |_, cx| {
                let key = key.clone();
                let _ = weak.update(cx, |model, cx| model.toggle_overlap(key, cx));
            })
            .child(semantic(
                facts,
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(PIECE_GAP)
                    .child(Disclosure::new(fade))
                    .child(div().flex_1().min_w_0().truncate().child(label.shared()))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_role(TextRole::Callout, window, cx)
                            .child(files),
                    ),
            ));
        div()
            .flex()
            .flex_col()
            .child(header)
            .child(Expander::new(item.node.child("paths"), height, fade, paths))
            .into_any_element()
    }
}

/// The recent sessions of the worktree; a click reviews the session.
fn sessions(
    strings: &Strings,
    snapshot: &WorktreesSnapshot,
    row: &WorktreeRow,
    model: &gpui::Entity<WorktreesModel>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let mut lines = section(heading(
        Text::key(strings, keys::worktrees::SESSIONS_TITLE),
        window,
        cx,
    ));
    if row.recent.is_empty() {
        lines = lines.child(
            div()
                .text_role(TextRole::Secondary, window, cx)
                .child(strings.get(keys::worktrees::NO_SESSIONS)),
        );
    }
    for session in row.recent.iter() {
        lines = lines.child(session_row(strings, snapshot, session, model, window, cx));
    }
    semantic(
        Semantic::new(nodes::detail_sessions(), Role::List)
            .attr("count", row.recent.len().to_string()),
        lines,
    )
    .into_any_element()
}

fn session_row(
    strings: &Strings,
    snapshot: &WorktreesSnapshot,
    session: &SessionRow,
    model: &gpui::Entity<WorktreesModel>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let title = match &session.title {
        Some(title) => Text::verbatim(title.clone()),
        None => Text::key(strings, keys::worktrees::UNTITLED_SESSION),
    };
    let when = if session.running {
        strings.format(keys::worktrees::RUNNING, &Args::new().num("count", 1u32))
    } else {
        present::relative(strings, snapshot.built_at, session.last_activity)
    };
    let facts = Semantic::new(nodes::facts(&session.node), Role::Group)
        .label(&title)
        .attr("agent", present::agent_attr(&session.agent))
        .attr("running", flag(session.running));
    let dot = div()
        .w(DOT_SLOT)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center();
    let dot = match present::dot_agent(&session.agent) {
        Some(agent) => dot.child(AgentDot::new(agent)),
        None => dot,
    };
    let weak = model.downgrade();
    let key = session.key.clone();
    Row::new(session.node.clone())
        .on_click(move |_, cx| {
            let key = key.clone();
            let _ = weak.update(cx, |model, cx| model.open_session(key, cx));
        })
        .child(semantic(
            facts,
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(PIECE_GAP)
                .child(dot)
                .child(div().flex_1().min_w_0().truncate().child(title.shared()))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_role(TextRole::Callout, window, cx)
                        .child(when),
                ),
        ))
        .into_any_element()
}
