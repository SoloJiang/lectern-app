//! Drawing the items of the narrative list (docs/design/review_view.md
//! 「界面」). Colours come from the `Palette`, text from `Text` and
//! `TextRole`, controls from `lectern_ui`; nothing here reads a file or
//! calls a service except to copy an `Arc`.
//!
//! The item renderer runs for the items the list lays out (the visible ones
//! and the overdraw), each frame it is asked: it builds an element from what
//! the model holds and does nothing else.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AnimationExt, AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Pixels, SharedString, StatefulInteractiveElement, Styled, Window, div, px, svg,
};
use lectern_i18n::{Args, Strings, keys};
use lectern_services::review::{
    Alignment, AnalysisState, AttributionState, FileEntry, FileKey, HunkAttribution, NarrativeItem,
    RowsKey, SeenState, StepEntry, Survival, TurnEntry, TurnRef,
};
use lectern_services::session::{Agent as SessionAgent, Timestamp, ToolStatus};
use lectern_ui::motion::{ARRIVE, ARRIVE_OFFSET, Effective};
use lectern_ui::semantic::{NodeId, Role, Semantic, State, semantic, semantic_interactive};
use lectern_ui::{
    Agent, AgentDot, Button, ButtonKind, ButtonSize, CircleCheck, Disclosure, Icon, Palette,
    SystemColor, Text, TextRole, TextRoleExt, Verbatim, colors,
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

/// The side margin of every item: the reading column's own padding.
pub(crate) const GUTTER: Pixels = px(32.0);

/// The corner radius of a file card and the hairline around it.
const CARD_RADIUS: Pixels = px(12.0);
const HAIRLINE: Pixels = px(0.5);

/// The space between two blocks of a turn (a card above a card, a card
/// below the agent's note).
const BLOCK_GAP: Pixels = px(14.0);

/// The space above the first and below the last line of a diff in its card.
const DIFF_PAD: Pixels = px(6.0);

/// A card's header: the padding and the gap between its parts.
const HEAD_PAD_X: Pixels = px(14.0);
const HEAD_PAD_Y: Pixels = px(10.0);
const HEAD_GAP: Pixels = px(10.0);

/// The separator between the parts of a line of facts.
const SEPARATOR: &str = " \u{00B7} ";

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

/// The agent's product name: ours from the resource files, another source's
/// as written (the same names as the project home's cards).
pub(crate) fn agent_name(strings: &Strings, agent: &SessionAgent) -> String {
    match agent {
        SessionAgent::ClaudeCode => strings
            .get(keys::project_home::AGENT_CLAUDE_CODE)
            .to_owned(),
        SessionAgent::Codex => strings.get(keys::project_home::AGENT_CODEX).to_owned(),
        SessionAgent::Other(name) => name.to_string(),
    }
}

/// How many distinct files the steps of a turn touch.
pub(crate) fn turn_files(turn: &TurnEntry) -> u32 {
    let files: HashSet<&FileKey> = turn
        .items
        .iter()
        .filter_map(|item| match item {
            NarrativeItem::Step(step) => Some(&step.file),
            _ => None,
        })
        .collect();
    files.len() as u32
}

/// Where an item sits in its card: a card is its title (a step or a file)
/// and the items of its diff, drawn one item at a time because the list lays
/// out and draws only what is in view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Slice {
    /// The title: the card's top edge.
    pub first: bool,
    /// The last item of the card: its bottom edge.
    pub last: bool,
    /// The first line of the diff: the room above it.
    pub pad_top: bool,
    /// The last item of the diff: the room below it.
    pub pad_bottom: bool,
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
            Item::Header => self.render_header(&palette, &strings, window, cx),
            Item::Brief => self.render_brief(&palette, &strings, window, cx),
            Item::Notice(notice) => self.render_notice(notice, &palette, &strings, window, cx),
            Item::EarlierTurns { count, expanded } => {
                self.render_earlier(count, expanded, &palette, &strings, window, cx)
            }
            Item::Turn { at, ix, collapsed } => {
                self.render_turn(at, ix, collapsed, &palette, &strings, window, cx)
            }
            Item::Prompt { at, ix } => self.render_prompt(at, ix, &palette, window, cx),
            Item::Text { key, at } => {
                self.render_text(key, at, false, &palette, &strings, window, cx)
            }
            Item::Injected { key, at } => {
                self.render_text(key, at, true, &palette, &strings, window, cx)
            }
            Item::Step { id, at } => {
                let head = self.render_step(ix, id, at, &palette, &strings, window, cx);
                let probe = self
                    .head_rows_key(&self.items[ix])
                    .map(|key| self.diff_node(&key));
                self.in_card(ix, head, probe, &palette)
            }
            Item::File { key, ix: file } => {
                let head = self.render_file(ix, key, file, &palette, &strings, window, cx);
                let probe = self
                    .head_rows_key(&self.items[ix])
                    .map(|key| self.diff_node(&key));
                self.in_card(ix, head, probe, &palette)
            }
            Item::Group { files, .. } => {
                self.render_group(self.first_group(ix), files, &palette, window, cx)
            }
            Item::Noise { count, expanded } => {
                self.render_noise(count, expanded, &palette, &strings, window, cx)
            }
            Item::Hunk {
                rows,
                hunk,
                old,
                new,
            } => {
                let line = self.render_hunk(rows, hunk, old, new, &palette, &strings, window, cx);
                self.in_card(ix, line, None, &palette)
            }
            Item::Row { rows, ix: row, .. } => {
                let line = self.render_row(rows, row, &palette, window, cx);
                self.in_card(ix, line, None, &palette)
            }
            Item::Fold {
                rows,
                ix: fold,
                old,
            } => {
                let line = self.render_fold(rows, fold, old, &palette, &strings, window, cx);
                self.in_card(ix, line, None, &palette)
            }
            Item::Placeholder { rows, lines } => {
                let spec = Semantic::new(
                    nodes::keyed("diff", &nodes::rows_key_text(&rows)).child("placeholder"),
                    Role::Group,
                )
                .attr("lines", lines.to_string());
                let block = semantic(spec, div().w_full().h(self.line_height * lines as f32))
                    .into_any_element();
                self.in_card(ix, block, None, &palette)
            }
            Item::Incomplete { kind, .. } => {
                let text = brief::incomplete_text(&strings, kind);
                let line = div()
                    .w_full()
                    .h(self.line_height)
                    .px(HEAD_PAD_X)
                    .flex()
                    .items_center()
                    .text_role(TextRole::Caption, window, cx)
                    .child(text.shared())
                    .into_any_element();
                self.in_card(ix, line, None, &palette)
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

    /// Whether the group title at `ix` is the first one.
    fn first_group(&self, ix: usize) -> bool {
        !self.items[..ix]
            .iter()
            .any(|item| matches!(item, Item::Group { .. }))
    }

    // ----- file cards ------------------------------------------------------

    /// The diff of the card whose title is `item`.
    fn head_rows_key(&self, item: &Item) -> Option<RowsKey> {
        match item {
            Item::Step { id, at } => {
                let step = items::step_entry(&self.snapshot, *at)?;
                Some(items::step_rows_key(*id, &step.file, &self.context))
            }
            Item::File { key, .. } => Some(items::final_rows_key(key, &self.context)),
            _ => None,
        }
    }

    /// Where item `ix` sits in its card.
    pub(crate) fn slice_of(&self, ix: usize) -> Slice {
        let Some(item) = self.items.get(ix) else {
            return Slice {
                first: true,
                last: true,
                pad_top: false,
                pad_bottom: false,
            };
        };
        let first = item.is_card_head();
        let own = if first {
            self.head_rows_key(item)
        } else {
            item.rows_key().cloned()
        };
        let last = match (own, self.items.get(ix + 1)) {
            (Some(own), Some(next)) => next.rows_key() != Some(&own),
            _ => true,
        };
        let after_head = ix > 0 && self.items[ix - 1].is_card_head();
        Slice {
            first,
            last,
            pad_top: !first && after_head,
            pad_bottom: !first && last,
        }
    }

    /// Puts the piece of a card that item `ix` draws in the card: the fill,
    /// the sides, and the corners and edges at the card's ends.
    /// `probe` is a zero-height node of the diff (the probe's `Diff`), which
    /// stays outside the clipped card so that it counts as on screen.
    fn in_card(
        &self,
        ix: usize,
        content: AnyElement,
        probe: Option<AnyElement>,
        palette: &Palette,
    ) -> AnyElement {
        let slice = self.slice_of(ix);
        let mut body = div()
            .relative()
            .w_full()
            .overflow_hidden()
            .bg(palette.control_background)
            .child(
                div()
                    .w_full()
                    .pt(if slice.pad_top { DIFF_PAD } else { px(0.0) })
                    .pb(if slice.pad_bottom { DIFF_PAD } else { px(0.0) })
                    .child(content),
            );
        // The ring is drawn over the content and takes no room, as the
        // grouped list's does, so that the card's edge shows on a page of
        // its own colour.
        let mut ring = div()
            .absolute()
            .inset_0()
            .border_l(HAIRLINE)
            .border_r(HAIRLINE)
            .border_color(palette.separator);
        if slice.first {
            body = body.rounded_t(CARD_RADIUS);
            ring = ring.rounded_t(CARD_RADIUS).border_t(HAIRLINE);
        }
        if slice.last {
            body = body.rounded_b(CARD_RADIUS);
            ring = ring.rounded_b(CARD_RADIUS).border_b(HAIRLINE);
        }
        div()
            .w_full()
            .px(GUTTER)
            .pt(if slice.first { BLOCK_GAP } else { px(0.0) })
            .child(body.child(ring))
            .children(probe)
            .into_any_element()
    }

    // ----- title and statistics ---------------------------------------------

    fn render_header(
        &self,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let title = self.toolbar_title(strings);
        let snapshot = &self.snapshot;
        let turns = snapshot.turns.as_deref().map(|turns| {
            turns
                .iter()
                .filter(|turn| turn.at.session == 0)
                .count()
                .min(u32::MAX as usize) as u32
        });
        let files = snapshot.files.len() as u32;
        let added = snapshot.totals.added;
        let removed = snapshot.totals.removed;
        let mut spec = Semantic::new(nodes::plain("header"), Role::Group)
            .label(&title)
            .attr("files", files.to_string())
            .attr("added", added.to_string())
            .attr("removed", removed.to_string());
        let mut stats =
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .text_role(TextRole::Secondary, window, cx);
        let separator = || div().child(SEPARATOR);
        let mut first = true;
        if let Some(turns) = turns {
            spec = spec.attr("turns", turns.to_string());
            stats = stats.child(
                div().child(
                    Text::fmt(
                        strings,
                        keys::review::STATS_TURNS,
                        &Args::new().num("count", turns),
                    )
                    .shared(),
                ),
            );
            first = false;
        }
        if !first {
            stats = stats.child(separator());
        }
        stats = stats.child(
            div().child(
                Text::fmt(
                    strings,
                    keys::review::STATS_FILES,
                    &Args::new().num("count", files),
                )
                .shared(),
            ),
        );
        if added > 0 || removed > 0 {
            stats = stats.child(separator()).child(counts(
                strings,
                added as f64,
                removed as f64,
                palette,
            ));
        }
        semantic(
            spec,
            div()
                .w_full()
                .px(GUTTER)
                .pt(GUTTER)
                .pb(px(10.0))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .text_role(TextRole::Title1, window, cx)
                        .font_weight(FontWeight::BOLD)
                        .text_color(palette.label)
                        .child(title.shared()),
                )
                .child(stats),
        )
        .into_any_element()
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
        let mut column = div()
            .w_full()
            .px(GUTTER)
            .pb(px(10.0))
            .flex()
            .flex_col()
            .gap(px(6.0));
        let (analysis, notes) = self.render_analysis(&brief.analysis, palette, strings, window, cx);
        let mut title = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .mb(px(2.0))
            .child(
                div()
                    .text_role(TextRole::Headline, window, cx)
                    .text_color(palette.label)
                    .child(Text::key(strings, keys::review::BRIEF_TITLE).shared()),
            )
            .child(
                div()
                    .text_role(TextRole::Callout, window, cx)
                    .child(Text::key(strings, keys::review::BRIEF_SOURCE).shared()),
            );
        if brief.pending_providers > 0 {
            title = title.child(
                div()
                    .text_role(TextRole::Caption, window, cx)
                    .child(Text::key(strings, keys::review::BRIEF_PENDING).shared()),
            );
        }
        column = column.child(title.child(div().flex_1()).child(analysis));
        if lines.is_empty() {
            column = column.child(
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
                .size(px(8.0))
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
                .gap(px(10.0))
                .cursor_pointer()
                .text_role(TextRole::Body, window, cx)
                .text_color(palette.label)
                .child(dot)
                .child(div().flex_1().min_w_0().child(line.text.shared()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.jump_to_brief(file.clone(), anchor, cx);
                }));
            column = column.child(semantic_interactive(spec, row));
        }
        if brief.omitted > 0 {
            column = column.child(
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
        for note in notes {
            column = column.child(note);
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
            let mut row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .text_role(TextRole::Callout, window, cx)
                .child(
                    svg()
                        .path(Icon::Worktrees.path())
                        .size(px(14.0))
                        .flex_shrink_0()
                        .text_color(palette.secondary_label),
                )
                .child(line.text.shared());
            if let Some(shared) = line.shared {
                row = row.child(div().child(shared.shared()));
            }
            column = column.child(semantic(spec, row));
        }
        semantic(spec, column).into_any_element()
    }

    /// The Analyze control, which sits at the right of the brief's title, and
    /// what a finished or a failed run says below the entries. The view sends
    /// `Analyze` and `CancelAnalyze` and reads `Brief::analysis`; the analysis
    /// itself is the domain's.
    fn render_analysis(
        &self,
        analysis: &AnalysisState,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> (AnyElement, Vec<AnyElement>) {
        let id = nodes::plain("brief").child("analyze");
        let caption = |text: Text, window: &mut Window, cx: &mut Context<ReviewModel>| {
            div()
                .text_role(TextRole::Callout, window, cx)
                .child(text.shared())
                .into_any_element()
        };
        let agent = self.agent_label(strings);
        let analyze_text = match &agent {
            Some(agent) => Text::fmt(
                strings,
                keys::review::ANALYZE_WITH,
                &Args::new().str("agent", agent),
            ),
            None => Text::key(strings, keys::review::ANALYZE),
        };
        let weak = cx.weak_entity();
        let button = |id: NodeId, text: Text, run: Analyze, kind: ButtonKind| {
            let weak = weak.clone();
            Button::new(id, text)
                .size(ButtonSize::Small)
                .kind(kind)
                .on_click(move |_, cx| {
                    let _ = weak.update(cx, |model, _| match run {
                        Analyze::Start => model.analyze(),
                        Analyze::Cancel => model.cancel_analyze(),
                    });
                })
        };
        match analysis {
            AnalysisState::Unavailable(_) => (
                Button::new(id, analyze_text)
                    .size(ButtonSize::Small)
                    .kind(ButtonKind::Secondary)
                    .disabled(true)
                    .into_any_element(),
                Vec::new(),
            ),
            AnalysisState::Idle => (
                button(id, analyze_text, Analyze::Start, ButtonKind::Secondary).into_any_element(),
                Vec::new(),
            ),
            AnalysisState::Running { .. } => (
                semantic(
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
                            ButtonKind::Secondary,
                        )),
                )
                .into_any_element(),
                Vec::new(),
            ),
            AnalysisState::Done { stale, partial } => {
                let mut notes = Vec::new();
                if *stale {
                    notes.push(caption(
                        Text::key(strings, keys::review::ANALYZE_STALE),
                        window,
                        cx,
                    ));
                }
                if *partial {
                    notes.push(caption(
                        Text::key(strings, keys::review::ANALYZE_PARTIAL),
                        window,
                        cx,
                    ));
                }
                (
                    semantic(
                        Semantic::new(id.clone(), Role::Group).attr("state", "done"),
                        button(
                            id.child("again"),
                            Text::key(strings, keys::review::ANALYZE_AGAIN),
                            Analyze::Start,
                            ButtonKind::Plain,
                        ),
                    )
                    .into_any_element(),
                    notes,
                )
            }
            AnalysisState::Failed(reason) => {
                let note = div()
                    .text_role(TextRole::Callout, window, cx)
                    .text_color(palette.secondary_label)
                    .child(
                        Text::fmt(
                            strings,
                            keys::review::ANALYZE_FAILED,
                            &Args::new().str("reason", reason),
                        )
                        .shared(),
                    )
                    .into_any_element();
                (
                    semantic(
                        Semantic::new(id.clone(), Role::Group).attr("state", "failed"),
                        button(
                            id.child("again"),
                            Text::key(strings, keys::review::ANALYZE_AGAIN),
                            Analyze::Start,
                            ButtonKind::Plain,
                        ),
                    )
                    .into_any_element(),
                    vec![note],
                )
            }
        }
    }

    /// The name of the reviewed session's agent, when the review is of a
    /// session.
    pub(crate) fn agent_label(&self, strings: &Strings) -> Option<String> {
        match self.target {
            lectern_services::review::ReviewTarget::Session(_) => self
                .snapshot
                .sessions
                .first()
                .map(|session| agent_name(strings, &session.agent)),
            _ => None,
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
            .py(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .cursor_pointer()
            .text_role(TextRole::Secondary, window, cx)
            .text_color(palette.secondary_label)
            .child(Disclosure::new(if expanded { 1.0 } else { 0.0 }))
            .child(text.shared())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_earlier(cx)));
        semantic_interactive(spec, row).into_any_element()
    }

    // ----- turns -----------------------------------------------------------

    #[allow(
        clippy::too_many_arguments,
        reason = "an item renderer: the pieces are the item's and the frame's"
    )]
    fn render_turn(
        &self,
        at: TurnRef,
        ix: u32,
        collapsed: bool,
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
        let files = turn_files(turn);
        let tokens = self.turn_tokens(turn);
        let mut spec = Semantic::new(node.clone(), Role::Group)
            .label(&title)
            .state(State {
                expanded: Some(!collapsed),
                ..State::default()
            })
            .attr("after_last_seen", turn.after_last_seen.to_string())
            .attr("running", turn.ended_at.is_none().to_string());
        if let Some(tokens) = tokens {
            spec = spec.attr("tokens", tokens.to_string());
        }
        let files_text = Text::fmt(
            strings,
            keys::review::TURN_FILES,
            &Args::new().num("count", files),
        );
        let toggle = move |this: &mut ReviewModel,
                           _: &gpui::ClickEvent,
                           _: &mut Window,
                           cx: &mut Context<ReviewModel>| {
            this.toggle_turn(at, cx);
        };
        let row = if collapsed {
            let aside = if files == 0 {
                Text::key(strings, keys::review::TURN_NONE)
            } else {
                files_text
            };
            div()
                .id(&node)
                .w_full()
                .py(px(10.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .cursor_pointer()
                .border_t(HAIRLINE)
                .border_color(palette.separator)
                .text_role(TextRole::Secondary, window, cx)
                .text_color(palette.secondary_label)
                .child(Disclosure::new(0.0))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.shared()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(turn.prompt_preview.clone())),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_role(TextRole::Callout, window, cx)
                        .child(aside.shared()),
                )
                .on_click(cx.listener(toggle))
        } else {
            let when = if turn.ended_at.is_none() {
                Text::key(strings, keys::review::TURN_RUNNING)
            } else {
                verbatim(relative(strings, turn.started_at))
            };
            let mut facts: Vec<Text> = vec![when];
            if files > 0 {
                facts.push(files_text);
            }
            if let Some(tokens) = tokens {
                facts.push(brief::turn_tokens_text(strings, tokens));
            }
            let joined: Vec<String> = facts.iter().map(|text| text.as_str().to_owned()).collect();
            div()
                .id(&node)
                .w_full()
                .pt(px(12.0))
                .flex()
                .flex_row()
                .items_baseline()
                .gap(px(10.0))
                .cursor_pointer()
                .border_t(HAIRLINE)
                .border_color(palette.separator)
                .child(
                    div()
                        .text_role(TextRole::Headline, window, cx)
                        .text_color(palette.secondary_label)
                        .child(title.shared()),
                )
                .child(
                    div()
                        .text_role(TextRole::Callout, window, cx)
                        .child(SharedString::from(joined.join(SEPARATOR))),
                )
                .on_click(cx.listener(toggle))
        };
        // The rule above the turn spans the column between its side margins.
        let row = div()
            .w_full()
            .pt(px(22.0))
            .px(GUTTER)
            .child(semantic_interactive(spec, row));
        if self.arriving.contains(&at) {
            let moves = matches!(ARRIVE.effective(cx), Effective::Move(_));
            let id = gpui::ElementId::from(&node);
            div()
                .w_full()
                .child(row)
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
            row.into_any_element()
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
            div()
                .w_full()
                .px(GUTTER)
                .pt(BLOCK_GAP)
                .text_role(TextRole::Title2, window, cx)
                .font_weight(FontWeight::MEDIUM)
                .text_color(palette.label)
                .child(body),
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
                .text_role(TextRole::Secondary, window, cx)
                .child(SharedString::from(text.clone()))
                .into_any_element(),
            None => lectern_ui::Skeleton::new(px(320.0), px(12.0)).into_any_element(),
        };
        let mut column = div()
            .w_full()
            .px(GUTTER)
            .pt(BLOCK_GAP)
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

    #[allow(
        clippy::too_many_arguments,
        reason = "an item renderer: the pieces are the item's and the frame's"
    )]
    fn render_step(
        &self,
        list_ix: usize,
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
        let head = self.card_head(
            list_ix,
            Head {
                node,
                file,
                path,
                seen,
                selected,
                added: step.added,
                removed: step.removed,
                notes,
                key: ItemKey::Step(id),
            },
            palette,
            strings,
            window,
            cx,
        );
        semantic_interactive(spec, head).into_any_element()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "an item renderer: the pieces are the item's and the frame's"
    )]
    fn render_file(
        &self,
        list_ix: usize,
        key: FileKey,
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
        let mut notes: Vec<Text> = self.seen_note(seen, strings).into_iter().collect();
        notes.extend(self.file_only_turns(&key, strings));
        let head = self.card_head(
            list_ix,
            Head {
                node,
                file: key.clone(),
                path,
                seen,
                selected,
                added,
                removed,
                notes,
                key: ItemKey::File(key),
            },
            palette,
            strings,
            window,
            cx,
        );
        semantic_interactive(spec, head).into_any_element()
    }

    /// The title of a file card: the circular check, the path, the counts and
    /// the button that opens the file; the notes about the step under them.
    /// A line under the title separates it from the diff when there is one.
    fn card_head(
        &self,
        list_ix: usize,
        head: Head,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> gpui::Stateful<gpui::Div> {
        let Head {
            node,
            file,
            path,
            seen,
            selected,
            added,
            removed,
            notes,
            key,
        } = head;
        let has_diff = !self.slice_of(list_ix).last;
        let check = self.check(&node.child("check"), &file, seen, cx);
        let path_color = if matches!(seen, SeenState::Seen) {
            palette.secondary_label
        } else {
            palette.label
        };
        let mut top = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(HEAD_GAP)
            .child(check)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_role(TextRole::Code, window, cx)
                    .text_color(path_color)
                    .child(SharedString::from(path)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_role(TextRole::Callout, window, cx)
                    .child(counts(
                        strings,
                        f64::from(added),
                        f64::from(removed),
                        palette,
                    )),
            )
            .child(div().flex_1())
            .child(self.open_button(&node, &file, key.clone(), strings, window, cx));
        top = top.min_w_0();
        let mut head = div()
            .id(&node)
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(HEAD_PAD_X)
            .py(HEAD_PAD_Y)
            .cursor_pointer()
            .when_selected(selected, palette)
            .child(top);
        if has_diff {
            head = head.border_b(HAIRLINE).border_color(palette.separator);
        }
        for note in notes {
            head = head.child(
                div()
                    .pl(px(32.0))
                    .text_role(TextRole::Caption, window, cx)
                    .child(note.shared()),
            );
        }
        head.on_click(cx.listener(move |this, _, _, cx| {
            this.select(Some(key.clone()), cx);
        }))
    }

    /// A file with only file-level attribution (a binary file, one past the
    /// chain's limit) names the turns that changed it in its title; its hunks
    /// carry no labels.
    fn file_only_turns(&self, file: &FileKey, strings: &Strings) -> Vec<Text> {
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

    /// The button of a card's title that opens the file in the reader
    /// (⇧⌘O): disabled when the reader is not there. The key stands after
    /// it, in the secondary colour.
    fn open_button(
        &self,
        node: &NodeId,
        file: &FileKey,
        select: ItemKey,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let weak = cx.weak_entity();
        let file = file.clone();
        let button = Button::new(
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
        });
        div()
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .child(button)
            .child(
                div()
                    .text_role(TextRole::Callout, window, cx)
                    .child(Text::key(strings, keys::review::OPEN_FILE_SHORTCUT).shared()),
            )
            .into_any_element()
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
        id: &NodeId,
        file: &FileKey,
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
        _window: &mut Window,
        _cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        // The header block above the entries already says how many files
        // there are; between groups a rule.
        let node = nodes::plain("group");
        let mut column = div().w_full().px(GUTTER).pt(px(12.0)).flex().flex_col();
        if !first {
            column = column.child(div().h(HAIRLINE).w_full().bg(palette.separator));
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

    /// A hunk's place in the list. A session's narrative draws nothing for it
    /// but the Drifted note, since a step is already under its turn; the
    /// rule-grouped view draws the turns and sessions the hunk comes from.
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
        let session_view = matches!(rows, RowsKey::Step { .. });
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
        let drift_note = (drifted > 0 || unaligned) && !(pending && !unaligned);
        // Titles of the rule-grouped view: the attribution, without the bare
        // "Drifted" the note says better.
        let labels: Vec<Text> = if session_view {
            Vec::new()
        } else {
            hunk_labels(info.as_ref(), pending, unaligned, strings)
                .into_iter()
                .filter(|label| label.message_id() != Some(keys::review::HUNK_DRIFTED.name()))
                .collect()
        };
        let has_title = !labels.is_empty();
        let mut column = div().w_full().flex().flex_col().id(&node);
        if has_title {
            let mut line = div()
                .w_full()
                .h(self.line_height)
                .px(HEAD_PAD_X)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .text_role(TextRole::Caption, window, cx)
                .when_selected(selected, palette);
            for label in labels {
                line = line.child(div().child(label.shared()));
            }
            column = column.child(line);
        }
        if drift_note {
            column = column.child(
                div()
                    .w_full()
                    .h(self.line_height)
                    .pl(self.code_indent(window, cx))
                    .pr(HEAD_PAD_X)
                    .flex()
                    .flex_row()
                    .items_center()
                    .when_selected(selected, palette)
                    .text_role(TextRole::Callout, window, cx)
                    .text_color(palette.system(SystemColor::Orange))
                    .child(Text::key(strings, keys::review::DRIFT_NOTE_UNTIMED).shared()),
            );
        }
        // A hunk with nothing to say keeps a hairline of height, so that it
        // has a place in the list (and counts as on screen for the probe).
        if !has_title && !drift_note {
            column = column.h(HAIRLINE);
        }
        let click_key = key;
        let column = column
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select(Some(click_key.clone()), cx);
            }));
        semantic_interactive(spec, column).into_any_element()
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
            .pt(px(6.0))
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
            .pt(px(6.0))
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
                .pt(px(4.0))
                .text_role(TextRole::Secondary, window, cx)
                .truncate()
                .child(text.shared()),
        )
        .into_any_element()
    }
}

/// What a card's title shows.
struct Head {
    node: NodeId,
    file: FileKey,
    path: String,
    seen: SeenState,
    selected: bool,
    added: u32,
    removed: u32,
    notes: Vec<Text>,
    key: ItemKey,
}

/// `+12 −3`: the counts of added and removed lines, in green and red. A
/// zero count is not shown, but one that is both is `+0`.
pub(crate) fn counts(strings: &Strings, added: f64, removed: f64, palette: &Palette) -> AnyElement {
    let green = palette.system(SystemColor::Green);
    let red = palette.system(SystemColor::Red);
    let mut row = div().flex().flex_row().gap(px(6.0));
    if added > 0.0 || removed <= 0.0 {
        row = row.child(
            div().text_color(green).child(
                Text::fmt(
                    strings,
                    keys::review::ADDED,
                    &Args::new().num("count", added),
                )
                .shared(),
            ),
        );
    }
    if removed > 0.0 {
        row = row.child(
            div().text_color(red).child(
                Text::fmt(
                    strings,
                    keys::review::REMOVED,
                    &Args::new().num("count", removed),
                )
                .shared(),
            ),
        );
    }
    row.into_any_element()
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
