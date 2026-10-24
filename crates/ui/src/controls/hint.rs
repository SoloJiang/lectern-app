//! The one-shot hint (`workspace`'s `workspace-command-failed` and the
//! like): one line of secondary text centred at the top of the content
//! area, on `control_background` with a radius and a shadow. It arrives
//! with `ARRIVE` from 8pt above, stays `duration`, fades out with
//! `HOVER_OUT` and is then dropped by the caller. Not interactive, no
//! scrim, no queue: a new hint replaces the text and restarts the clock.

use std::time::{Duration, Instant};

use gpui::{App, IntoElement, ParentElement, Pixels, RenderOnce, Styled, Window, div, px};

use crate::controls::require_ready;
use crate::font::{TextRole, TextRoleExt};
use crate::motion::{ARRIVE, ARRIVE_OFFSET, HINT_DURATION, HOVER_OUT, Motion, reduced};
use crate::palette::colors;
use crate::semantic::{self, NodeId, Role, Semantic};
use crate::text::Text;

#[derive(IntoElement, Clone, Debug)]
pub struct Hint {
    id: NodeId,
    text: Text,
    shown_at: Instant,
    duration: Duration,
}

/// Where the hint rests below the top of its container.
const TOP: Pixels = px(8.0);
const RADIUS: Pixels = px(8.0);
const PADDING_X: Pixels = px(12.0);
const PADDING_Y: Pixels = px(6.0);
const BORDER_WIDTH: Pixels = px(0.5);

/// One frame of the hint: its offset from `TOP` and its opacity.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HintFrame {
    pub offset: Pixels,
    pub opacity: f32,
}

impl Hint {
    /// `duration` is `motion::HINT_DURATION` (2.5 s) unless changed.
    pub fn new(id: NodeId, text: Text, shown_at: Instant) -> Hint {
        Hint {
            id,
            text,
            shown_at,
            duration: HINT_DURATION,
        }
    }

    pub fn duration(mut self, duration: Duration) -> Hint {
        self.duration = duration;
        self
    }

    pub fn text(&self) -> &Text {
        &self.text
    }

    /// Asked by the caller each frame: true once the fade-out after
    /// `duration` has finished, so the caller drops the hint.
    pub fn is_expired(&self, now: Instant) -> bool {
        self.frame_when(now, false).is_none()
    }

    /// The hint's offset and opacity at `now`, None once expired. Pure;
    /// `reduced` is the reduce-motion setting (the arrival becomes a fade
    /// with no offset, both fades take half the time).
    pub fn frame_when(&self, now: Instant, reduced: bool) -> Option<HintFrame> {
        let elapsed = now.saturating_duration_since(self.shown_at);
        let arrive = ARRIVE.effective_when(reduced);
        let (arrive_motion, moves) = match arrive {
            crate::motion::Effective::Move(motion) => (motion, true),
            crate::motion::Effective::Fade(motion) => (motion, false),
        };
        let out = HOVER_OUT.effective_when(reduced).motion();
        if elapsed < arrive_motion.duration {
            let progress = progress(elapsed, arrive_motion);
            let offset = if moves {
                -ARRIVE_OFFSET * (1.0 - progress)
            } else {
                px(0.0)
            };
            return Some(HintFrame {
                offset,
                opacity: progress,
            });
        }
        if elapsed < self.duration {
            return Some(HintFrame {
                offset: px(0.0),
                opacity: 1.0,
            });
        }
        let fading = elapsed - self.duration;
        if fading < out.duration {
            return Some(HintFrame {
                offset: px(0.0),
                opacity: 1.0 - progress(fading, out),
            });
        }
        None
    }
}

/// The eased progress of `motion` after `elapsed` of it.
fn progress(elapsed: Duration, motion: Motion) -> f32 {
    let t = elapsed.as_secs_f32() / motion.duration.as_secs_f32();
    motion.easing.eval(t)
}

impl RenderOnce for Hint {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let spec = Semantic::new(self.id.clone(), Role::Text).label(&self.text);
        let Some(frame) = self.frame_when(Instant::now(), reduced(cx)) else {
            return semantic::declare(spec, div());
        };
        // The next frame draws the next step of the arrival or the fade;
        // the caller renders each frame while a hint is up (it asks
        // `is_expired` then), so this costs no extra frame of its own.
        window.request_animation_frame();
        let pill = div()
            .px(PADDING_X)
            .py(PADDING_Y)
            .rounded(RADIUS)
            .border(BORDER_WIDTH)
            .border_color(palette.separator)
            .bg(palette.control_background)
            .shadow_md()
            .text_role(TextRole::Secondary, window, cx)
            .whitespace_nowrap()
            .child(self.text.shared());
        let hint = div()
            .absolute()
            .top(TOP + frame.offset)
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .opacity(frame.opacity)
            .child(pill);
        semantic::declare(spec, hint)
    }
}
