//! The permission cards (docs/design/conversation.md「数据结构」的「权限请求」).
//! The card of the oldest request is shown whole, fixed between the list and
//! the reply box; the others sit behind a line "n more requests". The
//! buttons come from the kinds of the request's options, in the order of
//! `OptionKind`, with the text of the resource files, never the agent's
//! words (ADR 0050). Nothing is the default: ↵ answers nothing and the
//! focus does not move to a card.

use gpui::{InteractiveElement, StatefulInteractiveElement};
use lectern_services::conversation::{EditPreview, PermissionDetail, PermissionRequest, RowKind};
use lectern_ui::semantic::{Role, Semantic, semantic};

use super::*;

const CARD_RADIUS: Pixels = px(12.0);
const CARD_PADDING: Pixels = px(10.0);
const BORDER: Pixels = px(0.5);
const BLOCK_RADIUS: Pixels = px(6.0);
/// A command shows this many lines before it scrolls.
const COMMAND_LINES: f32 = 12.0;
/// The edits of a card show this much before they scroll.
const EDITS_MAX: Pixels = px(240.0);
/// The height of the open "more requests" region; its cards scroll inside.
const MORE_HEIGHT: Pixels = px(260.0);
/// The width of the sign column of a diff row.
const SIGN_WIDTH: Pixels = px(12.0);

impl MessagesModel {
    /// The oldest request whole and, behind a line, the others. None when
    /// nothing waits for an answer.
    pub(super) fn render_permissions(
        &mut self,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> Option<AnyElement> {
        let requests = self.snapshot.as_ref()?.permissions.clone();
        let (first, rest) = requests.split_first()?;
        let mut column = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(GAP)
            .child(self.render_card(first, strings, window, cx));
        if rest.is_empty() {
            self.more_open = false;
            self.more = (Transition::new(0.0), Transition::new(0.0));
            return Some(column.into_any_element());
        }
        let target = if self.more_open { MORE_HEIGHT } else { px(0.0) };
        let now = Instant::now();
        Expander::go(&mut self.more.0, &mut self.more.1, target, now, cx);
        let height = px(self.more.0.tick(now, window));
        let fade = self.more.1.tick(now, window);
        let cards: Vec<AnyElement> = if self.more_open || height > px(0.0) {
            rest.iter()
                .map(|request| self.render_card(request, strings, window, cx))
                .collect()
        } else {
            Vec::new()
        };
        let model = cx.entity();
        let header = Button::new(
            nodes::area("permission").child("more"),
            Text::fmt(
                strings,
                keys::conversation::PERMISSION_MORE,
                &Args::new().num("count", rest.len() as f64),
            ),
        )
        .kind(ButtonKind::Plain)
        .size(ButtonSize::Small)
        .attr("expanded", self.more_open.to_string())
        .on_click(move |_, cx| model.update(cx, |model, cx| model.toggle_more(cx)));
        let scroller = div()
            .id("conversation-more-requests")
            .size_full()
            .overflow_y_scroll()
            .child(div().flex().flex_col().gap(GAP).children(cards));
        column = column.child(div().child(header)).child(Expander::new(
            nodes::area("permission").child("rest"),
            height,
            fade,
            scroller,
        ));
        Some(column.into_any_element())
    }

    /// One request: the question, the agent's title, the command or the
    /// changes, and the answer buttons.
    fn render_card(
        &self,
        request: &PermissionRequest,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> AnyElement {
        let palette = *colors(window, cx);
        let heading = present::permission_heading(strings, request);
        let id = request.id;
        let model = cx.entity();
        let mut buttons = div().w_full().flex().flex_row().flex_wrap().gap(px(6.0));
        for (kind, _) in present::option_buttons(&request.options) {
            let model = model.clone();
            buttons = buttons.child(
                Button::new(
                    nodes::permission_option(id, kind),
                    present::option_text(strings, kind),
                )
                .kind(if kind == OptionKind::AllowOnce {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Secondary
                })
                .size(ButtonSize::Small)
                .on_click(move |_, cx| model.update(cx, |model, cx| model.answer(id, kind, cx))),
            );
        }
        let card = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(GAP)
            .p(CARD_PADDING)
            .rounded(CARD_RADIUS)
            .bg(palette.control_background)
            .border(BORDER)
            .border_color(palette.separator)
            .child(
                div()
                    .text_role(TextRole::Headline, window, cx)
                    .child(heading.shared()),
            )
            .child(
                div()
                    .max_h(px(48.0))
                    .overflow_hidden()
                    .text_role(TextRole::Secondary, window, cx)
                    .child(Text::verbatim(request.title.clone()).shared()),
            )
            .child(self.render_detail(request, strings, window, cx))
            .child(buttons);
        let spec = Semantic::new(nodes::permission(id), Role::Dialog)
            .label(&heading)
            .attr("kind", diag::call_kind_name(request.kind))
            .attr("detail", diag::detail_name(&request.detail));
        let node = semantic(spec, card).into_any_element();
        if !self.is_arriving_card(id) {
            return node;
        }
        let moves = matches!(ARRIVE.effective(cx), Effective::Move(_));
        div()
            .w_full()
            .child(node)
            .with_animation(
                ElementId::NamedInteger("conversation-card-arrive".into(), id.0),
                ARRIVE.animation(cx),
                move |el, t| {
                    let offset = if moves {
                        ARRIVE_OFFSET * (1.0 - t)
                    } else {
                        Pixels::ZERO
                    };
                    el.opacity(t).mt(offset)
                },
            )
            .into_any_element()
    }

    /// Whether the card of a request arrived within the length of the
    /// arrival animation.
    fn is_arriving_card(&self, id: PermissionId) -> bool {
        self.arriving_cards
            .get(&id)
            .is_some_and(|at| at.elapsed() < ARRIVE.duration * 2)
    }

    /// What the request is about, as the agent sent it.
    fn render_detail(
        &self,
        request: &PermissionRequest,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> AnyElement {
        let id = request.id;
        match &request.detail {
            PermissionDetail::Command { text, workdir } => {
                let palette = *colors(window, cx);
                let command = Text::verbatim(text.clone());
                let block = div()
                    .id(ElementId::NamedInteger("conversation-command".into(), id.0))
                    .w_full()
                    .max_h(TextRole::Code.line_height() * COMMAND_LINES)
                    .overflow_y_scroll()
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(BLOCK_RADIUS)
                    .bg(palette.fill)
                    .text_role(TextRole::Code, window, cx)
                    .child(command.shared());
                let mut column = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(semantic(
                        Semantic::new(nodes::permission(id).child("command"), Role::Text)
                            .label(&command),
                        block,
                    ));
                if let Some(workdir) = workdir
                    && self.cwd.as_deref() != Some(workdir.as_ref())
                {
                    let shown = self
                        .cwd
                        .as_deref()
                        .and_then(|cwd| workdir.strip_prefix(cwd).ok())
                        .filter(|relative| !relative.as_os_str().is_empty())
                        .unwrap_or(workdir);
                    let shown = shown.display().to_string();
                    let place = Text::fmt(
                        strings,
                        keys::conversation::PERMISSION_WORKDIR,
                        &Args::new().str("path", &shown),
                    );
                    column = column.child(semantic(
                        Semantic::new(nodes::permission(id).child("workdir"), Role::Text)
                            .label(&place)
                            .attr("path", shown),
                        div()
                            .text_role(TextRole::Caption, window, cx)
                            .child(place.shared()),
                    ));
                }
                column.into_any_element()
            }
            PermissionDetail::Edits(edits) if !edits.is_empty() => {
                let files: Vec<AnyElement> = edits
                    .iter()
                    .map(|edit| self.render_edit(id, edit, strings, window, cx))
                    .collect();
                div()
                    .id(ElementId::NamedInteger("conversation-edits".into(), id.0))
                    .w_full()
                    .max_h(EDITS_MAX)
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(GAP)
                    .children(files)
                    .into_any_element()
            }
            PermissionDetail::Edits(_) | PermissionDetail::None => {
                let none = Text::key(strings, keys::conversation::PERMISSION_NO_DETAIL);
                semantic(
                    Semantic::new(nodes::permission(id).child("no-detail"), Role::Text)
                        .label(&none),
                    div()
                        .text_role(TextRole::Caption, window, cx)
                        .child(none.shared()),
                )
                .into_any_element()
            }
        }
    }

    /// One changed file: its place, the counts and the rows of the change.
    fn render_edit(
        &self,
        request: PermissionId,
        edit: &EditPreview,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> AnyElement {
        let palette = *colors(window, cx);
        let relative = self
            .cwd
            .as_deref()
            .and_then(|cwd| edit.path.strip_prefix(cwd).ok())
            .unwrap_or(&edit.path);
        let place = Text::verbatim(Arc::<str>::from(relative.display().to_string()));
        let counts = format!("+{} \u{2212}{}", edit.added, edit.removed);
        let file_node = nodes::permission(request)
            .child("edit")
            .item(&nodes::safe(&edit.path.display().to_string()));
        let rows: Vec<AnyElement> = edit
            .rows
            .iter()
            .enumerate()
            .map(|(at, row)| {
                let (sign, background, kind) = match row.kind {
                    RowKind::Added => ("+", Some(palette.diff_added_background), "added"),
                    RowKind::Removed => {
                        ("\u{2212}", Some(palette.diff_removed_background), "removed")
                    }
                    RowKind::Context => (" ", None, "context"),
                };
                let text = Text::verbatim(row.text.clone());
                let mut line = div()
                    .w_full()
                    .min_h(TextRole::Code.line_height())
                    .flex()
                    .flex_row()
                    .text_role(TextRole::Code, window, cx)
                    .child(div().w(SIGN_WIDTH).flex_shrink_0().child(sign))
                    .child(div().flex_1().min_w_0().child(text.shared()));
                if let Some(background) = background {
                    line = line.bg(background);
                }
                semantic(
                    Semantic::new(file_node.child("row").item(&at.to_string()), Role::DiffLine)
                        .label(&text)
                        .attr("kind", kind),
                    line,
                )
                .into_any_element()
            })
            .collect();
        let mut column = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .gap(GAP)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_role(TextRole::Callout, window, cx)
                            .text_color(palette.label)
                            .child(place.shared()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_role(TextRole::Caption, window, cx)
                            .child(counts),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .rounded(BLOCK_RADIUS)
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .children(rows),
            );
        if edit.omitted > 0 {
            column = column.child(
                div().text_role(TextRole::Caption, window, cx).child(
                    Text::fmt(
                        strings,
                        keys::conversation::PERMISSION_OMITTED,
                        &Args::new().num("count", edit.omitted),
                    )
                    .shared(),
                ),
            );
        }
        semantic(
            Semantic::new(file_node, Role::Group)
                .label(&place)
                .attr("added", edit.added.to_string())
                .attr("removed", edit.removed.to_string())
                .attr("new", edit.new_file.to_string())
                .attr("rows", edit.rows.len().to_string())
                .attr("omitted", edit.omitted.to_string()),
            column,
        )
        .into_any_element()
    }
}
