//! The list's items, computed from a snapshot by pure functions
//! (docs/design/conversation.md「数据结构」的「列表的项」): no service is read
//! here. [`build`] turns the snapshot into items, [`diff`] says which stretch
//! of the list changed between two builds, so that only that stretch is laid
//! out again (a streaming update changes the last item and nothing else).

use std::collections::HashSet;
use std::sync::Arc;

use lectern_services::conversation::{
    ConversationSnapshot, EndReason, EntryBody, EntryKey, Link, LinkError, NoteLocation, Passage,
    PromptOrigin, ToolEntry,
};
use lectern_ui::semantic::NodeId;

use crate::nodes;

/// The most tool calls one row names; the others are "n more".
pub(crate) const MAX_TOOLS: usize = 4;

/// What identifies an item across builds. An entry keeps its key from one
/// generation to the next; only a live entry that history replaces changes
/// it, once.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum ItemKey {
    /// The earlier-turns button.
    Earlier,
    Entry(EntryKey),
    /// The notice about a failed link, at the end.
    Notice,
}

/// One row of the list. The `NodeId` is built once, with the item.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Item {
    pub key: ItemKey,
    pub node: NodeId,
    pub body: Body,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Body {
    /// The earlier-turns button (`earlier > 0`, first in the list).
    Earlier(u32),
    Prompt {
        text: Passage,
    },
    /// A prompt that carried review notes: one line, "n review notes ·
    /// places".
    Notes {
        count: u32,
        locations: Arc<[NoteLocation]>,
    },
    Agent {
        text: Passage,
        streaming: bool,
    },
    Injected {
        text: Passage,
    },
    /// Consecutive tool calls in one row. The key is the first call's.
    Tools {
        calls: Vec<ToolEntry>,
    },
    Ended(EndReason),
    /// The link failed: the reason and "Dismiss".
    Failed(LinkError),
}

/// How a snapshot broke the provider's invariants (it should not happen):
/// the page logs a warning and shows what it can.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Inconsistency {
    DuplicateKey,
    StreamingNotLast,
}

impl Inconsistency {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Inconsistency::DuplicateKey => "duplicate_key",
            Inconsistency::StreamingNotLast => "streaming_not_last",
        }
    }
}

pub(crate) struct Built {
    pub items: Vec<Item>,
    pub inconsistency: Option<Inconsistency>,
}

/// The items of a snapshot, in the conversation's order: "earlier" first when
/// the history reaches further back, the entries (consecutive tool calls in
/// one row), and the notice when the link failed. A key that repeats keeps its
/// first entry. A `streaming` text that is not the last entry is shown as
/// finished.
pub(crate) fn build(snapshot: &ConversationSnapshot) -> Built {
    let mut items = Vec::with_capacity(snapshot.entries.len() + 2);
    let mut inconsistency = None;
    if snapshot.earlier > 0 {
        items.push(Item {
            key: ItemKey::Earlier,
            node: nodes::area("earlier"),
            body: Body::Earlier(snapshot.earlier),
        });
    }
    let mut seen: HashSet<&EntryKey> = HashSet::with_capacity(snapshot.entries.len());
    let last = snapshot.entries.len().saturating_sub(1);
    for (at, entry) in snapshot.entries.iter().enumerate() {
        if !seen.insert(&entry.key) {
            inconsistency.get_or_insert(Inconsistency::DuplicateKey);
            continue;
        }
        let body = match &entry.body {
            EntryBody::Prompt { text, origin } => match origin {
                PromptOrigin::User => Body::Prompt { text: text.clone() },
                PromptOrigin::ReviewNotes { count, locations } => Body::Notes {
                    count: *count,
                    locations: locations.clone(),
                },
            },
            EntryBody::AgentText { text, streaming } => {
                if *streaming && at != last {
                    inconsistency.get_or_insert(Inconsistency::StreamingNotLast);
                }
                Body::Agent {
                    text: text.clone(),
                    streaming: *streaming && at == last,
                }
            }
            EntryBody::Injected { text } => Body::Injected { text: text.clone() },
            EntryBody::Tool(call) => {
                // Consecutive calls share a row.
                if let Some(Item {
                    body: Body::Tools { calls },
                    ..
                }) = items.last_mut()
                {
                    calls.push(call.clone());
                    continue;
                }
                Body::Tools {
                    calls: vec![call.clone()],
                }
            }
            EntryBody::Ended(reason) => Body::Ended(*reason),
        };
        items.push(Item {
            key: ItemKey::Entry(entry.key.clone()),
            node: nodes::entry(&entry.key),
            body,
        });
    }
    if let Link::Failed(error) = &snapshot.link {
        items.push(Item {
            key: ItemKey::Notice,
            node: nodes::area("notice"),
            body: Body::Failed(error.clone()),
        });
    }
    Built {
        items,
        inconsistency,
    }
}

/// The one stretch of the list that differs between two builds: the items
/// `start..start + removed` of the old list are now the `added` items that
/// follow `start` in the new one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Splice {
    pub start: usize,
    pub removed: usize,
    pub added: usize,
}

/// What changed between two lists: the items they share at the start and at
/// the end (same key, same content) are kept, and the rest is one splice.
/// None when the two are equal.
pub(crate) fn diff(old: &[Item], new: &[Item]) -> Option<Splice> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let room = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    Some(Splice {
        start: prefix,
        removed: old.len() - prefix - suffix,
        added: new.len() - prefix - suffix,
    })
}

/// The entries that arrived between two builds, for the arrival animation:
/// new keys after the last entry that stayed. None when the list was empty
/// (the first draw), when an entry left (history replaced a live one, or the
/// oldest turns were dropped), and for entries before the ones that stayed
/// (earlier turns loaded in front).
pub(crate) fn arrivals(old: &[Item], new: &[Item]) -> Vec<EntryKey> {
    if old.is_empty() {
        return Vec::new();
    }
    let old_keys: HashSet<&ItemKey> = old.iter().map(|item| &item.key).collect();
    let new_keys: HashSet<&ItemKey> = new.iter().map(|item| &item.key).collect();
    let left = old
        .iter()
        .any(|item| matches!(item.key, ItemKey::Entry(_)) && !new_keys.contains(&item.key));
    if left {
        return Vec::new();
    }
    let last_kept = new
        .iter()
        .rposition(|item| matches!(item.key, ItemKey::Entry(_)) && old_keys.contains(&item.key));
    new.iter()
        .enumerate()
        .filter_map(|(ix, item)| match &item.key {
            ItemKey::Entry(key)
                if !old_keys.contains(&item.key) && last_kept.is_none_or(|last| ix > last) =>
            {
                Some(key.clone())
            }
            _ => None,
        })
        .collect()
}

/// The calls a row names and how many are left out.
pub(crate) fn shown_tools(calls: &[ToolEntry]) -> (&[ToolEntry], usize) {
    let shown = calls.len().min(MAX_TOOLS);
    (&calls[..shown], calls.len() - shown)
}
