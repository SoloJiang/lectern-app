//! The Welcome screen's view (docs/design/welcome.md「界面」「数据流与线程」
//! 第 4、5、7 步). It reads the model's immutable snapshot and sends
//! commands: `Contributions::dispatch`, the model's memory-only methods and
//! the language picker's calls. It does no IO. Colours, fonts, motion and
//! controls come from `lectern-ui`; every string is a message of
//! `lectern-i18n`, except the paths and names, which are shown verbatim.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Task, WeakEntity, Window, canvas, div, px,
};
use lectern_i18n::{Args, Key, Locale, Strings, keys};
use lectern_kernel::ServiceRef;
use lectern_logging::debug;
use lectern_platform::system::PrefError;
use lectern_services::session::{Agent, Timestamp};
use lectern_services::workspace::{Contributions, WorkspaceCommand};
use lectern_ui::motion::{LOADING_SKELETON_AT, Transition};
use lectern_ui::semantic::{NodeId, Role, Semantic, semantic, semantic_interactive};
use lectern_ui::{
    AgentDot, Button, ButtonKind, ButtonSize, GroupedList, Icon, LoadingPhase, Palette, Row,
    Segmented, Skeleton, SystemColor, Text, TextRole, TextRoleExt, colors, loading_phase,
    set_locale, strings,
};

use crate::actions::{
    CONTEXT, OpenSelected, RemoveSelected, SelectLanguage, SelectNext, SelectPrevious,
};
use crate::discover::DiscoveredFolder;
use crate::model::{
    DiscoveredState, Notice, OpenFailure, OpenSource, RecentState, WakeGuard, WelcomeModel,
    WelcomeSnapshot,
};
use crate::prefs::LanguagePrefs;
use crate::recent::RecentEntry;

/// The left column's width.
const LEFT_COLUMN_WIDTH: f32 = 300.0;
/// The padding of both columns.
const COLUMN_PADDING: f32 = 24.0;
/// The gap between the blocks of a column.
const BLOCK_GAP: f32 = 24.0;
/// The gap between the items of the left column.
const LEFT_GAP: f32 = 12.0;
/// The inset of the empty-state text and the skeleton, the same as the
/// lists' header.
const LIST_INSET: f32 = 12.0;
/// The gap inside a row.
const ROW_GAP: f32 = 8.0;
/// A second line on a selected row (accent background) is its row's text
/// colour at this opacity: no colour value of the view's own.
const SELECTED_SECONDARY_OPACITY: f32 = 0.8;
/// How much of the row's own padding and of its neighbour shows when the
/// lists scroll to the selected row.
const REVEAL_MARGIN: f32 = 18.0;
/// How many skeleton rows a list that is slow to arrive shows.
const SKELETON_ROWS: usize = 2;

/// What the view needs besides the GPUI context.
#[derive(Clone)]
pub(crate) struct ViewDeps {
    pub(crate) model: Arc<WelcomeModel>,
    pub(crate) contributions: ServiceRef<dyn Contributions>,
    pub(crate) prefs: Arc<dyn LanguagePrefs>,
    /// The home directory, shown as `~` in the paths.
    pub(crate) home: Option<PathBuf>,
}

/// The keyboard selection: the two lists taken as one sequence, the recent
/// projects first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Selection {
    Recent(usize),
    Discovered(usize),
}

/// The lengths of the two lists, as the snapshot has them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Lens {
    pub(crate) recent: usize,
    pub(crate) discovered: usize,
}

impl Lens {
    pub(crate) fn of(snapshot: &WelcomeSnapshot) -> Lens {
        Lens {
            recent: match &snapshot.recent {
                RecentState::Ready(entries) => entries.len(),
                RecentState::Loading => 0,
            },
            discovered: match &snapshot.discovered {
                DiscoveredState::Ready(folders) => folders.len(),
                DiscoveredState::Unavailable | DiscoveredState::Pending => 0,
            },
        }
    }

    fn total(self) -> usize {
        self.recent + self.discovered
    }
}

impl Selection {
    /// The selection kept in place after the lists changed: in the same list
    /// at the same position, the last item when that is out of range, none
    /// when the list is empty (「状态与异常」).
    pub(crate) fn clamped(self, lens: Lens) -> Option<Selection> {
        match self {
            Selection::Recent(index) => {
                (lens.recent > 0).then(|| Selection::Recent(index.min(lens.recent - 1)))
            }
            Selection::Discovered(index) => {
                (lens.discovered > 0).then(|| Selection::Discovered(index.min(lens.discovered - 1)))
            }
        }
    }

    fn linear(self, lens: Lens) -> usize {
        match self {
            Selection::Recent(index) => index,
            Selection::Discovered(index) => lens.recent + index,
        }
    }

    fn from_linear(index: usize, lens: Lens) -> Selection {
        if index < lens.recent {
            Selection::Recent(index)
        } else {
            Selection::Discovered(index - lens.recent)
        }
    }

    /// ↓: the next item; the first when nothing is selected; the same at
    /// the last item.
    pub(crate) fn next(current: Option<Selection>, lens: Lens) -> Option<Selection> {
        let total = lens.total();
        if total == 0 {
            return None;
        }
        let index = match current {
            None => 0,
            Some(selection) => (selection.linear(lens) + 1).min(total - 1),
        };
        Some(Selection::from_linear(index, lens))
    }

    /// ↑: the previous item; the last when nothing is selected; the same at
    /// the first item.
    pub(crate) fn previous(current: Option<Selection>, lens: Lens) -> Option<Selection> {
        let total = lens.total();
        if total == 0 {
            return None;
        }
        let index = match current {
            None => total - 1,
            Some(selection) => selection.linear(lens).saturating_sub(1),
        };
        Some(Selection::from_linear(index, lens))
    }

    fn list(self) -> &'static str {
        match self {
            Selection::Recent(_) => "recent",
            Selection::Discovered(_) => "discovered",
        }
    }

    fn index(self) -> usize {
        match self {
            Selection::Recent(index) | Selection::Discovered(index) => index,
        }
    }
}

/// `Screen(WELCOME)`'s view.
pub(crate) struct WelcomeView {
    deps: ViewDeps,
    focus: FocusHandle,
    selection: Option<Selection>,
    /// The picker's choice: `None` follows the system.
    language: Option<Locale>,
    /// The picker's slider, moved by `Segmented::select`.
    slider: Transition,
    /// When the view was built, by the executor's clock: the loading
    /// thresholds count from it.
    created: Instant,
    /// The lists' scroll position, and whether the selected row is to be
    /// brought into view the next time it is laid out.
    scroll: ScrollHandle,
    reveal: Rc<Cell<bool>>,
    _wake: WakeGuard,
    _wake_task: Task<()>,
    _skeleton_task: Task<()>,
}

impl WelcomeView {
    /// Registers for the model's wake-ups, reads the saved language once and
    /// arms the timer that shows the skeleton. Main thread; no IO.
    pub(crate) fn new(deps: ViewDeps, cx: &mut Context<Self>) -> WelcomeView {
        let (wake, mut woken) = mpsc::unbounded::<()>();
        let guard = deps.model.subscribe(Box::new(move || {
            let _ = wake.unbounded_send(());
        }));
        let wake_task = cx.spawn(async move |this, cx| {
            while woken.next().await.is_some() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        let skeleton_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOADING_SKELETON_AT).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        });
        let language = deps
            .prefs
            .language_override()
            .map(|tag| Locale::negotiate([tag.as_str()]));
        let slider = Transition::new(language_index(language) as f32);
        WelcomeView {
            focus: cx.focus_handle(),
            selection: None,
            language,
            slider,
            created: cx.background_executor().now(),
            scroll: ScrollHandle::new(),
            reveal: Rc::new(Cell::new(false)),
            _wake: guard,
            _wake_task: wake_task,
            _skeleton_task: skeleton_task,
            deps,
        }
    }

    pub(crate) fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The selection as the current lists have it.
    fn current(&self, snapshot: &WelcomeSnapshot) -> Option<Selection> {
        self.selection
            .and_then(|selection| selection.clamped(Lens::of(snapshot)))
    }

    fn select(&mut self, selection: Option<Selection>, cx: &mut Context<Self>) {
        if self.selection != selection {
            if let Some(selection) = selection {
                debug!("selection"; list = selection.list(), index = selection.index());
            }
            self.selection = selection;
            cx.notify();
        }
    }

    fn select_next(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.deps.model.snapshot();
        let next = Selection::next(self.current(&snapshot), Lens::of(&snapshot));
        self.reveal.set(true);
        self.select(next, cx);
    }

    fn select_previous(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.deps.model.snapshot();
        let previous = Selection::previous(self.current(&snapshot), Lens::of(&snapshot));
        self.reveal.set(true);
        self.select(previous, cx);
    }

    /// ↵ and a click: `begin_open`, then `OpenProject`. Nothing when the
    /// same path is already being opened.
    fn begin(&self, path: PathBuf, source: OpenSource) {
        let snapshot = self.deps.model.snapshot();
        if snapshot
            .opening
            .as_ref()
            .is_some_and(|opening| opening.path == path)
        {
            return;
        }
        let Some(contributions) = self.deps.contributions.get() else {
            return;
        };
        self.deps.model.begin_open(path.clone(), source);
        contributions.dispatch(WorkspaceCommand::OpenProject(path));
    }

    /// The path and source of an item of the current snapshot.
    fn target(snapshot: &WelcomeSnapshot, selection: Selection) -> Option<(PathBuf, OpenSource)> {
        match (selection, &snapshot.recent, &snapshot.discovered) {
            (Selection::Recent(index), RecentState::Ready(entries), _) => entries
                .get(index)
                .map(|entry| (entry.path.clone(), OpenSource::Recent)),
            (Selection::Discovered(index), _, DiscoveredState::Ready(folders)) => folders
                .get(index)
                .map(|folder| (folder.path.to_path_buf(), OpenSource::Discovered)),
            _ => None,
        }
    }

    fn open_selected(&mut self, _: &mut Context<Self>) {
        let snapshot = self.deps.model.snapshot();
        let Some(selection) = self.current(&snapshot) else {
            return;
        };
        if let Some((path, source)) = Self::target(&snapshot, selection) {
            self.begin(path, source);
        }
    }

    /// A click on a row: the selection moves there and the row opens. The
    /// row is found again by its path: the lists may have changed between
    /// the frame that drew it and the click, and a row that is gone is not
    /// opened.
    fn click(&mut self, source: OpenSource, path: PathBuf, cx: &mut Context<Self>) {
        let snapshot = self.deps.model.snapshot();
        let Some(selection) = Self::find(&snapshot, source, &path) else {
            return;
        };
        self.select(Some(selection), cx);
        self.begin(path, source);
    }

    /// Where a path stands in the current lists.
    fn find(snapshot: &WelcomeSnapshot, source: OpenSource, path: &Path) -> Option<Selection> {
        match (source, &snapshot.recent, &snapshot.discovered) {
            (OpenSource::Recent, RecentState::Ready(entries), _) => entries
                .iter()
                .position(|entry| entry.path == path)
                .map(Selection::Recent),
            (OpenSource::Discovered, _, DiscoveredState::Ready(folders)) => folders
                .iter()
                .position(|folder| &*folder.path == path)
                .map(Selection::Discovered),
            _ => None,
        }
    }

    /// ⌫ on a recent project: removes it and keeps the selection at the same
    /// position (the next entry, or the previous one at the end of the list).
    fn remove_selected(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.deps.model.snapshot();
        if let Some(Selection::Recent(index)) = self.current(&snapshot)
            && let RecentState::Ready(entries) = &snapshot.recent
            && let Some(entry) = entries.get(index)
        {
            self.remove_recent(entry.path.clone(), cx);
        }
    }

    /// The remove button of a row, or ⌫. The row is found again by its path.
    fn remove_recent(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let snapshot = self.deps.model.snapshot();
        let Some(Selection::Recent(index)) = Self::find(&snapshot, OpenSource::Recent, &path)
        else {
            return;
        };
        let current = self.current(&snapshot);
        self.deps.model.remove_recent(&path);
        // An entry above the selected one went: the selection follows its
        // own row. The selected row itself went: the same position stands
        // for the next entry, and the previous one at the end of the list.
        let selection = match current {
            Some(Selection::Recent(selected)) if selected > index => {
                Some(Selection::Recent(selected - 1))
            }
            other => other,
        };
        let lens = Lens::of(&self.deps.model.snapshot());
        self.selection = selection.and_then(|selection| selection.clamped(lens));
        self.reveal.set(true);
        cx.notify();
    }

    /// The picker: switches the strings at once and saves the choice.
    fn select_language(&mut self, locale: Option<Locale>, cx: &mut Context<Self>) {
        if self.language == locale {
            return;
        }
        self.language = locale;
        Segmented::select(&mut self.slider, language_index(locale), Instant::now(), cx);
        let effective = locale.unwrap_or_else(|| {
            let languages = self.deps.prefs.system_languages();
            Locale::negotiate(languages.iter().map(String::as_str))
        });
        set_locale(effective, cx);
        debug!("locale selected"; locale = locale_name(locale));
        let model = self.deps.model.clone();
        self.deps.prefs.set_language_override(
            locale.map(Locale::tag),
            Box::new(move |result| {
                // `Superseded` is a newer choice replacing this one: no notice.
                if let Err(PrefError::WriteFailed) = result {
                    model.language_not_saved();
                }
            }),
        );
        cx.notify();
    }
}

fn locale_name(locale: Option<Locale>) -> &'static str {
    match locale {
        None => "system",
        Some(Locale::En) => "en",
        Some(Locale::ZhCn) => "zh-cn",
    }
}

/// The position of a choice in the picker: Match System, English, 简体中文.
fn language_index(locale: Option<Locale>) -> usize {
    match locale {
        None => 0,
        Some(Locale::En) => 1,
        Some(Locale::ZhCn) => 2,
    }
}

/// The choice at a position of the picker.
fn language_at(index: usize) -> Option<Locale> {
    match index {
        1 => Some(Locale::En),
        2 => Some(Locale::ZhCn),
        _ => None,
    }
}

/// The key of a notice's message.
fn notice_key(notice: Notice) -> Key {
    match notice {
        Notice::OpenFailed(OpenFailure::NotFound) => keys::welcome::OPEN_FAILED_NOT_FOUND,
        Notice::OpenFailed(OpenFailure::NotADirectory) => {
            keys::welcome::OPEN_FAILED_NOT_A_DIRECTORY
        }
        Notice::OpenFailed(OpenFailure::PermissionDenied) => keys::welcome::OPEN_FAILED_PERMISSION,
        Notice::OpenFailed(OpenFailure::RepoUnreadable) => keys::welcome::OPEN_FAILED_REPO,
        Notice::OpenFailed(OpenFailure::Io) => keys::welcome::OPEN_FAILED_IO,
        Notice::LanguageNotSaved => keys::welcome::LANGUAGE_NOT_SAVED,
    }
}

/// A business key of a node: the path, with the two sequences a node key
/// may not hold replaced.
pub(crate) fn node_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\n', " ")
        .replace("].", "]\u{b7}")
}

/// The path with the home directory written as `~`.
pub(crate) fn display_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(home)
    {
        return if rest.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", rest.to_string_lossy())
        };
    }
    path.to_string_lossy().into_owned()
}

fn sessions_text(strings: &Strings, count: u32) -> String {
    Text::fmt(
        strings,
        keys::welcome::ROW_SESSIONS,
        &Args::new().num("count", count),
    )
    .as_str()
    .to_owned()
}

fn running_text(strings: &Strings, count: u32) -> String {
    Text::fmt(
        strings,
        keys::welcome::ROW_RUNNING,
        &Args::new().num("count", count),
    )
    .as_str()
    .to_owned()
}

/// The segments of a second line, joined with a middle dot.
fn join_line(parts: &[String]) -> String {
    parts.join(" \u{b7} ")
}

fn kind_name(key: &lectern_services::project::ProjectKey) -> &'static str {
    match key {
        lectern_services::project::ProjectKey::Git { .. } => "git",
        lectern_services::project::ProjectKey::Folder { .. } => "folder",
    }
}

fn agent_name(agent: &Agent) -> String {
    match agent {
        Agent::ClaudeCode => "claude-code".to_owned(),
        Agent::Codex => "codex".to_owned(),
        Agent::Other(name) => name.to_string(),
    }
}

fn dot_of(agent: &Agent) -> Option<AgentDot> {
    match agent {
        Agent::ClaudeCode => Some(AgentDot::new(lectern_ui::Agent::ClaudeCode)),
        Agent::Codex => Some(AgentDot::new(lectern_ui::Agent::Codex)),
        Agent::Other(_) => None,
    }
}

/// What differs between the two kinds of row.
struct RowParts {
    /// The hover group's name, unique per row.
    group: String,
    /// The node of the name line: the row's facts.
    name_spec: Semantic,
    name: Text,
    /// The second line.
    detail: String,
    leading: Option<AnyElement>,
    trailing: Option<Button>,
    selected: bool,
}

/// What the render passes around.
struct Paint<'a> {
    strings: Strings,
    palette: Palette,
    view: WeakEntity<WelcomeView>,
    snapshot: &'a WelcomeSnapshot,
    selection: Option<Selection>,
    skeleton: bool,
    now: i64,
}

impl Render for WelcomeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The factory focused the view when it was built; if the focus went
        // nowhere since (a popped layer took it with it), the screen takes it
        // back so its keys reach the actions. A focus that is somewhere else
        // (an overlay above the screen) is left alone.
        if window.focused(cx).is_none() {
            window.focus(&self.focus, cx);
        }
        let snapshot = self.deps.model.snapshot();
        let elapsed = cx
            .background_executor()
            .now()
            .saturating_duration_since(self.created);
        let paint = Paint {
            strings: strings(cx),
            palette: *colors(window, cx),
            view: cx.entity().downgrade(),
            selection: self.current(&snapshot),
            skeleton: skeleton_shown(elapsed),
            now: self.deps.model.now_millis(),
            snapshot: &snapshot,
        };
        let slider = self.slider.tick(Instant::now(), window);

        let root = NodeId::new("welcome", "root", None);
        let mut spec = Semantic::new(root, Role::Group)
            .attr(
                "recent",
                match snapshot.recent {
                    RecentState::Loading => "loading",
                    RecentState::Ready(_) => "ready",
                },
            )
            .attr(
                "discovered",
                match snapshot.discovered {
                    DiscoveredState::Unavailable => "unavailable",
                    DiscoveredState::Pending => "pending",
                    DiscoveredState::Ready(_) => "ready",
                },
            );
        if snapshot.opening.is_some() {
            spec = spec.attr("opening", "true");
        }
        let left = self.left_column(&paint, slider, window, cx);
        let right = self.right_column(&paint, window, cx);
        semantic_interactive(
            spec,
            div()
                .id("welcome")
                .key_context(CONTEXT)
                .track_focus(&self.focus)
                .size_full()
                .flex()
                .flex_row()
                .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select_next(cx)))
                .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.select_previous(cx)))
                .on_action(cx.listener(|this, _: &OpenSelected, _, cx| this.open_selected(cx)))
                .on_action(cx.listener(|this, _: &RemoveSelected, _, cx| this.remove_selected(cx)))
                .on_action(cx.listener(|this, action: &SelectLanguage, _, cx| {
                    this.select_language(action.locale, cx);
                }))
                .child(left)
                .child(right),
        )
    }
}

/// Whether a list that has not arrived yet shows its skeleton.
fn skeleton_shown(elapsed: Duration) -> bool {
    match loading_phase(elapsed) {
        LoadingPhase::Nothing => false,
        LoadingPhase::Skeleton | LoadingPhase::Progress => true,
    }
}

impl WelcomeView {
    /// Title, subtitle, Open Folder, the notice, and the language picker at
    /// the bottom.
    fn left_column(
        &self,
        paint: &Paint<'_>,
        slider: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = paint.palette;
        let title = Text::key(&paint.strings, keys::welcome::TITLE);
        let subtitle = Text::key(&paint.strings, keys::welcome::SUBTITLE);
        let open_folder = Button::new(
            NodeId::new("welcome", "open-folder", None),
            Text::key(&paint.strings, keys::welcome::OPEN_FOLDER),
        )
        .kind(ButtonKind::Primary)
        .size(ButtonSize::Regular)
        .icon(Icon::Folder)
        .on_click({
            let view = paint.view.clone();
            move |_, cx| {
                let _ = view.update(cx, |this, _| this.open_folder());
            }
        });
        let notice = paint.snapshot.notice.map(|notice| {
            let text = Text::key(&paint.strings, notice_key(notice));
            semantic(
                Semantic::new(NodeId::new("welcome", "notice", None), Role::Text).label(&text),
                div()
                    .text_role(TextRole::Secondary, window, cx)
                    .text_color(palette.system(SystemColor::Red))
                    .child(text.shared()),
            )
        });
        let picker = {
            let language = NodeId::new("welcome", "language", None);
            let item = |key: &str, locale: Option<Locale>| {
                let text = match locale {
                    None => Text::key(&paint.strings, keys::common::LANGUAGE_SYSTEM),
                    Some(locale) => Text::key(&paint.strings, locale.name_key()),
                };
                (language.item(key), text)
            };
            let items = vec![
                item("system", None),
                item("en", Some(Locale::En)),
                item("zh-CN", Some(Locale::ZhCn)),
            ];
            let view = paint.view.clone();
            Segmented::new(language, items, language_index(self.language))
                .slider(slider)
                .on_select(move |index, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.select_language(language_at(index), cx);
                    });
                })
        };
        div()
            .w(px(LEFT_COLUMN_WIDTH))
            .flex_shrink_0()
            .h_full()
            .p(px(COLUMN_PADDING))
            .flex()
            .flex_col()
            .justify_between()
            .border_r_1()
            .border_color(palette.separator)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(LEFT_GAP))
                    .child(semantic(
                        Semantic::new(NodeId::new("welcome", "title", None), Role::Text)
                            .label(&title),
                        div()
                            .text_role(TextRole::LargeTitle, window, cx)
                            .child(title.shared()),
                    ))
                    .child(
                        div()
                            .text_role(TextRole::Secondary, window, cx)
                            .child(subtitle.shared()),
                    )
                    .child(open_folder)
                    .children(notice),
            )
            .child(picker)
            .into_any_element()
    }

    /// Open Folder: clears the notice, then asks `workspace` for its panel.
    fn open_folder(&self) {
        let Some(contributions) = self.deps.contributions.get() else {
            return;
        };
        self.deps.model.clear_notice();
        contributions.dispatch(WorkspaceCommand::PromptOpenProject);
    }

    /// The two lists, one above the other, scrolling together.
    fn right_column(
        &self,
        paint: &Paint<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let recent = self.recent_list(paint, window, cx);
        let discovered = self.discovered_list(paint, window, cx);
        div()
            .id("welcome-lists")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(BLOCK_GAP))
                    .p(px(COLUMN_PADDING))
                    .child(recent)
                    .child(discovered),
            )
            .into_any_element()
    }

    fn recent_list(
        &self,
        paint: &Paint<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = NodeId::new("welcome", "recent", None);
        let header = Text::key(&paint.strings, keys::welcome::RECENT);
        let list = GroupedList::new(id.clone()).header(header);
        match &paint.snapshot.recent {
            RecentState::Loading => self.waiting(list, paint),
            RecentState::Ready(entries) if entries.is_empty() => {
                let empty = NodeId::new("welcome", "recent", None).child("empty");
                self.empty(
                    list,
                    empty,
                    Text::key(&paint.strings, keys::welcome::RECENT_EMPTY),
                    window,
                    cx,
                )
            }
            RecentState::Ready(entries) => {
                let mut list = list;
                for (index, entry) in entries.iter().enumerate() {
                    list = list.row(self.recent_row(entry, index, paint));
                }
                list.into_any_element()
            }
        }
    }

    fn discovered_list(
        &self,
        paint: &Paint<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = NodeId::new("welcome", "discovered", None);
        let header = Text::key(&paint.strings, keys::welcome::DISCOVERED);
        let list = GroupedList::new(id.clone()).header(header);
        match &paint.snapshot.discovered {
            DiscoveredState::Pending => self.waiting(list, paint),
            DiscoveredState::Unavailable => self.empty(
                list,
                id.child("empty"),
                Text::key(&paint.strings, keys::welcome::DISCOVERED_EMPTY),
                window,
                cx,
            ),
            DiscoveredState::Ready(folders) if folders.is_empty() => self.empty(
                list,
                id.child("empty"),
                Text::key(&paint.strings, keys::welcome::DISCOVERED_EMPTY),
                window,
                cx,
            ),
            DiscoveredState::Ready(folders) => {
                let mut list = list;
                for (index, folder) in folders.iter().enumerate() {
                    list = list.row(self.discovered_row(folder, index, paint));
                }
                list.into_any_element()
            }
        }
    }

    /// A list that has not arrived: its header alone, and after the loading
    /// threshold two skeleton rows.
    fn waiting(&self, list: GroupedList, paint: &Paint<'_>) -> AnyElement {
        let mut column = div().flex().flex_col().child(list);
        if paint.skeleton {
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(ROW_GAP))
                    .px(px(LIST_INSET))
                    .pt(px(ROW_GAP))
                    .children((0..SKELETON_ROWS).map(|_| Skeleton::new(px(220.0), px(12.0)))),
            );
        }
        column.into_any_element()
    }

    /// A list with nothing in it: its header and one line of text.
    fn empty(
        &self,
        list: GroupedList,
        id: NodeId,
        text: Text,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .child(list)
            .child(semantic(
                Semantic::new(id, Role::Text).label(&text),
                div()
                    .px(px(LIST_INSET))
                    .pt(px(ROW_GAP))
                    .text_role(TextRole::Secondary, window, cx)
                    .child(text.shared()),
            ))
            .into_any_element()
    }

    fn recent_row(&self, entry: &RecentEntry, index: usize, paint: &Paint<'_>) -> Row {
        let list = NodeId::new("welcome", "recent", None);
        let key = node_key(&entry.path);
        let id = list.item(&key);
        let selected = paint.selection == Some(Selection::Recent(index));
        let name = Text::verbatim(entry.name.clone());
        let name_spec = Semantic::new(id.child("name"), Role::Text)
            .label(&name)
            .attr("kind", kind_name(&entry.key))
            .attr("sessions", entry.sessions.to_string())
            .attr("running", entry.running.to_string());
        let name_spec = if entry.missing {
            name_spec.attr("missing", "true")
        } else {
            name_spec
        };
        let detail = if entry.missing {
            Text::key(&paint.strings, keys::welcome::RECENT_MISSING)
                .as_str()
                .to_owned()
        } else {
            let mut parts = vec![display_path(&entry.path, self.deps.home.as_deref())];
            if entry.sessions > 0 {
                parts.push(sessions_text(&paint.strings, entry.sessions));
                if entry.running > 0 {
                    parts.push(running_text(&paint.strings, entry.running));
                }
            }
            join_line(&parts)
        };
        let remove = {
            let view = paint.view.clone();
            let path = entry.path.clone();
            Button::new(
                id.child("remove"),
                Text::key(&paint.strings, keys::welcome::RECENT_REMOVE),
            )
            .kind(ButtonKind::Plain)
            .size(ButtonSize::Small)
            .icon(Icon::Close)
            .on_click(move |_, cx| {
                // The row's own click must not open the project too.
                cx.stop_propagation();
                let _ = view.update(cx, |this, cx| this.remove_recent(path.clone(), cx));
            })
        };
        let group = format!("welcome-recent-{key}");
        let content = self.row_content(
            RowParts {
                group,
                name_spec,
                name,
                detail,
                leading: None,
                trailing: Some(remove),
                selected,
            },
            paint,
        );
        let view = paint.view.clone();
        let path = entry.path.clone();
        Row::new(id)
            .selected(selected)
            .accent()
            .on_click(move |_, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.click(OpenSource::Recent, path.clone(), cx);
                });
            })
            .child(content)
    }

    fn discovered_row(&self, folder: &DiscoveredFolder, index: usize, paint: &Paint<'_>) -> Row {
        let list = NodeId::new("welcome", "discovered", None);
        let key = node_key(&folder.path);
        let id = list.item(&key);
        let selected = paint.selection == Some(Selection::Discovered(index));
        let name = Text::verbatim(folder.name.clone());
        let agents = folder
            .agents
            .iter()
            .map(agent_name)
            .collect::<Vec<_>>()
            .join(",");
        let name_spec = Semantic::new(id.child("name"), Role::Text)
            .label(&name)
            .attr("agents", agents)
            .attr("sessions", folder.sessions.to_string())
            .attr("running", folder.running.to_string());
        let mut parts = vec![
            display_path(&folder.path, self.deps.home.as_deref()),
            sessions_text(&paint.strings, folder.sessions),
        ];
        if folder.running > 0 {
            parts.push(running_text(&paint.strings, folder.running));
        }
        let Timestamp(newest) = folder.newest_activity;
        let age = Duration::from_secs(u64::try_from((paint.now - newest) / 1000).unwrap_or(0));
        parts.push(paint.strings.relative_time(age));
        let dots = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(4.0))
            .children(folder.agents.iter().filter_map(dot_of));
        let group = format!("welcome-discovered-{key}");
        let content = self.row_content(
            RowParts {
                group,
                name_spec,
                name,
                detail: join_line(&parts),
                leading: Some(dots.into_any_element()),
                trailing: None,
                selected,
            },
            paint,
        );
        let view = paint.view.clone();
        let path = folder.path.to_path_buf();
        Row::new(id)
            .selected(selected)
            .accent()
            .on_click(move |_, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.click(OpenSource::Discovered, path.clone(), cx);
                });
            })
            .child(content)
    }

    /// What a row holds: an optional leading element, the two lines and an
    /// optional trailing button that shows while the row is hovered or
    /// selected.
    fn row_content(&self, parts: RowParts, paint: &Paint<'_>) -> AnyElement {
        let RowParts {
            group,
            name_spec,
            name,
            detail,
            leading,
            trailing,
            selected,
        } = parts;
        let name_line = semantic(name_spec, div().truncate().child(name.shared()));
        let mut detail_line = div()
            .truncate()
            .text_size(TextRole::Caption.size())
            .line_height(TextRole::Caption.line_height())
            .child(detail);
        detail_line = if selected {
            detail_line.opacity(SELECTED_SECONDARY_OPACITY)
        } else {
            detail_line.text_color(paint.palette.secondary_label)
        };
        let mut row = div()
            .group(group.clone())
            .relative()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(ROW_GAP))
            .children(leading)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(name_line)
                    .child(detail_line),
            );
        if let Some(button) = trailing {
            let mut slot = div().flex_shrink_0();
            if !selected {
                slot.style().visibility = Some(gpui::Visibility::Hidden);
                slot = slot.group_hover(group, |mut style| {
                    style.visibility = Some(gpui::Visibility::Visible);
                    style
                });
            }
            row = row.child(slot.child(button));
        }
        if selected {
            row = row.child(self.reveal_probe());
        }
        row.into_any_element()
    }

    /// An empty element over the selected row that scrolls the lists to show
    /// it once after the keyboard moved the selection or the list changed.
    fn reveal_probe(&self) -> AnyElement {
        let scroll = self.scroll.clone();
        let pending = self.reveal.clone();
        canvas(
            move |bounds, window, _| {
                if !pending.replace(false) {
                    return;
                }
                let view = scroll.bounds();
                let mut offset = scroll.offset();
                let margin = px(REVEAL_MARGIN);
                if bounds.top() - margin < view.top() {
                    offset.y += view.top() - (bounds.top() - margin);
                } else if bounds.bottom() + margin > view.bottom() {
                    offset.y -= (bounds.bottom() + margin) - view.bottom();
                } else {
                    return;
                }
                offset.y = offset.y.min(px(0.0)).max(-scroll.max_offset().y);
                scroll.set_offset(offset);
                window.refresh();
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
}
