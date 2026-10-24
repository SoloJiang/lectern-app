//! Drawing the items of the narrative list (docs/design/review_view.md
//! 「界面」). Colours come from the `Palette`, text from `Text` and
//! `TextRole`, controls from `lectern_ui`; nothing here reads a file or
//! calls a service except to copy an `Arc`.
//!
//! The item renderer runs for the items the list lays out (the visible ones
//! and the overdraw), each frame it is asked: it builds an element from what
//! the model holds and does nothing else.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AnimationExt, AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use lectern_i18n::{Args, Strings, keys};
use lectern_services::review::{
    Alignment, AnalysisState, AttributionState, FileEntry, HunkAttribution, NarrativeItem, RowsKey,
    SeenState, StepEntry, Survival, TurnRef,
};
use lectern_services::session::{Agent as SessionAgent, Timestamp, ToolStatus};
use lectern_ui::motion::{ARRIVE, ARRIVE_OFFSET, Effective};
use lectern_ui::semantic::{Role, Semantic, State, semantic, semantic_interactive};
use lectern_ui::{
    Agent, AgentDot, Button, ButtonKind, ButtonSize, CircleCheck, Palette, SystemColor, Text,
    TextRole, TextRoleExt, Verbatim, colors,
};

use crate::brief::{self, BriefLine};

/// What an Analyze button asks for.
#[derive(Clone, Copy)]
enum Analyze {
    Start,
    Cancel,
}
use crate::items::{self, Item, ItemKey, Notice, Pos};
use crate::model::ReviewModel;
use crate::nodes;

/// The side margin of every item.
const GUTTER: gpui::Pixels = px(16.0);

fn verbatim(text: impl Into<Verbatim>) -> Text {
    Text::verbatim(text)
}

/// The agent dot for a session's agent; none for an agent without a colour.
fn dot_agent(agent: &SessionAgent) -> Option<Agent> {
    match agent {
        SessionAgent::ClaudeCode => Some(Agent::ClaudeCode),
        SessionAgent::Codex => Some(Agent::Codex),
        SessionAgent::Other(_) => None,
    }
}

impl ReviewModel {
    /// The element of item `ix`. An index past the end (the list asks for
    /// an item between a rebuild and its own `reset`) gives an empty one.
    pub(crate) fn render_item(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let Some(item) = self.items.get(ix).cloned() else {
            return div().into_any_element();
        };
        let palette = *colors(window, cx);
        let strings = lectern_ui::strings(cx);
        match item {
            Item::Brief => self.render_brief(&palette, &strings, window, cx),
            Item::Notice(notice) => self.render_notice(notice, &palette, &strings, window, cx),
            Item::EarlierTurns { count, expanded } => {
                self.render_earlier(count, expanded, &palette, &strings, window, cx)
            }
            Item::Turn { at, ix } => self.render_turn(at, ix, &palette, &strings, window, cx),
            Item::Prompt { at, ix } => self.render_prompt(at, ix, &palette, window, cx),
            Item::Text { key, at } => {
                self.render_text(key, at, false, &palette, &strings, window, cx)
            }
            Item::Injected { key, at } => {
                self.render_text(key, at, true, &palette, &strings, window, cx)
            }
            Item::Step { id, at } => self.render_step(id, at, &palette, &strings, window, cx),
            Item::File { key, ix } => self.render_file(key, ix, &palette, &strings, window, cx),
            Item::Group { files, .. } => {
                self.render_group(self.first_group(ix), files, &palette, &strings, window, cx)
            }
            Item::Noise { count, expanded } => {
                self.render_noise(count, expanded, &palette, &strings, window, cx)
            }
            Item::Hunk {
                rows,
                hunk,
                old,
                new,
            } => self.render_hunk(rows, hunk, old, new, &palette, &strings, window, cx),
            Item::Row { rows, ix, .. } => self.render_row(rows, ix, &palette, window, cx),
            Item::Fold { rows, ix, old } => {
                self.render_fold(rows, ix, old, &palette, &strings, window, cx)
            }
            Item::Placeholder { rows, lines } => {
                let spec = Semantic::new(
                    nodes::keyed("diff", &nodes::rows_key_text(&rows)).child("placeholder"),
                    Role::Group,
                )
                .attr("lines", lines.to_string());
                semantic(spec, div().w_full().h(self.line_height * lines as f32)).into_any_element()
            }
            Item::Incomplete { kind, .. } => {
                let text = brief::incomplete_text(&strings, kind);
                div()
                    .w_full()
                    .h(self.line_height)
                    .px(GUTTER)
                    .flex()
                    .items_center()
                    .text_role(TextRole::Caption, window, cx)
                    .child(text.shared())
                    .into_any_element()
            }
            Item::Command { call, at } => {
                self.render_command(call, at, &palette, &strings, window, cx)
            }
            Item::Subagent { session, call, at } => {
                self.render_subagent(session, call, at, &palette, &strings, window, cx)
            }
            Item::Tool { call, at } => self.render_tool(call, at, &palette, &strings, window, cx),
        }
    }

    /// Whether the group title at `ix` is the first one: only the first
    /// carries the heading of the grouped view.
    fn first_group(&self, ix: usize) -> bool {
        !self.items[..ix]
            .iter()
            .any(|item| matches!(item, Item::Group { .. }))
    }

    // ----- brief and notices ---------------------------------------------

    fn render_brief(
        &mut self,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let brief = self.snapshot.brief.clone();
        let lines: Vec<BriefLine> = brief::lines(&brief, strings);
        let root = nodes::plain("brief");
        let spec = Semantic::new(root.clone(), Role::Group)
            .attr("items", lines.len().to_string())
            .attr("omitted", brief.omitted.to_string())
            .attr("pending", brief.pending_providers.to_string())
            .attr("analysis", analysis_name(&brief.analysis));
        let mut card = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .mx(GUTTER)
            .my(px(12.0))
            .p(px(12.0))
            .rounded(px(12.0))
            .bg(palette.control_background);
        let mut title = div().flex().flex_row().items_center().gap(px(8.0)).child(
            div()
                .text_role(TextRole::Body, window, cx)
                .font_weight(FontWeight::SEMIBOLD)
                .child(Text::key(strings, keys::review::BRIEF_TITLE).shared()),
        );
        if brief.pending_providers > 0 {
            title = title.child(
                div()
                    .text_role(TextRole::Caption, window, cx)
                    .child(Text::key(strings, keys::review::BRIEF_PENDING).shared()),
            );
        }
        card = card.child(title);
        if lines.is_empty() {
            card = card.child(
                div()
                    .text_role(TextRole::Secondary, window, cx)
                    .child(Text::key(strings, keys::review::BRIEF_EMPTY).shared()),
            );
        }
        for line in lines {
            let node = root.item(&line.id);
            let mut spec = Semantic::new(node.clone(), Role::ListItem)
                .label(&line.text)
                .attr("critical", line.critical.to_string());
            if let Some(anchor) = line.line {
                spec = spec.attr("line", anchor.line.to_string());
            }
            let dot = div()
                .size(px(6.0))
                .flex_shrink_0()
                .rounded_full()
                .bg(if line.critical {
                    palette.system(SystemColor::Red)
                } else {
                    palette.tertiary_label
                });
            let file = line.file.clone();
            let anchor = line.line;
            let row = div()
                .id(&node)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .cursor_pointer()
                .text_role(TextRole::Body, window, cx)
                .child(dot)
                .child(div().flex_1().min_w_0().child(line.text.shared()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.jump_to_brief(file.clone(), anchor, cx);
                }));
            card = card.child(semantic_interactive(spec, row));
        }
        if brief.omitted > 0 {
            card = card.child(
                div().text_role(TextRole::Caption, window, cx).child(
                    Text::fmt(
                        strings,
                        keys::review::BRIEF_OMITTED,
                        &Args::new().num("count", brief.omitted),
                    )
                    .shared(),
                ),
            );
        }
        let usage_lines = self.usage_lines(strings);
        for line in usage_lines {
            let node = root.child("usage").item(&line.id);
            let mut spec = Semantic::new(node, Role::ListItem)
                .label(&line.text)
                .attr("tokens", line.tokens.to_string())
                .attr("percent", line.percent.to_string())
                .attr("shared", line.shared.is_some().to_string());
            if let Some(shared) = &line.shared {
                spec = spec.attr("shared_text", shared.as_str().to_owned());
            }
            let mut column = div()
                .flex()
                .flex_col()
                .text_role(TextRole::Secondary, window, cx)
                .child(line.text.shared());
            if let Some(shared) = line.shared {
                column = column.child(
                    div()
                        .text_role(TextRole::Caption, window, cx)
                        .child(shared.shared()),
                );
            }
            card = card.child(semantic(spec, column));
        }
        card = card.child(self.render_analysis(&brief.analysis, palette, strings, window, cx));
        semantic(spec, card).into_any_element()
    }

    /// The Analyze row: the button that starts it, its state while it runs
    /// with the one that cancels it, and what a finished or a failed run
    /// shows. The view sends `Analyze` and `CancelAnalyze` and reads
    /// `Brief::analysis`; the analysis itself is the domain's.
    fn render_analysis(
        &self,
        analysis: &AnalysisState,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let id = nodes::plain("brief").child("analyze");
        let caption = |text: Text, window: &mut Window, cx: &mut Context<ReviewModel>| {
            div()
                .text_role(TextRole::Caption, window, cx)
                .child(text.shared())
                .into_any_element()
        };
        let weak = cx.weak_entity();
        let button = |id: lectern_ui::semantic::NodeId, text: Text, run: Analyze| {
            let weak = weak.clone();
            Button::new(id, text)
                .size(ButtonSize::Small)
                .kind(ButtonKind::Secondary)
                .on_click(move |_, cx| {
                    let _ = weak.update(cx, |model, _| match run {
                        Analyze::Start => model.analyze(),
                        Analyze::Cancel => model.cancel_analyze(),
                    });
                })
        };
        match analysis {
            AnalysisState::Unavailable(_) => div()
                .flex()
                .flex_row()
                .child(
                    Button::new(id, Text::key(strings, keys::review::ANALYZE))
                        .size(ButtonSize::Small)
                        .kind(ButtonKind::Secondary)
                        .disabled(true),
                )
                .into_any_element(),
            AnalysisState::Idle => div()
                .flex()
                .flex_row()
                .child(button(
                    id,
                    Text::key(strings, keys::review::ANALYZE),
                    Analyze::Start,
                ))
                .into_any_element(),
            AnalysisState::Running { .. } => semantic(
                Semantic::new(id.clone(), Role::Group).attr("state", "running"),
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(caption(
                        Text::key(strings, keys::review::ANALYZE_RUNNING),
                        window,
                        cx,
                    ))
                    .child(button(
                        id.child("cancel"),
                        Text::key(strings, keys::review::CANCEL),
                        Analyze::Cancel,
                    )),
            )
            .into_any_element(),
            AnalysisState::Done { stale, partial } => {
                let mut column = div().flex().flex_col().gap(px(4.0));
                if *stale {
                    column = column.child(caption(
                        Text::key(strings, keys::review::ANALYZE_STALE),
                        window,
                        cx,
                    ));
                }
                if *partial {
                    column = column.child(caption(
                        Text::key(strings, keys::review::ANALYZE_PARTIAL),
                        window,
                        cx,
                    ));
                }
                column = column.child(div().flex().flex_row().child(button(
                    id.child("again"),
                    Text::key(strings, keys::review::ANALYZE_AGAIN),
                    Analyze::Start,
                )));
                semantic(Semantic::new(id, Role::Group).attr("state", "done"), column)
                    .into_any_element()
            }
            AnalysisState::Failed(reason) => semantic(
                Semantic::new(id.clone(), Role::Group).attr("state", "failed"),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_role(TextRole::Caption, window, cx)
                            .text_color(palette.secondary_label)
                            .child(
                                Text::fmt(
                                    strings,
                                    keys::review::ANALYZE_FAILED,
                                    &Args::new().str("reason", reason),
                                )
                                .shared(),
                            ),
                    )
                    .child(div().flex().flex_row().child(button(
                        id.child("again"),
                        Text::key(strings, keys::review::ANALYZE_AGAIN),
                        Analyze::Start,
                    ))),
            )
            .into_any_element(),
        }
    }

    fn render_notice(
        &self,
        notice: Notice,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let text = match notice {
            Notice::BaseCurrentHead => Text::key(strings, keys::review::NOTICE_BASE_CURRENT_HEAD),
            Notice::Unrecognized => Text::key(strings, keys::review::NOTICE_UNRECOGNIZED),
            Notice::Folder => Text::key(strings, keys::review::NOTICE_FOLDER),
            Notice::Unclaimed(count) => Text::fmt(
                strings,
                keys::review::NOTICE_UNCLAIMED,
                &Args::new().num("count", count),
            ),
            Notice::InvalidPaths(count) => Text::fmt(
                strings,
                keys::review::NOTICE_INVALID_PATHS,
                &Args::new().num("count", count),
            ),
        };
        let spec =
            Semantic::new(nodes::keyed("notice", notice.kind().name()), Role::Text).label(&text);
        semantic(
            spec,
            div()
                .w_full()
                .px(GUTTER)
                .py(px(4.0))
                .text_role(TextRole::Secondary, window, cx)
                .text_color(palette.secondary_label)
                .child(text.shared()),
        )
        .into_any_element()
    }

    fn render_earlier(
        &self,
        count: u32,
        expanded: bool,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let text = Text::fmt(
            strings,
            keys::review::EARLIER_TURNS,
            &Args::new().num("count", count),
        );
        let node = nodes::plain("earlier");
        let spec = Semantic::new(node.clone(), Role::Group)
            .label(&text)
            .state(State {
                expanded: Some(expanded),
                ..State::default()
            });
        let row = div()
            .id(&node)
            .w_full()
            .px(GUTTER)
            .py(px(6.0))
            .cursor_pointer()
            .text_role(TextRole::Secondary, window, cx)
            .text_color(palette.accent)
            .child(text.shared())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_earlier(cx)));
        semantic_interactive(spec, row).into_any_element()
    }

    // ----- turns -----------------------------------------------------------

    fn turn_agent(&self, at: TurnRef) -> Option<Agent> {
        self.snapshot
            .sessions
            .get(usize::from(at.session))
            .and_then(|session| dot_agent(&session.agent))
    }

    fn render_turn(
        &self,
        at: TurnRef,
        ix: u32,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let Some(turn) = self
            .snapshot
            .turns
            .as_deref()
            .and_then(|turns| turns.get(ix as usize))
        else {
            return div().into_any_element();
        };
        let node = nodes::keyed("turn", &nodes::turn_key(at));
        let title = Text::fmt(
            strings,
            keys::review::TURN,
            &Args::new().num("ordinal", at.ordinal),
        );
        let when = if turn.ended_at.is_none() {
            Text::key(strings, keys::review::TURN_RUNNING)
        } else {
            verbatim(relative(strings, turn.started_at))
        };
        let tokens = self.turn_tokens(turn);
        let mut spec = Semantic::new(node.clone(), Role::Group)
            .label(&title)
            .attr("after_last_seen", turn.after_last_seen.to_string())
            .attr("running", turn.ended_at.is_none().to_string());
        if let Some(tokens) = tokens {
            spec = spec.attr("tokens", tokens.to_string());
        }
        let mut row = div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(GUTTER)
            .pt(px(20.0))
            .pb(px(6.0));
        if let Some(agent) = self.turn_agent(at) {
            row = row.child(AgentDot::new(agent));
        }
        let row = row
            .child(
                div()
                    .text_role(TextRole::Body, window, cx)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.shared()),
            )
            .child(
                div()
                    .text_role(TextRole::Caption, window, cx)
                    .text_color(palette.secondary_label)
                    .child(when.shared()),
            );
        let row = match tokens {
            Some(tokens) => row.child(
                div()
                    .text_role(TextRole::Caption, window, cx)
                    .text_color(palette.secondary_label)
                    .child(brief::turn_tokens_text(strings, tokens).shared()),
            ),
            None => row,
        };
        let element = semantic(spec, row).into_any_element();
        if self.arriving.contains(&at) {
            let moves = matches!(ARRIVE.effective(cx), Effective::Move(_));
            let id = gpui::ElementId::from(&node);
            div()
                .w_full()
                .child(element)
                .with_animation(id, ARRIVE.animation(cx), move |el, t| {
                    let offset = if moves {
                        ARRIVE_OFFSET * (1.0 - t)
                    } else {
                        gpui::Pixels::ZERO
                    };
                    el.opacity(t).mt(offset)
                })
                .into_any_element()
        } else {
            element
        }
    }

    fn render_prompt(
        &self,
        at: TurnRef,
        ix: u32,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let Some(turn) = self
            .snapshot
            .turns
            .as_deref()
            .and_then(|turns| turns.get(ix as usize))
        else {
            return div().into_any_element();
        };
        let body: SharedString = turn
            .prompt
            .and_then(|key| self.texts.get(&key))
            .map_or_else(
                || SharedString::from(turn.prompt_preview.clone()),
                |t| SharedString::from(t.clone()),
            );
        let node = nodes::keyed("turn", &nodes::turn_key(at)).child("prompt");
        let spec = Semantic::new(node, Role::Text).label(&verbatim(body.to_string()));
        semantic(
            spec,
            div().w_full().px(GUTTER).pb(px(6.0)).child(
                div()
                    .border_l(px(2.0))
                    .border_color(palette.separator)
                    .pl(px(10.0))
                    .text_role(TextRole::Secondary, window, cx)
                    .child(body),
            ),
        )
        .into_any_element()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "an item renderer: the pieces are the item's and the frame's"
    )]
    fn render_text(
        &self,
        key: lectern_services::review::TextKey,
        at: Pos,
        injected: bool,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let turn = self
            .snapshot
            .turns
            .as_deref()
            .and_then(|turns| turns.get(at.turn as usize));
        let turn_node = turn.map(|turn| nodes::keyed("turn", &nodes::turn_key(turn.at)));
        let area = if injected { "injected" } else { "text" };
        let body = self.texts.get(&key).cloned();
        let element = match &body {
            Some(text) => div()
                .text_role(TextRole::Body, window, cx)
                .child(SharedString::from(text.clone()))
                .into_any_element(),
            None => lectern_ui::Skeleton::new(px(320.0), px(12.0)).into_any_element(),
        };
        let mut column = div()
            .w_full()
            .px(GUTTER)
            .py(px(4.0))
            .flex()
            .flex_col()
            .gap(px(2.0));
        if injected {
            column = column.child(
                div()
                    .text_role(TextRole::Caption, window, cx)
                    .child(Text::key(strings, keys::review::INJECTED).shared()),
            );
            column = column.child(div().text_color(palette.secondary_label).child(element));
        } else {
            column = column.child(element);
        }
        match turn_node {
            Some(node) => {
                let mut spec =
                    Semantic::new(node.child(area).item(&at.item.to_string()), Role::Text);
                if let Some(text) = &body {
                    spec = spec.label(&verbatim(text.clone()));
                }
                semantic(spec, column).into_any_element()
            }
            None => column.into_any_element(),
        }
    }

    // ----- steps and files -------------------------------------------------

    fn seen_note(&self, seen: SeenState, strings: &Strings) -> Option<Text> {
        match seen {
            SeenState::ChangedSinceSeen { since } => Some(Text::fmt(
                strings,
                keys::review::SEEN_CHANGED,
                &Args::new().str("when", &relative(strings, since)),
            )),
            _ => None,
        }
    }

    fn render_step(
        &self,
        id: lectern_services::review::StepId,
        at: Pos,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let Some(step) = items::step_entry(&self.snapshot, at) else {
            return div().into_any_element();
        };
        let step: StepEntry = step.clone();
        let file = step.file.clone();
        let seen = self.seen_of(&file).unwrap_or(SeenState::Unseen);
        let selected = self.selection == Some(ItemKey::Step(id));
        let node = nodes::keyed("step", &nodes::step_key(id));
        let path = brief::file_text(&file);
        let mut notes: Vec<Text> = Vec::new();
        match &step.alignment {
            Alignment::Aligned => {}
            Alignment::Partial { .. } => notes.push(Text::key(strings, keys::review::STEP_PARTIAL)),
            Alignment::Unaligned => notes.push(Text::key(strings, keys::review::STEP_UNALIGNED)),
        }
        match step.survival {
            Survival::All => {}
            Survival::Partly => notes.push(Text::key(strings, keys::review::STEP_SURVIVAL_PARTLY)),
            Survival::None => notes.push(Text::key(strings, keys::review::STEP_SURVIVAL_NONE)),
        }
        notes.extend(self.seen_note(seen, strings));
        notes.extend(self.file_only_turns(&file, strings));
        let spec = Semantic::new(node.clone(), Role::Group)
            .label(&verbatim(path.clone()))
            .attr("added", step.added.to_string())
            .attr("removed", step.removed.to_string())
            .attr("alignment", alignment_name(&step.alignment))
            .attr("survival", survival_name(step.survival))
            .attr("seen", seen_name(seen));
        let check = self.check(&node.child("check"), &file, seen, cx);
        let counts = Text::fmt(
            strings,
            keys::review::STEP_COUNTS,
            &Args::new()
                .num("added", step.added)
                .num("removed", step.removed),
        );
        let key = ItemKey::Step(id);
        let mut head = div()
            .id(&node)
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(GUTTER)
            .py(px(6.0))
            .cursor_pointer()
            .when_selected(selected, palette)
            .child(check)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_role(TextRole::Body, window, cx)
                    .truncate()
                    .child(SharedString::from(path)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_role(TextRole::Caption, window, cx)
                    .child(counts.shared()),
            )
            .child(self.open_button(&node, &file, key.clone(), strings, cx));
        head = head.on_click(cx.listener(move |this, _, _, cx| {
            this.select(Some(key.clone()), cx);
        }));
        let rows_key = items::step_rows_key(step.step, &file, &self.context);
        let mut column = div()
            .w_full()
            .flex()
            .flex_col()
            .child(semantic_interactive(spec, head))
            .child(self.diff_node(&rows_key));
        for note in notes {
            column = column.child(
                div()
                    .px(GUTTER)
                    .pl(px(16.0 + 24.0))
                    .text_role(TextRole::Caption, window, cx)
                    .child(note.shared()),
            );
        }
        column.into_any_element()
    }

    fn render_file(
        &self,
        key: lectern_services::review::FileKey,
        ix: u32,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let Some(entry) = self.snapshot.files.get(ix as usize).cloned() else {
            return div().into_any_element();
        };
        let seen = self.seen_of(&key).unwrap_or(SeenState::Unseen);
        let path = brief::file_text(&key);
        let node = nodes::keyed("file", &nodes::file_key(&key));
        let selected = self.selection == Some(ItemKey::File(key.clone()));
        let (added, removed) = counts_of(&entry);
        let drifted = entry.attribution.drifted_hunks;
        let attribution = match entry.attribution.state {
            AttributionState::Pending => "pending",
            AttributionState::Lines => "lines",
            AttributionState::FileOnly => "file-only",
            AttributionState::None => "none",
        };
        let spec = Semantic::new(node.clone(), Role::Group)
            .label(&verbatim(path.clone()))
            .attr("seen", seen_name(seen))
            .attr("attribution", attribution)
            .attr("drifted_hunks", drifted.to_string());
        let check = self.check(&node.child("check"), &key, seen, cx);
        let counts = Text::fmt(
            strings,
            keys::review::STEP_COUNTS,
            &Args::new().num("added", added).num("removed", removed),
        );
        let item_key = ItemKey::File(key.clone());
        let head = div()
            .id(&node)
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(GUTTER)
            .py(px(6.0))
            .cursor_pointer()
            .when_selected(selected, palette)
            .child(check)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_role(TextRole::Body, window, cx)
                    .truncate()
                    .child(SharedString::from(path)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_role(TextRole::Caption, window, cx)
                    .child(counts.shared()),
            )
            .child(self.open_button(&node, &key, item_key.clone(), strings, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select(Some(item_key.clone()), cx);
            }));
        let rows_key = items::final_rows_key(&key, &self.context);
        let mut column = div()
            .w_full()
            .flex()
            .flex_col()
            .child(semantic_interactive(spec, head))
            .child(self.diff_node(&rows_key));
        let mut notes: Vec<Text> = self.seen_note(seen, strings).into_iter().collect();
        notes.extend(self.file_only_turns(&key, strings));
        for note in notes {
            column = column.child(
                div()
                    .pl(px(16.0 + 24.0))
                    .text_role(TextRole::Caption, window, cx)
                    .child(note.shared()),
            );
        }
        column.into_any_element()
    }

    /// A file with only file-level attribution (a binary file, one past the
    /// chain's limit) names the turns that changed it in its title; its hunks
    /// carry no labels.
    fn file_only_turns(
        &self,
        file: &lectern_services::review::FileKey,
        strings: &Strings,
    ) -> Vec<Text> {
        let Some(entry) = self.snapshot.files.iter().find(|entry| entry.key == *file) else {
            return Vec::new();
        };
        if entry.attribution.state != AttributionState::FileOnly {
            return Vec::new();
        }
        entry
            .attribution
            .turns
            .iter()
            .map(|turn| {
                Text::fmt(
                    strings,
                    keys::review::HUNK_TURN,
                    &Args::new().num("ordinal", turn.ordinal),
                )
            })
            .collect()
    }

    /// The button of a step's or a file's title that opens the file in the
    /// reader (⇧⌘O): disabled when the reader is not there.
    fn open_button(
        &self,
        node: &lectern_ui::semantic::NodeId,
        file: &lectern_services::review::FileKey,
        select: ItemKey,
        strings: &Strings,
        cx: &mut Context<ReviewModel>,
    ) -> Button {
        let weak = cx.weak_entity();
        let file = file.clone();
        Button::new(
            node.child("open"),
            Text::key(strings, keys::review::OPEN_FILE),
        )
        .kind(ButtonKind::Plain)
        .size(ButtonSize::Small)
        .disabled(!self.can_hand_off())
        .on_click(move |_, cx| {
            let file = file.clone();
            let select = select.clone();
            let _ = weak.update(cx, |model, cx| {
                model.select(Some(select), cx);
                model.open_file_of(&file);
            });
        })
    }

    /// The usage rows of the brief, when the session's usage is there.
    fn usage_lines(&self, strings: &Strings) -> Vec<brief::UsageLine> {
        let Some(usage) = self.session_usage() else {
            return Vec::new();
        };
        brief::usage_lines(usage, self.focus_worktree().as_ref(), strings)
    }

    /// The circular check of a file: a click toggles it.
    fn check(
        &self,
        id: &lectern_ui::semantic::NodeId,
        file: &lectern_services::review::FileKey,
        seen: SeenState,
        cx: &mut Context<ReviewModel>,
    ) -> CircleCheck {
        let weak = cx.weak_entity();
        let file = file.clone();
        CircleCheck::new(id.clone(), matches!(seen, SeenState::Seen)).on_change(move |_, _, cx| {
            let file = file.clone();
            let _ = weak.update(cx, |this, cx| this.toggle_seen(file, cx));
        })
    }

    fn render_group(
        &self,
        first: bool,
        files: u32,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let _ = files;
        let total = self.snapshot.files.len() as u32;
        let node = nodes::plain("group");
        let mut column = div()
            .w_full()
            .px(GUTTER)
            .pt(px(12.0))
            .pb(px(4.0))
            .flex()
            .flex_col();
        if first {
            column = column.child(
                div()
                    .text_role(TextRole::Body, window, cx)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(
                        Text::fmt(
                            strings,
                            keys::review::FILES_CHANGED,
                            &Args::new().num("count", total),
                        )
                        .shared(),
                    ),
            );
        } else {
            column = column.child(div().h(px(0.5)).w_full().bg(palette.separator));
        }
        semantic(
            Semantic::new(node.item(&files.to_string()), Role::Group),
            column,
        )
        .into_any_element()
    }

    fn render_noise(
        &self,
        count: u32,
        expanded: bool,
        _palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let text = Text::fmt(
            strings,
            keys::review::NOISE_FOLDED,
            &Args::new().num("count", count),
        );
        let node = nodes::plain("noise");
        let spec = Semantic::new(node.clone(), Role::Group)
            .label(&text)
            .state(State {
                expanded: Some(expanded),
                ..State::default()
            })
            .attr("count", count.to_string());
        let weak = cx.weak_entity();
        let button = Button::new(
            node.child("toggle"),
            Text::key(
                strings,
                if expanded {
                    keys::review::NOISE_HIDE
                } else {
                    keys::review::NOISE_SHOW
                },
            ),
        )
        .kind(ButtonKind::Plain)
        .size(ButtonSize::Small)
        .on_click(move |_, cx| {
            let _ = weak.update(cx, |this, cx| this.toggle_noise(cx));
        });
        semantic(
            spec,
            div()
                .w_full()
                .px(GUTTER)
                .py(px(12.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .text_role(TextRole::Secondary, window, cx)
                .child(div().flex_1().child(text.shared()))
                .child(button),
        )
        .into_any_element()
    }

    // ----- diffs -----------------------------------------------------------

    #[allow(
        clippy::too_many_arguments,
        reason = "an item renderer: the pieces are the item's and the frame's"
    )]
    fn render_hunk(
        &self,
        rows: RowsKey,
        hunk: u32,
        old: u32,
        new: u32,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let info = self
            .rows
            .get(&rows)
            .and_then(|r| r.hunks.get(hunk as usize))
            .map(|info| info.attribution.clone());
        let file = items::rows_file(&self.snapshot, &rows);
        let pending = file
            .as_ref()
            .and_then(|file| self.snapshot.files.iter().find(|e| e.key == *file))
            .is_some_and(|e| e.attribution.state == AttributionState::Pending);
        let unaligned = match &rows {
            RowsKey::Step { step, .. } => items::step_unaligned(&self.snapshot, *step),
            RowsKey::Final { .. } => false,
        };
        let labels = hunk_labels(info.as_ref(), pending, unaligned, strings);
        let key = ItemKey::Hunk {
            rows: rows.clone(),
            old,
            new,
        };
        let selected = self.selection.as_ref() == Some(&key);
        let node = nodes::keyed("diff", &nodes::rows_key_text(&rows))
            .child("hunk")
            .item(&nodes::lines_key(Some(old), Some(new)));
        let (turns, others, drifted) = info
            .as_ref()
            .map_or((0, 0, 0), |a| (a.turns.len(), a.others.len(), a.drifted));
        let spec = Semantic::new(node.clone(), Role::DiffHunk)
            .state(State {
                selected,
                ..State::default()
            })
            .attr("turns", turns.to_string())
            .attr("others", others.to_string())
            .attr("drifted", drifted.to_string());
        let mut line = div()
            .id(&node)
            .w_full()
            .h(self.line_height)
            .px(GUTTER)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .text_role(TextRole::Caption, window, cx)
            .when_selected(selected, palette);
        for label in labels {
            line = line.child(div().child(label.shared()));
        }
        line = line.on_click(cx.listener(move |this, _, _, cx| {
            this.select(Some(key.clone()), cx);
        }));
        semantic_interactive(spec, line).into_any_element()
    }

    // ----- calls -------------------------------------------------------------

    fn render_command(
        &self,
        call: lectern_services::review::CallRef,
        at: Pos,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let entry = self
            .snapshot
            .turns
            .as_deref()
            .and_then(|turns| turns.get(at.turn as usize))
            .and_then(|turn| turn.items.get(at.item as usize));
        let Some(NarrativeItem::Command { name, files, .. }) = entry else {
            return div().into_any_element();
        };
        let text = Text::fmt(
            strings,
            keys::review::COMMAND,
            &Args::new().str("command", name),
        );
        let spec = Semantic::new(nodes::keyed("call", &call.call.0), Role::Text)
            .label(&text)
            .attr("kind", "command")
            .attr("files", files.len().to_string());
        let mut column = div()
            .w_full()
            .px(GUTTER)
            .py(px(4.0))
            .flex()
            .flex_col()
            .text_role(TextRole::Secondary, window, cx)
            .child(div().truncate().child(text.shared()));
        if !files.is_empty() {
            column = column.child(
                div()
                    .text_role(TextRole::Caption, window, cx)
                    .text_color(palette.secondary_label)
                    .child(
                        Text::fmt(
                            strings,
                            keys::review::COMMAND_FILES,
                            &Args::new().num("count", files.len() as u32),
                        )
                        .shared(),
                    ),
            );
        }
        semantic(spec, column).into_any_element()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "an item renderer: the pieces are the item's and the frame's"
    )]
    fn render_subagent(
        &self,
        session: u16,
        call: lectern_services::session::ToolCallId,
        _at: Pos,
        _palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let text = Text::key(strings, keys::review::SUBAGENT);
        let node = nodes::keyed("call", &call.0);
        let spec = Semantic::new(node.clone(), Role::Text)
            .label(&text)
            .attr("kind", "subagent");
        let mut row = div()
            .id(&node)
            .cursor_pointer()
            .w_full()
            .px(GUTTER)
            .py(px(4.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .text_role(TextRole::Secondary, window, cx);
        let agent = self
            .snapshot
            .sessions
            .get(usize::from(session))
            .and_then(|s| dot_agent(&s.agent));
        if let Some(agent) = agent {
            row = row.child(AgentDot::new(agent));
        }
        let row = row
            .child(text.shared())
            .on_click(cx.listener(move |this, _, _, cx| this.jump_to_session(session, cx)));
        semantic_interactive(spec, row).into_any_element()
    }

    fn render_tool(
        &self,
        call: lectern_services::review::CallRef,
        at: Pos,
        _palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let entry = self
            .snapshot
            .turns
            .as_deref()
            .and_then(|turns| turns.get(at.turn as usize))
            .and_then(|turn| turn.items.get(at.item as usize));
        let Some(NarrativeItem::Tool { name, status, .. }) = entry else {
            return div().into_any_element();
        };
        let failed = matches!(
            status,
            Some(ToolStatus::Failed | ToolStatus::Denied | ToolStatus::Interrupted)
        );
        let text = Text::fmt(
            strings,
            if failed {
                keys::review::TOOL_FAILED
            } else {
                keys::review::TOOL
            },
            &Args::new().str("name", name),
        );
        let spec = Semantic::new(nodes::keyed("call", &call.call.0), Role::Text)
            .label(&text)
            .attr("kind", "tool")
            .attr("status", tool_status_name(*status));
        semantic(
            spec,
            div()
                .w_full()
                .px(GUTTER)
                .py(px(2.0))
                .text_role(TextRole::Secondary, window, cx)
                .truncate()
                .child(text.shared()),
        )
        .into_any_element()
    }
}

// ----- small helpers -----------------------------------------------------------

/// The extra styling the renderers share.
trait Shade: Styled + Sized {
    fn when_selected(self, selected: bool, palette: &Palette) -> Self {
        if selected {
            self.bg(palette.unemphasized_selected_content_background)
        } else {
            self
        }
    }
}

impl<T: Styled + Sized> Shade for T {}

fn counts_of(entry: &FileEntry) -> (u32, u32) {
    use lectern_services::changes::ChangeDetail;
    match &entry.change.detail {
        ChangeDetail::Text { added, removed, .. } | ChangeDetail::Truncated { added, removed } => {
            (*added, *removed)
        }
        _ => (0, 0),
    }
}

/// The labels of a hunk's title (docs/design/review_view.md「归因、Drifted
/// 与 Pending」): the turns it is attributed to, the other sessions, and the
/// Drifted mark.
pub(crate) fn hunk_labels(
    info: Option<&HunkAttribution>,
    pending: bool,
    unaligned: bool,
    strings: &Strings,
) -> Vec<Text> {
    let mut labels = Vec::new();
    if pending && !unaligned {
        return labels;
    }
    let Some(info) = info else {
        if unaligned {
            labels.push(Text::key(strings, keys::review::HUNK_DRIFTED));
        }
        return labels;
    };
    if unaligned {
        labels.push(Text::key(strings, keys::review::HUNK_DRIFTED));
        return labels;
    }
    if !info.lines {
        return labels;
    }
    let attributed = !info.turns.is_empty() || !info.others.is_empty();
    if !info.turns.is_empty() {
        // One label per turn, so that the connector between them is the
        // language's own (each is a complete message).
        for turn in info.turns.iter() {
            labels.push(Text::fmt(
                strings,
                keys::review::HUNK_TURN,
                &Args::new().num("ordinal", turn.ordinal),
            ));
        }
    }
    if !info.others.is_empty() {
        labels.push(Text::fmt(
            strings,
            keys::review::HUNK_OTHER_SESSION,
            &Args::new().num("count", info.others.len() as u32),
        ));
    }
    if info.drifted > 0 {
        if attributed {
            labels.push(Text::fmt(
                strings,
                keys::review::HUNK_DRIFTED_LINES,
                &Args::new().num("count", info.drifted),
            ));
        } else {
            labels.push(Text::key(strings, keys::review::HUNK_DRIFTED));
        }
    }
    labels
}

/// A time as the interface says it: "3 min ago" and the like.
pub(crate) fn relative(strings: &Strings, at: Timestamp) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let elapsed = u64::try_from((now - at.0).max(0) / 1000).unwrap_or(0);
    strings.relative_time(std::time::Duration::new(elapsed, 0))
}

fn analysis_name(analysis: &AnalysisState) -> &'static str {
    match analysis {
        AnalysisState::Unavailable(_) => "unavailable",
        AnalysisState::Idle => "idle",
        AnalysisState::Running { .. } => "running",
        AnalysisState::Done { .. } => "done",
        AnalysisState::Failed(_) => "failed",
    }
}

fn alignment_name(alignment: &Alignment) -> &'static str {
    match alignment {
        Alignment::Aligned => "aligned",
        Alignment::Partial { .. } => "partial",
        Alignment::Unaligned => "unaligned",
    }
}

fn survival_name(survival: Survival) -> &'static str {
    match survival {
        Survival::All => "all",
        Survival::Partly => "partly",
        Survival::None => "none",
    }
}

pub(crate) fn seen_name(seen: SeenState) -> &'static str {
    match seen {
        SeenState::Unseen => "unseen",
        SeenState::Seen => "seen",
        SeenState::ChangedSinceSeen { .. } => "changed",
    }
}

fn tool_status_name(status: Option<ToolStatus>) -> &'static str {
    match status {
        None => "running",
        Some(ToolStatus::Completed) => "completed",
        Some(ToolStatus::Failed) => "failed",
        Some(ToolStatus::Denied) => "denied",
        Some(ToolStatus::Interrupted) => "interrupted",
    }
}
