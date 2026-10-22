//! The change narrative (docs/design/review.md「改动叙事」): the turns of
//! the review target's sessions in order, a subagent's turns right after
//! the turn that spawned it, and per turn its texts, its change steps (one
//! per file and turn, where the turn first changed the file), its commands
//! with the files they claimed, its subagents and its other calls.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lectern_services::review::{NarrativeItem, StepEntry, TextKey, TurnEntry, TurnRef};
use lectern_services::session::{
    SessionSummary, Timestamp, ToolCallId, ToolKind, TurnIndex, TurnItem,
};

/// One of the review target's sessions: index 0 is the target.
pub(crate) struct Own {
    pub summary: SessionSummary,
    /// None when the log is unrecognized.
    pub index: Option<Arc<TurnIndex>>,
}

/// Where a subagent's turns go: after this turn of this session (indices
/// into `own` and its turns), and the spawning call when the parent has it.
#[derive(Clone, Debug)]
pub(crate) struct Placement {
    pub parent: u16,
    pub turn: usize,
    pub call: Option<ToolCallId>,
}

/// Places each subagent (`own[1..]`) under its parent's turns: the turn
/// holding the call that spawned it (`Parent::tool_call`), otherwise the
/// parent's last turn that started before the subagent's first one.
pub(crate) fn place(own: &[Own]) -> Vec<Option<Placement>> {
    let at = |key: &lectern_services::session::SessionKey| {
        own.iter().position(|o| o.summary.key == *key)
    };
    own.iter()
        .enumerate()
        .map(|(i, o)| {
            if i == 0 {
                return None;
            }
            let parent = o.summary.parent.as_ref()?;
            let p = at(&parent.session)?;
            let turns = own[p].index.as_ref().map(|x| &x.turns[..]).unwrap_or(&[]);
            if let Some(call) = &parent.tool_call
                && let Some(t) = turns.iter().position(|t| {
                    t.items
                        .iter()
                        .any(|item| matches!(item, TurnItem::Tool(c) if c.id == *call))
                })
            {
                return Some(Placement {
                    parent: p as u16,
                    turn: t,
                    call: Some(call.clone()),
                });
            }
            let start = o
                .index
                .as_ref()
                .and_then(|x| x.turns.first())
                .map_or(o.summary.started_at, |t| t.started_at);
            let turn = turns
                .iter()
                .rposition(|t| t.started_at <= start)
                .unwrap_or(0);
            Some(Placement {
                parent: p as u16,
                turn,
                call: None,
            })
        })
        .collect()
}

/// The turns in narrative order, as (session, index into its turns).
pub(crate) fn order(own: &[Own], placed: &[Option<Placement>]) -> Vec<(u16, usize)> {
    fn emit(
        s: usize,
        own: &[Own],
        children: &HashMap<(u16, usize), Vec<usize>>,
        out: &mut Vec<(u16, usize)>,
    ) {
        let Some(index) = &own[s].index else {
            return;
        };
        for t in 0..index.turns.len() {
            out.push((s as u16, t));
            for &c in children.get(&(s as u16, t)).into_iter().flatten() {
                emit(c, own, children, out);
            }
        }
    }
    let mut children: HashMap<(u16, usize), Vec<usize>> = HashMap::new();
    for (c, p) in placed.iter().enumerate() {
        if let Some(p) = p {
            children.entry((p.parent, p.turn)).or_default().push(c);
        }
    }
    for list in children.values_mut() {
        list.sort_by_key(|&c| (own[c].summary.started_at, c));
    }
    let mut out = Vec::new();
    if !own.is_empty() {
        emit(0, own, &children, &mut out);
    }
    out
}

/// What the narrative needs from the round, per session and call.
pub(crate) struct Inputs<'a> {
    pub own: &'a [Own],
    pub placed: &'a [Option<Placement>],
    pub order: &'a [(u16, usize)],
    /// Per (session, call): the files (indices into the snapshot's
    /// `files`) whose narrative step in that call's turn includes it.
    pub step_files: &'a HashMap<(u16, ToolCallId), Vec<u32>>,
    /// Per (session, call): the files the command claimed.
    pub command_files: &'a HashMap<(u16, ToolCallId), Vec<u32>>,
    /// The time of this target's last `seen` mark.
    pub last_seen: Option<Timestamp>,
}

/// The narrative. `step` gives the entry of a file's step in a turn.
pub(crate) fn build(
    inputs: &Inputs<'_>,
    file_key: &dyn Fn(u32) -> lectern_services::review::FileKey,
    step: &dyn Fn(u16, u32, u32) -> Option<StepEntry>,
) -> Arc<[TurnEntry]> {
    let mut out = Vec::with_capacity(inputs.order.len());
    for &(s, t) in inputs.order {
        let Some(index) = &inputs.own[s as usize].index else {
            continue;
        };
        let turn = &index.turns[t];
        let mut items = Vec::with_capacity(turn.items.len());
        let mut emitted: HashSet<u32> = HashSet::new();
        for item in turn.items.iter() {
            match item {
                TurnItem::AgentText(at) => items.push(NarrativeItem::AgentText(TextKey {
                    session: s,
                    at: *at,
                })),
                TurnItem::Injected(at) => items.push(NarrativeItem::Injected(TextKey {
                    session: s,
                    at: *at,
                })),
                TurnItem::Tool(call) => {
                    let tool = || NarrativeItem::Tool {
                        call: call.id.clone(),
                        name: call.name.clone(),
                        status: call.finished.map(|(_, status)| status),
                    };
                    let key = (s, call.id.clone());
                    match &call.kind {
                        ToolKind::Edit { .. } => {
                            let files = inputs.step_files.get(&key);
                            let mut any = false;
                            for &f in files.into_iter().flatten() {
                                any = true;
                                if emitted.insert(f)
                                    && let Some(entry) = step(s, turn.ordinal, f)
                                {
                                    items.push(NarrativeItem::Step(entry));
                                }
                            }
                            // An edit with no change step in the review (the
                            // file is not in it, or the change is before the
                            // path's cutoff): one line, as the other calls.
                            if !any {
                                items.push(tool());
                            }
                        }
                        ToolKind::Execute { .. } => match inputs.command_files.get(&key) {
                            Some(files) if !files.is_empty() => {
                                items.push(NarrativeItem::Command {
                                    call: call.id.clone(),
                                    name: call.name.clone(),
                                    files: files.iter().map(|&f| file_key(f)).collect(),
                                })
                            }
                            _ => items.push(tool()),
                        },
                        ToolKind::SpawnAgent => {
                            let child = inputs.placed.iter().position(|p| {
                                p.as_ref().is_some_and(|p| {
                                    p.parent == s && p.call.as_ref() == Some(&call.id)
                                })
                            });
                            match child {
                                Some(c) => items.push(NarrativeItem::Subagent {
                                    session: c as u16,
                                    call: call.id.clone(),
                                }),
                                None => items.push(tool()),
                            }
                        }
                        _ => items.push(tool()),
                    }
                }
            }
        }
        out.push(TurnEntry {
            at: TurnRef {
                session: s,
                ordinal: turn.ordinal,
            },
            id: turn.id.clone(),
            started_at: turn.started_at,
            ended_at: turn.ended_at,
            prompt: turn.prompt.map(|at| TextKey { session: s, at }),
            prompt_preview: turn.prompt_preview.clone(),
            items: items.into(),
            after_last_seen: inputs.last_seen.is_some_and(|seen| turn.started_at > seen),
        });
    }
    out.into()
}
