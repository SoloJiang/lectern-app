//! The inspector's Files page: the Progress list (docs/design/review_view.md
//! 「视图」「界面」). It reads the model of the review screen on top from the
//! GPUI global `Current`; with no review open it says so.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, EntityId, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ListAlignment, ListState, ParentElement, Render, SharedString, Styled, Subscription,
    WeakEntity, Window, div, list, px,
};
use lectern_i18n::{Args, Strings, keys};
use lectern_services::review::{Degraded, FileEntry, FileKey, ReviewSnapshot, SeenState};
use lectern_ui::semantic::{Role, Semantic, semantic};
use lectern_ui::{CircleCheck, Row, Text, TextRole, TextRoleExt, colors};

use crate::actions::{self, JumpToSelected, MarkSeenAndNext, SelectNext, SelectPrevious};
use crate::brief::file_text;
use crate::model::{self, ReviewModel};
use crate::nodes;
use crate::render::{relative, seen_name};

/// One line of the list: a heading or a file (an index into
/// `ReviewSnapshot::files`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Entry {
    NoiseHeading,
    File(u32),
}

/// The lines for a snapshot: the files in the snapshot's order (the
/// narrative's, noise last), the noise behind its heading.
pub(crate) fn entries(snapshot: &ReviewSnapshot) -> Vec<Entry> {
    let mut out = Vec::with_capacity(snapshot.files.len() + 1);
    let mut heading = false;
    for (ix, file) in snapshot.files.iter().enumerate() {
        if file.noise.is_some() && !heading {
            heading = true;
            out.push(Entry::NoiseHeading);
        }
        out.push(Entry::File(ix as u32));
    }
    out
}

/// The Progress page: `InspectorTab("files")`'s factory builds it, hiding
/// the inspector or switching the page drops it.
pub struct ProgressTab {
    model: Option<WeakEntity<ReviewModel>>,
    /// The model whose changes this page redraws for.
    observing: Option<(EntityId, Subscription)>,
    focus: FocusHandle,
    selected: Option<FileKey>,
    list: ListState,
    /// The files the list's lines were made for.
    shown: Option<Arc<[FileEntry]>>,
    lines: Arc<Vec<Entry>>,
}

impl ProgressTab {
    pub(crate) fn new(cx: &mut Context<ProgressTab>) -> ProgressTab {
        ProgressTab {
            model: None,
            observing: None,
            focus: cx.focus_handle(),
            selected: None,
            list: ListState::new(0, ListAlignment::Top, px(200.0)),
            shown: None,
            lines: Arc::new(Vec::new()),
        }
    }

    /// Follows the review screen on top: a different model, or none.
    fn attach(&mut self, cx: &mut Context<ProgressTab>) {
        let current = model::current(cx);
        let id = current.as_ref().map(|model| model.entity_id());
        if self.observing.as_ref().map(|(seen, _)| *seen) == id {
            return;
        }
        self.observing = current
            .as_ref()
            .map(|model| (model.entity_id(), cx.observe(model, |_, _, cx| cx.notify())));
        self.model = current.map(|model| model.downgrade());
        self.shown = None;
        self.selected = None;
    }

    fn upgrade(&self) -> Option<gpui::Entity<ReviewModel>> {
        self.model.as_ref()?.upgrade()
    }

    fn files(&self, cx: &App) -> Vec<(Entry, FileKey)> {
        let Some(model) = self.upgrade() else {
            return Vec::new();
        };
        let snapshot = model.read(cx).snapshot.clone();
        self.lines
            .iter()
            .filter_map(|entry| match entry {
                Entry::File(ix) => snapshot
                    .files
                    .get(*ix as usize)
                    .map(|file| (*entry, file.key.clone())),
                Entry::NoiseHeading => None,
            })
            .collect()
    }

    /// ↓ / ↑: the selection moves by one file.
    fn step(&mut self, delta: i32, cx: &mut Context<ProgressTab>) {
        let files = self.files(cx);
        if files.is_empty() {
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|selected| files.iter().position(|(_, key)| key == selected));
        let next = match at {
            Some(at) => (at as i32 + delta).clamp(0, files.len() as i32 - 1) as usize,
            None if delta >= 0 => 0,
            None => files.len() - 1,
        };
        self.selected = Some(files[next].1.clone());
        let line = self.lines.iter().position(|entry| *entry == files[next].0);
        if let Some(line) = line {
            self.list.scroll_to_reveal_item(line);
        }
        cx.notify();
    }

    /// ↵ or a click: the narrative goes to the file.
    fn jump(&mut self, file: FileKey, cx: &mut Context<ProgressTab>) {
        self.selected = Some(file.clone());
        if let Some(model) = self.upgrade() {
            model.update(cx, |model, cx| model.jump_to_brief(Some(file), None, cx));
        }
        cx.notify();
    }
}

impl Focusable for ProgressTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ProgressTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.attach(cx);
        let strings = lectern_ui::strings(cx);
        let palette = *colors(window, cx);
        let model = self.upgrade();
        let snapshot = model.as_ref().map(|model| model.read(cx).snapshot.clone());
        let usable = snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.degraded != Some(Degraded::FolderProject) && !snapshot.files.is_empty()
        });
        let root = div()
            .id("review-progress")
            .key_context(actions::PROGRESS)
            .track_focus(&self.focus)
            .size_full()
            .p(px(12.0))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &JumpToSelected, _, cx| {
                if let Some(file) = this.selected.clone() {
                    this.jump(file, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &MarkSeenAndNext, _, cx| {
                if let Some(model) = this.upgrade() {
                    let selected = this.selected.clone();
                    model.update(cx, |model, cx| {
                        if selected.is_some() {
                            model.jump_to_brief(selected, None, cx);
                        }
                        model.mark_seen_and_next(cx);
                    });
                }
            }));
        let (Some(snapshot), true) = (snapshot, usable) else {
            let empty = Text::key(&strings, keys::review::PROGRESS_EMPTY);
            return root
                .child(semantic(
                    Semantic::new(nodes::plain("progress"), Role::Text).label(&empty),
                    div()
                        .text_role(TextRole::Secondary, window, cx)
                        .child(empty.shared()),
                ))
                .into_any_element();
        };
        let fresh = !self
            .shown
            .as_ref()
            .is_some_and(|shown| Arc::ptr_eq(shown, &snapshot.files));
        if fresh {
            self.lines = Arc::new(entries(&snapshot));
            self.list.reset(self.lines.len());
            self.shown = Some(snapshot.files.clone());
        }
        let progress = snapshot.progress;
        let heading = Text::fmt(
            &strings,
            keys::review::PROGRESS,
            &Args::new()
                .num("reviewed", progress.seen)
                .num("total", progress.total),
        );
        let list_spec = Semantic::new(nodes::plain("progress"), Role::List)
            .attr("total", progress.total.to_string())
            .attr("seen", progress.seen.to_string())
            .attr("changed", progress.changed_since_seen.to_string());
        let lines = self.lines.clone();
        let selected = self.selected.clone();
        let weak = cx.weak_entity();
        let view_snapshot = snapshot.clone();
        let rows = list(self.list.clone(), move |ix, window, cx| {
            line(
                lines.get(ix).copied(),
                &view_snapshot,
                selected.as_ref(),
                &weak,
                model.as_ref(),
                window,
                cx,
            )
        })
        .size_full();
        root.flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .px(px(4.0))
                    .text_role(TextRole::Body, window, cx)
                    .child(heading.shared()),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded(px(12.0))
                    .bg(palette.control_background)
                    .overflow_hidden()
                    .child(semantic(list_spec, rows)),
            )
            .into_any_element()
    }
}

/// One line of the list.
fn line(
    entry: Option<Entry>,
    snapshot: &ReviewSnapshot,
    selected: Option<&FileKey>,
    tab: &WeakEntity<ProgressTab>,
    model: Option<&gpui::Entity<ReviewModel>>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let strings: Strings = lectern_ui::strings(cx);
    match entry {
        None => div().into_any_element(),
        Some(Entry::NoiseHeading) => {
            let text = Text::key(&strings, keys::review::PROGRESS_NOISE);
            div()
                .px(px(12.0))
                .pt(px(10.0))
                .pb(px(4.0))
                .text_role(TextRole::Caption, window, cx)
                .child(text.shared())
                .into_any_element()
        }
        Some(Entry::File(ix)) => {
            let Some(file) = snapshot.files.get(ix as usize) else {
                return div().into_any_element();
            };
            let seen = snapshot
                .seen
                .get(ix as usize)
                .copied()
                .unwrap_or(SeenState::Unseen);
            let path = file_text(&file.key);
            let is_selected = selected == Some(&file.key);
            let node = nodes::keyed("progress", &nodes::file_key(&file.key));
            let group = format!("{:?}", file.group).to_lowercase();
            let key = file.key.clone();
            let toggle_key = file.key.clone();
            let tab = tab.clone();
            let toggle_model = model.map(gpui::Entity::downgrade);
            let check = CircleCheck::new(node.child("check"), matches!(seen, SeenState::Seen))
                .on_change(move |_, _, cx| {
                    if let Some(model) = &toggle_model {
                        let key = toggle_key.clone();
                        let _ = model.update(cx, |model, cx| model.toggle_seen(key, cx));
                    }
                });
            let mut text = div().flex_1().min_w_0().flex().flex_col().child(
                div()
                    .text_role(TextRole::Body, window, cx)
                    .truncate()
                    .child(SharedString::from(path.clone())),
            );
            if is_selected {
                text = text.child(
                    div()
                        .text_role(TextRole::Caption, window, cx)
                        .truncate()
                        .child(
                            Text::fmt(
                                &strings,
                                keys::review::REVIEWING,
                                &Args::new().str("path", &path),
                            )
                            .shared(),
                        ),
                );
            }
            if let SeenState::ChangedSinceSeen { since } = seen {
                text = text.child(
                    div().text_role(TextRole::Caption, window, cx).child(
                        Text::fmt(
                            &strings,
                            keys::review::SEEN_CHANGED,
                            &Args::new().str("when", &relative(&strings, since)),
                        )
                        .shared(),
                    ),
                );
            }
            // The row's own node is the control's; what the page knows about
            // the file hangs on a node of its own around it.
            let spec = Semantic::new(
                nodes::keyed("progress-file", &nodes::file_key(&file.key)),
                Role::Group,
            )
            .attr("seen", seen_name(seen))
            .attr("group", group)
            .attr("noise", file.noise.is_some().to_string());
            let row = Row::new(node)
                .selected(is_selected)
                .on_click(move |_, cx| {
                    let key = key.clone();
                    let _ = tab.update(cx, |tab, cx| tab.jump(key, cx));
                })
                .child(check)
                .child(text);
            semantic(spec, row).into_any_element()
        }
    }
}
