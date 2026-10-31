//! Drawing review notes and their editor (docs/design/review_view.md「审阅意见
//! （#22）」「界面」): a note's block below its row, the editor, the block of
//! notes on files that are not in the review. Colours from the `Palette`,
//! text from `Text` and `TextRole`, controls from `lectern_ui`.

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use lectern_i18n::{Args, Strings, keys};
use lectern_services::review::{DiffSide, Note, NoteId, NotePlace, NoteState, SentVia};
use lectern_ui::semantic::{Role, Semantic, State, semantic, semantic_interactive};
use lectern_ui::{
    Button, ButtonKind, ButtonSize, Palette, SystemColor, Text, TextArea, TextRole, TextRoleExt,
};

use crate::actions::{CancelNote, SaveNote};
use crate::brief;
use crate::items::ItemKey;
use crate::model::ReviewModel;
use crate::nodes;
use crate::notes::{EditorTarget, Saving};
use crate::render::{GUTTER, relative};

/// The block's corner radius and the room it keeps around itself.
const RADIUS: Pixels = px(8.0);
const MARGIN_Y: Pixels = px(4.0);
const PAD_X: Pixels = px(10.0);
const PAD_Y: Pixels = px(8.0);
const GAP: Pixels = px(4.0);

impl ReviewModel {
    /// The element of a note item. `in_rows` says whether it is drawn inside
    /// a diff's card, aligned with the code, or in the block of notes on
    /// files that are not in the review.
    #[allow(
        clippy::too_many_arguments,
        reason = "an item renderer: the pieces are the item's and the frame's"
    )]
    pub(crate) fn render_note(
        &mut self,
        id: NoteId,
        in_rows: bool,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let Some(note) = self.note(id).cloned() else {
            return div().into_any_element();
        };
        let key = format!("{:016x}", id.0);
        let node = nodes::keyed("note", &key);
        let selected = self.selection == Some(ItemKey::Note(id));
        let (place, side) = (
            match &note.place {
                NotePlace::Rows(_) => "rows",
                NotePlace::Drifted { .. } => "drifted",
                NotePlace::Elsewhere => "elsewhere",
            },
            match note.anchor.side {
                DiffSide::Old => "old",
                DiffSide::New => "new",
            },
        );
        let state_name = match note.state {
            NoteState::Pending => "pending",
            NoteState::Sending(_) => "sending",
            NoteState::Sent(_) => "sent",
        };
        let spec = Semantic::new(node.clone(), Role::Comment)
            .label(&Text::verbatim(note.text.clone()))
            .state(State {
                selected,
                ..State::default()
            })
            .attr("state", state_name)
            .attr("place", place)
            .attr("side", side)
            .attr("line", note.anchor.line.to_string());

        let status = self.note_status(&note, strings);
        let weak = cx.weak_entity();
        let mut header = div().flex().flex_row().items_center().gap(GAP).child(
            div()
                .flex_1()
                .min_w_0()
                .text_role(TextRole::Caption, window, cx)
                .text_color(palette.secondary_label)
                .child(status.shared()),
        );
        // Not sent: change or delete; sent: delete; being sent: neither.
        if matches!(note.state, NoteState::Pending) {
            let edit = weak.clone();
            header = header.child(
                Button::new(
                    node.child("edit"),
                    Text::key(strings, keys::review::NOTE_EDIT),
                )
                .kind(ButtonKind::Plain)
                .size(ButtonSize::Small)
                .on_click(move |window, cx| {
                    let _ = edit.update(cx, |model, cx| model.edit_note(id, window, cx));
                }),
            );
        }
        if !matches!(note.state, NoteState::Sending(_)) {
            let delete = weak.clone();
            header = header.child(
                Button::new(
                    node.child("delete"),
                    Text::key(strings, keys::review::NOTE_DELETE),
                )
                .kind(ButtonKind::Plain)
                .size(ButtonSize::Small)
                .on_click(move |_, cx| {
                    let _ = delete.update(cx, |model, cx| model.delete_note(id, cx));
                }),
            );
        }
        let mut body = div()
            .rounded(RADIUS)
            .bg(if selected {
                palette.fill_strong
            } else {
                palette.fill
            })
            .px(PAD_X)
            .py(PAD_Y)
            .flex()
            .flex_col()
            .gap(GAP)
            .child(header);
        if !in_rows {
            body = body.child(
                div()
                    .text_role(TextRole::Caption, window, cx)
                    .text_color(palette.secondary_label)
                    .child(Text::verbatim(brief::file_text(&note.anchor.file)).shared()),
            );
        }
        if matches!(note.place, NotePlace::Drifted { .. }) {
            body = body.child(
                div()
                    .text_role(TextRole::Caption, window, cx)
                    .text_color(palette.system(SystemColor::Orange))
                    .child(
                        Text::fmt(
                            strings,
                            keys::review::NOTE_DRIFTED,
                            &Args::new().num("line", note.anchor.line.saturating_add(1)),
                        )
                        .shared(),
                    ),
            );
        }
        if !matches!(note.place, NotePlace::Rows(_))
            && let Some(excerpt) = &note.excerpt
        {
            let mut lines = div()
                .flex()
                .flex_col()
                .text_role(TextRole::Code, window, cx);
            for (n, line) in excerpt.lines.iter().enumerate() {
                let own = excerpt.first.saturating_add(n as u32) == note.anchor.line;
                lines = lines.child(
                    div()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_color(if own {
                            palette.label
                        } else {
                            palette.secondary_label
                        })
                        .child(SharedString::from(line.to_string())),
                );
            }
            body = body.child(lines);
        }
        body = body.child(
            div()
                .text_role(TextRole::Body, window, cx)
                .text_color(palette.label)
                .child(SharedString::from(note.text.clone())),
        );
        let click = node.clone();
        let block =
            div()
                .id(&click)
                .w_full()
                .child(body)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select(Some(ItemKey::Note(id)), cx);
                }));
        let block = semantic_interactive(spec, block).into_any_element();
        self.note_frame(block, in_rows, window, cx)
    }

    /// The status line of a note: not sent, being sent, sent (or copied)
    /// some time ago.
    fn note_status(&self, note: &Note, strings: &Strings) -> Text {
        match note.state {
            NoteState::Pending => Text::key(strings, keys::review::NOTE_PENDING),
            NoteState::Sending(_) => Text::key(strings, keys::review::NOTES_SENDING),
            NoteState::Sent(batch) => {
                let sent = self
                    .snapshot
                    .notes_state
                    .batches
                    .iter()
                    .find(|sent| sent.id == batch);
                let (key, at) = match sent {
                    Some(sent) if sent.via == SentVia::Clipboard => {
                        (keys::review::NOTE_COPIED, sent.at)
                    }
                    Some(sent) => (keys::review::NOTE_SENT, sent.at),
                    None => (keys::review::NOTE_SENT, note.created_at),
                };
                Text::fmt(
                    strings,
                    key,
                    &Args::new().str("when", &relative(strings, at)),
                )
            }
        }
    }

    /// Puts a note's or the editor's block where it goes: aligned with the
    /// code inside a card, or between the side margins outside one.
    fn note_frame(
        &self,
        block: AnyElement,
        in_rows: bool,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        if in_rows {
            let indent = self.code_indent(window, cx);
            div()
                .w_full()
                .pl(indent)
                .pr(px(12.0))
                .py(MARGIN_Y)
                .child(block)
                .into_any_element()
        } else {
            div()
                .w_full()
                .px(GUTTER)
                .py(MARGIN_Y)
                .child(block)
                .into_any_element()
        }
    }

    /// The title of the block of notes on files that are not in the review.
    pub(crate) fn render_notes_elsewhere(
        &self,
        count: u32,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let title = Text::key(strings, keys::review::NOTES_ELSEWHERE);
        let spec = Semantic::new(nodes::plain("notes-elsewhere"), Role::Group)
            .label(&title)
            .attr("count", count.to_string());
        semantic(
            spec,
            div()
                .w_full()
                .px(GUTTER)
                .pt(px(22.0))
                .pb(px(4.0))
                .text_role(TextRole::Callout, window, cx)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(palette.secondary_label)
                .child(title.shared()),
        )
        .into_any_element()
    }

    /// The editor: the text box, Cancel and Save (docs/design/review_view.md
    /// 「审阅意见（#22）」).
    pub(crate) fn render_note_editor(
        &mut self,
        in_rows: bool,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let Some(editor) = &self.editor else {
            return div().into_any_element();
        };
        let state = editor.state.clone();
        let target = editor.target.clone();
        let saving = self.saving(cx);
        let (kind, side, line, save_key) = match &target {
            EditorTarget::New { anchor, .. } => {
                ("new", anchor.side, anchor.line, keys::review::NOTE_ADD)
            }
            EditorTarget::Existing(id) => match self.note(*id) {
                Some(note) => (
                    "existing",
                    note.anchor.side,
                    note.anchor.line,
                    keys::review::NOTE_SAVE,
                ),
                None => ("existing", DiffSide::New, 0, keys::review::NOTE_SAVE),
            },
        };
        let node = nodes::plain("note-editor");
        let spec = Semantic::new(node.clone(), Role::Group)
            .attr("target", kind)
            .attr(
                "side",
                match side {
                    DiffSide::Old => "old",
                    DiffSide::New => "new",
                },
            )
            .attr("line", line.to_string())
            .attr("too_long", (saving == Saving::TooLong).to_string());
        let area = TextArea::new(node.child("text"), &state)
            .lines(2, 8)
            .placeholder(Text::key(strings, keys::review::NOTE_PLACEHOLDER))
            .on_submit(|window, cx| window.dispatch_action(Box::new(SaveNote), cx))
            .on_cancel(|window, cx| window.dispatch_action(Box::new(CancelNote), cx));
        let weak = cx.weak_entity();
        let cancel = {
            let weak = weak.clone();
            Button::new(
                node.child("cancel"),
                Text::key(strings, keys::review::CANCEL),
            )
            .kind(ButtonKind::Plain)
            .size(ButtonSize::Small)
            .on_click(move |window, cx| {
                let _ = weak.update(cx, |model, cx| model.cancel_note(window, cx));
            })
        };
        let hint = strings.get(keys::review::SEND_TO_SHORTCUT);
        let save = Button::new(
            node.child("save"),
            Text::verbatim(format!("{} {hint}", strings.get(save_key))),
        )
        .kind(ButtonKind::Primary)
        .size(ButtonSize::Small)
        .disabled(saving != Saving::Ready)
        .on_click(move |window, cx| {
            let _ = weak.update(cx, |model, cx| model.save_note(window, cx));
        });
        let mut footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(div().flex_1().min_w_0());
        if saving == Saving::TooLong {
            let hint = Text::key(strings, keys::review::NOTE_TOO_LONG);
            footer = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(semantic(
                    Semantic::new(node.child("too-long"), Role::Text).label(&hint),
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_role(TextRole::Caption, window, cx)
                        .text_color(palette.system(SystemColor::Orange))
                        .child(hint.shared()),
                ));
        }
        let block = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(area)
            .child(footer.child(cancel).child(save));
        let block = semantic(spec, block).into_any_element();
        self.note_frame(block, in_rows, window, cx)
    }
}
