//! The hover fill of the clickable rows (docs/interaction.md「动效」「悬停」:
//! the background fades in over `HOVER_IN` and out over `HOVER_OUT`).
//!
//! `lectern-ui` keeps the transition its own list controls share to itself
//! (`controls::hover`), so this is the same thing from the public pieces: a
//! `Transition` in GPUI's keyed element state, which survives the re-renders
//! of a row that is built again every frame, and `palette.fill` scaled by the
//! strength, the colour of the sidebar's rows. The state is made once for a
//! row and goes with it when the row stops being drawn; a row that is not
//! hovered costs a lookup. It is for the few rows of the narrative that are
//! not diff lines (turn titles, brief entries, file and step titles, folds,
//! sub-agent lines); the diff lines, which come in thousands, fade through
//! one pair of transitions in the model (`RowFade`).

use std::sync::Arc;
use std::time::Instant;

use gpui::{App, ElementId, Entity, Hsla, Window};
use lectern_ui::Palette;
use lectern_ui::motion::{HOVER_IN, HOVER_OUT, Transition};
use lectern_ui::semantic::NodeId;

use crate::items::ItemKey;

pub(crate) struct Hover {
    state: Entity<Transition>,
    /// The strength at this frame, `0.0..=1.0`.
    strength: f32,
}

impl Hover {
    /// Ticks the row's hover transition; call it where the row is built.
    pub(crate) fn track(id: &NodeId, window: &mut Window, cx: &mut App) -> Hover {
        let key = ElementId::NamedChild(Arc::new(ElementId::from(id)), "hover".into());
        let state = window.use_keyed_state(key, cx, |_, _| Transition::new(0.0));
        let now = Instant::now();
        let strength = state.update(cx, |hover, _| hover.tick(now, window));
        Hover { state, strength }
    }

    /// The row's background, None when the pointer is not (any more) on it.
    pub(crate) fn fill(&self, palette: &Palette) -> Option<Hsla> {
        (self.strength > 0.0).then(|| fill(palette, self.strength))
    }

    /// The listener for `on_hover`: starts the transition towards the new
    /// state and asks for a frame.
    pub(crate) fn listener(&self) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        let state = self.state.clone();
        move |hovered, window, cx| {
            let (target, motion) = if *hovered {
                (1.0, HOVER_IN)
            } else {
                (0.0, HOVER_OUT)
            };
            state.update(cx, |hover, cx| hover.go(target, motion, Instant::now(), cx));
            window.refresh();
        }
    }
}

/// `palette.fill` at `strength` of its own alpha.
pub(crate) fn fill(palette: &Palette, strength: f32) -> Hsla {
    palette
        .fill
        .alpha(palette.fill.a * strength.clamp(0.0, 1.0))
}

/// The hover fade of the diff lines: the line under the pointer fades in and
/// the one it left fades out, two transitions in all however many lines there
/// are (a keyed state per line would be made for every line that scrolls into
/// view). A line is looked up by its key when it is drawn.
#[derive(Default)]
pub(crate) struct RowFade {
    on: Option<(ItemKey, Transition)>,
    off: Option<(ItemKey, Transition)>,
}

impl RowFade {
    /// The pointer entered the line `key`.
    pub(crate) fn enter(&mut self, key: &ItemKey, cx: &App) {
        if self.on.as_ref().is_some_and(|(at, _)| at == key) {
            return;
        }
        // Back on the line it was leaving: from where it is.
        let back = match self.off.take() {
            Some((at, fade)) if at == *key => Some(fade),
            other => {
                self.off = other;
                None
            }
        };
        self.fade_out(cx);
        let mut fade = back.unwrap_or_else(|| Transition::new(0.0));
        fade.go(1.0, HOVER_IN, Instant::now(), cx);
        self.on = Some((key.clone(), fade));
    }

    /// The pointer left the line `key`; a line that is not the one fading in
    /// (the pointer crossed to the next line first) is left alone.
    pub(crate) fn leave(&mut self, key: &ItemKey, cx: &App) {
        if self.on.as_ref().is_some_and(|(at, _)| at == key) {
            self.fade_out(cx);
        }
    }

    fn fade_out(&mut self, cx: &App) {
        if let Some((key, mut fade)) = self.on.take() {
            fade.go(0.0, HOVER_OUT, Instant::now(), cx);
            self.off = Some((key, fade));
        }
    }

    /// Moves both transitions to this frame; called once per frame by the
    /// screen's `render`, whether or not their lines are drawn (a line that
    /// scrolled away while it faded would otherwise keep its transition
    /// running, and counted in `active_transitions`, for ever). It asks for
    /// the next frame while one runs, and lets go of a fade-out that is over.
    pub(crate) fn tick(&mut self, window: &mut Window) {
        let now = Instant::now();
        for (_, fade) in self.on.iter_mut().chain(self.off.iter_mut()) {
            fade.tick(now, window);
        }
        if self
            .off
            .as_ref()
            .is_some_and(|(_, fade)| fade.value() == 0.0 && !fade.is_running(now))
        {
            self.off = None;
        }
    }

    /// The strength of the fill of the line `key` at the last `tick`.
    pub(crate) fn strength(&self, key: &ItemKey) -> f32 {
        [&self.on, &self.off]
            .into_iter()
            .flatten()
            .find(|(at, _)| at == key)
            .map_or(0.0, |(_, fade)| fade.value())
    }

    /// Whether a transition is running (the test of the fade).
    #[cfg(test)]
    pub(crate) fn running(&self) -> bool {
        let now = Instant::now();
        [&self.on, &self.off]
            .into_iter()
            .flatten()
            .any(|(_, fade)| fade.is_running(now))
    }
}
