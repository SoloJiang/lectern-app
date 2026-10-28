//! `HomeScreen`: the main area of the project home (docs/design/project_home.md
//! 「界面」): the large title and the three grouped lists Needs Review,
//! Outside Any Session and Recently Done, over the immutable [`HomeSnapshot`].
//!
//! The view only reads the snapshot and sends commands (`Home::dispatch`); it
//! does no IO and calls no blocking service method. It is woken by `Home`
//! (a channel from the executor threads) and swaps the `Arc` it shows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use gpui::{
    AnyElement, Bounds, Context, FocusHandle, Focusable, HighlightStyle, InteractiveElement,
    IntoElement, ParentElement, Pixels, Render, ScrollHandle, StatefulInteractiveElement, Styled,
    StyledText, Task, Window, canvas, div, px, svg,
};
use lectern_i18n::{Args, Strings, keys};
use lectern_logging::debug;
use lectern_services::session::SessionKey;
use lectern_ui::motion::{self, LOADING_PROGRESS_AT, LOADING_SKELETON_AT, Transition};
use lectern_ui::semantic::{Role, Semantic, semantic};
use lectern_ui::{
    AgentDot, Button, ButtonKind, ButtonSize, GroupedList, Icon, LoadingPhase, Progress,
    ProgressShape, Row, Skeleton, SystemColor, Text, TextRole, TextRoleExt, colors, loading_phase,
    strings,
};

use crate::actions::{
    CONTEXT, Open, OpenOutside, OpenSession, SelectFirst, SelectLast, SelectNext, SelectPrevious,
};
use crate::home::Home;
use crate::nodes;
use crate::present::{self, flag};
use crate::snapshot::{Card, HomeSnapshot, OutsideRow, SessionStatus};

/// The main area's padding and the gap between its sections, and between the
/// large title and the sentence under it (the design canvas's Overview).
const PADDING_X: Pixels = px(32.0);
const PADDING_TOP: Pixels = px(36.0);
const PADDING_BOTTOM: Pixels = px(48.0);
const SECTION_GAP: Pixels = px(28.0);
const TITLE_GAP: Pixels = px(4.0);
/// The gap between the lines of a card.
const LINE_GAP: Pixels = px(3.0);
/// The gap between the parts of a card row: `Row`'s own, so that the text
/// starts where the separators do.
const CARD_GAP: Pixels = px(14.0);
/// The gap inside the group at a card's end (ring, figures, chevron).
const END_GAP: Pixels = px(8.0);
/// The slot of the agent dot: 10pt wide, so that the text begins 40pt from
/// the card's edge (16 + 10 + 14), where `separator_inset` starts the lines.
const DOT_SLOT: Pixels = px(10.0);
const SEPARATOR_INSET: Pixels = px(40.0);
const CHEVRON_SIZE: Pixels = px(14.0);
/// The placeholder's blocks.
const BLOCK_WIDE: Pixels = px(180.0);
const BLOCK_NARROW: Pixels = px(120.0);
const BLOCK_HEIGHT: Pixels = px(10.0);
const NUMBER_WIDTH: Pixels = px(72.0);

/// What is selected: a session's card, or the Outside Any Session row.
/// Kept by identity, not by position, so that a new snapshot that reorders
/// the cards leaves the selection on the same session.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Pick {
    Session(SessionKey),
    Outside,
}

/// The keyboard's moves through the items (「Action 与快捷键」).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Move {
    Next,
    Previous,
    First,
    Last,
}

/// The overview has no first result yet; `since` is when the view saw that
/// first (the executor's clock, so that tests move it).
struct Waiting {
    since: Instant,
    /// Notifies at the two thresholds of `loading_phase`; dropped with this.
    _timers: Task<()>,
}

pub struct HomeScreen {
    home: Arc<Home>,
    snapshot: Option<Arc<HomeSnapshot>>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    pick: Option<Pick>,
    /// Scroll the picked item into view on the next frame.
    reveal: bool,
    /// The progress ring of each Needs Review card, kept across snapshots so that a
    /// ring goes from its old value to the new one.
    rings: HashMap<SessionKey, Transition>,
    waiting: Option<Waiting>,
    /// Where each item was painted in the last frame (window coordinates).
    bounds: Rc<RefCell<HashMap<Pick, Bounds<Pixels>>>>,
    _wake: Task<()>,
}

impl HomeScreen {
    /// Main thread; reads the current snapshot from `home` (a screen pushed
    /// again later shows the latest at once). Does no IO.
    pub(crate) fn new(home: Arc<Home>, cx: &mut Context<Self>) -> HomeScreen {
        let mut wake = home.attach_view("screen");
        let task = cx.spawn(async move |this, cx| {
            while wake.next().await.is_some() {
                // Several wake-ups are one refresh.
                while wake.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        let mut screen = HomeScreen {
            snapshot: home.snapshot(),
            home,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            pick: None,
            reveal: false,
            rings: HashMap::new(),
            waiting: None,
            bounds: Rc::default(),
            _wake: task,
        };
        screen.sync(cx);
        screen
    }

    /// The snapshot changed (or the project closed): swap it in.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let next = self.home.snapshot();
        let same = match (&self.snapshot, &next) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.snapshot = next;
        self.sync(cx);
        cx.notify();
    }

    /// Brings the view's own state in line with the snapshot it now shows.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let mut rings = HashMap::new();
        if let Some(snapshot) = &self.snapshot {
            // Only the Needs Review cards have a ring; a Recently Done row has none.
            for card in snapshot.main.needs_review.iter() {
                let Some(progress) = card.progress else {
                    continue;
                };
                let target = if progress.total == 0 {
                    0.0
                } else {
                    progress.seen as f32 / progress.total as f32
                };
                let ring = match self.rings.remove(&card.session.key) {
                    Some(mut ring) => {
                        ring.go(target, motion::PROGRESS, now, cx);
                        ring
                    }
                    None => Transition::new(target),
                };
                rings.insert(card.session.key.clone(), ring);
            }
        }
        self.rings = rings;

        // A pick whose session is gone (or whose project closed) is dropped.
        if let Some(pick) = &self.pick {
            let kept = self
                .snapshot
                .as_deref()
                .is_some_and(|snapshot| order(snapshot).contains(pick));
            if !kept {
                self.pick = None;
            }
        }

        let waiting = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.main.overview_ready);
        match (waiting, self.waiting.is_some()) {
            (true, false) => {
                let since = cx.background_executor().now();
                let timers = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(LOADING_SKELETON_AT).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                    cx.background_executor()
                        .timer(LOADING_PROGRESS_AT.saturating_sub(LOADING_SKELETON_AT))
                        .await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                });
                self.waiting = Some(Waiting {
                    since,
                    _timers: timers,
                });
            }
            (false, true) => self.waiting = None,
            _ => {}
        }
    }

    /// What the placeholders show now.
    fn phase(&self, cx: &Context<Self>) -> LoadingPhase {
        match &self.waiting {
            Some(waiting) => loading_phase(
                cx.background_executor()
                    .now()
                    .saturating_duration_since(waiting.since),
            ),
            None => LoadingPhase::Nothing,
        }
    }

    // ------------------------------------------------------------ commands

    fn select(&mut self, movement: Move, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let items = order(&snapshot);
        if items.is_empty() {
            return;
        }
        let at = self
            .pick
            .as_ref()
            .and_then(|pick| items.iter().position(|item| item == pick));
        let last = items.len() - 1;
        let index = match (movement, at) {
            (Move::First, _) | (Move::Next, None) => 0,
            (Move::Last, _) | (Move::Previous, None) => last,
            (Move::Next, Some(i)) => (i + 1).min(last),
            (Move::Previous, Some(i)) => i.saturating_sub(1),
        };
        self.pick = Some(items[index].clone());
        self.reveal = true;
        cx.notify();
    }

    /// ↵: opens the picked item; with nothing picked, picks the first.
    fn open(&mut self, cx: &mut Context<Self>) {
        match self.pick.clone() {
            Some(Pick::Session(key)) => self.open_session(key, cx),
            Some(Pick::Outside) => self.open_outside(cx),
            None => self.select(Move::First, cx),
        }
    }

    fn open_session(&mut self, key: SessionKey, cx: &mut Context<Self>) {
        self.pick = Some(Pick::Session(key.clone()));
        self.home.dispatch(present::review_session(key), "session");
        cx.notify();
    }

    fn open_outside(&mut self, cx: &mut Context<Self>) {
        let Some(command) = self.snapshot.as_deref().and_then(present::review_outside) else {
            return;
        };
        self.pick = Some(Pick::Outside);
        self.home.dispatch(command, "outside");
        cx.notify();
    }

    /// Scrolls the picked item into view by the bounds of the last frame.
    fn reveal_pick(&mut self) {
        if !std::mem::take(&mut self.reveal) {
            return;
        }
        let Some(pick) = &self.pick else { return };
        let Some(item) = self.bounds.borrow().get(pick).copied() else {
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

    // ----------------------------------------------------------- rendering

    /// A Needs Review card: the agent's dot, the title with the lines under
    /// it, and the progress at the end.
    fn card_row(
        &mut self,
        card: &Card,
        strings: &Strings,
        now: lectern_services::session::Timestamp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Row {
        let key = card.session.key.clone();
        let pick = Pick::Session(key.clone());
        let selected = self.pick.as_ref() == Some(&pick);
        let node = card.session.node.clone();
        let palette = *colors(window, cx);
        let fraction = card.progress.and_then(|_| {
            self.rings
                .get_mut(&key)
                .map(|ring| ring.tick(Instant::now(), window))
        });

        let title = present::title(strings, &card.session);
        let meta = present::meta(strings, card, now);
        let signals = present::signals(strings, &card.signals);

        let mut text = StyledText::new(meta.text);
        if let Some((range, status)) = meta.status {
            // Running and waiting are the system green; ended stays grey.
            if status != SessionStatus::Ended {
                text = text.with_highlights([(
                    range,
                    HighlightStyle {
                        color: Some(palette.system(SystemColor::Green)),
                        ..HighlightStyle::default()
                    },
                )]);
            }
        }
        let mut lines = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(LINE_GAP)
            .child(
                div()
                    .truncate()
                    .text_role(TextRole::Headline, window, cx)
                    .child(title.shared()),
            )
            .child(
                div()
                    .truncate()
                    .text_role(TextRole::Callout, window, cx)
                    .child(text),
            );
        if let Some(signals) = signals {
            lines = lines.child(
                div()
                    .truncate()
                    .text_role(TextRole::Callout, window, cx)
                    .child(signals),
            );
        }

        let mut content = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(CARD_GAP)
            .child(dot_slot(&card.session.agent))
            .child(lines);
        if let (Some(progress), Some(fraction)) = (card.progress, fraction) {
            let name = Text::fmt(
                strings,
                keys::project_home::PROGRESS,
                &Args::new()
                    .num("seen", progress.seen as f64)
                    .num("total", progress.total as f64),
            );
            let short = strings.format(
                keys::project_home::PROGRESS_SHORT,
                &Args::new()
                    .num("seen", progress.seen as f64)
                    .num("total", progress.total as f64),
            );
            content = content.child(semantic(
                Semantic::new(node.child("seen"), Role::Text).label(&name),
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(END_GAP)
                    .child(Progress::new(
                        node.child("progress"),
                        ProgressShape::Ring,
                        fraction,
                    ))
                    .child(div().text_role(TextRole::Callout, window, cx).child(short))
                    .child(
                        svg()
                            .path(Icon::ChevronRight.path())
                            .size(CHEVRON_SIZE)
                            .flex_shrink_0()
                            .text_color(palette.tertiary_label),
                    ),
            ));
        }

        let this = cx.entity().downgrade();
        Row::new(node.clone())
            .selected(selected)
            .on_click(move |_, cx| {
                let key = key.clone();
                this.update(cx, |this, cx| this.open_session(key, cx)).ok();
            })
            .child(semantic(
                card_facts(&node, card, &title),
                content.relative().child(self.probe(pick)),
            ))
    }

    /// A Recently Done row: the title, and on the right how long ago the
    /// session moved (the result, "Committed", needs git and merge request
    /// data that does not exist yet).
    fn done_row(
        &mut self,
        card: &Card,
        strings: &Strings,
        now: lectern_services::session::Timestamp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Row {
        let key = card.session.key.clone();
        let pick = Pick::Session(key.clone());
        let selected = self.pick.as_ref() == Some(&pick);
        let node = card.session.node.clone();
        let title = present::title(strings, &card.session);
        let when = present::relative(strings, &card.session, now);
        let content = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(END_GAP)
            .relative()
            .child(div().flex_1().min_w_0().truncate().child(title.shared()))
            .child(
                div()
                    .flex_shrink_0()
                    .text_role(TextRole::Callout, window, cx)
                    .child(when),
            )
            .child(self.probe(pick));
        let this = cx.entity().downgrade();
        Row::new(node.clone())
            .selected(selected)
            .on_click(move |_, cx| {
                let key = key.clone();
                this.update(cx, |this, cx| this.open_session(key, cx)).ok();
            })
            .child(semantic(card_facts(&node, card, &title), content))
    }

    /// The Outside Any Session card: how many files changed on the branch and
    /// the first paths on the left, the Review button on the right.
    fn outside_row(
        &mut self,
        snapshot: &HomeSnapshot,
        strings: &Strings,
        phase: LoadingPhase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Row {
        let node = nodes::outside();
        let pick = Pick::Outside;
        let selected = self.pick.as_ref() == Some(&pick);
        let facts = Semantic::new(node.child("facts"), Role::Group);
        let branch = snapshot.branch.as_deref().unwrap_or("HEAD");
        let mut names = None;
        let (facts, text) = match &snapshot.main.outside {
            OutsideRow::Files {
                count,
                names: paths,
            } => {
                let text = Text::fmt(
                    strings,
                    keys::project_home::OUTSIDE_FILES,
                    &Args::new().num("count", *count).str("branch", branch),
                );
                names = present::outside_names(*count, paths);
                (
                    facts.label(&text).attr("files", count.to_string()),
                    Some(text),
                )
            }
            OutsideRow::Capped(n) => {
                let text = Text::fmt(
                    strings,
                    keys::project_home::OUTSIDE_CAPPED,
                    &Args::new().num("count", *n),
                );
                (facts.label(&text).attr("capped", n.to_string()), Some(text))
            }
            OutsideRow::Pending | OutsideRow::Hidden => (facts.attr("pending", flag(true)), None),
        };
        let mut lines = div().flex().flex_col().flex_1().min_w_0().gap(LINE_GAP);
        lines = match text {
            Some(text) => lines.child(div().truncate().child(text.shared())),
            // The number is not known yet: a block once the wait is noticed.
            None if phase != LoadingPhase::Nothing => {
                lines.child(Skeleton::new(NUMBER_WIDTH, BLOCK_HEIGHT))
            }
            None => lines,
        };
        if let Some(names) = names {
            lines = lines.child(
                div()
                    .truncate()
                    .text_role(TextRole::Code, window, cx)
                    .text_color(colors(window, cx).secondary_label)
                    .child(names),
            );
        }
        let this = cx.entity().downgrade();
        let button = Button::new(
            node.child("review"),
            Text::key(strings, keys::project_home::OUTSIDE_REVIEW),
        )
        .kind(ButtonKind::Secondary)
        .size(ButtonSize::Toolbar);
        let content = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(CARD_GAP)
            .relative()
            .child(lines)
            .child(div().flex_shrink_0().child(button))
            .child(self.probe(pick));
        Row::new(node)
            .selected(selected)
            .on_click(move |_, cx| {
                this.update(cx, |this, cx| this.open_outside(cx)).ok();
            })
            .child(semantic(facts, content))
    }

    /// Records where the item's row is painted, for scrolling it into view.
    fn probe(&self, pick: Pick) -> AnyElement {
        let bounds = self.bounds.clone();
        canvas(
            move |item, _, _| {
                bounds.borrow_mut().insert(pick, item);
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    /// The placeholder of Needs Review while the overview has no result.
    fn placeholder(&self, strings: &Strings, phase: LoadingPhase) -> GroupedList {
        let facts = Semantic::new(nodes::area("loading"), Role::Group).attr(
            "phase",
            match phase {
                LoadingPhase::Nothing => "nothing",
                LoadingPhase::Skeleton => "skeleton",
                LoadingPhase::Progress => "progress",
            },
        );
        let blocks = div()
            .flex()
            .flex_col()
            .gap(LINE_GAP * 3.0)
            .child(Skeleton::new(BLOCK_WIDE, BLOCK_HEIGHT))
            .child(Skeleton::new(BLOCK_NARROW, BLOCK_HEIGHT));
        GroupedList::new(nodes::needs_review())
            .header(Text::key(strings, keys::project_home::NEEDS_REVIEW))
            .row(Row::new(nodes::area("loading").child("row")).child(semantic(facts, blocks)))
    }
}

/// The slot an agent's dot sits in; empty for an agent without one (its name
/// leads the first line anyway), so that the text of every card starts at
/// the same place.
fn dot_slot(agent: &lectern_services::session::Agent) -> gpui::Div {
    let slot = div()
        .w(DOT_SLOT)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center();
    match present::dot_agent(agent) {
        Some(agent) => slot.child(AgentDot::new(agent)),
        None => slot,
    }
}

/// What the probe reads off a card, on the node of its content (`Row` has
/// no attributes of its own).
fn card_facts(node: &lectern_ui::semantic::NodeId, card: &Card, title: &Text) -> Semantic {
    let mut facts = Semantic::new(node.child("facts"), Role::Group)
        .label(title)
        .attr("agent", agent_attr(&card.session.agent))
        .attr("critical", card.signals.critical.to_string())
        .attr("changed", card.signals.changed_since_seen.to_string())
        .attr("drifted", flag(card.signals.drifted));
    if let Some(progress) = card.progress {
        facts = facts
            .attr("seen", progress.seen.to_string())
            .attr("total", progress.total.to_string());
    }
    facts
}

/// Nothing is focused, or the focus is on the workspace's root view (its key
/// context is the innermost one of the focus path).
fn focus_is_on_the_root(window: &Window, cx: &gpui::App) -> bool {
    window.focused(cx).is_none()
        || window
            .context_stack()
            .last()
            .is_some_and(|context| context.contains("Workspace"))
}

/// The three lists in the order the keyboard walks them: Needs Review,
/// Outside Any Session (when the project has a base), Recently Done.
fn order(snapshot: &HomeSnapshot) -> Vec<Pick> {
    let main = &snapshot.main;
    let outside = (main.outside != OutsideRow::Hidden).then_some(Pick::Outside);
    main.needs_review
        .iter()
        .map(|card| Pick::Session(card.session.key.clone()))
        .chain(outside)
        .chain(
            main.done
                .iter()
                .map(|card| Pick::Session(card.session.key.clone())),
        )
        .collect()
}

fn agent_attr(agent: &lectern_services::session::Agent) -> String {
    match agent {
        lectern_services::session::Agent::ClaudeCode => "claude-code".to_owned(),
        lectern_services::session::Agent::Codex => "codex".to_owned(),
        lectern_services::session::Agent::Other(_) => "other".to_owned(),
    }
}

impl Drop for HomeScreen {
    fn drop(&mut self) {
        debug!("home view detached"; view = "screen");
    }
}

impl Focusable for HomeScreen {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for HomeScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .id("project-home-screen")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select(Move::Next, cx)))
            .on_action(
                cx.listener(|this, _: &SelectPrevious, _, cx| this.select(Move::Previous, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select(Move::First, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.select(Move::Last, cx)))
            .on_action(cx.listener(|this, _: &Open, _, cx| this.open(cx)))
            .on_action(cx.listener(|this, _: &OpenOutside, _, cx| this.open_outside(cx)))
            .on_action(cx.listener(|this, action: &OpenSession, _, cx| {
                this.open_session(action.key.clone(), cx);
            }));
        // A screen on top keeps the keyboard: when the one that had the focus
        // is popped, `workspace` gives it to its root view, and this screen
        // (which was kept below) takes it back. A focus in the inspector, a
        // panel, an overlay or the sidebar stays where it is.
        if !self.focus.contains_focused(window, cx) && focus_is_on_the_root(window, cx) {
            window.focus(&self.focus, cx);
        }
        let Some(snapshot) = self.snapshot.clone() else {
            return root;
        };
        self.home.frame_drawn(&snapshot);
        self.reveal_pick();
        self.bounds.borrow_mut().clear();

        let strings = strings(cx);
        let phase = self.phase(cx);
        let main = &snapshot.main;
        let now = snapshot.now;
        let empty = snapshot.sidebar.session_rows == 0
            && main.needs_review.is_empty()
            && main.done.is_empty();
        let palette = *colors(window, cx);

        let mut column = div()
            .flex()
            .flex_col()
            .gap(SECTION_GAP)
            .px(PADDING_X)
            .pt(PADDING_TOP)
            .pb(PADDING_BOTTOM);
        if empty {
            let text = Text::key(&strings, keys::project_home::EMPTY);
            column = column.child(semantic(
                Semantic::new(nodes::empty(), Role::Text).label(&text),
                div()
                    .text_role(TextRole::Title1, window, cx)
                    .child(text.shared()),
            ));
        } else {
            let count = main.needs_review.len();
            let title = Text::fmt(
                &strings,
                keys::project_home::TITLE,
                &Args::new().num("count", count as f64),
            );
            let mut heading = div().flex().flex_col().gap(TITLE_GAP).child(semantic(
                Semantic::new(nodes::title(), Role::Text)
                    .label(&title)
                    .attr("count", count.to_string())
                    .attr("ready", flag(main.overview_ready)),
                div()
                    .text_role(TextRole::LargeTitle, window, cx)
                    .child(title.shared()),
            ));
            if let Some(lede) = present::lede(&strings, main) {
                let outside = match &main.outside {
                    OutsideRow::Files { count, .. } => *count,
                    _ => 0,
                };
                heading = heading.child(semantic(
                    Semantic::new(nodes::lede(), Role::Text)
                        .label(&Text::verbatim(lede.clone()))
                        .attr("running", main.running.to_string())
                        .attr("outside", outside.to_string()),
                    div()
                        .text_role(TextRole::Title3, window, cx)
                        .text_color(palette.secondary_label)
                        .child(lede),
                ));
            }
            column = column.child(heading);

            if !main.needs_review.is_empty() {
                let mut group = GroupedList::new(nodes::needs_review())
                    .header(Text::key(&strings, keys::project_home::NEEDS_REVIEW))
                    .separator_inset(SEPARATOR_INSET);
                for card in main.needs_review.iter() {
                    group = group.row(self.card_row(card, &strings, now, window, cx));
                }
                column = column.child(group);
            } else if main.overview_ready {
                let text = Text::key(&strings, keys::project_home::ALL_REVIEWED);
                column = column.child(semantic(
                    Semantic::new(nodes::empty(), Role::Text).label(&text),
                    div()
                        .text_role(TextRole::Secondary, window, cx)
                        .child(text.shared()),
                ));
            } else if phase != LoadingPhase::Nothing {
                column = column.child(self.placeholder(&strings, phase));
            }

            if main.outside != OutsideRow::Hidden {
                let row = self.outside_row(&snapshot, &strings, phase, window, cx);
                column = column.child(
                    GroupedList::new(nodes::outside().child("group"))
                        .header(Text::key(&strings, keys::project_home::OUTSIDE))
                        .row(row),
                );
            }

            if !main.done.is_empty() {
                let mut group = GroupedList::new(nodes::done())
                    .header(Text::key(&strings, keys::project_home::DONE));
                for card in main.done.iter() {
                    group = group.row(self.done_row(card, &strings, now, window, cx));
                }
                column = column.child(group);
            }
        }

        root.child(
            div()
                .id("project-home-scroll")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(column),
        )
    }
}

impl HomeScreen {
    /// The ring of a card: its current value and its target (tests).
    #[cfg(test)]
    pub(crate) fn ring(&self, key: &SessionKey) -> Option<(f32, f32)> {
        self.rings
            .get(key)
            .map(|ring| (ring.value(), ring.target()))
    }

    /// The scroll offset (tests).
    #[cfg(test)]
    pub(crate) fn scrolled(&self) -> Pixels {
        self.scroll.offset().y
    }
}
