//! A load over 1 s (interaction spec「状态与异常」「加载超过 1 s」): a
//! centred indeterminate spinner, one line of secondary text and an
//! optional plain Cancel. With a known fraction the spinner becomes
//! `Progress`'s `Bar`.
//!
//! The spinner is the macOS one: twelve spokes, the lead spoke darkest,
//! stepping one spoke per `SPIN_STEP` (a turn a second). GPUI draws the
//! whole window again whenever anything is notified (Lectern caches no
//! view), so the spinner steps at 12 Hz from a one-shot timer, not every
//! frame: each render arms the next step, a frame that is not drawn (the
//! element is gone, the window is covered) arms nothing, and the spinner
//! stops. It also stops after `SPIN_LIMIT`, since a load can stay pending
//! (a failed overview); and under reduced motion it never steps.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, EntityId, Hsla, IntoElement, ParentElement, PathBuilder, Pixels, RenderOnce,
    Styled, Task, Window, canvas, div, point, px,
};
use lectern_i18n::keys;

use crate::controls::require_ready;
use crate::controls::{Button, ButtonKind, Progress, ProgressShape};
use crate::font::{TextRole, TextRoleExt};
use crate::motion::{SPIN_LIMIT, SPIN_STEP, reduced};
use crate::palette::colors;
use crate::semantic::{self, NodeId, Role, Semantic};
use crate::text::{Text, strings};

/// The spinner's spokes, one step each.
pub const SPOKES: usize = 12;
/// The HIG's small indeterminate spinner.
const SIDE: Pixels = px(16.0);
const GAP: Pixels = px(8.0);

#[derive(IntoElement)]
pub struct LoadingProgress {
    id: NodeId,
    label: Text,
    fraction: Option<f32>,
    cancel: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
}

/// The spinner's clock: when it was first drawn, and the timer of the next
/// step. Kept in the element's keyed state, so it lives as long as the
/// element is drawn in consecutive frames and is dropped (with the timer)
/// after the first frame without it.
struct Spin {
    shown_at: Instant,
    timer: Option<Task<()>>,
}

impl LoadingProgress {
    pub fn new(id: NodeId, label: Text) -> LoadingProgress {
        LoadingProgress {
            id,
            label,
            fraction: None,
            cancel: None,
        }
    }

    /// The loader reported how far it is (0–1): a bar instead of the
    /// spinner.
    pub fn fraction(mut self, fraction: f32) -> LoadingProgress {
        self.fraction = Some(fraction);
        self
    }

    /// What Cancel does (the caller sends `Pop`; only a load that fills a
    /// pushed screen has one, interaction spec「加载超过 1 s」).
    pub fn on_cancel(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> LoadingProgress {
        self.cancel = Some(Rc::new(f));
        self
    }
}

/// The spoke that leads at `elapsed` after the spinner appeared, and how
/// long until the next one leads (None: it no longer steps). Pure.
pub fn spin_step(elapsed: Duration, reduced: bool) -> (usize, Option<Duration>) {
    if reduced || elapsed >= SPIN_LIMIT {
        return (0, None);
    }
    let step = SPIN_STEP.as_nanos();
    let index = elapsed.as_nanos() / step;
    let next = Duration::from_nanos(((index + 1) * step - elapsed.as_nanos()) as u64);
    ((index % SPOKES as u128) as usize, Some(next))
}

impl RenderOnce for LoadingProgress {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let spec = Semantic::new(self.id.clone(), Role::Group)
            .label(&self.label)
            .attr("phase", "progress");

        let indicator = match self.fraction {
            Some(fraction) => div()
                .w(px(160.0))
                .child(Progress::new(
                    self.id.child("bar"),
                    ProgressShape::Bar,
                    fraction,
                ))
                .into_any_element(),
            None => {
                let lead = spinner_lead(&self.id, window, cx);
                let color = palette.secondary_label;
                div()
                    .size(SIDE)
                    .flex_shrink_0()
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, (), window, _| paint_spokes(bounds, lead, color, window),
                        )
                        .size_full(),
                    )
                    .into_any_element()
            }
        };

        let mut column = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(GAP)
            .child(indicator)
            .child(
                div()
                    .text_role(TextRole::Secondary, window, cx)
                    .child(self.label.shared()),
            );
        if let Some(cancel) = self.cancel {
            let text = Text::key(&strings(cx), keys::common::CANCEL);
            column = column.child(
                Button::new(self.id.child("cancel"), text)
                    .kind(ButtonKind::Plain)
                    .on_click(move |window, cx| cancel(window, cx)),
            );
        }
        semantic::declare(spec, column)
    }
}

/// The lead spoke now, arming the timer of the next step when it steps.
fn spinner_lead(id: &NodeId, window: &mut Window, cx: &mut App) -> usize {
    let now = cx.background_executor().now();
    let spin = window.use_keyed_state(id.part("spin"), cx, |_, _| Spin {
        shown_at: now,
        timer: None,
    });
    let elapsed = now.saturating_duration_since(spin.read(cx).shown_at);
    let (lead, next) = spin_step(elapsed, reduced(cx));
    match next {
        None => spin.update(cx, |spin, _| spin.timer = None),
        Some(next) if spin.read(cx).timer.is_none() => {
            let view: EntityId = window.current_view();
            let weak = spin.downgrade();
            let executor = cx.background_executor().clone();
            let task = window.spawn(cx, async move |cx| {
                executor.timer(next).await;
                let _ = cx.update(|_, cx| {
                    // Clear the slot without a notify (the owner observes
                    // the keyed state), then redraw the owner once. A
                    // spinner no longer drawn has dropped its state: no redraw.
                    if let Some(spin) = weak.upgrade() {
                        spin.update(cx, |spin, _| spin.timer = None);
                        cx.notify(view);
                    }
                });
            });
            spin.update(cx, |spin, _| spin.timer = Some(task));
        }
        Some(_) => {}
    }
    lead
}

/// Twelve spokes around the centre, `lead` darkest and the others fading
/// behind it clockwise.
fn paint_spokes(bounds: Bounds<Pixels>, lead: usize, color: Hsla, window: &mut Window) {
    let side = bounds.size.width.min(bounds.size.height);
    let centre = point(
        bounds.origin.x + bounds.size.width / 2.0,
        bounds.origin.y + bounds.size.height / 2.0,
    );
    let (inner, outer) = (side * 0.25, side * 0.5);
    let width = side / 9.0;
    for spoke in 0..SPOKES {
        let behind = (lead + SPOKES - spoke) % SPOKES;
        let alpha = 1.0 - behind as f32 / SPOKES as f32 * 0.8;
        let angle = spoke as f32 / SPOKES as f32 * TAU - FRAC_PI_2;
        let at = |r: Pixels| point(centre.x + r * angle.cos(), centre.y + r * angle.sin());
        let mut builder = PathBuilder::stroke(width);
        builder.move_to(at(inner));
        builder.line_to(at(outer - width / 2.0));
        if let Ok(path) = builder.build() {
            window.paint_path(path, color.opacity(alpha));
        }
    }
}
