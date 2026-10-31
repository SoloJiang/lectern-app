//! `WorktreesScreen`: the main area of the Worktrees screen (docs/design/
//! worktrees.md「界面」): the Active and Idle groups of worktrees, one row
//! each, over the immutable [`WorktreesSnapshot`] of the shared
//! [`WorktreesModel`]. The toolbar's title, subtitle and Refresh button are
//! here too.
//!
//! The view only reads the snapshot and sends commands (through the model); it
//! does no IO and calls no blocking service method.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, ElementId, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, ScrollHandle,
    StatefulInteractiveElement, Styled, Subscription, Window, canvas, div, px,
};
use lectern_i18n::{Args, Strings, keys};
use lectern_logging::debug;
use lectern_services::project::WorktreeKey;
use lectern_ui::contrib::ToolbarItems;
use lectern_ui::semantic::{Role, Semantic, semantic};
use lectern_ui::{
    AgentDot, Button, ButtonKind, ButtonSize, GroupedList, LoadingPhase, Row, Skeleton, Text,
    TextRole, TextRoleExt, strings,
};

use crate::actions::{
    CONTEXT, OpenSession, OpenWorktree, Refresh, ReviewChanges, SelectFirst, SelectLast,
    SelectNext, SelectPrevious, SwitchTo, ToggleOverlap,
};
use crate::hub::Hub;
use crate::model::{self, Move, WorktreesModel, counts_pending};
use crate::nodes;
use crate::present::{self, End, Part};
use crate::snapshot::{WorktreeRow, WorktreesSnapshot};

/// The main area's padding and the gap between its sections (the same canvas
/// as the project home).
const PADDING_X: Pixels = px(32.0);
const PADDING_TOP: Pixels = px(36.0);
const PADDING_BOTTOM: Pixels = px(48.0);
const SECTION_GAP: Pixels = px(28.0);
/// The gap between the lines of a row.
const LINE_GAP: Pixels = px(3.0);
/// The gap between the parts of a row: `Row`'s own, so that the text starts
/// where the separators do.
const ROW_GAP: Pixels = px(14.0);
/// The gap between the pieces of one line.
const PIECE_GAP: Pixels = px(6.0);
/// The slot of the agent dot: 10pt wide, so that the text begins 40pt from
/// the card's edge (16 + 10 + 14), where `separator_inset` starts the lines.
const DOT_SLOT: Pixels = px(10.0);
const SEPARATOR_INSET: Pixels = px(40.0);
/// The placeholders' blocks.
const BLOCK_WIDE: Pixels = px(72.0);
const BLOCK_NARROW: Pixels = px(48.0);
const BLOCK_HEIGHT: Pixels = px(10.0);

/// What every row of a frame is drawn against.
struct Frame<'a> {
    snapshot: &'a WorktreesSnapshot,
    strings: &'a Strings,
    phase: LoadingPhase,
}

pub struct WorktreesScreen {
    model: Entity<WorktreesModel>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// Where each row was painted in the last frame (window coordinates).
    bounds: Rc<RefCell<HashMap<WorktreeKey, Bounds<Pixels>>>>,
    _observe: Subscription,
}

impl WorktreesScreen {
    /// Main thread; builds the model the page and the toolbar read, too.
    /// Does no IO.
    pub(crate) fn new(hub: Arc<Hub>, cx: &mut Context<Self>) -> WorktreesScreen {
        let model = cx.new(|cx| WorktreesModel::new(hub, cx));
        model::set_current(&model, cx);
        WorktreesScreen {
            _observe: cx.observe(&model, |_, _, cx| cx.notify()),
            model,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            bounds: Rc::default(),
        }
    }

    /// The model (tests).
    #[cfg(test)]
    pub(crate) fn model(&self) -> &Entity<WorktreesModel> {
        &self.model
    }

    /// Scrolls the selected row into view by the bounds of the last frame.
    fn reveal_selection(&mut self, cx: &mut Context<Self>) {
        let wanted = self
            .model
            .update(cx, |model, _| std::mem::take(&mut model.reveal));
        if !wanted {
            return;
        }
        let Some(selected) = self.model.read(cx).selected.clone() else {
            return;
        };
        let Some(item) = self.bounds.borrow().get(&selected).copied() else {
            return;
        };
        let view = self.scroll.bounds();
        let mut offset = self.scroll.offset();
        if item.top() < view.top() {
            offset.y += view.top() - item.top();
        } else if item.bottom() > view.bottom() {
            offset.y -= item.bottom() - view.bottom();
        } else {
            return;
        }
        let farthest = -self.scroll.max_offset().y;
        offset.y = offset.y.max(farthest).min(px(0.0));
        self.scroll.set_offset(offset);
    }

    /// Records where the row is painted, for scrolling it into view.
    fn probe(&self, key: WorktreeKey) -> AnyElement {
        let bounds = self.bounds.clone();
        canvas(
            move |item, _, _| {
                bounds.borrow_mut().insert(key, item);
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    /// A row of a group: the agent's dot, the branch with its marks, the
    /// sessions and counts under it, and the tokens at the end.
    fn row(
        &self,
        row: &WorktreeRow,
        snapshot: &WorktreesSnapshot,
        strings: &Strings,
        phase: LoadingPhase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Row {
        let (label, suffix) = present::head(strings, &row.head);
        let selected = self.model.read(cx).selected.as_ref() == Some(&row.key);
        let round = self.model.read(cx).blink_round(&row.key);

        // The first line: the branch, what is special about its HEAD, the marks.
        let role = if row.prunable {
            TextRole::Secondary
        } else {
            TextRole::Headline
        };
        let mut first = div().flex().items_center().gap(PIECE_GAP).child(
            div()
                .min_w_0()
                .truncate()
                .text_role(role, window, cx)
                .child(label.shared()),
        );
        if let Some(suffix) = suffix {
            first = first.child(
                div()
                    .flex_shrink_0()
                    .text_role(TextRole::Callout, window, cx)
                    .child(suffix.shared()),
            );
        }
        for (which, text) in present::badges(strings, row) {
            first = first.child(semantic(
                Semantic::new(nodes::badge(&row.node, which), Role::Badge).label(&text),
                div()
                    .flex_shrink_0()
                    .text_role(TextRole::Caption, window, cx)
                    .child(text.shared()),
            ));
        }

        // The second line: the sessions, then the counts; a count that has
        // not arrived is a block once the wait is noticed.
        let mut second =
            div()
                .flex()
                .items_center()
                .gap(PIECE_GAP)
                .text_role(TextRole::Callout, window, cx);
        let mut shown = 0;
        for part in present::second_line(strings, snapshot, row) {
            let piece = match part {
                Part::Text(text) => div().truncate().child(text).into_any_element(),
                Part::Pending if phase != LoadingPhase::Nothing => {
                    Skeleton::new(BLOCK_WIDE, BLOCK_HEIGHT).into_any_element()
                }
                Part::Pending => continue,
            };
            if shown > 0 {
                second = second.child(present::SEPARATOR);
            }
            second = second.child(piece);
            shown += 1;
        }

        let lines = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(LINE_GAP)
            .child(first)
            .child(second);

        let mut content = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(ROW_GAP)
            .relative()
            .child(dot_slot(row, round))
            .child(lines);
        let tokens_node = nodes::tokens(&row.node);
        match present::end(row) {
            End::Nothing => {}
            End::Pending => {
                let block = if phase != LoadingPhase::Nothing {
                    Some(Skeleton::new(BLOCK_NARROW, BLOCK_HEIGHT))
                } else {
                    None
                };
                content = content.child(semantic(
                    Semantic::new(tokens_node, Role::Text).attr("tokens", "pending"),
                    div().flex_shrink_0().children(block),
                ));
            }
            End::Tokens { total, more } => {
                let mut end = div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(PIECE_GAP)
                    .text_role(TextRole::Callout, window, cx)
                    .child(present::tokens(strings, total));
                if more > 0 {
                    end = end.child(present::more_mrs(strings, more));
                }
                content = content.child(semantic(
                    Semantic::new(tokens_node, Role::Text).attr("tokens", total.to_string()),
                    end,
                ));
            }
        }
        let content = content.child(self.probe(row.key.clone()));

        let model = self.model.downgrade();
        let key = row.key.clone();
        Row::new(row.node.clone())
            .selected(selected)
            .on_click(move |_, cx| {
                let key = key.clone();
                let _ = model.update(cx, |model, cx| model.open_worktree(key, cx));
            })
            .child(semantic(facts(row, &label, snapshot), content))
    }

    /// One group: the heading and the rows; None when it has none.
    fn group(
        &self,
        id: lectern_ui::semantic::NodeId,
        heading: lectern_i18n::Key,
        rows: &[WorktreeRow],
        frame: &Frame<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<GroupedList> {
        if rows.is_empty() {
            return None;
        }
        let mut group = GroupedList::new(id)
            .header(Text::key(frame.strings, heading))
            .separator_inset(SEPARATOR_INSET);
        for row in rows {
            group =
                group.row(self.row(row, frame.snapshot, frame.strings, frame.phase, window, cx));
        }
        Some(group)
    }
}

/// The slot an agent's dot sits in; empty for a worktree without a session
/// or with an agent that has no dot, so that the text of every row starts at
/// the same place. The dot blinks once when the worktree starts running
/// (`round` counts the times).
fn dot_slot(row: &WorktreeRow, round: u64) -> gpui::Div {
    let slot = div()
        .w(DOT_SLOT)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center();
    let agent = row
        .recent
        .first()
        .and_then(|session| present::dot_agent(&session.agent));
    match agent {
        Some(agent) => {
            let mut dot = AgentDot::new(agent);
            if round > 0 {
                dot = dot.blink(ElementId::NamedInteger(
                    row.node.as_str().to_owned().into(),
                    round,
                ));
            }
            slot.child(dot)
        }
        None => slot,
    }
}

/// What the probe reads off a row, on the node of its content.
fn facts(row: &WorktreeRow, label: &Text, snapshot: &WorktreesSnapshot) -> Semantic {
    let mut facts = Semantic::new(nodes::facts(&row.node), Role::Group)
        .label(label)
        .attr("current", flag(row.is_active))
        .attr("main", flag(row.is_main))
        .attr("locked", flag(row.locked))
        .attr("prunable", flag(row.prunable))
        .attr("sessions", row.sessions.to_string())
        .attr("running", row.running.to_string())
        .attr("uncommitted", present::count_attr(row.changes.uncommitted));
    if snapshot.base.is_some() {
        facts = facts.attr("vs-base", present::count_attr(row.changes.vs_base));
    }
    facts
}

pub(crate) fn flag(set: bool) -> &'static str {
    if set { "true" } else { "false" }
}

/// Nothing is focused, or the focus is on the workspace's root view (its key
/// context is the innermost one of the focus path).
fn focus_is_on_the_root(window: &Window, cx: &App) -> bool {
    window.focused(cx).is_none()
        || window
            .context_stack()
            .last()
            .is_some_and(|context| context.contains("Workspace"))
}

impl Drop for WorktreesScreen {
    fn drop(&mut self) {
        debug!("worktrees view detached"; view = "screen");
    }
}

impl Focusable for WorktreesScreen {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for WorktreesScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let root = div()
            .id("worktrees-screen")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.model.update(cx, |m, cx| m.select_move(Move::Next, cx));
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.model
                    .update(cx, |m, cx| m.select_move(Move::Previous, cx));
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                this.model
                    .update(cx, |m, cx| m.select_move(Move::First, cx));
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.model.update(cx, |m, cx| m.select_move(Move::Last, cx));
            }))
            .on_action(cx.listener(|this, _: &ReviewChanges, _, cx| {
                this.model.update(cx, |m, cx| m.review_changes(cx));
            }))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| {
                this.model.update(cx, |m, _| m.refresh());
            }))
            .on_action(cx.listener(|this, _: &SwitchTo, _, cx| {
                this.model.update(cx, |m, cx| m.switch_to(cx));
            }))
            .on_action(cx.listener(|this, action: &OpenWorktree, _, cx| {
                let key = action.key.clone();
                this.model.update(cx, |m, cx| m.open_worktree(key, cx));
            }))
            .on_action(cx.listener(|this, action: &OpenSession, _, cx| {
                let key = action.key.clone();
                this.model.update(cx, |m, cx| m.open_session(key, cx));
            }))
            .on_action(cx.listener(|this, action: &ToggleOverlap, _, cx| {
                let key = action.key.clone();
                this.model.update(cx, |m, cx| m.toggle_overlap(key, cx));
            }));
        // A screen on top keeps the keyboard: when the one that had the focus
        // is popped, `workspace` gives it to its root view, and this screen
        // (which was kept below) takes it back. A focus in the inspector, a
        // panel, an overlay or the sidebar stays where it is.
        if !self.focus.contains_focused(window, cx) && focus_is_on_the_root(window, cx) {
            window.focus(&self.focus, cx);
        }
        let Some(snapshot) = model.read(cx).snapshot.clone() else {
            return root;
        };
        model.update(cx, |model, _| model.frame_drawn());
        self.reveal_selection(cx);
        self.bounds.borrow_mut().clear();

        let strings = strings(cx);
        let phase = model.read(cx).phase(cx);
        let mut column = div()
            .flex()
            .flex_col()
            .gap(SECTION_GAP)
            .px(PADDING_X)
            .pt(PADDING_TOP)
            .pb(PADDING_BOTTOM);
        let total = snapshot.active_rows.len() + snapshot.idle_rows.len();
        if snapshot.folder {
            let text = Text::key(&strings, keys::worktrees::NOT_GIT);
            column = column.child(semantic(
                Semantic::new(nodes::area("empty"), Role::Text).label(&text),
                div()
                    .text_role(TextRole::Secondary, window, cx)
                    .child(text.shared()),
            ));
        } else if total > 0 {
            let frame = Frame {
                snapshot: &snapshot,
                strings: &strings,
                phase,
            };
            let active = self.group(
                nodes::active(),
                keys::worktrees::ACTIVE,
                &snapshot.active_rows,
                &frame,
                window,
                cx,
            );
            let idle = self.group(
                nodes::idle(),
                keys::worktrees::IDLE,
                &snapshot.idle_rows,
                &frame,
                window,
                cx,
            );
            let ready = snapshot.sources.changes.is_some() && !counts_pending(&snapshot);
            let list = Semantic::new(nodes::list(), Role::List)
                .attr("rows", total.to_string())
                .attr("active", snapshot.active_rows.len().to_string())
                .attr("idle", snapshot.idle_rows.len().to_string())
                .attr("ready", flag(ready))
                .attr("phase", phase_attr(phase));
            let groups = div()
                .flex()
                .flex_col()
                .gap(SECTION_GAP)
                .children(active)
                .children(idle);
            column = column.child(semantic(list, groups));
            if total == 1 {
                let text = Text::key(&strings, keys::worktrees::ONLY_ONE);
                column = column.child(semantic(
                    Semantic::new(nodes::area("only-one"), Role::Text).label(&text),
                    div()
                        .text_role(TextRole::Secondary, window, cx)
                        .child(text.shared()),
                ));
            }
        }

        root.child(
            div()
                .id("worktrees-scroll")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(column),
        )
    }
}

fn phase_attr(phase: LoadingPhase) -> &'static str {
    match phase {
        LoadingPhase::Nothing => "nothing",
        LoadingPhase::Skeleton => "skeleton",
        LoadingPhase::Progress => "progress",
    }
}

/// The toolbar of the screen (`Screen::toolbar`, main thread, every frame):
/// the title, the number of Active worktrees and of all, and Refresh.
pub(crate) fn toolbar(cx: &mut App) -> ToolbarItems {
    let strings = strings(cx);
    let model = model::current(cx);
    let subtitle = model.as_ref().and_then(|model| {
        let snapshot = model.read(cx).snapshot.as_ref()?;
        let total = snapshot.active_rows.len() + snapshot.idle_rows.len();
        (!snapshot.folder && total > 0).then(|| {
            Text::fmt(
                &strings,
                keys::worktrees::SUBTITLE,
                &Args::new()
                    .num("active", snapshot.active_rows.len() as f64)
                    .num("total", total as f64),
            )
        })
    });
    let weak = model.map(|model| model.downgrade());
    let refresh = Button::new(
        nodes::refresh(),
        Text::key(&strings, keys::worktrees::REFRESH),
    )
    .kind(ButtonKind::Plain)
    .size(ButtonSize::Toolbar)
    .on_click(move |_, cx| {
        if let Some(model) = weak.as_ref() {
            let _ = model.update(cx, |model, _| model.refresh());
        }
    });
    ToolbarItems {
        title: Text::key(&strings, keys::worktrees::TITLE),
        subtitle,
        controls: vec![refresh.into_any_element()],
    }
}
