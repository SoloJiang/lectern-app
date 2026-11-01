//! The reply box (docs/design/conversation.md「数据结构」的「一处驱动」「草稿」,
//! 「状态与异常」): the text box with Send, or Stop while a turn runs, the line
//! that says why it cannot send, the line that says the session may still run
//! somewhere else, and the sending itself.

use lectern_services::conversation::Unavailable;
use lectern_ui::TextArea;
use lectern_ui::semantic::{Role, Semantic, semantic};

use super::*;
use crate::drive::{Block, send_block};

impl MessagesModel {
    /// ⌘↩ and Send: sends the text as a prompt. Nothing is sent when the
    /// text is blank, a terminal drives the session or the provider cannot
    /// take a prompt; the text stays then, and stays too when the provider
    /// did not start the turn. The box is emptied once the link is
    /// `Connecting` or `Working`.
    pub(crate) fn submit(&mut self, window: &mut Window, cx: &mut Context<MessagesModel>) {
        let _scope = self.scope();
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let text: Arc<str> = Arc::from(self.draft.trim());
        if text.is_empty() {
            debug!("send skipped"; reason = "empty");
            return;
        }
        match send_block(
            &snapshot.can_prompt,
            self.shared.terminal_drives(&self.session),
        ) {
            Some(Block::InTerminal) => {
                debug!("send skipped"; reason = "in_terminal");
                return;
            }
            Some(Block::Unavailable(_)) => {
                debug!("send skipped"; reason = "unavailable");
                return;
            }
            None => {}
        }
        let bytes = diag::count(text.len());
        let connects = matches!(snapshot.link, Link::None | Link::Failed(_));
        self.send(ConversationCommand::Prompt {
            text,
            origin: PromptOrigin::User,
        });
        self.refresh(cx);
        let started = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| matches!(snapshot.link, Link::Connecting | Link::Working));
        if !started {
            debug!("send skipped"; reason = "refused");
            return;
        }
        info!("prompt sent"; bytes = bytes, connects = connects);
        self.composer
            .update(cx, |state, cx| state.set_text("", window, cx));
    }

    /// The line "the session may still run somewhere else", above the box.
    /// Only a hint: it does not stop a send.
    pub(super) fn render_elsewhere(
        &self,
        strings: &Strings,
        in_terminal: bool,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> Option<AnyElement> {
        let snapshot = self.snapshot.as_ref()?;
        if in_terminal || !matches!(snapshot.link, Link::None) {
            return None;
        }
        let summary = self.shared.session_summary(&self.session)?;
        if !drive::elsewhere(&summary, &snapshot.link, in_terminal, drive::now()) {
            return None;
        }
        Some(crate::view::elsewhere_line(strings, window, cx))
    }

    /// The box with its button, or what stands in its place. None before the
    /// provider has a snapshot (whether a prompt can go is not known).
    pub(super) fn render_composer(
        &self,
        strings: &Strings,
        in_terminal: bool,
        window: &mut Window,
        cx: &mut Context<MessagesModel>,
    ) -> Option<AnyElement> {
        let snapshot = self.snapshot.clone()?;
        let block = send_block(&snapshot.can_prompt, in_terminal);
        let working = matches!(snapshot.link, Link::Working);
        let mut column = div().w_full().flex().flex_col().gap(px(6.0));

        // The reason, and the button that goes with it.
        let (reason, action): (Option<Text>, Option<Button>) = match &block {
            Some(Block::InTerminal) => {
                let model = cx.entity();
                (
                    Some(Text::key(strings, keys::conversation::IN_TERMINAL)),
                    Some(
                        Button::new(
                            nodes::area("composer").child("notice").child("action"),
                            Text::key(strings, keys::conversation::SHOW_TERMINAL),
                        )
                        .size(ButtonSize::Small)
                        .on_click(move |_, cx| {
                            model.update(cx, |_, cx| cx.emit(MessagesEvent::ShowTerminal));
                        }),
                    ),
                )
            }
            Some(Block::Unavailable(why)) => (
                present::unavailable_text(strings, why, &snapshot.agent),
                None,
            ),
            None => (None, None),
        };

        if !matches!(block, Some(Block::InTerminal)) {
            let model = cx.entity();
            // Busy only keeps Send from working; the text can be written
            // meanwhile. Any other reason closes the box.
            let closed =
                matches!(&block, Some(Block::Unavailable(why)) if *why != Unavailable::Busy);
            let field = TextArea::new(nodes::area("composer").child("field"), &self.composer)
                .placeholder(present::reply_placeholder(strings, &snapshot.agent))
                .lines(2, 6)
                .disabled(closed)
                .on_submit(move |window, cx| {
                    model.update(cx, |model, cx| model.submit(window, cx));
                });
            column = column.child(field);
            let blank = self.draft.trim().is_empty();
            let model = cx.entity();
            let button = if working {
                Button::new(
                    nodes::area("composer").child("stop"),
                    Text::key(strings, keys::conversation::STOP),
                )
                .size(ButtonSize::Small)
                .on_click(move |_, cx| model.update(cx, |model, cx| model.stop(cx)))
            } else {
                Button::new(
                    nodes::area("composer").child("send"),
                    Text::key(strings, keys::conversation::SEND),
                )
                .kind(ButtonKind::Primary)
                .size(ButtonSize::Small)
                .disabled(blank || block.is_some())
                .on_click(move |window, cx| {
                    model.update(cx, |model, cx| model.submit(window, cx));
                })
            };
            let notice = reason.map(|reason| notice_line(reason, window, cx));
            column = column.child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(GAP)
                    .child(div().flex_1().min_w_0().children(notice))
                    .child(div().flex_shrink_0().child(button)),
            );
        } else {
            let notice = reason.map(|reason| notice_line(reason, window, cx));
            column = column.child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(GAP)
                    .children(notice)
                    .children(action),
            );
        }
        Some(
            semantic(Semantic::new(nodes::area("composer"), Role::Group), column)
                .into_any_element(),
        )
    }
}

/// The line under the box: why it cannot send.
fn notice_line(reason: Text, window: &Window, cx: &App) -> AnyElement {
    semantic(
        Semantic::new(nodes::area("composer").child("notice"), Role::Text).label(&reason),
        div()
            .w_full()
            .text_role(TextRole::Caption, window, cx)
            .child(reason.shared()),
    )
    .into_any_element()
}
