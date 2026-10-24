//! The push button: macOS heights 24pt (`Small`) and 30pt (`Regular`), the
//! primary one on the accent colour. Wraps gpui-component's `Button` with
//! its variant, radius and colours replaced by the system's; views never
//! use gpui-component's `Button` directly.

use std::rc::Rc;

use gpui::{
    App, ClickEvent, IntoElement, ParentElement, Pixels, RenderOnce, Styled, Window, div, px,
};
use gpui_component::button::{ButtonCustomVariant, ButtonRounded, ButtonVariants};
use gpui_component::{Disableable, Sizable, Size};

use crate::controls::require_ready;
use crate::font::{TextRole, TextRoleExt};
use crate::icon::Icon;
use crate::palette::{Palette, colors, on_accent, shade};
use crate::semantic::{self, NodeId, Role, Semantic, State};
use crate::text::Text;

type ClickHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A push button. The text is a `Text`, so a literal does not compile:
///
/// ```compile_fail,E0308
/// let id = lectern_ui::semantic::NodeId::new("welcome", "actions", None);
/// let button = lectern_ui::Button::new(id, "Open");
/// ```
#[derive(IntoElement)]
pub struct Button {
    id: NodeId,
    text: Text,
    kind: ButtonKind,
    size: ButtonSize,
    icon: Option<Icon>,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonKind {
    /// The default action: accent fill, white text.
    Primary,
    /// A bordered button on `control_background`.
    #[default]
    Secondary,
    /// Text only, no fill until hovered.
    Plain,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonSize {
    /// 24pt high.
    Small,
    /// 30pt high.
    #[default]
    Regular,
}

impl ButtonSize {
    fn height(self) -> Pixels {
        match self {
            ButtonSize::Small => px(24.0),
            ButtonSize::Regular => px(30.0),
        }
    }

    fn radius(self) -> Pixels {
        match self {
            ButtonSize::Small => px(5.0),
            ButtonSize::Regular => px(6.0),
        }
    }

    fn horizontal_padding(self) -> Pixels {
        match self {
            ButtonSize::Small => px(8.0),
            ButtonSize::Regular => px(12.0),
        }
    }
}

/// How far the lightness moves for a hovered and a pressed fill.
const HOVER_SHADE: f32 = -0.04;
const ACTIVE_SHADE: f32 = -0.10;
/// The alpha of the `label`-coloured wash a plain button shows when hovered
/// and when pressed.
const PLAIN_HOVER_ALPHA: f32 = 0.06;
const PLAIN_ACTIVE_ALPHA: f32 = 0.12;
/// How much of the `secondary_label` a disabled button's text keeps.
const DISABLED_TEXT_OPACITY: f32 = 0.5;

impl Button {
    pub fn new(id: NodeId, text: Text) -> Button {
        Button {
            id,
            text,
            kind: ButtonKind::default(),
            size: ButtonSize::default(),
            icon: None,
            disabled: false,
            on_click: None,
        }
    }

    pub fn kind(mut self, kind: ButtonKind) -> Button {
        self.kind = kind;
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Button {
        self.size = size;
        self
    }

    pub fn icon(mut self, icon: Icon) -> Button {
        self.icon = Some(icon);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Button {
        self.disabled = disabled;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Button {
        self.on_click = Some(Rc::new(f));
        self
    }

    /// The colours of one kind: fill, text, hovered fill, pressed fill and
    /// whether a border is drawn.
    fn style(&self, palette: &Palette, cx: &App) -> ButtonCustomVariant {
        let base = ButtonCustomVariant::new(cx);
        match self.kind {
            ButtonKind::Primary => base
                .color(palette.accent)
                .foreground(on_accent())
                .hover(shade(palette.accent, HOVER_SHADE))
                .active(shade(palette.accent, ACTIVE_SHADE)),
            ButtonKind::Secondary => base
                .color(palette.control_background)
                .foreground(palette.label)
                .hover(palette.unemphasized_selected_content_background)
                .active(shade(
                    palette.unemphasized_selected_content_background,
                    ACTIVE_SHADE,
                )),
            ButtonKind::Plain => base
                .color(palette.label.alpha(0.0))
                .foreground(palette.label)
                .hover(palette.label.alpha(PLAIN_HOVER_ALPHA))
                .active(palette.label.alpha(PLAIN_ACTIVE_ALPHA)),
        }
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let style = self.style(&palette, cx);
        let text_color = if self.disabled {
            palette.secondary_label.opacity(DISABLED_TEXT_OPACITY)
        } else {
            match self.kind {
                ButtonKind::Primary => on_accent(),
                ButtonKind::Secondary | ButtonKind::Plain => palette.label,
            }
        };
        // The normal fill is set on the instance: gpui-component thins a
        // custom variant's fill to 20 % alpha, and the instance style is
        // replayed last in the normal state but also inside the disabled
        // state, so it is set only while enabled.
        let fill = match self.kind {
            ButtonKind::Primary => Some(palette.accent),
            ButtonKind::Secondary => Some(palette.control_background),
            ButtonKind::Plain => None,
        };
        let spec = Semantic::new(self.id.clone(), Role::Button)
            .label(&self.text)
            .state(State {
                disabled: self.disabled,
                ..State::default()
            });
        let gpui_size = match self.size {
            ButtonSize::Small => Size::Small,
            ButtonSize::Regular => Size::Medium,
        };
        let label = div()
            .text_role(TextRole::Body, window, cx)
            .text_color(text_color)
            .whitespace_nowrap()
            .child(self.text.shared());
        let mut button = gpui_component::button::Button::new(self.id.element_id())
            .custom(style)
            .with_size(gpui_size)
            .rounded(ButtonRounded::Size(self.size.radius()))
            .disabled(self.disabled)
            .accessibility_label(self.text.shared())
            .h(self.size.height())
            .px(self.size.horizontal_padding())
            .child(label);
        if let (false, Some(fill)) = (self.disabled, fill) {
            button = button.bg(fill);
        }
        if self.kind == ButtonKind::Secondary {
            button = button.border_1().border_color(palette.separator);
        }
        if let Some(icon) = self.icon {
            button = button.icon(icon);
        }
        if let Some(on_click) = self.on_click {
            button = button.on_click(move |_: &ClickEvent, window, cx| on_click(window, cx));
        }
        semantic::declare(spec, button)
    }
}
