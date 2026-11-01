//! `TerminalPage`: the Terminal page of one session (docs/design/conversation.md
//! 「视图」「数据流与线程」「状态与异常」). It resumes the session in a terminal
//! (`claude --resume`, `codex resume`), embeds the terminal's grid
//! (`TerminalPane`, which is created only here) and shows the terminal's state.
//! It reads `dyn Terminals` (every call copies a value or queues an input) and
//! sends commands; it does no IO. It holds the interest in the terminal's
//! frames only while the page is shown.

use std::sync::Arc;

use gpui::{
    AnyElement, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Pixels, Size, Styled, Window, canvas, div, px,
};
use lectern_i18n::{Args, Strings, keys};
use lectern_logging::{debug, info};
use lectern_services::conversation::{ConversationCommand, Link};
use lectern_services::session::SessionKey;
use lectern_services::terminal::{
    FrameInterest, Launch, LaunchError, OpenSpec, RunState, TerminalId,
};
use lectern_terminal_view::{TerminalPane, grid_size, theme};
use lectern_ui::semantic::{Role, Semantic, semantic};
use lectern_ui::{Button, ButtonKind, ButtonSize, Text, TextRole, TextRoleExt, strings};

use crate::actions::TERMINAL_CONTEXT;
use crate::diag;
use crate::drive::{self, Resume};
use crate::nodes;
use crate::plugin::Shared;
use crate::present;
use crate::view::{elsewhere_line, text_line};

/// The gap between the parts of the page.
const GAP: Pixels = px(8.0);

pub(crate) struct TerminalPage {
    shared: Arc<Shared>,
    session: SessionKey,
    /// The session's terminal, in any state (an exited one keeps its last
    /// picture).
    terminal: Option<TerminalId>,
    /// `Terminals::info(terminal)`'s `state` as of the last `sync`.
    state: Option<RunState>,
    pane: Option<Entity<TerminalPane>>,
    /// `Terminals::watch(terminal)`'s interest: held only while the page is
    /// shown.
    frames: Option<FrameInterest>,
    shown: bool,
    /// The size of the region of the page that holds the grid, as of the last
    /// frame (the canvas in the body records it); the terminal is opened with
    /// the grid that fits it.
    area: Option<Size<Pixels>>,
    /// Why the last `open` was refused.
    refused: Option<LaunchError>,
}

impl TerminalPage {
    /// Main thread. Takes the session's terminal if it has one (the page may
    /// be built after a terminal was resumed earlier). Does no IO.
    pub(crate) fn new(
        shared: Arc<Shared>,
        session: SessionKey,
        shown: bool,
        window: &mut Window,
        cx: &mut Context<TerminalPage>,
    ) -> TerminalPage {
        let mut page = TerminalPage {
            shared,
            session,
            terminal: None,
            state: None,
            pane: None,
            frames: None,
            shown,
            area: None,
            refused: None,
        };
        page.sync(window, cx);
        page
    }

    #[cfg(test)]
    pub(crate) fn pane(&self) -> Option<&Entity<TerminalPane>> {
        self.pane.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn area(&self) -> Option<Size<Pixels>> {
        self.area
    }

    fn scope(&self) -> lectern_logging::Scope {
        lectern_logging::scope(diag::session_ctx(&self.session.id))
    }

    /// Whether the terminal starts or runs: it drives the session.
    fn drives(&self) -> bool {
        drive::drives(self.state.as_ref())
    }

    /// The ACP link of the session, None without `dyn Conversations`.
    fn link(&self) -> Option<Link> {
        self.shared
            .with_conversations(|conversations| conversations.snapshot(&self.session))
            .flatten()
            .map(|snapshot| snapshot.link.clone())
    }

    // ------------------------------------------------------- the terminal

    /// Reads the session's terminal again: its ID and state. A terminal that
    /// is new (opened here, replaced after it ended, or found when the page
    /// was built) gets a pane; one that is gone (closed, the service
    /// stopped) takes its pane and interest with it; one that stays only has
    /// its pane refreshed. Called with every `Listed` and when the service
    /// comes or goes.
    pub(crate) fn sync(&mut self, window: &mut Window, cx: &mut Context<TerminalPage>) {
        let found = self
            .shared
            .with_terminals(|terminals| {
                let id = terminals.for_session(&self.session)?;
                Some((id, terminals.info(id)?.state))
            })
            .flatten();
        match found {
            Some((id, state)) => {
                self.state = Some(state);
                if self.terminal != Some(id) || self.pane.is_none() {
                    self.terminal = Some(id);
                    self.frames = None;
                    self.pane = self.build_pane(id, window, cx);
                } else if let Some(pane) = &self.pane {
                    pane.update(cx, |pane, cx| pane.refresh(cx));
                }
            }
            None => {
                self.terminal = None;
                self.state = None;
                self.pane = None;
                self.frames = None;
            }
        }
        self.rewatch();
        cx.notify();
    }

    /// The pane for a terminal, with the token of this activation of the
    /// plugin that brought the service. None when either is missing.
    fn build_pane(
        &self,
        id: TerminalId,
        window: &mut Window,
        cx: &mut Context<TerminalPage>,
    ) -> Option<Entity<TerminalPane>> {
        let terminals = self.shared.terminals()?;
        let token = self.shared.terminal_token()?;
        Some(cx.new(|cx| TerminalPane::new(terminals, token, id, window, cx)))
    }

    /// The interest in the terminal's frames follows the page: held while the
    /// page is shown and has a terminal, dropped otherwise.
    fn rewatch(&mut self) {
        let wanted = self.shown.then_some(self.terminal).flatten();
        match wanted {
            None => self.frames = None,
            Some(id) if self.frames.is_none() => {
                self.frames = self
                    .shared
                    .with_terminals(|terminals| terminals.watch(id))
                    .flatten();
            }
            Some(_) => {}
        }
    }

    /// The page is shown or hidden (the page switch, the inspector). Showing
    /// it again catches the pane up with what the terminal did meanwhile.
    pub(crate) fn set_shown(&mut self, shown: bool, cx: &mut Context<TerminalPage>) {
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        self.rewatch();
        if shown && let Some(pane) = &self.pane {
            pane.update(cx, |pane, cx| pane.refresh(cx));
        }
        cx.notify();
    }

    /// What the terminal's events asked since the last time: any `Listed`
    /// reads the terminal again (and refreshes its pane once); a `Frame` of
    /// the terminal on this page refreshes its pane once; a `Frame` of
    /// another terminal does nothing.
    pub(crate) fn on_events(
        &mut self,
        listed: bool,
        frames: &[TerminalId],
        window: &mut Window,
        cx: &mut Context<TerminalPage>,
    ) {
        if listed {
            self.sync(window, cx);
        } else if let (Some(id), Some(pane)) = (self.terminal, &self.pane)
            && frames.contains(&id)
        {
            pane.update(cx, |pane, cx| pane.refresh(cx));
        }
    }

    // ------------------------------------------------------------ commands

    /// The resume button: opens the session in a terminal. While a connection
    /// starts or a turn runs nothing happens; an idle or failed link is
    /// disconnected first (the session is driven from one place only). A
    /// refusal is shown under the button; `AlreadyRunning` adopts the
    /// terminal that runs. The new terminal gets the keyboard focus.
    pub(crate) fn resume(&mut self, window: &mut Window, cx: &mut Context<TerminalPage>) {
        let _scope = self.scope();
        if self.drives() {
            return;
        }
        let mut disconnected = false;
        match drive::resume_mode(self.link().as_ref()) {
            Resume::Blocked => {
                debug!("terminal resume skipped"; reason = "linked");
                return;
            }
            Resume::DisconnectFirst => {
                self.shared.with_conversations(|conversations| {
                    conversations.command(&self.session, ConversationCommand::Disconnect);
                });
                // `Disconnect` swaps the snapshot before it returns; a link
                // that is still there (the provider refused) keeps the
                // terminal from opening.
                if !matches!(drive::resume_mode(self.link().as_ref()), Resume::Open) {
                    debug!("terminal resume skipped"; reason = "disconnect_refused");
                    return;
                }
                disconnected = true;
            }
            Resume::Open => {}
        }
        self.refused = None;
        let spec = OpenSpec {
            launch: Launch::Resume(self.session.clone()),
            size: self.area.map(|area| grid_size(area, window, cx)),
            theme: theme(window, cx),
        };
        let Some(opened) = self.shared.with_terminals(|terminals| terminals.open(spec)) else {
            debug!("terminal resume skipped"; reason = "unavailable");
            return;
        };
        let outcome = match &opened {
            Ok(_) => "started",
            Err(error) => diag::launch_error_name(error),
        };
        info!(
            "terminal resume requested";
            outcome = outcome,
            disconnected = disconnected
        );
        match opened {
            Ok(_) | Err(LaunchError::AlreadyRunning(_)) => {
                self.sync(window, cx);
                if let Some(pane) = &self.pane {
                    let focus = pane.read(cx).focus_handle();
                    window.focus(&focus, cx);
                }
            }
            Err(error) => {
                self.refused = Some(error);
                cx.notify();
            }
        }
    }

    /// The Close button: closes the terminal (a running program is ended by
    /// the terminal service) and shows the resume button again.
    pub(crate) fn close(&mut self, window: &mut Window, cx: &mut Context<TerminalPage>) {
        let Some(id) = self.terminal else {
            return;
        };
        let _scope = self.scope();
        info!("terminal closed"; state = diag::run_state_name(self.state.as_ref()));
        self.shared.with_terminals(|terminals| terminals.close(id));
        self.sync(window, cx);
    }

    // -------------------------------------------------------------- drawing

    /// The page: the terminal's state and Close, or the explanation and the
    /// resume button, and the grid under them.
    pub(crate) fn render_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<TerminalPage>,
    ) -> AnyElement {
        if self.shared.terminals().is_none() || self.shared.terminal_token().is_none() {
            return text_line(
                nodes::area("notice"),
                keys::conversation::TERMINAL_UNAVAILABLE,
                window,
                cx,
            );
        }
        let strings = strings(cx);
        let drives = self.drives();
        let mut column = div()
            .size_full()
            .flex()
            .flex_col()
            .gap(GAP)
            .key_context(TERMINAL_CONTEXT);
        if let Some(state) = self.state {
            column = column.child(self.render_status(&strings, &state, window, cx));
        }
        if !drives {
            column = column.child(self.render_resume(&strings, window, cx));
        }
        let page = cx.entity();
        let measure = canvas(
            move |bounds, _, cx| {
                page.update(cx, |page, _| page.area = Some(bounds.size));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let mut body = div().relative().flex_1().min_h_0().w_full().child(measure);
        if let Some(pane) = &self.pane {
            body = body.child(div().size_full().child(pane.clone()));
        }
        semantic(
            Semantic::new(nodes::area("terminal"), Role::Group)
                .attr("state", diag::run_state_name(self.state.as_ref())),
            column.child(body),
        )
        .into_any_element()
    }

    /// The line about the terminal's state, and Close.
    fn render_status(
        &self,
        strings: &Strings,
        state: &RunState,
        window: &Window,
        cx: &mut Context<TerminalPage>,
    ) -> AnyElement {
        let text = present::terminal_status(strings, state);
        let page = cx.entity();
        let close = Button::new(
            nodes::area("terminal").child("close"),
            Text::key(strings, keys::conversation::TERMINAL_CLOSE),
        )
        .size(ButtonSize::Small)
        .on_click(move |window, cx| {
            page.update(cx, |page, cx| page.close(window, cx));
        });
        div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(GAP)
            .child(
                div().flex_1().min_w_0().child(semantic(
                    Semantic::new(nodes::area("terminal").child("status"), Role::Text).label(&text),
                    div()
                        .w_full()
                        .text_role(TextRole::Secondary, window, cx)
                        .child(text.shared()),
                )),
            )
            .child(div().flex_shrink_0().child(close))
            .into_any_element()
    }

    /// What stands above the grid while no terminal drives the session: the
    /// explanation (when there is no terminal at all), the hint that the
    /// session may run elsewhere, the resume button and the line about why it
    /// cannot.
    fn render_resume(
        &self,
        strings: &Strings,
        window: &Window,
        cx: &mut Context<TerminalPage>,
    ) -> AnyElement {
        let link = self.link();
        let mode = drive::resume_mode(link.as_ref());
        let summary = self.shared.session_summary(&self.session);
        let mut column = div().w_full().flex().flex_col().items_start().gap(GAP);
        if self.terminal.is_none()
            && let Some(command) = summary
                .as_ref()
                .and_then(|summary| present::resume_command(&summary.agent))
        {
            let line = Text::fmt(
                strings,
                keys::conversation::TERMINAL_EXPLAIN,
                &Args::new().str("command", command),
            );
            column = column.child(semantic(
                Semantic::new(nodes::area("terminal").child("explain"), Role::Text).label(&line),
                div()
                    .w_full()
                    .text_role(TextRole::Secondary, window, cx)
                    .child(line.shared()),
            ));
        }
        if let Some(summary) = &summary
            && drive::elsewhere(
                summary,
                link.as_ref().unwrap_or(&Link::None),
                false,
                drive::now(),
            )
        {
            column = column.child(elsewhere_line(strings, window, cx));
        }
        let key = if mode == Resume::DisconnectFirst {
            keys::conversation::TERMINAL_DISCONNECT_RESUME
        } else {
            keys::conversation::TERMINAL_RESUME
        };
        // A refusal other than "too many terminals" will come again.
        let refused_for_good = self
            .refused
            .as_ref()
            .is_some_and(|error| !matches!(error, LaunchError::TooMany));
        let page = cx.entity();
        column = column.child(
            Button::new(
                nodes::area("terminal").child("resume"),
                Text::key(strings, key),
            )
            .kind(ButtonKind::Primary)
            .size(ButtonSize::Small)
            .disabled(mode == Resume::Blocked || refused_for_good)
            .on_click(move |window, cx| {
                page.update(cx, |page, cx| page.resume(window, cx));
            }),
        );
        let warning = self
            .refused
            .as_ref()
            .and_then(|error| present::terminal_refusal(strings, error))
            .or_else(|| {
                (mode == Resume::Blocked)
                    .then(|| Text::key(strings, keys::conversation::TERMINAL_LINKED))
            });
        if let Some(warning) = warning {
            column = column.child(semantic(
                Semantic::new(nodes::area("terminal").child("warning"), Role::Text).label(&warning),
                div()
                    .w_full()
                    .text_role(TextRole::Caption, window, cx)
                    .child(warning.shared()),
            ));
        }
        column.into_any_element()
    }
}
