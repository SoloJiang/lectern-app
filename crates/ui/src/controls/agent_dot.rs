//! The agent identification dot: 8pt, the one custom colour (ADR 0015).
//! `blink` lets it dim once (`ARRIVE`'s length, opacity 1 → 0.3 → 1).

use std::f32::consts::PI;

use gpui::{AnimationExt, App, ElementId, IntoElement, RenderOnce, Styled, Window, div, px};

use crate::controls::require_ready;
use crate::motion::ARRIVE;
use crate::palette::colors;
use crate::semantic::{self, NodeId, Role, Semantic};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Agent {
    ClaudeCode,
    Codex,
}

impl Agent {
    /// The `agent` attribute of the dot's node.
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
        }
    }
}

#[derive(IntoElement)]
pub struct AgentDot {
    agent: Agent,
    blink: Option<ElementId>,
    node: Option<NodeId>,
}

/// The dot's lowest opacity while it blinks.
const BLINK_FLOOR: f32 = 0.3;

impl AgentDot {
    pub fn new(agent: Agent) -> AgentDot {
        AgentDot {
            agent,
            blink: None,
            node: None,
        }
    }

    /// Blinks once, as `id`'s animation: a new `id` (the round's) starts a
    /// new blink; the same `id` on a later render does not repeat it.
    pub fn blink(mut self, id: ElementId) -> AgentDot {
        self.blink = Some(id);
        self
    }

    /// Declares the dot as a `Badge` node. Without it the dot is decoration
    /// and declares nothing.
    pub fn node(mut self, id: NodeId) -> AgentDot {
        self.node = Some(id);
        self
    }

    /// Opacity at blink progress `t`: 1 at both ends, `BLINK_FLOOR` midway.
    pub(crate) fn blink_opacity(t: f32) -> f32 {
        1.0 - (1.0 - BLINK_FLOOR) * (PI * t.clamp(0.0, 1.0)).sin()
    }
}

impl RenderOnce for AgentDot {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = colors(window, cx);
        let color = match self.agent {
            Agent::ClaudeCode => palette.agent_claude,
            Agent::Codex => palette.agent_codex,
        };
        let dot = div().size(px(8.0)).rounded_full().flex_shrink_0().bg(color);
        let dot = match self.blink {
            Some(id) => dot
                .with_animation(id, ARRIVE.animation(cx), |dot, t| {
                    dot.opacity(AgentDot::blink_opacity(t))
                })
                .into_any_element(),
            None => dot.into_any_element(),
        };
        match self.node {
            Some(id) => semantic::declare(
                Semantic::new(id, Role::Badge).attr("agent", self.agent.as_str()),
                dot,
            ),
            None => dot,
        }
    }
}
