//! `MessagesModel`: the Messages page of one session (docs/design/conversation.md
//! 「数据结构」). It holds the interest in the session's conversation, the
//! latest snapshot, the list's items and state, and draws the page. It reads
//! snapshots and sends commands; it does no IO and calls no blocking service
//! method (the services' snapshot reads copy an `Arc`).

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnimationExt, AnyElement, App, Context, ElementId, FollowMode, IntoElement, ListAlignment,
    ListOffset, ListState, ParentElement, Pixels, Styled, Task, Window, div, list, px, svg,
};
use lectern_i18n::{Args, Strings, keys};
use lectern_logging::{debug, info, warn};
use lectern_services::conversation::{
    ConversationCommand, ConversationInterest, ConversationSnapshot, EntryKey, HistoryError, Link,
    Load, Passage, PlanStep, StepState,
};
use lectern_services::session::SessionKey;
use lectern_ui::motion::{
    ARRIVE, ARRIVE_OFFSET, Effective, LOADING_PROGRESS_AT, LOADING_SKELETON_AT, Transition,
};
use lectern_ui::semantic::{Role, Semantic, State, semantic};
use lectern_ui::{
    AgentDot, Button, ButtonKind, ButtonSize, Expander, Icon, LoadingPhase, Skeleton, Text,
    TextRole, TextRoleExt, colors, loading_phase, strings,
};

use crate::diag;
use crate::items::{self, Body, Item, ItemKey, Splice};
use crate::nodes;
use crate::plugin::Shared;
use crate::present;
use crate::view::text_line;

/// How far past the visible area the list measures.
const OVERDRAW: Pixels = px(400.0);
/// The page's inner padding and the gaps between its parts.
const PADDING: Pixels = px(12.0);
const GAP: Pixels = px(8.0);
/// The gap between two items of the list.
const ITEM_GAP: Pixels = px(4.0);
/// One step of the plan, the unit of the open plan's height.
const STEP_HEIGHT: Pixels = px(20.0);
const PROMPT_RADIUS: Pixels = px(8.0);

pub(crate) struct MessagesModel {
    shared: Arc<Shared>,
    session: SessionKey,
    /// The session's folder, for the places of review notes.
    cwd: Option<Arc<Path>>,
    /// `Conversations::watch`'s guard: held, the provider keeps this
    /// session's snapshot. None without `dyn Conversations`.
    interest: Option<ConversationInterest>,
    /// When the page started waiting for the history; the loading phases
    /// count from it (the executor's clock, so that tests move it).
    opened_at: Instant,
    snapshot: Option<Arc<ConversationSnapshot>>,
    items: Vec<Item>,
    list: ListState,
    /// The entries that arrived after the page was first drawn, with the
    /// time they did.
    arriving: HashMap<EntryKey, Instant>,
    /// The plan's steps: height and opacity, and whether it is open.
    plan: (Transition, Transition),
    plan_open: bool,
    /// How many times the agent started a turn since the page saw the link
    /// idle: a new round blinks the dot once.
    blink_round: u64,
    /// The items replaced in the list by snapshots after the first one, and
    /// the last stretch (tests read them).
    spliced: usize,
    last_splice: Option<Splice>,
    /// Notifies at the two thresholds of `loading_phase`; dropped when the
    /// history has loaded.
    timers: Option<Task<()>>,
}

impl MessagesModel {
    /// Main thread. Watches the session when `dyn Conversations` is there and
    /// shows its snapshot if the provider has one. Does no IO.
    pub(crate) fn new(
        shared: Arc<Shared>,
        session: SessionKey,
        cx: &mut Context<MessagesModel>,
    ) -> MessagesModel {
        let summary = shared.session_summary(&session);
        let list = ListState::new(0, ListAlignment::Top, OVERDRAW);
        // The list follows the end while the reader is at the end (and starts
        // there); scrolling up stops it, and scrolling back down resumes it.
        list.set_follow_mode(FollowMode::Tail);
        let mut model = MessagesModel {
            shared,
            cwd: summary.map(|summary| summary.cwd),
            session,
            interest: None,
            opened_at: cx.background_executor().now(),
            snapshot: None,
            items: Vec::new(),
            list,
            arriving: HashMap::new(),
            plan: (Transition::new(0.0), Transition::new(0.0)),
            plan_open: false,
            blink_round: 0,
            spliced: 0,
            last_splice: None,
            timers: None,
        };
        model.sync_service(cx);
        model.sync_timers(cx);
        model
    }

    pub(crate) fn session(&self) -> &SessionKey {
        &self.session
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> Option<&Arc<ConversationSnapshot>> {
        self.snapshot.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn spliced(&self) -> (usize, Option<Splice>) {
        (self.spliced, self.last_splice)
    }

    #[cfg(test)]
    pub(crate) fn list(&self) -> &ListState {
        &self.list
    }

    /// The key of the item at `ix` (tests read where the list is).
    #[cfg(test)]
    pub(crate) fn item_key(&self, ix: usize) -> Option<ItemKey> {
        self.items.get(ix).map(|item| item.key.clone())
    }

    #[cfg(test)]
    pub(crate) fn item_count(&self) -> usize {
        self.items.len()
    }

    /// The scope the records about this session are written in.
    fn scope(&self) -> lectern_logging::Scope {
        lectern_logging::scope(diag::session_ctx(&self.session.id))
    }

    /// `conversation shown`: the page was built for a session.
    pub(crate) fn log_shown(&self) {
        let _scope = self.scope();
        let (link, entries, pending) = match &self.snapshot {
            Some(snapshot) => (
                diag::link_name(&snapshot.link),
                snapshot.entries.len(),
                snapshot.permissions.len(),
            ),
            None => ("none", 0, 0),
        };
        info!(
            "conversation shown";
            page = "messages",
            link = link,
            entries = diag::count(entries),
            pending = diag::count(pending)
        );
    }

    // ------------------------------------------------------------ services

    /// `dyn Conversations` came or went: watches the session, or lets go of
    /// it and forgets the snapshot (a provider that comes back counts its
    /// generations again).
    pub(crate) fn sync_service(&mut self, cx: &mut Context<MessagesModel>) {
        let has = self.shared.has_conversations();
        if has && self.interest.is_none() {
            self.interest = self
                .shared
                .with_conversations(|conversations| conversations.watch(&self.session));
            self.refresh(cx);
        } else if !has && self.interest.is_some() {
            self.interest = None;
            self.snapshot = None;
            let old = self.items.len();
            self.items.clear();
            self.list.splice(0..old, 0);
            self.opened_at = cx.background_executor().now();
            self.sync_timers(cx);
        }
        cx.notify();
    }

    /// A `Changed` event for this session: reads the snapshot again unless the
    /// event is older than what is shown.
    pub(crate) fn changed(&mut self, generation: u64, cx: &mut Context<MessagesModel>) {
        if let Some(shown) = &self.snapshot
            && generation < shown.generation
        {
            let _scope = self.scope();
            debug!("snapshot dropped"; generation = generation, shown = shown.generation);
            return;
        }
        self.refresh(cx);
    }

    /// Reads the provider's snapshot and applies it if it is new.
    fn refresh(&mut self, cx: &mut Context<MessagesModel>) {
        let snapshot = self
            .shared
            .with_conversations(|conversations| conversations.snapshot(&self.session))
            .flatten();
        if let Some(snapshot) = snapshot {
            self.apply(snapshot, cx);
        }
    }

    // ------------------------------------------------------- the snapshot

    /// A snapshot arrived. An older one than the one shown is dropped; the
    /// list keeps what did not change and lays out only the stretch that did.
    fn apply(&mut self, snapshot: Arc<ConversationSnapshot>, cx: &mut Context<MessagesModel>) {
        if let Some(shown) = &self.snapshot
            && (snapshot.generation < shown.generation || Arc::ptr_eq(shown, &snapshot))
        {
            return;
        }
        let _scope = self.scope();
        let started = Instant::now();
        let built = items::build(&snapshot);
        if let Some(kind) = built.inconsistency {
            warn!(
                "snapshot inconsistent";
                generation = snapshot.generation,
                kind = kind.name()
            );
        }
        let first = self.snapshot.is_none();
        if let Link::Failed(error) = &snapshot.link
            && self
                .snapshot
                .as_ref()
                .is_none_or(|shown| shown.link != snapshot.link)
        {
            warn!("conversation failed shown"; error = diag::link_error_name(error));
        }
        self.track_blink(&snapshot);
        let new_items = built.items;
        let splice = items::diff(&self.items, &new_items);
        let mut anchor_kept = true;
        if let Some(splice) = splice {
            anchor_kept = self.splice_list(splice, &new_items);
            self.spliced += splice.added;
            self.last_splice = Some(splice);
            if !first {
                let now = Instant::now();
                self.arriving
                    .retain(|_, at| now.duration_since(*at) < ARRIVE.duration * 2);
                for key in items::arrivals(&self.items, &new_items) {
                    self.arriving.insert(key, now);
                }
            }
        }
        let applied = snapshot.generation;
        let entries = snapshot.entries.len();
        self.items = new_items;
        self.snapshot = Some(snapshot);
        self.sync_timers(cx);
        debug!(
            "snapshot applied";
            generation = applied,
            entries = diag::count(entries),
            items = diag::count(self.items.len()),
            spliced = diag::count(splice.map_or(0, |splice| splice.added)),
            anchor_kept = anchor_kept,
            took_us = started.elapsed()
        );
        cx.notify();
    }

    /// A new round of the agent blinks the dot once. The first snapshot does
    /// not.
    fn track_blink(&mut self, snapshot: &ConversationSnapshot) {
        let before = self.snapshot.as_ref().map(|shown| &shown.link);
        if let Some(before) = before
            && !matches!(before, Link::Working)
            && matches!(snapshot.link, Link::Working)
        {
            self.blink_round += 1;
        }
    }

    /// Tells the list that `splice` of the old items became the new ones, and
    /// keeps the reader where they were. gpui moves the position of a splice
    /// before or after the top item; one that contains the top item is found
    /// again by the item's key. When the top item is the earlier-turns button
    /// and it is the one that changed, the turns just loaded come in above
    /// the entry below it, and the reader stays on that entry. Returns
    /// whether the item was found (or nothing had to be found).
    fn splice_list(&mut self, splice: Splice, new_items: &[Item]) -> bool {
        let top = self.list.logical_scroll_top();
        let range: Range<usize> = splice.start..splice.start + splice.removed;
        let in_range = range.contains(&top.item_ix);
        let (anchor_ix, offset_in_item) = match self.items.get(top.item_ix) {
            Some(Item {
                body: Body::Earlier(_),
                ..
            }) => (top.item_ix + 1, Pixels::ZERO),
            _ => (top.item_ix, top.offset_in_item),
        };
        let anchor = self.items.get(anchor_ix).map(|item| item.key.clone());
        self.list.splice(range, splice.added);
        if !in_range || self.list.is_following_tail() {
            return true;
        }
        let found = anchor
            .and_then(|key| new_items.iter().position(|item| item.key == key))
            .map(|ix| ListOffset {
                item_ix: ix,
                offset_in_item,
            });
        match found {
            Some(at) => {
                self.list.scroll_to(at);
                true
            }
            // The item is gone (a live entry the history replaced): the
            // reader goes to the end, where the conversation goes on.
            None => {
                self.list.scroll_to_end();
                false
            }
        }
    }

    /// Starts or stops the wait for the history: the two thresholds of
    /// `loading_phase` each notify once.
    fn sync_timers(&mut self, cx: &mut Context<MessagesModel>) {
        let loading = self
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| matches!(snapshot.load, Load::Loading));
        match (loading, self.timers.is_some()) {
            (true, false) => {
                self.opened_at = self.opened_at.min(cx.background_executor().now());
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(LOADING_SKELETON_AT).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                    cx.background_executor()
                        .timer(LOADING_PROGRESS_AT.saturating_sub(LOADING_SKELETON_AT))
                        .await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                });
                self.timers = Some(task);
            }
            (false, true) => self.timers = None,
            _ => {}
        }
    }

    /// What the placeholders show now.
    pub(crate) fn phase(&self, cx: &App) -> LoadingPhase {
        loading_phase(
            cx.background_executor()
                .now()
                .saturating_duration_since(self.opened_at),
        )
    }

    // ------------------------------------------------------------ commands

    fn send(&self, command: ConversationCommand) {
        self.shared.with_conversations(|conversations| {
            conversations.command(&self.session, command);
        });
    }

    /// The earlier-turns button.
    pub(crate) fn load_earlier(&mut self, cx: &mut Context<MessagesModel>) {
        self.send(ConversationCommand::LoadEarlier);
        self.refresh(cx);
    }

    /// "Dismiss" on the notice about a failed link.
    pub(crate) fn dismiss(&mut self, cx: &mut Context<MessagesModel>) {
        self.send(ConversationCommand::Dismiss);
        self.refresh(cx);
    }

    /// ⌘.: stops the turn that is running; nothing to stop otherwise.
    pub(crate) fn stop(&mut self, cx: &mut Context<MessagesModel>) {
        let _scope = self.scope();
        let working = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| matches!(snapshot.link, Link::Working));
        if !working {
            return;
        }
        info!("stop requested");
        self.send(ConversationCommand::Cancel);
        self.refresh(cx);
    }

    pub(crate) fn toggle_plan(&mut self, cx: &mut Context<MessagesModel>) {
        self.plan_open = !self.plan_open;
        cx.notify();
    }

    // -------------------------------------------------------------- drawing

    /// The whole Messages page: the status line, the list (or what stands in
    /// its place) and the plan.
    pub(crate) fn render_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> AnyElement {
        let strings = strings(cx);
        if !self.shared.has_conversations() {
            return text_line(
                nodes::area("notice"),
                keys::conversation::UNAVAILABLE_SERVICE,
                window,
                cx,
            );
        }
        let mut page = div().size_full().flex().flex_col().gap(GAP);
        if let Some(status) = self.render_status(&strings, window, cx) {
            page = page.child(status);
        }
        let body = self.render_body(window, cx);
        page = page.child(div().flex_1().min_h_0().child(body));
        if let Some(plan) = self.render_plan(&strings, window, cx) {
            page = page.child(plan);
        }
        page.into_any_element()
    }

    /// The agent dot, the link's state and, when the agent reported it, the
    /// context window's occupancy.
    fn render_status(
        &mut self,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> Option<AnyElement> {
        let snapshot = self.snapshot.clone()?;
        let waiting = present::is_waiting(&snapshot.permissions);
        let status = present::status_text(strings, &snapshot.link, waiting);
        let percent = snapshot.context.as_ref().and_then(present::context_percent);
        let mut line = status.shared().to_string();
        if let Some(percent) = percent {
            line.push_str(present::SEPARATOR);
            line.push_str(present::context_text(strings, percent).as_str());
        }
        let mut dot = div().w(px(10.0)).flex_shrink_0();
        if let Some(agent) = present::dot_agent(&snapshot.agent) {
            let mut mark = AgentDot::new(agent);
            if self.blink_round > 0 {
                mark = mark.blink(ElementId::NamedInteger(
                    "conversation-dot".into(),
                    self.blink_round,
                ));
            }
            dot = dot.child(mark);
        }
        let spec = Semantic::new(nodes::area("status"), Role::Text)
            .label(&status)
            .attr("link", diag::link_name(&snapshot.link))
            .attr(
                "context",
                percent.map_or_else(|| "none".to_owned(), |percent| percent.to_string()),
            )
            .attr("agent", present::agent_attr(&snapshot.agent));
        Some(
            semantic(
                spec,
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(GAP)
                    .text_role(TextRole::Secondary, window, cx)
                    .child(dot)
                    .child(line),
            )
            .into_any_element(),
        )
    }

    /// The list, or what stands in its place while the history loads, failed
    /// or is empty.
    fn render_body(&mut self, window: &mut Window, cx: &mut Context<MessagesModel>) -> AnyElement {
        let Some(snapshot) = self.snapshot.clone() else {
            return self.render_loading(window, cx);
        };
        match snapshot.load {
            Load::Loading => self.render_loading(window, cx),
            Load::Failed(HistoryError::Unrecognized) => text_line(
                nodes::area("empty"),
                keys::conversation::UNRECOGNIZED,
                window,
                cx,
            ),
            Load::Failed(HistoryError::NotFound) => text_line(
                nodes::area("empty"),
                keys::conversation::EMPTY_MISSING,
                window,
                cx,
            ),
            Load::Failed(HistoryError::Io) => text_line(
                nodes::area("empty"),
                keys::conversation::ERROR_HISTORY,
                window,
                cx,
            ),
            Load::Ready if self.items.is_empty() => text_line(
                nodes::area("empty"),
                keys::conversation::EMPTY_TURNS,
                window,
                cx,
            ),
            Load::Ready => {
                let model = cx.entity();
                let spec = Semantic::new(nodes::area("list"), Role::List)
                    .attr("entries", snapshot.entries.len().to_string())
                    .attr("earlier", snapshot.earlier.to_string());
                semantic(
                    spec,
                    list(self.list.clone(), move |ix, window, cx| {
                        model.update(cx, |model, cx| model.render_item(ix, window, cx))
                    })
                    .size_full(),
                )
                .into_any_element()
            }
        }
    }

    /// 100 ms of nothing, then skeleton blocks, then the loading message.
    fn render_loading(
        &mut self,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> AnyElement {
        match self.phase(cx) {
            LoadingPhase::Nothing => div().into_any_element(),
            LoadingPhase::Skeleton => div()
                .w_full()
                .flex()
                .flex_col()
                .gap(GAP)
                .child(Skeleton::new(px(220.0), px(14.0)))
                .child(Skeleton::new(px(260.0), px(14.0)))
                .child(Skeleton::new(px(180.0), px(14.0)))
                .into_any_element(),
            LoadingPhase::Progress => text_line(
                nodes::area("empty"),
                keys::conversation::LOADING,
                window,
                cx,
            ),
        }
    }

    /// The plan: a line "Plan 2/5" that opens to the steps.
    fn render_plan(
        &mut self,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> Option<AnyElement> {
        let steps: Arc<[PlanStep]> = self
            .snapshot
            .as_ref()?
            .plan
            .clone()
            .filter(|steps| !steps.is_empty())?;
        let total = steps.len();
        let done = steps
            .iter()
            .filter(|step| step.state == StepState::Completed)
            .count();
        let target = if self.plan_open {
            STEP_HEIGHT * total as f32
        } else {
            px(0.0)
        };
        let now = Instant::now();
        Expander::go(&mut self.plan.0, &mut self.plan.1, target, now, cx);
        let height = px(self.plan.0.tick(now, window));
        let fade = self.plan.1.tick(now, window);
        let palette = *colors(window, cx);
        let rows = steps.iter().map(|step| {
            let mut mark = div().w(px(12.0)).h(px(12.0)).flex_shrink_0();
            if step.state == StepState::Completed {
                mark = mark.child(
                    svg()
                        .path(Icon::Check.path())
                        .size(px(12.0))
                        .text_color(palette.secondary_label),
                );
            }
            let row = div()
                .h(STEP_HEIGHT)
                .flex()
                .flex_row()
                .items_center()
                .gap(GAP)
                .text_role(TextRole::Callout, window, cx)
                .child(mark)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(Text::verbatim(step.text.clone()).shared()),
                );
            if step.state == StepState::InProgress {
                row.text_color(palette.label)
            } else {
                row
            }
        });
        let model = cx.entity();
        let header = Button::new(
            nodes::area("plan").child("toggle"),
            Text::fmt(
                strings,
                keys::conversation::PLAN,
                &Args::new()
                    .num("done", done as f64)
                    .num("total", total as f64),
            ),
        )
        .kind(ButtonKind::Plain)
        .size(ButtonSize::Small)
        .on_click(move |_, cx| model.update(cx, |model, cx| model.toggle_plan(cx)));
        let steps_view = Expander::new(
            nodes::area("plan").child("steps"),
            height,
            fade,
            div().flex().flex_col().children(rows),
        );
        let spec = Semantic::new(nodes::area("plan"), Role::Group)
            .state(State {
                expanded: Some(self.plan_open),
                ..State::default()
            })
            .attr("done", done.to_string())
            .attr("total", total.to_string());
        Some(
            semantic(
                spec,
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(steps_view),
            )
            .into_any_element(),
        )
    }

    /// One row of the list. Called by the list for the rows it lays out.
    fn render_item(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> AnyElement {
        let Some(item) = self.items.get(ix).cloned() else {
            return div().into_any_element();
        };
        let strings = strings(cx);
        let palette = *colors(window, cx);
        let kind = match &item.body {
            Body::Earlier(_) | Body::Failed(_) => None,
            Body::Prompt { .. } => Some("prompt"),
            Body::Notes { .. } => Some("notes"),
            Body::Agent { .. } => Some("agent"),
            Body::Injected { .. } => Some("injected"),
            Body::Tools { .. } => Some("tools"),
            Body::Ended(_) => Some("ended"),
        };
        let mut streaming = false;
        let mut truncated = false;
        let content = match &item.body {
            Body::Earlier(count) => {
                let model = cx.entity();
                Button::new(
                    item.node.clone(),
                    Text::fmt(
                        &strings,
                        keys::conversation::EARLIER,
                        &Args::new().num("count", *count),
                    ),
                )
                .kind(ButtonKind::Plain)
                .size(ButtonSize::Small)
                .on_click(move |_, cx| model.update(cx, |model, cx| model.load_earlier(cx)))
                .into_any_element()
            }
            Body::Prompt { text } => {
                truncated = text.truncated;
                div()
                    .w_full()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(PROMPT_RADIUS)
                    .bg(palette.unemphasized_selected_content_background)
                    .child(self.passage(text, TextRole::Body, &strings, window, cx))
                    .into_any_element()
            }
            Body::Agent {
                text,
                streaming: running,
            } => {
                streaming = *running;
                truncated = text.truncated;
                let mut block =
                    div()
                        .w_full()
                        .child(self.passage(text, TextRole::Body, &strings, window, cx));
                if *running {
                    block = block.text_color(palette.secondary_label);
                }
                block.into_any_element()
            }
            Body::Injected { text } => {
                truncated = text.truncated;
                div()
                    .w_full()
                    .child(self.passage(text, TextRole::Caption, &strings, window, cx))
                    .into_any_element()
            }
            Body::Notes { count, locations } => div()
                .w_full()
                .text_role(TextRole::Caption, window, cx)
                .child(present::notes_line(
                    &strings,
                    *count,
                    locations,
                    self.cwd.as_deref(),
                ))
                .into_any_element(),
            Body::Tools { calls } => div()
                .w_full()
                .text_role(TextRole::Caption, window, cx)
                .child(present::tools_line(&strings, calls))
                .into_any_element(),
            Body::Ended(reason) => div()
                .w_full()
                .text_role(TextRole::Caption, window, cx)
                .child(present::ended_text(&strings, *reason).shared())
                .into_any_element(),
            Body::Failed(error) => self.render_failed(error, &strings, window, cx),
        };
        let content = match kind {
            Some(kind) => semantic(
                Semantic::new(item.node.clone(), Role::ListItem)
                    .attr("kind", kind)
                    .attr("streaming", streaming.to_string())
                    .attr("truncated", truncated.to_string()),
                content,
            )
            .into_any_element(),
            None => content,
        };
        let row = div()
            .w_full()
            .px(PADDING)
            .py(ITEM_GAP)
            .child(content)
            .into_any_element();
        match &item.key {
            ItemKey::Entry(key) if self.is_arriving(key) => {
                let moves = matches!(ARRIVE.effective(cx), Effective::Move(_));
                let id = ElementId::from(&item.node);
                div()
                    .w_full()
                    .child(row)
                    .with_animation(id, ARRIVE.animation(cx), move |el, t| {
                        let offset = if moves {
                            ARRIVE_OFFSET * (1.0 - t)
                        } else {
                            Pixels::ZERO
                        };
                        el.opacity(t).mt(offset)
                    })
                    .into_any_element()
            }
            _ => row,
        }
    }

    /// Whether an entry arrived within the length of the arrival animation
    /// (the animation's state ends with its element; a row scrolled back
    /// into view later must not play it again).
    fn is_arriving(&self, key: &EntryKey) -> bool {
        self.arriving
            .get(key)
            .is_some_and(|at| at.elapsed() < ARRIVE.duration * 2)
    }

    /// The text of a passage, and the line that says it was cut.
    fn passage(
        &self,
        passage: &Passage,
        role: TextRole,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> AnyElement {
        let mut block = div()
            .w_full()
            .flex()
            .flex_col()
            .text_role(role, window, cx)
            .child(Text::verbatim(passage.text.clone()).shared());
        if passage.truncated {
            block = block.child(
                div()
                    .text_role(TextRole::Caption, window, cx)
                    .child(Text::key(strings, keys::conversation::TRUNCATED).shared()),
            );
        }
        block.into_any_element()
    }

    /// The notice at the end of the list: why the link failed and "Dismiss".
    fn render_failed(
        &self,
        error: &lectern_services::conversation::LinkError,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> AnyElement {
        let Some(agent) = self.snapshot.as_ref().map(|snapshot| &snapshot.agent) else {
            return div().into_any_element();
        };
        let (reason, detail) = present::link_error_text(strings, error, agent);
        let model = cx.entity();
        let mut column = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(ITEM_GAP)
            .child(semantic(
                Semantic::new(nodes::area("notice"), Role::Text).label(&reason),
                div()
                    .w_full()
                    .text_role(TextRole::Body, window, cx)
                    .child(reason.shared()),
            ));
        if let Some(detail) = detail {
            column = column.child(semantic(
                Semantic::new(nodes::area("notice").child("detail"), Role::Text).label(&detail),
                div()
                    .w_full()
                    .text_role(TextRole::Secondary, window, cx)
                    .child(detail.shared()),
            ));
        }
        column
            .child(
                div().child(
                    Button::new(
                        nodes::area("notice").child("action"),
                        Text::key(strings, keys::conversation::DISMISS),
                    )
                    .size(ButtonSize::Small)
                    .on_click(move |_, cx| model.update(cx, |model, cx| model.dismiss(cx))),
                ),
            )
            .into_any_element()
    }
}
