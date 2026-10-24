//! `HomeSidebar`: the project sidebar panel (docs/design/project_home.md「界面」).
//! The scrolling area is one `uniform_list` over the `Project` entries, the
//! snapshot's rows (Sessions, Packages) and the `Views` entries, all 24pt, so
//! a frame builds only the visible rows however many sessions and
//! sub-projects there are. The `Bottom` entries (Usage) sit below it.
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
    AnyElement, App, Context, ElementId, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, SharedString, StatefulInteractiveElement, Styled, Task, UniformListScrollHandle,
    Window, div, px, svg, uniform_list,
};
use lectern_i18n::{Args, Strings, count_compact, keys};
use lectern_logging::debug;
use lectern_services::session::{SessionKey, Timestamp};
use lectern_services::workspace::{SidebarEntry, SidebarSection};
use lectern_ui::contrib::SidebarItem;
use lectern_ui::motion::{HOVER_IN, HOVER_OUT, Transition};
use lectern_ui::semantic::{NodeId, Role, Semantic, State, semantic, semantic_interactive};
use lectern_ui::{
    AgentDot, Palette, Progress, ProgressShape, Skeleton, Text, TextRole, TextRoleExt, colors,
    strings,
};

use crate::actions::CONTEXT;
use crate::home::Home;
use crate::nodes;
use crate::present::{self, flag};
use crate::snapshot::{
    GroupKind, HomeSnapshot, PackageRow, SessionRow, SidebarRow, UsageModel, WorktreeGroup,
};

/// A row of the scrolling area, and the inset of its content.
const ROW_HEIGHT: Pixels = px(24.0);
const INSET: Pixels = px(12.0);
const GAP: Pixels = px(8.0);
/// What a nested sub-project adds to the inset per level.
const INDENT: Pixels = px(12.0);
const ICON_SIZE: Pixels = px(14.0);
const SEPARATOR: Pixels = px(0.5);
const BLOCK_WIDTH: Pixels = px(96.0);
const BLOCK_HEIGHT: Pixels = px(10.0);
/// The alpha of the `label`-coloured hover wash at full strength, as in `Row`.
const HOVER_ALPHA: f32 = 0.06;

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
    rounds: HashMap<SessionKey, Round>,
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
            rounds: HashMap::new(),
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

    fn snapshot_rows(&self) -> Arc<[SidebarRow]> {
        match &self.snapshot {
            Some(snapshot) => snapshot.sidebar.rows.clone(),
            None => Arc::from(Vec::new()),
        }
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
        let rows = self.snapshot_rows();
        let (first, second) = (
            self.entries.project.len(),
            self.entries.project.len() + rows.len(),
        );
        range
            .map(|index| {
                if index < first {
                    let entry = self.entries.project[index].clone();
                    let selected = entry.id == OVERVIEW_ENTRY && self.on_home;
                    self.entry_row("project", &entry, selected, &strings, &palette, window, cx)
                } else if index < second {
                    self.snapshot_row(rows[index - first], &strings, &palette, window, cx)
                } else {
                    let entry = self.entries.views[index - second].clone();
                    self.entry_row("views", &entry, false, &strings, &palette, window, cx)
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
                self.heading(nodes::sidebar_sessions(), text, None, window, cx)
            }
            SidebarRow::WorktreeHeader(group) => {
                self.worktree_header(&sidebar.groups[group as usize], strings, window, cx)
            }
            SidebarRow::Session { group, index } => {
                let row = &sidebar.groups[group as usize].sessions[index as usize];
                self.session_row(row, snapshot.now, strings, palette, window, cx)
            }
            SidebarRow::SessionsEmpty => {
                let text = Text::key(strings, keys::project_home::SESSIONS_EMPTY);
                semantic(
                    Semantic::new(nodes::sidebar_sessions().child("empty"), Role::Text)
                        .label(&text),
                    self.line(window, cx)
                        .text_role(TextRole::Secondary, window, cx)
                        .child(text.shared()),
                )
                .into_any_element()
            }
            SidebarRow::PackagesHeader => {
                let text = Text::key(strings, keys::project_home::PACKAGES);
                self.heading(
                    nodes::sidebar_packages(),
                    text,
                    Some(("counts", flag(sidebar.package_counts))),
                    window,
                    cx,
                )
            }
            SidebarRow::Package(index) => {
                let row = &sidebar.packages[index as usize];
                self.package_row(row, sidebar.package_counts, strings, palette, window, cx)
            }
            SidebarRow::PackagesSkeleton => self
                .line(window, cx)
                .child(Skeleton::new(BLOCK_WIDTH, BLOCK_HEIGHT))
                .into_any_element(),
        }
    }

    /// The base of a row of the scrolling area: 24pt, the inset, centred.
    fn line(&self, window: &Window, cx: &App) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(GAP)
            .w_full()
            .h(ROW_HEIGHT)
            .flex_shrink_0()
            .px(INSET)
            .text_role(TextRole::Body, window, cx)
    }

    /// A section title: Sessions, Packages.
    fn heading(
        &self,
        node: NodeId,
        text: Text,
        attr: Option<(&'static str, &'static str)>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let mut spec = Semantic::new(node, Role::Text).label(&text);
        if let Some((key, value)) = attr {
            spec = spec.attr(key, value);
        }
        semantic(
            spec,
            self.line(window, cx)
                .text_role(TextRole::Caption, window, cx)
                .child(text.shared()),
        )
        .into_any_element()
    }

    /// A worktree's small heading: its branch (or "other worktrees"), and
    /// `current` for the active one.
    fn worktree_header(
        &self,
        group: &WorktreeGroup,
        strings: &Strings,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let (label, branch, active) = match &group.kind {
            GroupKind::Worktree { branch, is_active } => (
                Text::verbatim(branch.clone()),
                branch.to_string(),
                *is_active,
            ),
            GroupKind::Other => (
                Text::key(strings, keys::project_home::OTHER_WORKTREES),
                String::new(),
                false,
            ),
            GroupKind::Folder => return div().into_any_element(),
        };
        let mut line = self
            .line(window, cx)
            .text_role(TextRole::Secondary, window, cx)
            .child(div().truncate().child(label.shared()));
        if active {
            let current = Text::key(strings, keys::project_home::CURRENT_WORKTREE);
            line = line.child(
                div()
                    .flex_shrink_0()
                    .text_role(TextRole::Caption, window, cx)
                    .child(current.shared()),
            );
        }
        semantic(
            Semantic::new(group.node.clone(), Role::Text)
                .label(&label)
                .attr("branch", branch)
                .attr("active", flag(active)),
            line,
        )
        .into_any_element()
    }

    fn session_row(
        &self,
        row: &SessionRow,
        now: Timestamp,
        strings: &Strings,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = present::title(strings, row);
        let spec = Semantic::new(row.node.clone(), Role::ListItem)
            .label(&title)
            .state(State::default())
            .attr("agent", agent_attr(&row.agent))
            .attr("running", flag(row.running))
            .attr("pending", flag(row.tail_pending));
        let home = self.home.clone();
        let key = row.key.clone();
        let mut line = clickable(self.line(window, cx), &row.node, palette, window, cx).on_click(
            move |_, _, _| {
                home.dispatch(present::review_session(key.clone()), "session");
            },
        );
        if let Some(agent) = present::dot_agent(&row.agent) {
            let mut dot = AgentDot::new(agent);
            let round = self.rounds.get(&row.key).map_or(0, |round| round.count);
            if row.running && round > 0 {
                dot = dot.blink(ElementId::NamedInteger(
                    SharedString::from(format!("project-home-blink-{}", row.key.id)),
                    round,
                ));
            }
            line = line.child(dot);
        }
        let activity = present::activity(strings, row, now);
        let line = line
            .child(div().flex_1().min_w_0().truncate().child(title.shared()))
            .child(
                div()
                    .flex_shrink_0()
                    .text_role(TextRole::Secondary, window, cx)
                    .child(activity),
            );
        semantic_interactive(spec, line).into_any_element()
    }

    fn package_row(
        &self,
        row: &PackageRow,
        counts: bool,
        strings: &Strings,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let spec = Semantic::new(row.node.clone(), Role::ListItem)
            .attr("changed", row.changed_today.to_string())
            .attr("depth", row.depth.to_string());
        // Packages are not clickable yet (「本 issue 之外」), so no hover wash.
        let _ = palette;
        let mut line = self
            .line(window, cx)
            .pl(INSET + INDENT * f32::from(row.depth))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(row.name.to_string()),
            );
        if counts && row.changed_today > 0 {
            let text = Text::fmt(
                strings,
                keys::project_home::CHANGED_TODAY,
                &Args::new().num("count", row.changed_today),
            );
            line = line.child(
                div()
                    .flex_shrink_0()
                    .text_role(TextRole::Secondary, window, cx)
                    .child(text.shared()),
            );
        }
        semantic(spec, line).into_any_element()
    }

    /// A contributed entry: icon, label; the Overview entry is selected on the
    /// home screen; the Usage entry is drawn with the figures.
    #[allow(clippy::too_many_arguments, reason = "one call site per section")]
    fn entry_row(
        &self,
        section: &'static str,
        entry: &Entry,
        selected: bool,
        strings: &Strings,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if section == "bottom" && entry.id == USAGE_ENTRY {
            return self.usage_row(entry, strings, palette, window, cx);
        }
        let node = if entry.id == OVERVIEW_ENTRY {
            nodes::sidebar_overview()
        } else {
            nodes::sidebar_entry(section, &entry.id)
        };
        let label = Text::key(strings, entry.item.label);
        let spec = Semantic::new(node.clone(), Role::ListItem)
            .label(&label)
            .state(State {
                selected,
                ..State::default()
            });
        let home = self.home.clone();
        let command = entry.item.on_select.clone();
        let mut line = clickable(self.line(window, cx), &node, palette, window, cx)
            .on_click(move |_, _, _| home.dispatch(command.clone(), "entry"));
        if selected {
            line = line.bg(palette.unemphasized_selected_content_background);
        }
        if let Some(icon) = entry.item.icon {
            line = line.child(
                svg()
                    .path(icon.path())
                    .size(ICON_SIZE)
                    .flex_shrink_0()
                    .text_color(palette.secondary_label),
            );
        }
        let line = line.child(div().flex_1().min_w_0().truncate().child(label.shared()));
        semantic_interactive(spec, line).into_any_element()
    }

    /// The Usage entry: the 7-day tokens, and the tightest limit's bar.
    fn usage_row(
        &self,
        entry: &Entry,
        strings: &Strings,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let model: Option<&UsageModel> = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.usage.as_ref());
        let first: AnyElement = match model {
            None => div()
                .truncate()
                .child(Text::key(strings, entry.item.label).shared())
                .into_any_element(),
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
                div().truncate().child(text.shared()).into_any_element()
            }
            Some(_) => Skeleton::new(BLOCK_WIDTH, BLOCK_HEIGHT).into_any_element(),
        };
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
            .gap(px(4.0))
            .w_full()
            .px(INSET)
            .py(GAP)
            .text_role(TextRole::Body, window, cx)
            .child(first);
        if let Some(limit) = model.and_then(|model| model.tightest.as_ref()) {
            spec = spec.attr("percent", limit.used_percent.to_string());
            let agent = present::agent_name(strings, &limit.agent);
            let text = Text::fmt(
                strings,
                keys::project_home::LIMIT,
                &Args::new()
                    .str("agent", &agent)
                    .num("percent", limit.used_percent),
            );
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap(GAP)
                    .child(div().w(px(56.0)).flex_shrink_0().child(Progress::new(
                        node.child("limit"),
                        ProgressShape::Bar,
                        f32::from(limit.used_percent) / 100.0,
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_role(TextRole::Secondary, window, cx)
                            .child(text.shared()),
                    ),
            );
        }
        let project = self
            .snapshot
            .as_deref()
            .map(|snapshot| snapshot.project.clone());
        let home = self.home.clone();
        let content = clickable(content, &node, palette, window, cx).on_click(move |_, _, _| {
            if let Some(project) = project.clone() {
                home.dispatch(present::usage_screen(project), "entry");
            }
        });
        semantic_interactive(spec, content).into_any_element()
    }

    /// The `Bottom` entries, below the scrolling area.
    fn bottom(
        &self,
        strings: &Strings,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows: Vec<AnyElement> = self
            .entries
            .bottom
            .iter()
            .map(|entry| self.entry_row("bottom", entry, false, strings, palette, window, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .border_t(SEPARATOR)
            .border_color(palette.separator)
            .children(rows)
            .into_any_element()
    }
}

/// A row that answers the pointer: the 6% `label` wash that fades in and out
/// with `HOVER_IN` / `HOVER_OUT`, as `Row` does. The fade's state lives in
/// GPUI's keyed element state under the row's id.
fn clickable(
    row: gpui::Div,
    node: &NodeId,
    palette: &Palette,
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
    let mut row = row.id(id).cursor_default();
    if strength > 0.0 {
        row = row.bg(palette.label.alpha(HOVER_ALPHA * strength));
    }
    row.on_hover(move |hovered, window, cx| {
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
        let palette = *colors(window, cx);
        let strings = strings(cx);
        let total =
            self.entries.project.len() + self.snapshot_rows().len() + self.entries.views.len();
        let (worktrees, sessions) = match &self.snapshot {
            Some(snapshot) => (
                snapshot.sidebar.groups.len(),
                snapshot.sidebar.session_rows as usize,
            ),
            None => (0, 0),
        };
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
        let bottom = self.bottom(&strings, &palette, window, cx);
        semantic(
            Semantic::new(nodes::sidebar(), Role::Group)
                .attr("worktrees", worktrees.to_string())
                .attr("sessions", sessions.to_string()),
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
