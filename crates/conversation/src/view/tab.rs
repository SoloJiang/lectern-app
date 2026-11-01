//! `ConversationTab`: the inspector's Conversation page (docs/design/conversation.md
//! 「视图」). It follows the review on the screen (`Shared::current`), holds
//! the Messages page of that session, and takes what the listeners send
//! (route changes, new snapshots, services coming and going, terminals
//! listed) from a channel on the main thread. The Terminal page is not here
//! yet: its place says the terminal is not available, which is what the page
//! says without `dyn Terminals`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use gpui::{
    AnyElement, AppContext, Context, ElementId, Entity, FocusHandle, InteractiveElement,
    IntoElement, ParentElement, Render, Styled, Subscription, Task, Window, div, px,
};
use lectern_i18n::keys;
use lectern_logging::debug;
use lectern_services::conversation::OptionKind;
use lectern_services::session::SessionKey;
use lectern_ui::motion::Transition;
use lectern_ui::semantic::{Role, Semantic, semantic_interactive};
use lectern_ui::{Segmented, Text, strings};

use crate::actions::{
    self, AllowAlways, AllowOnce, Dismiss, LoadEarlier, RejectAlways, RejectOnce, Send,
    ShowMessages, ShowTerminal, Stop, ToggleMoreRequests, TogglePlan,
};
use crate::nodes;
use crate::plugin::{Current, Job, Shared};
use crate::view::{MessagesEvent, MessagesModel, text_line};

/// The page's inner padding and the gap between its parts.
const PADDING: gpui::Pixels = px(12.0);
const GAP: gpui::Pixels = px(8.0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Messages,
    Terminal,
}

impl Page {
    fn index(self) -> usize {
        match self {
            Page::Messages => 0,
            Page::Terminal => 1,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Page::Messages => "messages",
            Page::Terminal => "terminal",
        }
    }
}

/// The page: `InspectorTab("conversation")`'s factory builds it; hiding the
/// inspector or switching the page drops it, and with it the interest in the
/// session's conversation.
pub struct ConversationTab {
    shared: Arc<Shared>,
    current: Current,
    page: Page,
    /// The page switch's slider (`SEGMENT`).
    page_slider: Transition,
    /// Built when `current` is a session, replaced when it changes.
    messages: Option<Entity<MessagesModel>>,
    /// What the Messages page asks of this page (shows the Terminal page).
    _messages_events: Option<Subscription>,
    focus: FocusHandle,
    /// Takes the jobs of the channel; dropped with the page.
    _jobs: Task<()>,
}

impl Drop for ConversationTab {
    /// The page the session was last on is remembered (hiding the inspector
    /// drops the page).
    fn drop(&mut self) {
        self.remember_page();
    }
}

impl ConversationTab {
    /// Main thread. Registers the channel before it reads `current`, so that
    /// a change in between is applied again rather than lost. Does no IO.
    /// The window is for the reply box, which is built with the page.
    pub(crate) fn new(
        shared: Arc<Shared>,
        window: &mut Window,
        cx: &mut Context<ConversationTab>,
    ) -> ConversationTab {
        let mut receiver = shared.register_tab();
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = receiver.next().await {
                // Several jobs are one refresh.
                let mut batch = vec![first];
                while let Ok(next) = receiver.try_recv() {
                    batch.push(next);
                }
                if this
                    .update_in(cx, |this, window, cx| this.apply(batch, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut tab = ConversationTab {
            current: Current::None,
            page: Page::Messages,
            page_slider: Transition::new(0.0),
            messages: None,
            _messages_events: None,
            focus: cx.focus_handle(),
            _jobs: task,
            shared,
        };
        let current = tab.shared.current();
        tab.set_current(current, window, cx);
        tab
    }

    #[cfg(test)]
    pub(crate) fn messages(&self) -> Option<&Entity<MessagesModel>> {
        self.messages.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn page(&self) -> Page {
        self.page
    }

    #[cfg(test)]
    pub(crate) fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Records the page the session under review is on.
    fn remember_page(&self) {
        if let Current::Session(key) = &self.current {
            self.shared
                .with_memory(|memory| memory.set_page(key, self.page));
        }
    }

    /// Follows the review on the screen: a different session gets a new
    /// Messages page (the old one is dropped and releases its interest), and
    /// opens on the page it was last on.
    fn set_current(
        &mut self,
        current: Current,
        window: &mut Window,
        cx: &mut Context<ConversationTab>,
    ) {
        if self.current == current {
            return;
        }
        self.remember_page();
        self.current = current;
        self._messages_events = None;
        self.messages = match &self.current {
            Current::Session(key) => {
                let shared = self.shared.clone();
                let key: SessionKey = key.clone();
                let page = shared
                    .with_memory(|memory| memory.page(&key))
                    .unwrap_or(Page::Messages);
                self.page = page;
                self.page_slider = Transition::new(page.index() as f32);
                let model = cx.new(|cx| MessagesModel::new(shared, key, window, cx));
                model.read(cx).log_shown();
                self._messages_events = Some(cx.subscribe(
                    &model,
                    |this, _, event: &MessagesEvent, cx| match event {
                        MessagesEvent::ShowTerminal => this.show(Page::Terminal, cx),
                    },
                ));
                Some(model)
            }
            Current::None | Current::NotSession => None,
        };
        cx.notify();
    }

    /// What the listeners sent since the last time. Route changes first, so
    /// that an event for the session that left finds no page for it; of the
    /// conversation events only the newest generation per session counts.
    fn apply(&mut self, jobs: Vec<Job>, window: &mut Window, cx: &mut Context<ConversationTab>) {
        let mut route = None;
        let mut services = false;
        let mut changes: HashMap<SessionKey, u64> = HashMap::new();
        for job in jobs {
            match job {
                Job::Route(current) => route = Some(current),
                Job::Services => services = true,
                // The page reads the state of the terminal when it draws.
                Job::Terminal => {}
                Job::Conversation {
                    session,
                    generation,
                } => {
                    let newest = changes.entry(session).or_insert(generation);
                    *newest = (*newest).max(generation);
                }
            }
        }
        if let Some(current) = route {
            debug!("route applied"; current = current.name());
            self.set_current(current, window, cx);
        }
        if let Some(model) = self.messages.clone() {
            if services {
                model.update(cx, |model, cx| model.sync_service(cx));
            }
            let generation = changes.get(model.read(cx).session()).copied();
            if let Some(generation) = generation {
                model.update(cx, |model, cx| model.changed(generation, cx));
            }
        }
        cx.notify();
    }

    /// The page switch.
    fn show(&mut self, page: Page, cx: &mut Context<ConversationTab>) {
        if self.page == page {
            return;
        }
        Segmented::select(&mut self.page_slider, page.index(), Instant::now(), cx);
        self.page = page;
        self.remember_page();
        debug!("page switched"; page = page.name());
        cx.notify();
    }

    fn with_messages(
        &mut self,
        cx: &mut Context<ConversationTab>,
        f: impl FnOnce(&mut MessagesModel, &mut Context<MessagesModel>),
    ) {
        if let Some(model) = self.messages.clone() {
            model.update(cx, f);
        }
    }

    /// An action that needs the window (the reply box's text is set from it).
    fn with_messages_in(
        &mut self,
        window: &mut Window,
        cx: &mut Context<ConversationTab>,
        f: impl FnOnce(&mut MessagesModel, &mut Window, &mut Context<MessagesModel>),
    ) {
        if let Some(model) = self.messages.clone() {
            model.update(cx, |model, cx| f(model, window, cx));
        }
    }

    fn body(&mut self, window: &mut Window, cx: &mut Context<ConversationTab>) -> AnyElement {
        let Some(model) = self.messages.clone() else {
            return text_line(
                nodes::area("empty"),
                keys::conversation::EMPTY_NO_SESSION,
                window,
                cx,
            );
        };
        match self.page {
            Page::Messages => model.update(cx, |model, cx| model.render_page(window, cx)),
            Page::Terminal => text_line(
                nodes::area("notice"),
                keys::conversation::TERMINAL_UNAVAILABLE,
                window,
                cx,
            ),
        }
    }
}

impl Render for ConversationTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strings = strings(cx);
        let now = Instant::now();
        let slider = self.page_slider.tick(now, window);
        let tab = cx.entity();
        let switch = Segmented::new(
            nodes::pages(),
            vec![
                (
                    nodes::page_item("messages"),
                    Text::key(&strings, keys::conversation::PAGE_MESSAGES),
                ),
                (
                    nodes::page_item("terminal"),
                    Text::key(&strings, keys::conversation::PAGE_TERMINAL),
                ),
            ],
            self.page.index(),
        )
        .fill()
        .slider(slider)
        .on_select(move |index, _, cx| {
            let page = if index == 0 {
                Page::Messages
            } else {
                Page::Terminal
            };
            tab.update(cx, |tab, cx| tab.show(page, cx));
        });
        let body = self.body(window, cx);
        let root = div()
            .id(ElementId::from(&nodes::page()))
            .key_context(actions::CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .gap(GAP)
            .p(PADDING)
            .on_action(cx.listener(|this, _: &Send, window, cx| {
                this.with_messages_in(window, cx, |m, window, cx| m.submit(window, cx));
            }))
            .on_action(
                cx.listener(|this, _: &Stop, _, cx| this.with_messages(cx, |m, cx| m.stop(cx))),
            )
            .on_action(cx.listener(|this, _: &AllowOnce, _, cx| {
                this.with_messages(cx, |m, cx| m.answer_oldest(OptionKind::AllowOnce, cx));
            }))
            .on_action(cx.listener(|this, _: &AllowAlways, _, cx| {
                this.with_messages(cx, |m, cx| m.answer_oldest(OptionKind::AllowAlways, cx));
            }))
            .on_action(cx.listener(|this, _: &RejectOnce, _, cx| {
                this.with_messages(cx, |m, cx| m.answer_oldest(OptionKind::RejectOnce, cx));
            }))
            .on_action(cx.listener(|this, _: &RejectAlways, _, cx| {
                this.with_messages(cx, |m, cx| m.answer_oldest(OptionKind::RejectAlways, cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleMoreRequests, _, cx| {
                this.with_messages(cx, |m, cx| m.toggle_more(cx));
            }))
            .on_action(cx.listener(|this, _: &ShowMessages, _, cx| this.show(Page::Messages, cx)))
            .on_action(cx.listener(|this, _: &ShowTerminal, _, cx| this.show(Page::Terminal, cx)))
            .on_action(cx.listener(|this, _: &LoadEarlier, _, cx| {
                this.with_messages(cx, |m, cx| m.load_earlier(cx));
            }))
            .on_action(cx.listener(|this, _: &TogglePlan, _, cx| {
                this.with_messages(cx, |m, cx| m.toggle_plan(cx));
            }))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| {
                this.with_messages(cx, |m, cx| m.dismiss(cx));
            }))
            .child(switch)
            .child(div().flex_1().min_h_0().child(body));
        semantic_interactive(
            Semantic::new(nodes::page(), Role::Group).attr("page", self.page.name()),
            root,
        )
    }
}
