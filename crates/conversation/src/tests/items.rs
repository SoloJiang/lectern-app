//! `items_*`: the pure functions that turn a snapshot into the list's items,
//! find what changed between two builds, and say it in words
//! (docs/design/conversation.md「测试」的纯函数). No window.

use std::path::Path;
use std::sync::Arc;

use lectern_i18n::{Locale, Strings};
use lectern_services::conversation::{
    CallKind, CallState, ContextUsage, EndReason, Entry, EntryBody, Link, LinkError, NoteLocation,
    PromptOrigin,
};

use crate::items::{self, Body, Inconsistency, ItemKey, Splice, build, diff, shown_tools};
use crate::present;
use crate::testing::{agent_text, entry_key, passage, prompt, session, tool, with_entries};

fn en() -> Strings {
    Strings::of(Locale::En)
}

fn zh() -> Strings {
    Strings::of(Locale::ZhCn)
}

/// The text without the directional isolates that wrap the arguments.
fn plain(text: impl AsRef<str>) -> String {
    text.as_ref().replace(['\u{2068}', '\u{2069}'], "")
}

fn kinds(items: &[items::Item]) -> Vec<&'static str> {
    items
        .iter()
        .map(|item| match item.body {
            Body::Earlier(_) => "earlier",
            Body::Prompt { .. } => "prompt",
            Body::Notes { .. } => "notes",
            Body::Agent { .. } => "agent",
            Body::Injected { .. } => "injected",
            Body::Tools { .. } => "tools",
            Body::Ended(_) => "ended",
            Body::Failed(_) => "failed",
        })
        .collect()
}

#[test]
fn items_follow_the_conversation_order() {
    let key = session(1);
    let injected = Entry {
        key: entry_key("h:0:2"),
        body: EntryBody::Injected {
            text: passage("a background task finished"),
        },
    };
    let ended = Entry {
        key: entry_key("h:0:4"),
        body: EntryBody::Ended(EndReason::MaxTokens),
    };
    let snap = with_entries(
        &key,
        1,
        vec![
            prompt("h:0:0", "fix it"),
            agent_text("h:0:1", "looking", false),
            injected,
            tool("h:0:3", CallKind::Edit, "a.rs", CallState::Completed),
            ended,
        ],
    );
    let built = build(&snap);
    assert_eq!(built.inconsistency, None);
    assert_eq!(
        kinds(&built.items),
        ["prompt", "agent", "injected", "tools", "ended"]
    );
    assert_eq!(
        built
            .items
            .iter()
            .map(|item| item.key.clone())
            .collect::<Vec<_>>(),
        ["h:0:0", "h:0:1", "h:0:2", "h:0:3", "h:0:4"]
            .map(|key| ItemKey::Entry(entry_key(key)))
            .to_vec()
    );
    // The probe's node carries the entry key.
    assert_eq!(
        built.items[1].node.as_str(),
        "conversation.list.item[h:0:1]"
    );
}

#[test]
fn items_join_consecutive_tool_calls_and_name_at_most_four() {
    let key = session(1);
    let entries: Vec<Entry> = (0..6)
        .map(|n| {
            tool(
                &format!("l:0:{n}"),
                CallKind::Edit,
                &format!("file{n}.rs"),
                CallState::Completed,
            )
        })
        .collect();
    let snap = with_entries(&key, 1, entries);
    let built = build(&snap);
    assert_eq!(kinds(&built.items), ["tools"]);
    // The row's key is the first call's.
    assert_eq!(built.items[0].key, ItemKey::Entry(entry_key("l:0:0")));
    let Body::Tools { calls } = &built.items[0].body else {
        panic!("a tools row");
    };
    assert_eq!(calls.len(), 6);
    let (shown, more) = shown_tools(calls);
    assert_eq!((shown.len(), more), (4, 2));
    assert_eq!(
        plain(present::tools_line(&en(), calls)),
        "Edit file0.rs · Edit file1.rs · Edit file2.rs · Edit file3.rs · 2 more"
    );
    assert_eq!(
        plain(present::tools_line(&zh(), &calls[..1])),
        "修改 file0.rs"
    );
}

#[test]
fn items_do_not_join_tool_calls_across_other_entries() {
    let key = session(1);
    let snap = with_entries(
        &key,
        1,
        vec![
            tool("h:0:0", CallKind::Read, "a.rs", CallState::Completed),
            agent_text("h:0:1", "now the edit", false),
            tool("h:0:2", CallKind::Edit, "a.rs", CallState::Failed),
        ],
    );
    assert_eq!(kinds(&build(&snap).items), ["tools", "agent", "tools"]);
}

#[test]
fn items_say_how_a_tool_call_ended() {
    let calls = [
        ToolOf(CallKind::Execute, "yarn test unit", CallState::Failed),
        ToolOf(CallKind::SpawnAgent, "", CallState::Denied),
        ToolOf(CallKind::Other, "mcp tool", CallState::Cancelled),
        ToolOf(CallKind::Think, "thinking", CallState::Running),
    ]
    .map(ToolOf::entry);
    assert_eq!(
        plain(present::tools_line(&en(), &calls)),
        "Run yarn test unit · failed · Start a subagent · denied · mcp tool · cancelled · thinking"
    );
}

struct ToolOf(CallKind, &'static str, CallState);

impl ToolOf {
    fn entry(self) -> lectern_services::conversation::ToolEntry {
        lectern_services::conversation::ToolEntry {
            kind: self.0,
            state: self.2,
            summary: Arc::from(self.1),
        }
    }
}

#[test]
fn items_turn_review_notes_into_one_line() {
    let key = session(1);
    let notes = Entry {
        key: entry_key("l:0:0"),
        body: EntryBody::Prompt {
            text: passage("the full prompt that is not shown"),
            origin: PromptOrigin::ReviewNotes {
                count: 2,
                locations: Arc::from(vec![
                    NoteLocation {
                        path: Arc::from(Path::new("/work/app/src/a.rs")),
                        line: 12,
                    },
                    NoteLocation {
                        path: Arc::from(Path::new("/elsewhere/b.rs")),
                        line: 3,
                    },
                ]),
            },
        },
    };
    let snap = with_entries(&key, 1, vec![notes]);
    let built = build(&snap);
    assert_eq!(kinds(&built.items), ["notes"]);
    let Body::Notes { count, locations } = &built.items[0].body else {
        panic!("a notes row");
    };
    assert_eq!(
        present::notes_line(&en(), *count, locations, Some(Path::new("/work/app"))),
        "2 review notes · src/a.rs:12, /elsewhere/b.rs:3"
    );
    assert_eq!(present::notes_line(&zh(), 1, &[], None), "1 条审阅意见");
}

#[test]
fn items_put_the_earlier_turns_first_and_a_failed_link_last() {
    let key = session(1);
    let mut snap = with_entries(&key, 1, vec![prompt("h:5:0", "hello")]);
    snap.earlier = 5;
    snap.link = Link::Failed(LinkError::Spawn);
    let built = build(&snap);
    assert_eq!(kinds(&built.items), ["earlier", "prompt", "failed"]);
    assert_eq!(built.items[0].body, Body::Earlier(5));
    assert_eq!(built.items[0].node.as_str(), "conversation.earlier");
    assert_eq!(built.items[2].node.as_str(), "conversation.notice");
}

#[test]
fn items_diff_reports_only_the_stretch_that_changed() {
    let key = session(1);
    let base = vec![
        prompt("h:0:0", "go"),
        agent_text("h:0:1", "first", false),
        prompt("l:1:0", "again"),
        agent_text("l:1:1", "the answer so", true),
    ];
    let before = build(&with_entries(&key, 1, base.clone())).items;

    // A streaming update changes the last item and nothing else.
    let mut streamed = base.clone();
    streamed[3] = agent_text("l:1:1", "the answer so far", true);
    let after = build(&with_entries(&key, 2, streamed)).items;
    assert_eq!(
        diff(&before, &after),
        Some(Splice {
            start: 3,
            removed: 1,
            added: 1
        })
    );

    // Nothing changed.
    assert_eq!(diff(&before, &before.clone()), None);

    // The streaming text ends and a tool call follows: the item is replaced
    // and one is added.
    let mut next = base.clone();
    next[3] = agent_text("l:1:1", "the answer", false);
    next.push(tool("l:1:2", CallKind::Edit, "a.rs", CallState::Running));
    let after = build(&with_entries(&key, 3, next)).items;
    assert_eq!(
        diff(&before, &after),
        Some(Splice {
            start: 3,
            removed: 1,
            added: 2
        })
    );

    // Earlier turns load in front: the change is at the front only.
    let mut earlier = with_entries(&key, 4, base.clone());
    earlier.earlier = 3;
    let with_earlier = build(&earlier).items;
    let mut older = base.clone();
    older.insert(0, prompt("h:-1:0", "older"));
    let mut loaded = with_entries(&key, 5, older);
    loaded.earlier = 2;
    let after = build(&loaded).items;
    assert_eq!(
        diff(&with_earlier, &after),
        Some(Splice {
            start: 0,
            removed: 1,
            added: 2
        })
    );
}

#[test]
fn items_replace_a_live_entry_by_the_history_once() {
    let key = session(1);
    let live = build(&with_entries(
        &key,
        1,
        vec![prompt("h:0:0", "go"), prompt("l:1:0", "again")],
    ))
    .items;
    let history = build(&with_entries(
        &key,
        2,
        vec![prompt("h:0:0", "go"), prompt("h:1:0", "again")],
    ))
    .items;
    assert_eq!(
        diff(&live, &history),
        Some(Splice {
            start: 1,
            removed: 1,
            added: 1
        })
    );
    // The replacement is not an arrival: the reader saw it already.
    assert!(items::arrivals(&live, &history).is_empty());
}

#[test]
fn items_arrivals_are_new_entries_after_the_ones_that_stayed() {
    let key = session(1);
    let before = build(&with_entries(&key, 1, vec![prompt("h:0:0", "go")])).items;
    // Nothing to compare with: the first draw has no arrivals.
    assert!(items::arrivals(&[], &before).is_empty());
    let after = build(&with_entries(
        &key,
        2,
        vec![prompt("h:0:0", "go"), agent_text("l:1:0", "ok", true)],
    ))
    .items;
    assert_eq!(items::arrivals(&before, &after), [entry_key("l:1:0")]);
    // Earlier turns in front are not arrivals.
    let older = build(&with_entries(
        &key,
        3,
        vec![prompt("h:-1:0", "older"), prompt("h:0:0", "go")],
    ))
    .items;
    assert!(items::arrivals(&before, &older).is_empty());
}

#[test]
fn items_keep_the_first_of_a_repeated_key_and_say_so() {
    let key = session(1);
    let snap = with_entries(
        &key,
        1,
        vec![prompt("h:0:0", "one"), prompt("h:0:0", "two")],
    );
    let built = build(&snap);
    assert_eq!(built.items.len(), 1);
    assert_eq!(built.inconsistency, Some(Inconsistency::DuplicateKey));
    let Body::Prompt { text } = &built.items[0].body else {
        panic!("a prompt");
    };
    assert_eq!(&*text.text, "one");
}

#[test]
fn items_show_a_streaming_text_that_is_not_last_as_finished() {
    let key = session(1);
    let snap = with_entries(
        &key,
        1,
        vec![
            agent_text("l:0:0", "first", true),
            prompt("l:0:1", "second"),
        ],
    );
    let built = build(&snap);
    assert_eq!(built.inconsistency, Some(Inconsistency::StreamingNotLast));
    assert!(matches!(
        built.items[0].body,
        Body::Agent {
            streaming: false,
            ..
        }
    ));
}

#[test]
fn items_say_why_a_turn_ended() {
    for (reason, text) in [
        (EndReason::MaxTokens, "The reply reached the output limit."),
        (
            EndReason::MaxTurnRequests,
            "The turn reached the request limit.",
        ),
        (EndReason::Refusal, "The agent declined to continue."),
        (EndReason::Cancelled, "Stopped."),
    ] {
        assert_eq!(present::ended_text(&en(), reason).as_str(), text);
    }
}

#[test]
fn items_say_how_full_the_context_is() {
    let percent = |used, size| present::context_percent(&ContextUsage { used, size });
    assert_eq!(percent(72, 100), Some(72));
    assert_eq!(percent(0, 200_000), Some(0));
    // Over the window: the line still reads as a percentage of it.
    assert_eq!(percent(300, 200), Some(100));
    assert_eq!(percent(1, 0), None);
    assert_eq!(present::context_text(&en(), 72).as_str(), "context 72%");
    assert_eq!(present::context_text(&zh(), 72).as_str(), "上下文 72%");
}
