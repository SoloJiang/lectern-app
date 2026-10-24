//! The review screen (docs/design/review_view.md「视图」「界面」): the
//! narrative list, the floating bar, the loading and failed states, the
//! toolbar's texts and the actions. The state is the [`ReviewModel`]'s;
//! this view holds the focus, the progress bar's transition and the
//! milestone flag, and draws.

use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, AnyView, App, AppContext, Context, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, Styled, Subscription, Window,
    div, list, px,
};
use lectern_i18n::{Args, Strings, keys};
use lectern_logging::warn;
use lectern_services::changes::{BaseSpec, ChangeError};
use lectern_services::review::{LoadState, ReviewError, ReviewTarget};
use lectern_services::workspace::{Milestone, Target, WorkspaceCommand};
use lectern_ui::contrib::ToolbarItems;
use lectern_ui::motion::{self, Transition};
use lectern_ui::semantic::{Role, Semantic, semantic};
use lectern_ui::{
    Button, ButtonKind, ButtonSize, LoadingPhase, Progress, ProgressShape, Skeleton, Text,
    TextRole, TextRoleExt, colors, loading_phase, mono_metrics,
};

use crate::actions::{
    self, Back, Escape, LooksGood, MarkSeenAndNext, NextHunk, PreviousHunk, Refresh,
    ToggleEarlierTurns, ToggleNoise, ToggleSeen,
};
use crate::model::{self, Direction, ReviewModel};
use crate::nodes;
use crate::plugin::Shared;

/// The room the floating bar takes at the bottom of the list.
const BAR_RESERVE: Pixels = px(72.0);

/// The review screen. `Screen("review")`'s factory builds one for each
/// `Push(Route { screen: "review", target: Target::Review(…) })`; popping
/// drops it, and the model with it (docs/design/review_view.md「视图」).
pub struct ReviewScreen {
    model: Entity<ReviewModel>,
    focus: FocusHandle,
    shared: Arc<Shared>,
    /// The seen-progress bar's fraction (`PROGRESS`).
    progress: Transition,
    milestone_reported: bool,
    _observe: Subscription,
}

/// The factory of `Screen("review")` (main thread). A target that is not a
/// review is reported and shows the missing-review message.
pub(crate) fn build(
    shared: &Arc<Shared>,
    target: &Target,
    window: &mut Window,
    cx: &mut App,
) -> AnyView {
    let model = match target {
        Target::Review(review) => {
            let shared = shared.clone();
            let review = review.clone();
            cx.new(|cx| ReviewModel::new(shared, review, cx))
        }
        other => {
            warn!("screen built without a review target"; target = target_name(other));
            let shared = shared.clone();
            cx.new(|_| ReviewModel::missing(shared))
        }
    };
    model::set_current(&model, cx);
    let screen = cx.new(|cx| ReviewScreen::new(shared.clone(), model, cx));
    let focus = screen.read(cx).focus.clone();
    window.focus(&focus, cx);
    screen.into()
}

fn target_name(target: &Target) -> &'static str {
    match target {
        Target::None => "none",
        Target::Project(_) => "project",
        Target::Review(_) => "review",
        Target::Read(_) => "read",
    }
}

impl ReviewScreen {
    fn new(
        shared: Arc<Shared>,
        model: Entity<ReviewModel>,
        cx: &mut Context<ReviewScreen>,
    ) -> ReviewScreen {
        let fraction = model.read(cx).progress_fraction();
        let observe = cx.observe(&model, |_, _, cx| cx.notify());
        ReviewScreen {
            model,
            focus: cx.focus_handle(),
            shared,
            progress: Transition::new(fraction),
            milestone_reported: false,
            _observe: observe,
        }
    }

    /// The model, for the toolbar, the Progress page and the tests.
    pub fn model(&self) -> &Entity<ReviewModel> {
        &self.model
    }
}

impl Focusable for ReviewScreen {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ReviewScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *colors(window, cx);
        let strings = lectern_ui::strings(cx);
        let mono = mono_metrics(window, cx);
        let model = self.model.clone();
        model.update(cx, |model, cx| {
            if model.line_height != mono.line_height {
                model.line_height = mono.line_height;
                model.list.remeasure();
            }
            model.tick_glide(window, Instant::now());
            model.sync_layout(cx);
        });
        let (state, shows_rows, fraction, ready, degraded_folder) = {
            let model = model.read(cx);
            (
                model.snapshot.state.clone(),
                model.shows_rows(),
                model.progress_fraction(),
                model.is_ready(),
                model.snapshot.degraded == Some(lectern_services::review::Degraded::FolderProject),
            )
        };
        if !self.milestone_reported && shows_rows {
            self.milestone_reported = true;
            model.read(cx).log_diff_shown();
            let shared = self.shared.clone();
            window.on_next_frame(move |_, _| shared.report_milestone(Milestone::DiffShown));
        }
        let now = Instant::now();
        if ready {
            if self.progress.target() != fraction {
                self.progress.go(fraction, motion::PROGRESS, now, cx);
            }
        } else {
            self.progress.set(fraction);
        }
        let shown = self.progress.tick(now, window);

        let body: AnyElement = match &state {
            LoadState::Failed(error) => self.render_failed(error, &strings, window, cx),
            LoadState::Loading => self.render_loading(&strings, window, cx),
            LoadState::Ready => {
                let list_state = model.read(cx).list.clone();
                let items = model.clone();
                let (count, visible) = {
                    let model = model.read(cx);
                    (model.items.len(), model.visible.clone())
                };
                let spec = Semantic::new(nodes::plain("narrative"), Role::List)
                    .attr("items", count.to_string())
                    .attr("visible", format!("{}..{}", visible.start, visible.end))
                    .attr("mode", "unified");
                semantic(
                    spec,
                    list(list_state, move |ix, window, cx| {
                        items.update(cx, |model, cx| model.render_item(ix, window, cx))
                    })
                    .size_full()
                    .pb(BAR_RESERVE),
                )
                .into_any_element()
            }
        };
        let mut root = div()
            .id("review")
            .key_context(actions::REVIEW)
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .bg(palette.window_background)
            .on_action(cx.listener(|this, _: &NextHunk, _, cx| {
                this.model
                    .update(cx, |model, cx| model.walk_hunks(Direction::Forward, cx));
            }))
            .on_action(cx.listener(|this, _: &PreviousHunk, _, cx| {
                this.model
                    .update(cx, |model, cx| model.walk_hunks(Direction::Backward, cx));
            }))
            .on_action(cx.listener(|this, _: &MarkSeenAndNext, _, cx| {
                this.model
                    .update(cx, |model, cx| model.mark_seen_and_next(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleSeen, _, cx| {
                this.model.update(cx, |model, cx| {
                    if let Some(file) = model.current_file() {
                        model.toggle_seen(file, cx);
                    }
                });
            }))
            .on_action(cx.listener(|this, _: &LooksGood, _, cx| {
                this.model.update(cx, |model, cx| model.looks_good(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleNoise, _, cx| {
                this.model.update(cx, |model, cx| model.toggle_noise(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleEarlierTurns, _, cx| {
                this.model.update(cx, |model, cx| model.toggle_earlier(cx));
            }))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| {
                this.model.update(cx, |model, _| model.refresh());
            }))
            .on_action(cx.listener(|this, _: &Back, _, cx| {
                this.model.read(cx).back();
            }))
            .on_action(cx.listener(|this, _: &Escape, _, cx| {
                // Not handled: the event goes on to `workspace`.
                if !this.model.update(cx, |model, cx| model.escape(cx)) {
                    cx.propagate();
                }
            }))
            .child(body);
        if ready {
            root = root.child(self.render_bar(shown, degraded_folder, &strings, window, cx));
        }
        root
    }
}

impl ReviewScreen {
    fn render_failed(
        &self,
        error: &ReviewError,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = match error {
            ReviewError::Missing => Text::key(strings, keys::review::FAILED_MISSING),
            ReviewError::NotInOpenProject => {
                Text::key(strings, keys::review::FAILED_NOT_IN_PROJECT)
            }
            ReviewError::Changes(error) => {
                let reason = strings.get(change_error_key(error));
                Text::fmt(
                    strings,
                    keys::review::FAILED_CHANGES,
                    &Args::new().str("reason", reason),
                )
            }
        };
        let spec = Semantic::new(nodes::plain("failed"), Role::Text).label(&text);
        let weak = self.model.downgrade();
        let back = Button::new(
            nodes::plain("failed").child("back"),
            Text::key(strings, keys::review::BACK),
        )
        .on_click(move |_, cx| {
            let _ = weak.update(cx, |model, _| model.back());
        });
        semantic(
            spec,
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.0))
                .text_role(TextRole::Secondary, window, cx)
                .child(text.shared())
                .child(back),
        )
        .into_any_element()
    }

    fn render_loading(
        &self,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let elapsed = self.model.read(cx).opened_at.elapsed();
        let phase = loading_phase(elapsed);
        let name = match phase {
            LoadingPhase::Nothing => "nothing",
            LoadingPhase::Skeleton => "skeleton",
            LoadingPhase::Progress => "progress",
        };
        let spec = Semantic::new(nodes::plain("loading"), Role::Group).attr("phase", name);
        let content: AnyElement = match phase {
            LoadingPhase::Nothing => div().into_any_element(),
            LoadingPhase::Skeleton => div()
                .p(px(16.0))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(Skeleton::new(px(420.0), px(72.0)))
                .child(Skeleton::new(px(160.0), px(14.0)))
                .child(Skeleton::new(px(520.0), px(14.0)))
                .child(Skeleton::new(px(160.0), px(14.0)))
                .child(Skeleton::new(px(480.0), px(14.0)))
                .into_any_element(),
            LoadingPhase::Progress => {
                let shared = self.shared.clone();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(12.0))
                    .text_role(TextRole::Secondary, window, cx)
                    .child(Text::key(strings, keys::review::LOADING).shared())
                    .child(
                        Button::new(
                            nodes::plain("loading").child("cancel"),
                            Text::key(strings, keys::review::CANCEL),
                        )
                        .on_click(move |_, _| shared.dispatch(WorkspaceCommand::Pop)),
                    )
                    .into_any_element()
            }
        };
        semantic(spec, div().size_full().child(content)).into_any_element()
    }

    /// The floating bar: the seen progress, the comment count, Send (not
    /// available before #22) and Looks good.
    fn render_bar(
        &self,
        shown: f32,
        folder: bool,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = *colors(window, cx);
        let (progress, comments) = {
            let model = self.model.read(cx);
            (model.progress(), model.snapshot.notes.len() as u32)
        };
        let bar = nodes::plain("bar");
        let comment_text = Text::fmt(
            strings,
            keys::review::COMMENTS,
            &Args::new().num("count", comments),
        );
        let mut inner = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(8.0))
            .rounded(px(12.0))
            .bg(palette.control_background)
            .border(px(0.5))
            .border_color(palette.separator)
            .shadow_md()
            .text_role(TextRole::Secondary, window, cx);
        if !folder {
            inner = inner
                .child(div().w(px(120.0)).child(Progress::new(
                    bar.child("progress"),
                    ProgressShape::Bar,
                    shown,
                )))
                .child(
                    div().child(
                        Text::fmt(
                            strings,
                            keys::review::PROGRESS,
                            &Args::new()
                                .num("reviewed", progress.seen)
                                .num("total", progress.total),
                        )
                        .shared(),
                    ),
                );
        }
        inner = inner.child(semantic(
            Semantic::new(bar.child("comments"), Role::Text).label(&comment_text),
            div().child(comment_text.shared()),
        ));
        if !folder {
            inner = inner.child(
                Button::new(bar.child("send"), Text::key(strings, keys::review::SEND))
                    .disabled(true),
            );
            let all_seen = progress.total > 0 && progress.seen == progress.total;
            if all_seen {
                inner = inner
                    .child(div().child(Text::key(strings, keys::review::ALL_REVIEWED).shared()));
            } else {
                let weak = self.model.downgrade();
                inner = inner.child(
                    Button::new(
                        bar.child("looks-good"),
                        Text::key(strings, keys::review::LOOKS_GOOD),
                    )
                    .kind(ButtonKind::Primary)
                    .size(ButtonSize::Regular)
                    .disabled(progress.total == 0)
                    .on_click(move |_, cx| {
                        let _ = weak.update(cx, |model, cx| model.looks_good(cx));
                    }),
                );
            }
        }
        semantic(
            Semantic::new(bar, Role::Group),
            div()
                .absolute()
                .bottom(px(16.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(inner),
        )
        .into_any_element()
    }
}

fn change_error_key(error: &ChangeError) -> lectern_i18n::Key {
    match error {
        ChangeError::Unsupported(_) => keys::review::ERROR_UNSUPPORTED,
        ChangeError::NoMergeBase => keys::review::ERROR_NO_MERGE_BASE,
        ChangeError::TooManyEntries => keys::review::ERROR_TOO_MANY_ENTRIES,
        ChangeError::Io(_) => keys::review::ERROR_IO,
        ChangeError::Cancelled => keys::review::ERROR_CANCELLED,
    }
}

/// The toolbar's title, subtitle and controls (`Screen::toolbar`): called
/// every frame by `workspace`, with no window; it reads the model on top.
pub(crate) fn toolbar(cx: &mut App) -> ToolbarItems {
    let strings = lectern_ui::strings(cx);
    let current = model::current(cx);
    let Some(model) = current else {
        return ToolbarItems {
            title: Text::key(&strings, keys::review::TITLE_UNTITLED),
            subtitle: None,
            controls: Vec::new(),
        };
    };
    let (title, subtitle) = {
        let model = model.read(cx);
        (
            model.toolbar_title(&strings),
            model.toolbar_subtitle(&strings),
        )
    };
    let weak = model.downgrade();
    let refresh = Button::new(
        nodes::plain("toolbar").child("refresh"),
        Text::key(&strings, keys::review::REFRESH),
    )
    .kind(ButtonKind::Plain)
    .size(ButtonSize::Small)
    .on_click(move |_, cx| {
        let _ = weak.update(cx, |model, _| model.refresh());
    });
    ToolbarItems {
        title,
        subtitle,
        controls: vec![refresh.into_any_element()],
    }
}

impl ReviewModel {
    /// The toolbar's title: the session's title, or the kind of review.
    pub(crate) fn toolbar_title(&self, strings: &Strings) -> Text {
        match &self.target {
            ReviewTarget::Session(_) => match &self.title {
                Some(title) => Text::verbatim(title.clone()),
                None => Text::key(strings, keys::review::TITLE_UNTITLED),
            },
            ReviewTarget::Outside { .. } => Text::key(strings, keys::review::TITLE_OUTSIDE),
            ReviewTarget::Range { base, .. } => Text::fmt(
                strings,
                keys::review::TITLE_RANGE,
                &Args::new().str("base", &base_name(base)),
            ),
        }
    }

    /// `2/6 reviewed`, once the review is ready.
    pub(crate) fn toolbar_subtitle(&self, strings: &Strings) -> Option<Text> {
        if !self.is_ready()
            || self.snapshot.degraded == Some(lectern_services::review::Degraded::FolderProject)
        {
            return None;
        }
        let progress = self.progress();
        Some(Text::fmt(
            strings,
            keys::review::PROGRESS,
            &Args::new()
                .num("reviewed", progress.seen)
                .num("total", progress.total),
        ))
    }

    /// The seen share, `0` without files.
    pub(crate) fn progress_fraction(&self) -> f32 {
        let progress = self.progress();
        if progress.total == 0 {
            0.0
        } else {
            progress.seen as f32 / progress.total as f32
        }
    }
}

/// A base as the interface names it: a reference, a short commit, `HEAD`.
fn base_name(base: &BaseSpec) -> String {
    match base {
        BaseSpec::Head => "HEAD".to_owned(),
        BaseSpec::Commit(id) => {
            let hex: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
            hex.chars().take(7).collect()
        }
        BaseSpec::MergeBase(name) => name
            .strip_prefix("refs/heads/")
            .or_else(|| name.strip_prefix("refs/remotes/"))
            .unwrap_or(name)
            .to_owned(),
    }
}
