//! The review screen (docs/design/review_view.md「视图」「界面」): the
//! narrative list, the floating bar, the loading and failed states, the
//! toolbar's texts and the actions. The state is the [`ReviewModel`]'s;
//! this view holds the focus, the progress bar's transition and the
//! milestone flag, and draws.

use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, AnyView, App, AppContext, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Pixels, PromptLevel, Render, Styled,
    Subscription, Window, div, list, px,
};
use lectern_i18n::{Args, Strings, keys};
use lectern_logging::warn;
use lectern_services::changes::{BaseSpec, ChangeError};
use lectern_services::review::{
    LoadState, ReviewError, ReviewTarget, RouteKind, SendOutcome, SentVia, limits,
};
use lectern_services::workspace::{Milestone, Target, WorkspaceCommand};
use lectern_ui::contrib::ToolbarItems;
use lectern_ui::semantic::{Role, Semantic, semantic};
use lectern_ui::{
    Button, ButtonKind, ButtonSize, Capsule, LoadingPhase, Skeleton, SystemColor, Text, TextRole,
    TextRoleExt, colors, loading_phase, mono_metrics,
};

use crate::actions::{
    self, AddNote, Analyze, Back, CancelAnalyze, CancelNote, CopyNotesAgain, CopyNotesAsPrompt,
    DeleteNote, DiscardNotes, Edit, EditNote, Escape, ExpandFold, LooksGood, MarkSeenAndNext,
    NextHunk, OpenContext, OpenFile, PreviousHunk, Refresh, SaveNote, SendNotes, ToggleDiffMode,
    ToggleEarlierTurns, ToggleNoise, ToggleSeen,
};
use crate::brief;
use crate::model::{self, Direction, ReviewModel};
use crate::nodes;
use crate::notes::{Reason, Status};
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
        let observe = cx.observe(&model, |_, _, cx| cx.notify());
        let focus = cx.focus_handle();
        model.update(cx, |model, _| model.set_screen_focus(focus.clone()));
        ReviewScreen {
            model,
            focus,
            shared,
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
        let (state, shows_rows, ready, degraded_folder) = {
            let model = model.read(cx);
            (
                model.snapshot.state.clone(),
                model.shows_rows(),
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
        let body: AnyElement = match &state {
            LoadState::Failed(error) => self.render_failed(error, &strings, window, cx),
            LoadState::Loading => self.render_loading(&strings, window, cx),
            LoadState::Ready => {
                let list_state = model.read(cx).list.clone();
                let items = model.clone();
                let (count, visible, mode) = {
                    let model = model.read(cx);
                    (model.items.len(), model.visible.clone(), model.mode)
                };
                let spec = Semantic::new(nodes::plain("narrative"), Role::List)
                    .attr("items", count.to_string())
                    .attr("visible", format!("{}..{}", visible.start, visible.end))
                    .attr("mode", mode.name());
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
            .on_action(cx.listener(|this, _: &OpenFile, _, cx| {
                this.model.update(cx, |model, _| model.open_file());
            }))
            .on_action(cx.listener(|this, _: &Edit, _, cx| {
                this.model.update(cx, |model, _| model.open_file());
            }))
            .on_action(cx.listener(|this, _: &OpenContext, _, cx| {
                this.model.update(cx, |model, _| model.open_context());
            }))
            .on_action(cx.listener(|this, _: &ToggleDiffMode, _, cx| {
                this.model.update(cx, |model, cx| model.toggle_mode(cx));
            }))
            .on_action(cx.listener(|this, _: &ExpandFold, _, cx| {
                // Not on a fold, or above one: the key goes on.
                if !this
                    .model
                    .update(cx, |model, cx| model.expand_selected_fold(cx))
                {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &Analyze, _, cx| {
                this.model.update(cx, |model, _| model.analyze());
            }))
            .on_action(cx.listener(|this, _: &CancelAnalyze, _, cx| {
                this.model.update(cx, |model, _| model.cancel_analyze());
            }))
            .on_action(cx.listener(|this, _: &Back, _, cx| {
                this.model.read(cx).back();
            }))
            .on_action(cx.listener(|this, _: &AddNote, window, cx| {
                this.model
                    .update(cx, |model, cx| model.add_note_at_selection(window, cx));
            }))
            .on_action(cx.listener(|this, _: &SendNotes, _, cx| {
                this.model.update(cx, |model, cx| model.send_notes(cx));
            }))
            .on_action(cx.listener(|this, _: &CopyNotesAsPrompt, _, cx| {
                this.model
                    .update(cx, |model, cx| model.copy_notes_as_prompt(cx));
            }))
            .on_action(cx.listener(|this, _: &DiscardNotes, window, cx| {
                this.ask_discard(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CopyNotesAgain, _, cx| {
                this.model
                    .update(cx, |model, cx| model.copy_notes_again(cx));
            }))
            .on_action(cx.listener(|this, _: &SaveNote, window, cx| {
                this.model
                    .update(cx, |model, cx| model.save_note(window, cx));
            }))
            .on_action(cx.listener(|this, _: &CancelNote, window, cx| {
                this.model
                    .update(cx, |model, cx| model.cancel_note(window, cx));
            }))
            .on_action(cx.listener(|this, _: &EditNote, window, cx| {
                this.model
                    .update(cx, |model, cx| model.edit_selected_note(window, cx));
            }))
            .on_action(cx.listener(|this, _: &DeleteNote, _, cx| {
                this.model
                    .update(cx, |model, cx| model.delete_selected_note(cx));
            }))
            .on_action(cx.listener(|this, _: &Escape, _, cx| {
                // Not handled: the event goes on to `workspace`.
                if !this.model.update(cx, |model, cx| model.escape(cx)) {
                    cx.propagate();
                }
            }))
            .child(body);
        if ready {
            root = root.child(self.render_bar(degraded_folder, &strings, window, cx));
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

    /// The floating bar (docs/design/review_view.md「审阅意见（#22）」): a
    /// capsule with what happened to the notes (or how many there are), Copy
    /// Again, Discard, Looks Good, Copy as Prompt after a failed send, and
    /// Send.
    fn render_bar(
        &self,
        folder: bool,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = *colors(window, cx);
        let (progress, status, agent, notes, copy_again, can_discard) = {
            let model = self.model.read(cx);
            (
                model.progress(),
                model.status(),
                model.agent_label(strings),
                model.snapshot.notes_state.clone(),
                model.copy_again_batch(),
                model.can_discard(),
            )
        };
        let bar = nodes::plain("bar");
        let (comment_text, warn) = status_text(strings, status);
        let sending = notes.sending.is_some();
        let mut shown = div()
            .text_role(TextRole::Body, window, cx)
            .font_weight(FontWeight::MEDIUM)
            .child(comment_text.shared());
        if warn {
            shown = shown.text_color(palette.system(SystemColor::Orange));
        }
        let mut capsule = Capsule::new().child(semantic(
            Semantic::new(bar.child("comments"), Role::Text)
                .label(&comment_text)
                .attr("pending", notes.pending.to_string())
                .attr("sending", sending.to_string()),
            shown,
        ));
        if !folder {
            if let Some((batch, _)) = copy_again {
                let weak = self.model.downgrade();
                capsule = capsule.child(
                    Button::new(
                        bar.child("copy-again"),
                        Text::key(strings, keys::review::NOTES_COPY_AGAIN),
                    )
                    .kind(ButtonKind::Plain)
                    .size(ButtonSize::Regular)
                    .pill()
                    .attr("batch", format!("{:016x}", batch.0))
                    .on_click(move |_, cx| {
                        let _ = weak.update(cx, |model, cx| model.copy_notes_again(cx));
                    }),
                );
            }
            if can_discard {
                capsule = capsule.child(
                    Button::new(
                        bar.child("discard"),
                        Text::key(strings, keys::review::NOTES_DISCARD),
                    )
                    .kind(ButtonKind::Plain)
                    .size(ButtonSize::Regular)
                    .pill()
                    .on_click(|window, cx| {
                        window.dispatch_action(Box::new(DiscardNotes), cx);
                    }),
                );
            }
            let all_seen = progress.total > 0 && progress.seen == progress.total;
            if all_seen {
                capsule = capsule.child(
                    div()
                        .text_role(TextRole::Secondary, window, cx)
                        .child(Text::key(strings, keys::review::ALL_REVIEWED).shared()),
                );
            } else {
                let weak = self.model.downgrade();
                capsule = capsule.child(
                    Button::new(
                        bar.child("looks-good"),
                        Text::key(strings, keys::review::LOOKS_GOOD),
                    )
                    .kind(ButtonKind::Secondary)
                    .size(ButtonSize::Regular)
                    .pill()
                    .disabled(progress.total == 0)
                    .on_click(move |_, cx| {
                        let _ = weak.update(cx, |model, cx| model.looks_good(cx));
                    }),
                );
            }
            let failed_into_session = matches!(
                notes.last,
                Some(SendOutcome::Failed {
                    route: RouteKind::Session,
                    ..
                })
            );
            let has_clipboard = notes
                .routes
                .iter()
                .any(|route| route.kind == RouteKind::Clipboard);
            if failed_into_session && has_clipboard {
                capsule = capsule.child(
                    Button::new(
                        bar.child("copy"),
                        Text::key(strings, keys::review::COPY_PROMPT),
                    )
                    .kind(ButtonKind::Plain)
                    .size(ButtonSize::Regular)
                    .pill()
                    .disabled(notes.pending == 0 || sending)
                    .on_click(|window, cx| {
                        window.dispatch_action(Box::new(CopyNotesAsPrompt), cx);
                    }),
                );
            }
            let route = notes.routes.first().map(|route| route.kind);
            let send = match route {
                Some(RouteKind::Clipboard) => strings.get(keys::review::COPY_PROMPT).to_owned(),
                _ => match &agent {
                    Some(agent) => {
                        strings.format(keys::review::SEND_TO, &Args::new().str("agent", agent))
                    }
                    None => strings.get(keys::review::SEND).to_owned(),
                },
            };
            let hint = strings.get(keys::review::SEND_TO_SHORTCUT);
            let mut button =
                Button::new(bar.child("send"), Text::verbatim(format!("{send} {hint}")))
                    .kind(ButtonKind::Primary)
                    .size(ButtonSize::Regular)
                    .pill()
                    .disabled(route.is_none() || notes.pending == 0 || sending)
                    .on_click(|window, cx| {
                        window.dispatch_action(Box::new(SendNotes), cx);
                    });
            if let Some(route) = route {
                button = button.attr(
                    "route",
                    match route {
                        RouteKind::Session => "session",
                        RouteKind::Clipboard => "clipboard",
                    },
                );
            }
            capsule = capsule.child(button);
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
                .child(capsule),
        )
        .into_any_element()
    }

    /// Discard: asks first; only a yes sends `DiscardNotes`.
    fn ask_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (allowed, count) = {
            let model = self.model.read(cx);
            (model.can_discard(), model.snapshot.notes_state.pending)
        };
        if !allowed {
            return;
        }
        let strings = lectern_ui::strings(cx);
        let message = strings.format(
            keys::review::NOTES_DISCARD_CONFIRM,
            &Args::new().num("count", count),
        );
        let detail = strings
            .get(keys::review::NOTES_DISCARD_CONFIRM_DETAIL)
            .to_owned();
        let discard = strings.get(keys::review::NOTES_DISCARD).to_owned();
        let cancel = strings.get(keys::common::CANCEL).to_owned();
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(&detail),
            &[discard.as_str(), cancel.as_str()],
            cx,
        );
        let model = self.model.downgrade();
        cx.spawn(async move |_, cx| {
            let confirmed = matches!(answer.await, Ok(0));
            let _ = model.update(cx, |model, cx| {
                if confirmed {
                    model.discard_confirmed(cx);
                } else {
                    model.discard_declined();
                }
            });
        })
        .detach();
    }
}

/// What the first place of the bar says, and whether it is a warning.
fn status_text(strings: &Strings, status: Status) -> (Text, bool) {
    let reason = |reason: Reason| {
        strings
            .get(match reason {
                Reason::Starting => keys::review::NOTES_ERROR_STARTING,
                Reason::Busy => keys::review::NOTES_ERROR_BUSY,
                Reason::NoSession => keys::review::NOTES_ERROR_NO_SESSION,
                Reason::TimedOut => keys::review::NOTES_ERROR_TIMED_OUT,
                Reason::Clipboard => keys::review::NOTES_ERROR_CLIPBOARD,
                Reason::Failed => keys::review::NOTES_ERROR_FAILED,
            })
            .to_owned()
    };
    let counted = |key, count: u32| Text::fmt(strings, key, &Args::new().num("count", count));
    match status {
        Status::Full => (
            counted(keys::review::NOTES_FULL, limits::PENDING_NOTES),
            true,
        ),
        Status::Sending => (Text::key(strings, keys::review::NOTES_SENDING), false),
        Status::Sent { via, count } => (
            counted(
                match via {
                    SentVia::Clipboard => keys::review::NOTES_COPIED,
                    SentVia::TerminalPaste => keys::review::NOTES_PASTED,
                    SentVia::Acp => keys::review::NOTES_SENT,
                },
                count,
            ),
            false,
        ),
        Status::NotSent { reason: why, .. } => (
            Text::fmt(
                strings,
                keys::review::NOTES_NOT_SENT,
                &Args::new().str("reason", &reason(why)),
            ),
            true,
        ),
        Status::Copied { count } => (counted(keys::review::NOTES_COPIED, count), false),
        Status::NotCopied { reason: why } => (
            Text::fmt(
                strings,
                keys::review::NOTES_NOT_COPIED,
                &Args::new().str("reason", &reason(why)),
            ),
            true,
        ),
        Status::Count(count) => (counted(keys::review::COMMENTS, count), false),
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
    let (ready, folder, no_files) = {
        let model = model.read(cx);
        (
            model.is_ready(),
            model.snapshot.degraded == Some(lectern_services::review::Degraded::FolderProject),
            model.progress().total == 0,
        )
    };
    let weak = model.downgrade();
    let label = strings.get(keys::review::MARK_REVIEWED);
    let hint = strings.get(keys::review::MARK_REVIEWED_SHORTCUT);
    let mark = Button::new(
        nodes::plain("toolbar").child("mark-reviewed"),
        Text::verbatim(format!("{label} {hint}")),
    )
    .kind(ButtonKind::Secondary)
    .size(ButtonSize::Toolbar)
    .disabled(!ready || folder || no_files)
    .on_click(move |_, cx| {
        let _ = weak.update(cx, |model, cx| model.mark_seen_and_next(cx));
    });
    ToolbarItems {
        title,
        subtitle,
        controls: vec![mark.into_any_element()],
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

    /// The line under the title, once the review is ready: a session's agent,
    /// its branch (when usage knows it) and its turns; for changes outside a
    /// session and against a base, the branch and how many files changed.
    pub(crate) fn toolbar_subtitle(&self, strings: &Strings) -> Option<Text> {
        if !self.is_ready() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        match &self.target {
            ReviewTarget::Session(_) => {
                parts.extend(self.agent_label(strings));
                parts.extend(self.branch_label());
                if let Some(turns) = self.snapshot.turns.as_deref() {
                    let count = turns.iter().filter(|turn| turn.at.session == 0).count() as u32;
                    parts.push(
                        strings.format(keys::review::STATS_TURNS, &Args::new().num("count", count)),
                    );
                }
            }
            _ => {
                parts.extend(self.branch_label());
                parts.push(strings.format(
                    keys::review::FILES_CHANGED,
                    &Args::new().num("count", self.snapshot.files.len() as u32),
                ));
            }
        }
        (!parts.is_empty()).then(|| Text::verbatim(parts.join(" \u{00B7} ")))
    }

    /// The branch of the worktree under review, from the session's usage
    /// (the row of that worktree); None without it.
    pub(crate) fn branch_label(&self) -> Option<String> {
        brief::branch_of(self.session_usage()?, self.focus_worktree().as_ref())
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
