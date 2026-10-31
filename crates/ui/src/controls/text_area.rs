//! The multi-line text box (docs/design/ui.md「多行文本框」; conversation's
//! reply box and the review notes' editor): a system-styled frame around
//! gpui-component's `Textarea`. The editing itself (IME, undo and redo, ↵
//! for a new line, selection, the context menu) is the `Textarea`'s.
//!
//! ⌘↩ and Esc are taken in the capture phase on the frame, before the
//! `Textarea`'s own handlers: its key context is deeper than a screen's, so
//! a screen's binding never sees these keys, and its `Enter` action inserts
//! a newline even for ⌘↩.

use std::rc::Rc;

use gpui::{
    App, AppContext as _, BoxShadow, Context, Entity, EntityInputHandler as _, FocusHandle,
    Focusable as _, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled, Subscription, Window, div, point, prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Enter, Escape, InputEvent, Textarea, TextareaState};
use gpui_component::{Sizable, Size};

use crate::controls::require_ready;
use crate::font::{TextRole, TextRoleExt};
use crate::palette::colors;
use crate::semantic::{self, NodeId, Role, Semantic, State};
use crate::text::Text;

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The frame's corner radius and the focus ring's width.
const RADIUS: gpui::Pixels = px(6.0);
const BORDER: gpui::Pixels = px(0.5);
const RING: gpui::Pixels = px(3.0);
/// The focus ring is the accent colour at this opacity.
const RING_OPACITY: f32 = 0.35;
/// A disabled box is drawn at this opacity.
const DISABLED_OPACITY: f32 = 0.5;
/// The height range when `lines` is not called.
const DEFAULT_LINES: (u8, u8) = (2, 6);

/// The state of a `TextArea`: the editing engine, the text and the focus.
/// The view keeps the `Entity` and observes it (`cx.observe`) to follow the
/// text; it notifies when the user changes the text and when `set_text` does.
pub struct TextAreaState {
    input: Entity<TextareaState>,
    /// The text as of the last change; `text()` has no `cx` to read the
    /// engine with.
    text: SharedString,
    focus: FocusHandle,
    /// What the element last pushed into the engine, so that an unchanged
    /// value does not make the engine notify (a redraw every frame).
    placeholder: SharedString,
    lines: (usize, usize),
    _change: Subscription,
}

impl TextAreaState {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextareaState::new(window, cx));
        let focus = input.read(cx).focus_handle(cx);
        let change = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.text = input.read(cx).value();
                cx.notify();
            }
        });
        let (min, max) = DEFAULT_LINES;
        input.update(cx, |input, cx| {
            input.set_auto_grow(usize::from(min), usize::from(max), cx);
        });
        TextAreaState {
            input,
            text: SharedString::default(),
            focus,
            placeholder: SharedString::default(),
            lines: (usize::from(min), usize::from(max)),
            _change: change,
        }
    }

    pub fn text(&self) -> SharedString {
        self.text.clone()
    }

    /// Replaces the text (the undo history is cleared, the cursor goes to
    /// the start). The engine does not announce this change itself, so this
    /// notifies.
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.text = self.input.read(cx).value();
        cx.notify();
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Pushes the element's placeholder and line range into the engine when
    /// they differ from what it holds. No notification of this state: it
    /// runs in a render of the view that observes it.
    fn configure(
        &mut self,
        placeholder: SharedString,
        lines: (usize, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.placeholder != placeholder {
            self.placeholder = placeholder.clone();
            self.input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
        if self.lines != lines {
            self.lines = lines;
            self.input
                .update(cx, |input, cx| input.set_auto_grow(lines.0, lines.1, cx));
        }
    }
}

#[derive(IntoElement)]
pub struct TextArea {
    id: NodeId,
    state: Entity<TextAreaState>,
    placeholder: Option<Text>,
    lines: (u8, u8),
    disabled: bool,
    on_submit: Option<Handler>,
    on_cancel: Option<Handler>,
}

impl TextArea {
    pub fn new(id: NodeId, state: &Entity<TextAreaState>) -> TextArea {
        TextArea {
            id,
            state: state.clone(),
            placeholder: None,
            lines: DEFAULT_LINES,
            disabled: false,
            on_submit: None,
            on_cancel: None,
        }
    }

    pub fn placeholder(mut self, text: Text) -> TextArea {
        self.placeholder = Some(text);
        self
    }

    /// The height follows the content from `min` to `max` lines, then the
    /// box scrolls. `min` is at least 1 and `max` at least `min`; without
    /// this call the range is 2 to 6.
    pub fn lines(mut self, min: u8, max: u8) -> TextArea {
        let min = min.max(1);
        self.lines = (min, max.max(min));
        self
    }

    pub fn disabled(mut self, disabled: bool) -> TextArea {
        self.disabled = disabled;
        self
    }

    /// ⌘↩, handled by the box itself: no newline is inserted.
    pub fn on_submit(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> TextArea {
        self.on_submit = Some(Rc::new(f));
        self
    }

    /// Esc, handled by the box itself. While an input method is composing,
    /// Esc is the input method's and this is not called. Without it Esc
    /// bubbles as usual.
    pub fn on_cancel(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> TextArea {
        self.on_cancel = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for TextArea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let placeholder = self
            .placeholder
            .as_ref()
            .map(Text::shared)
            .unwrap_or_default();
        let lines = (usize::from(self.lines.0), usize::from(self.lines.1));
        let (input, focus) = self.state.update(cx, |state, cx| {
            state.configure(placeholder, lines, window, cx);
            (state.input.clone(), state.focus.clone())
        });
        let focused = !self.disabled && focus.is_focused(window);
        let spec = Semantic::new(self.id.clone(), Role::TextField).state(State {
            focused,
            disabled: self.disabled,
            ..State::default()
        });

        let text = Textarea::new(&input)
            .appearance(false)
            .bordered(false)
            .disabled(self.disabled)
            .with_size(Size::Medium)
            .text_role(TextRole::Body, window, cx);
        let mut frame = div()
            .id(self.id.element_id())
            .w_full()
            .bg(palette.control_background)
            .rounded(RADIUS)
            .border(BORDER)
            .border_color(if focused {
                palette.accent
            } else {
                palette.separator
            })
            .when_focused(focused, palette.accent.opacity(RING_OPACITY))
            .when(self.disabled, |frame| frame.opacity(DISABLED_OPACITY))
            .child(text);
        // A disabled box takes no keys of its own (it can still hold focus).
        let (on_submit, on_cancel) = if self.disabled {
            (None, None)
        } else {
            (self.on_submit, self.on_cancel)
        };
        if let Some(on_submit) = on_submit {
            frame = frame.capture_action(move |action: &Enter, window, cx| {
                if action.secondary && !action.shift {
                    on_submit(window, cx);
                    cx.stop_propagation();
                }
            });
        }
        if let Some(on_cancel) = on_cancel {
            let composing = input.clone();
            frame = frame
                .capture_action(move |_: &Escape, window, cx| {
                    let composing =
                        composing.update(cx, |input, cx| input.marked_text_range(window, cx));
                    if composing.is_none() {
                        on_cancel(window, cx);
                        cx.stop_propagation();
                    }
                })
                // While composing, the engine ends the composition and lets
                // Esc bubble on; the box has taken it.
                .on_action(|_: &Escape, _, _| {});
        }
        semantic::declare(spec, frame)
    }
}

/// The focus ring, a spread shadow in the given colour.
trait FocusRing: Styled + Sized {
    fn when_focused(self, focused: bool, ring: gpui::Hsla) -> Self {
        if !focused {
            return self;
        }
        self.shadow(vec![BoxShadow {
            color: ring,
            offset: point(px(0.0), px(0.0)),
            blur_radius: px(0.0),
            spread_radius: RING,
            inset: false,
        }])
    }
}

impl<E: Styled> FocusRing for E {}

#[cfg(test)]
pub(crate) fn engine(state: &TextAreaState) -> Entity<TextareaState> {
    state.input.clone()
}
