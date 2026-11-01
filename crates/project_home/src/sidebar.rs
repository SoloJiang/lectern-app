//! `HomeSidebar`: the project sidebar panel (docs/design/project_home.md「界面」).
//! The scrolling area is one `uniform_list` over the `Project` entries, the
//! snapshot's rows (Sessions, Packages), the heading of the `Views` section and
//! its entries, all 28pt (the sidebar row of `ui`), so a frame builds only the
//! visible rows however many sessions and sub-projects there are. The
//! Sessions list shows the newest three and folds the rest into "N more"
//! (the view's state). The `Bottom` entries (the Usage card) sit below it.
//!
//! Like [`HomeScreen`](crate::screen::HomeScreen) the view only reads the
//! snapshot and the contributed entries and sends commands. The sidebar has
//! no keyboard focus (as in Finder): its rows answer clicks.

use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use gpui::{
    AnyElement, App, Context, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement,
    ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement, Styled, Task,
    UniformListScrollHandle, Window, div, px, relative, svg, uniform_list,
};
use lectern_i18n::{Args, Strings, count_compact, keys};
use lectern_logging::debug;
use lectern_services::session::{SessionKey, Timestamp};
use lectern_services::workspace::{SidebarEntry, SidebarSection};
use lectern_ui::contrib::SidebarItem;
use lectern_ui::motion::{HOVER_IN, HOVER_OUT, Transition};
use lectern_ui::semantic::{NodeId, Role, Semantic, semantic, semantic_interactive};
use lectern_ui::{
    AgentDot, Icon, Palette, SidebarRow as Row, Skeleton, SystemColor, Text, TextRole, TextRoleExt,
    colors, strings,
};

use crate::actions::CONTEXT;
use crate::home::Home;
use crate::nodes;
use crate::present::{self, flag};
use crate::snapshot::{HomeSnapshot, PackageRow, SessionRow, SidebarRow, UsageModel};

/// A row of the scrolling area (the canvas's 28pt) and the inset of the rows
/// from the panel's edge; a heading's text starts `HEADING_INSET` further in,
/// where the text of a row does (the row's own 8pt padding).
const ROW_HEIGHT: Pixels = px(28.0);
const SIDE: Pixels = px(10.0);
const HEADING_INSET: Pixels = px(8.0);
/// The gap under a heading's text; its height is a row's, so the text sits
/// low in it and the section above is set off.
const HEADING_BOTTOM: Pixels = px(4.0);
/// What a nested sub-project adds to the inset per level.
const INDENT: Pixels = px(12.0);
const ICON_SIZE: Pixels = px(16.0);
/// The gap inside a row's content, as `SidebarRow::compact`'s.
const GAP: Pixels = px(8.0);
/// The width the agent dot and the gap after it take: the "N more" row and
/// the third-party agent's name start after it.
const DOT_INDENT: Pixels = px(16.0);
/// The dot at the end of a session's row: green while it runs, orange while
/// a permission request of it waits for the user.
const RUNNING_DOT: Pixels = px(6.0);
const BLOCK_WIDTH: Pixels = px(96.0);
const BLOCK_HEIGHT: Pixels = px(10.0);
/// The Usage card: its margin, padding, radius and the gap between its lines.
const CARD_MARGIN_TOP: Pixels = px(8.0);
const CARD_MARGIN_BOTTOM: Pixels = px(12.0);
const CARD_PADDING: Pixels = px(10.0);
const CARD_RADIUS: Pixels = px(8.0);
const CARD_GAP: Pixels = px(6.0);
const BAR_HEIGHT: Pixels = px(4.0);

/// How many of the newest sessions the list shows before "N more".
pub(crate) const FOLDED: usize = 3;

/// The id of the entry this crate registers and draws as the selectable
/// Overview row, and the Usage entry it draws with the figures.
pub(crate) const OVERVIEW_ENTRY: &str = "overview";
pub(crate) const USAGE_ENTRY: &str = "usage";

/// A contributed entry, as the sidebar keeps it.
#[derive(Clone)]
struct Entry {
    id: String,
    item: Arc<SidebarItem>,
}

/// The contributed entries by section, and the registry's list they came from
/// (compared by pointer to find out that it changed).
struct Entries {
    source: Arc<[SidebarEntry]>,
    project: Vec<Entry>,
    views: Vec<Entry>,
    bottom: Vec<Entry>,
}

impl Entries {
    fn empty() -> Entries {
        Entries {
            source: Arc::from(Vec::new()),
            project: Vec::new(),
            views: Vec::new(),
            bottom: Vec::new(),
        }
    }

    fn from(source: Arc<[SidebarEntry]>) -> Entries {
        let mut entries = Entries {
            source: source.clone(),
            ..Entries::empty()
        };
        for entry in source.iter() {
            // A payload of another type is not ours to draw.
            let Ok(item) = Arc::downcast::<SidebarItem>(entry.payload.0.clone()) else {
                continue;
            };
            let target = match entry.section {
                SidebarSection::Project => &mut entries.project,
                SidebarSection::Views => &mut entries.views,
                SidebarSection::Bottom => &mut entries.bottom,
            };
            target.push(Entry {
                id: entry.id.to_string(),
                item,
            });
        }
        entries
    }
}

/// The last round a running session started, for the dot's blink: a new
/// round is `running` turning true, or `last_activity` moving while running.
#[derive(Clone, Copy)]
struct Round {
    running: bool,
    last: Timestamp,
    count: u64,
}

pub struct HomeSidebar {
    home: Arc<Home>,
    snapshot: Option<Arc<HomeSnapshot>>,
    entries: Entries,
    /// The window shows the project home: the Overview entry is selected.
    on_home: bool,
    scroll: UniformListScrollHandle,
    /// The Sessions list shows all the sessions of the last 14 days, not
    /// only the newest `FOLDED`. The view's state, not the snapshot's.
    expanded: bool,
    rounds: HashMap<SessionKey, Round>,
    /// The sessions that wait for the user, as of the last wake-up
    /// (`Home::waiting`). Read there, not per row.
    waiting: Arc<[SessionKey]>,
    /// How many rows the last frame built (the probe's `visible`, and tests).
    built: Rc<Cell<usize>>,
    _wake: Task<()>,
}

impl HomeSidebar {
    /// Main thread; reads the current snapshot and entries (a panel shown
    /// again later is current at once). Does no IO.
    pub(crate) fn new(home: Arc<Home>, cx: &mut Context<Self>) -> HomeSidebar {
        let mut wake = home.attach_view("sidebar");
        let task = cx.spawn(async move |this, cx| {
            while wake.next().await.is_some() {
                // Several wake-ups are one refresh.
                while wake.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        let mut sidebar = HomeSidebar {
            snapshot: None,
            entries: Entries::empty(),
            on_home: home.on_home_screen(),
            scroll: UniformListScrollHandle::new(),
            expanded: false,
            rounds: HashMap::new(),
            waiting: home.waiting(),
            built: Rc::new(Cell::new(0)),
            home,
            _wake: task,
        };
        sidebar.snapshot = sidebar.home.snapshot();
        sidebar.entries = Entries::from(sidebar.home.sidebar_entries());
        sidebar.track_rounds();
        sidebar
    }

    /// Something changed: the snapshot, the entries or the current screen.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let snapshot = self.home.snapshot();
        let same = match (&self.snapshot, &snapshot) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.snapshot = snapshot;
            self.track_rounds();
            changed = true;
        }
        let source = self.home.sidebar_entries();
        if !Arc::ptr_eq(&source, &self.entries.source) {
            self.entries = Entries::from(source);
            changed = true;
        }
        let on_home = self.home.on_home_screen();
        if on_home != self.on_home {
            self.on_home = on_home;
            changed = true;
        }
        let waiting = self.home.waiting();
        if *waiting != *self.waiting {
            self.waiting = waiting;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// Counts the rounds of the sessions shown.
    fn track_rounds(&mut self) {
        let mut rounds = HashMap::new();
        if let Some(snapshot) = &self.snapshot {
            for group in snapshot.sidebar.groups.iter() {
                for row in group.sessions.iter() {
                    let round = match self.rounds.get(&row.key) {
                        Some(before) => Round {
                            running: row.running,
                            last: row.last_activity,
                            count: before.count
                                + u64::from(
                                    row.running
                                        && (!before.running || before.last != row.last_activity),
                                ),
                        },
                        None => Round {
                            running: row.running,
                            last: row.last_activity,
                            count: 0,
                        },
                    };
                    rounds.insert(row.key.clone(), round);
                }
            }
        }
        self.rounds = rounds;
    }

    fn plan(&self) -> Plan {
        let (rows, sessions) = match &self.snapshot {
            Some(snapshot) => (
                snapshot.sidebar.rows.clone(),
                snapshot.sidebar.session_rows as usize,
            ),
            None => (Arc::from(Vec::new()), 0),
        };
        Plan::new(
            self.entries.project.len(),
            rows,
            sessions,
            self.expanded,
            self.entries.views.len(),
        )
    }

    // ------------------------------------------------------------- rows

    /// The rows of `range` of the scrolling area: `Project` entries, the
    /// snapshot's rows, `Views` entries.
    fn build_rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.built.set(range.len());
        let palette = *colors(window, cx);
        let strings = strings(cx);
        let plan = self.plan();
        range
            .map(|index| match plan.at(index) {
                Line::Project(i) => {
                    let entry = self.entries.project[i].clone();
                    let selected = entry.id == OVERVIEW_ENTRY && self.on_home;
                    self.entry_row("project", &entry, selected, &strings, window, cx)
                }
                Line::Snapshot(row) => self.snapshot_row(row, &strings, &palette, window, cx),
                Line::More => self.more_row(&plan, &strings, cx),
                Line::ViewsHeading => {
                    let text = Text::key(&strings, keys::project_home::VIEWS);
                    heading(nodes::sidebar_views(), text, None, &palette, window, cx)
                }
                Line::View(i) => {
                    let entry = self.entries.views[i].clone();
                    self.entry_row("views", &entry, false, &strings, window, cx)
                }
            })
            .collect()
    }

    fn snapshot_row(
        &self,
        row: SidebarRow,
        strings: &Strings,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(snapshot) = self.snapshot.clone() else {
            return div().h(ROW_HEIGHT).into_any_element();
        };
        let sidebar = &snapshot.sidebar;
        match row {
            SidebarRow::SessionsHeader => {
                let text = Text::key(strings, keys::project_home::SESSIONS);
                heading(nodes::sidebar_sessions(), text, None, palette, window, cx)
            }
            SidebarRow::Session { group, index } => {
                let row = &sidebar.groups[group as usize].sessions[index as usize];
                self.session_row(row, strings, palette, window, cx)
            }
            SidebarRow::SessionsEmpty => {
                let text = Text::key(strings, keys::project_home::SESSIONS_EMPTY);
                semantic(
                    Semantic::new(nodes::sidebar_sessions().child("empty"), Role::Text)
                        .label(&text),
                    div()
                        .flex()
                        .items_center()
                        .h(ROW_HEIGHT)
                        .pl(SIDE + HEADING_INSET)
                        .text_role(TextRole::Secondary, window, cx)
                        .child(text.shared()),
                )
                .into_any_element()
            }
            SidebarRow::PackagesHeader => {
                let text = Text::key(strings, keys::project_home::PACKAGES);
                heading(
                    nodes::sidebar_packages(),
                    text,
                    Some(("counts", flag(sidebar.package_counts))),
                    palette,
                    window,
                    cx,
                )
            }
            SidebarRow::Package(index) => {
                let row = &sidebar.packages[index as usize];
                self.package_row(row, sidebar.package_counts, strings, palette)
            }
            SidebarRow::PackagesSkeleton => div()
                .flex()
                .items_center()
                .h(ROW_HEIGHT)
                .pl(SIDE + HEADING_INSET)
                .child(Skeleton::new(BLOCK_WIDTH, BLOCK_HEIGHT))
                .into_any_element(),
        }
    }

    /// A session: the agent's dot, the title, and a dot at the end: orange
    /// while a permission request of it waits for the user (this wins over
    /// running, a waiting session has an open turn), green while it runs.
    /// Clicking opens its review.
    fn session_row(
        &self,
        row: &SessionRow,
        strings: &Strings,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = present::title(strings, row);
        let waiting = self.waiting.contains(&row.key);
        let facts = Semantic::new(row.node.child("facts"), Role::Group)
            .label(&title)
            .attr("agent", agent_attr(&row.agent))
            .attr("running", flag(row.running))
            .attr("waiting", flag(waiting))
            .attr("pending", flag(row.tail_pending));
        let mut content = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(GAP)
            .text_role(TextRole::Body, window, cx);
        if let Some(agent) = present::dot_agent(&row.agent) {
            let mut dot = AgentDot::new(agent);
            let round = self.rounds.get(&row.key).map_or(0, |round| round.count);
            if row.running && round > 0 {
                dot = dot.blink(ElementId::NamedInteger(
                    SharedString::from(format!("project-home-blink-{}", row.key.id)),
                    round,
                ));
            }
            content = content.child(dot);
        } else {
            // A third party's agent has no dot: its name leads the title.
            content = content.child(
                div()
                    .flex_shrink_0()
                    .text_color(palette.secondary_label)
                    .child(present::agent_name(strings, &row.agent)),
            );
        }
        content = content.child(div().flex_1().min_w_0().truncate().child(title.shared()));
        if waiting {
            let name = Text::key(strings, keys::project_home::WAITING);
            content = content.child(semantic(
                Semantic::new(row.node.child("waiting"), Role::Badge).label(&name),
                div()
                    .flex_shrink_0()
                    .size(RUNNING_DOT)
                    .rounded_full()
                    .bg(palette.system(SystemColor::Orange)),
            ));
        } else if row.running {
            let name = Text::key(strings, keys::project_home::RUNNING);
            content = content.child(semantic(
                Semantic::new(row.node.child("running"), Role::Badge).label(&name),
                div()
                    .flex_shrink_0()
                    .size(RUNNING_DOT)
                    .rounded_full()
                    .bg(palette.system(SystemColor::Green)),
            ));
        }
        let home = self.home.clone();
        let key = row.key.clone();
        inset(
            Row::new(row.node.clone())
                .compact()
                .on_click(move |_, _| {
                    home.dispatch(present::review_session(key.clone()), "session");
                })
                .child(semantic(facts, content)),
        )
        .into_any_element()
    }

    /// The row under the newest sessions that says how many are folded away
    /// (or offers to fold them again when all are shown): unfolds and folds
    /// the list.
    fn more_row(&self, plan: &Plan, strings: &Strings, cx: &mut Context<Self>) -> AnyElement {
        let hidden = plan.sessions - FOLDED.min(plan.sessions);
        let text = if self.expanded {
            Text::key(strings, keys::project_home::SESSIONS_LESS)
        } else {
            Text::fmt(
                strings,
                keys::project_home::SESSIONS_MORE,
                &Args::new().num("count", hidden as f64),
            )
        };
        let node = nodes::sidebar_more();
        let facts = Semantic::new(node.child("facts"), Role::Button)
            .label(&text)
            .attr("hidden", hidden.to_string())
            .attr("expanded", flag(self.expanded));
        let this = cx.entity().downgrade();
        inset(
            Row::new(node)
                .compact()
                .on_click(move |_, cx| {
                    this.update(cx, |this, cx| {
                        this.expanded = !this.expanded;
                        cx.notify();
                    })
                    .ok();
                })
                .child(semantic(
                    facts,
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .pl(DOT_INDENT)
                        .child(text.shared()),
                )),
        )
        .into_any_element()
    }

    /// A sub-project: the folder icon, the name, and today's change count at
    /// the end. Not clickable yet (「本 issue 之外」).
    fn package_row(
        &self,
        row: &PackageRow,
        counts: bool,
        strings: &Strings,
        palette: &Palette,
    ) -> AnyElement {
        let mut facts = Semantic::new(row.node.child("facts"), Role::Group)
            .attr("changed", row.changed_today.to_string())
            .attr("depth", row.depth.to_string());
        let mut line = Row::new(row.node.clone()).compact();
        if counts && row.changed_today > 0 {
            // The figure alone is drawn; its meaning is the accessible name.
            let name = Text::fmt(
                strings,
                keys::project_home::CHANGED_TODAY,
                &Args::new().num("count", row.changed_today),
            );
            facts = facts.label(&name);
            line = line.trailing(Text::verbatim(row.changed_today.to_string()));
        }
        let content = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(GAP)
            .child(icon(Icon::Folder, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(row.name.to_string()),
            );
        div()
            .flex()
            .flex_col()
            .w_full()
            .flex_shrink_0()
            .px(SIDE)
            .pl(SIDE + INDENT * f32::from(row.depth))
            .child(line.child(semantic(facts, content)))
            .into_any_element()
    }

    /// A contributed entry: icon, label; the Overview entry is selected on the
    /// home screen; the Usage entry is drawn as the card with the figures.
    fn entry_row(
        &self,
        section: &'static str,
        entry: &Entry,
        selected: bool,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = *colors(window, cx);
        let palette = &palette;
        if section == "bottom" && entry.id == USAGE_ENTRY {
            return self.usage_card(strings, palette, window, cx);
        }
        let node = if entry.id == OVERVIEW_ENTRY {
            nodes::sidebar_overview()
        } else {
            nodes::sidebar_entry(section, &entry.id)
        };
        let label = Text::key(strings, entry.item.label);
        let facts = Semantic::new(node.child("facts"), Role::Text).label(&label);
        let mut content = div().flex().flex_1().min_w_0().items_center().gap(GAP);
        if let Some(item) = entry.item.icon {
            content = content.child(icon(item, palette));
        }
        content = content.child(div().flex_1().min_w_0().truncate().child(label.shared()));
        let home = self.home.clone();
        let command = entry.item.on_select.clone();
        inset(
            Row::new(node)
                .compact()
                .selected(selected)
                .on_click(move |_, _| home.dispatch(command.clone(), "entry"))
                .child(semantic(facts, content)),
        )
        .into_any_element()
    }

    /// The Usage entry as a card: the label and the 7-day tokens, the tightest
    /// limit's bar, and the line naming that limit. The whole card is the
    /// entry's button.
    fn usage_card(
        &self,
        strings: &Strings,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let model: Option<&UsageModel> = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.usage.as_ref());
        let label = Text::key(strings, keys::project_home::USAGE);
        let mut head = div().flex().items_center().gap(CARD_GAP).child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(label.shared()),
        );
        match model {
            None => {}
            Some(UsageModel {
                tokens_7d: Some(tokens),
                ..
            }) => {
                let compact = count_compact(*tokens);
                let text = Text::fmt(
                    strings,
                    keys::project_home::USAGE_7D,
                    &Args::new().str("tokens", &compact),
                );
                head = head.child(
                    div()
                        .ml_auto()
                        .min_w_0()
                        .truncate()
                        .text_color(palette.secondary_label)
                        .child(text.shared()),
                );
            }
            Some(_) => {
                head = head.child(
                    div()
                        .ml_auto()
                        .child(Skeleton::new(BLOCK_WIDTH, BLOCK_HEIGHT)),
                );
            }
        }
        let node = nodes::sidebar_usage();
        let mut spec = Semantic::new(node.clone(), Role::ListItem).attr(
            "tokens",
            model
                .and_then(|model| model.tokens_7d)
                .map_or_else(String::new, |tokens| tokens.to_string()),
        );
        let mut content = div()
            .flex()
            .flex_col()
            .gap(CARD_GAP)
            .w_full()
            .p(CARD_PADDING)
            .rounded(CARD_RADIUS)
            .text_role(TextRole::Callout, window, cx);
        content = content.child(head);
        if let Some(limit) = model.and_then(|model| model.tightest.as_ref()) {
            spec = spec.attr("percent", limit.used_percent.to_string());
            let fraction = f32::from(limit.used_percent) / 100.0;
            // `ui::Progress::Bar` paints the accent; the limit's bar is the
            // system orange (project_home.md「界面」), so it is drawn here.
            let bar = div()
                .w_full()
                .h(BAR_HEIGHT)
                .rounded_full()
                .bg(palette.fill_strong)
                .child(
                    div()
                        .h_full()
                        .w(relative(fraction))
                        .rounded_full()
                        .bg(palette.system(SystemColor::Orange)),
                );
            content = content
                .child(semantic(
                    Semantic::new(node.child("limit"), Role::Progress)
                        .attr("fraction", format!("{fraction:.3}")),
                    bar,
                ))
                .child(
                    div()
                        .truncate()
                        .text_color(palette.secondary_label)
                        .child(present::limit(strings, limit)),
                );
        }
        let project = self
            .snapshot
            .as_deref()
            .map(|snapshot| snapshot.project.clone());
        let home = self.home.clone();
        let card = clickable(
            content,
            &node,
            palette.fill,
            palette.fill_strong,
            window,
            cx,
        )
        .on_click(move |_, _, _| {
            if let Some(project) = project.clone() {
                home.dispatch(present::usage_screen(project), "entry");
            }
        });
        div()
            .w_full()
            .flex_shrink_0()
            .px(SIDE)
            .mt(CARD_MARGIN_TOP)
            .mb(CARD_MARGIN_BOTTOM)
            .child(semantic_interactive(spec, card))
            .into_any_element()
    }

    /// The `Bottom` entries, below the scrolling area.
    fn bottom(&self, strings: &Strings, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<AnyElement> = self
            .entries
            .bottom
            .iter()
            .map(|entry| self.entry_row("bottom", entry, false, strings, window, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .children(rows)
            .into_any_element()
    }
}

/// Which line of the scrolling area an index is: the contributed entries and
/// the snapshot's rows, with the Sessions folded, and the heading of the
/// `Views` section when it has entries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Line {
    Project(usize),
    Snapshot(SidebarRow),
    More,
    ViewsHeading,
    View(usize),
}

/// The arithmetic behind the folded list, with no allocation: the snapshot's
/// rows are the Sessions heading, `sessions` session rows (or the one
/// `SessionsEmpty` row), then the Packages, so the sessions are the rows
/// `1..=sessions` and the first `shown` of them stay.
struct Plan {
    project: usize,
    rows: Arc<[SidebarRow]>,
    sessions: usize,
    shown: usize,
    more: bool,
    views: usize,
}

impl Plan {
    fn new(
        project: usize,
        rows: Arc<[SidebarRow]>,
        sessions: usize,
        expanded: bool,
        views: usize,
    ) -> Plan {
        Plan {
            project,
            rows,
            sessions,
            shown: if expanded {
                sessions
            } else {
                sessions.min(FOLDED)
            },
            more: sessions > FOLDED,
            views,
        }
    }

    /// The lines that come from the snapshot.
    fn snapshot_len(&self) -> usize {
        self.rows.len() - self.sessions + self.shown + usize::from(self.more)
    }

    fn len(&self) -> usize {
        self.project + self.snapshot_len() + if self.views > 0 { 1 + self.views } else { 0 }
    }

    fn at(&self, index: usize) -> Line {
        if index < self.project {
            return Line::Project(index);
        }
        let at = index - self.project;
        if at >= self.snapshot_len() {
            return match at - self.snapshot_len() {
                0 => Line::ViewsHeading,
                view => Line::View(view - 1),
            };
        }
        if at <= self.shown {
            Line::Snapshot(self.rows[at])
        } else if self.more && at == self.shown + 1 {
            Line::More
        } else {
            Line::Snapshot(self.rows[at - self.shown - usize::from(self.more) + self.sessions])
        }
    }
}

/// The inset of a row from the panel's edge.
fn inset(row: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .flex_shrink_0()
        .px(SIDE)
        .child(row)
}

/// A section title: Sessions, Packages, Project. A row high, the text low in
/// it, semibold and quiet.
fn heading(
    node: NodeId,
    text: Text,
    attr: Option<(&'static str, &'static str)>,
    palette: &Palette,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let mut spec = Semantic::new(node, Role::Text).label(&text);
    if let Some((key, value)) = attr {
        spec = spec.attr(key, value);
    }
    semantic(
        spec,
        div()
            .flex()
            .items_end()
            .w_full()
            .h(ROW_HEIGHT)
            .flex_shrink_0()
            .pl(SIDE + HEADING_INSET)
            .pb(HEADING_BOTTOM)
            .text_role(TextRole::Caption, window, cx)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(palette.secondary_label)
            .child(text.shared()),
    )
    .into_any_element()
}

/// An icon in the accent colour, as the entries and the sub-projects wear it.
fn icon(icon: Icon, palette: &Palette) -> AnyElement {
    svg()
        .path(icon.path())
        .size(ICON_SIZE)
        .flex_shrink_0()
        .text_color(palette.accent)
        .into_any_element()
}

/// The Usage card answers the pointer: its background goes from `rest` to
/// `raised` (the same colour at a higher opacity, so one background and no dip
/// in between) with `HOVER_IN` / `HOVER_OUT`, as `ActionCard` does. The
/// fade's state lives in GPUI's keyed element state under the card's id.
fn clickable(
    row: gpui::Div,
    node: &NodeId,
    rest: Hsla,
    raised: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Stateful<gpui::Div> {
    let id = ElementId::from(node);
    let hover = window.use_keyed_state(
        ElementId::NamedChild(Arc::new(id.clone()), SharedString::from("hover")),
        cx,
        |_, _| Transition::new(0.0),
    );
    let strength = hover.update(cx, |hover, _| hover.tick(Instant::now(), window));
    let background = Hsla {
        a: rest.a + (raised.a - rest.a) * strength,
        ..rest
    };
    row.id(id)
        .cursor_default()
        .bg(background)
        .on_hover(move |hovered, window, cx| {
            let (target, motion) = if *hovered {
                (1.0, HOVER_IN)
            } else {
                (0.0, HOVER_OUT)
            };
            hover.update(cx, |hover, cx| hover.go(target, motion, Instant::now(), cx));
            window.refresh();
        })
}

fn agent_attr(agent: &lectern_services::session::Agent) -> &'static str {
    match agent {
        lectern_services::session::Agent::ClaudeCode => "claude-code",
        lectern_services::session::Agent::Codex => "codex",
        lectern_services::session::Agent::Other(_) => "other",
    }
}

impl Drop for HomeSidebar {
    fn drop(&mut self) {
        debug!("home view detached"; view = "sidebar");
    }
}

impl Render for HomeSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strings = strings(cx);
        let plan = self.plan();
        let total = plan.len();
        let (sessions, shown) = (plan.sessions, plan.shown);
        let list = uniform_list(
            "project-home-sidebar-list",
            total,
            cx.processor(|this, range: Range<usize>, window, cx| {
                this.build_rows(range, window, cx)
            }),
        )
        .track_scroll(&self.scroll)
        .size_full();
        let list = semantic(
            Semantic::new(nodes::sidebar_list(), Role::List)
                .attr("rows", total.to_string())
                .attr("visible", self.built.get().to_string()),
            list,
        );
        let bottom = self.bottom(&strings, window, cx);
        semantic(
            Semantic::new(nodes::sidebar(), Role::Group)
                .attr("sessions", sessions.to_string())
                .attr("shown", shown.to_string()),
            div()
                .id("project-home-sidebar")
                .key_context(CONTEXT)
                .size_full()
                .flex()
                .flex_col()
                .child(div().flex_1().min_h_0().child(list))
                .child(bottom),
        )
    }
}

impl HomeSidebar {
    /// How many rows the last frame built (tests).
    #[cfg(test)]
    pub(crate) fn built_rows(&self) -> usize {
        self.built.get()
    }

    /// How many rounds the dot of a session has blinked for (tests).
    #[cfg(test)]
    pub(crate) fn rounds_of(&self, key: &SessionKey) -> Option<u64> {
        self.rounds.get(key).map(|round| round.count)
    }
}
