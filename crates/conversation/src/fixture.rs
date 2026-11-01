//! The input of the `conversation/stream_apply` benchmark
//! (docs/design/conversation.md「性能」), behind the `test-support` feature,
//! for `lectern-bench`.
//!
//! A real [`MessagesModel`] in a test window of GPUI's test platform (no real
//! window is opened, no frame is presented), holding a snapshot of 40 turns
//! with 1 MiB of text. [`StreamApply::step`] hands it the next snapshot, in
//! which the last agent passage is 200 bytes longer, and draws a frame: the
//! timed work is `items::build`, the comparison with the shown items, the
//! `ListState::splice` of the last item and the layout and paint of what is
//! on the screen. The names, paths and texts are synthetic.

use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled, TestAppContext,
    Window, WindowHandle, div, px,
};
use lectern_i18n::{Locale, Strings};
use lectern_services::conversation::{
    CallKind, CallState, ConversationSnapshot, Entry, EntryBody, EntryKey, Link, Load, Passage,
    PromptOrigin, ToolEntry,
};
use lectern_services::session::{Agent, SessionKey, SourceId};
use lectern_ui::{Appearance, init_fixed};

use crate::plugin::Shared;
use crate::view::MessagesModel;

/// Turns in the snapshot.
const TURNS: usize = 40;
/// The text of the snapshot before the first step: 1 MiB.
const BODY: usize = 1 << 20;
/// Snapshots in a cycle, and the bytes each adds to the last passage.
const STEPS: usize = 200;
const GROWTH: usize = 200;
/// The last passage before the first step; 200 steps of 200 bytes later it is
/// still under the 64 KiB a passage may have.
const LAST_BASE: usize = 16 * 1024;
const PROMPT: usize = 240;

/// The page's height and width as `workspace` lays out the inspector.
const WIDTH: f32 = 300.0;
const HEIGHT: f32 = 560.0;

/// The list of the Messages page in a window.
struct Stage {
    model: Entity<MessagesModel>,
}

impl Render for Stage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self
            .model
            .update(cx, |model, cx| model.render_body(window, cx));
        div().w(px(WIDTH)).h(px(HEIGHT)).child(body)
    }
}

/// Text of `len` bytes: words of lower-case letters, a line break every few
/// words. Deterministic and the same for the same `seed`.
fn text(len: usize, seed: usize) -> String {
    let mut out = String::with_capacity(len + 16);
    let mut n = seed;
    let mut words = 0;
    while out.len() < len {
        n = n.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let size = 2 + (n >> 16) % 8;
        for i in 0..size {
            out.push((b'a' + ((n >> (i % 16)) % 26) as u8) as char);
        }
        words += 1;
        out.push(if words % 12 == 0 { '\n' } else { ' ' });
    }
    out.truncate(len);
    out
}

fn passage(text: String) -> Passage {
    Passage {
        text: Arc::from(text),
        truncated: false,
    }
}

fn entry(key: String, body: EntryBody) -> Entry {
    Entry {
        key: EntryKey(Arc::from(key)),
        body,
    }
}

/// The entries of the base snapshot, without the last passage: for each turn
/// a prompt, three tool calls and an agent passage of about 26 KiB.
fn base_entries() -> Vec<Entry> {
    let rest = BODY - LAST_BASE - TURNS * PROMPT;
    let per_turn = rest / (TURNS - 1);
    // The first passage takes what does not divide, so that the text is
    // exactly `BODY` bytes.
    let remainder = rest % (TURNS - 1);
    let mut entries = Vec::new();
    for turn in 0..TURNS {
        entries.push(entry(
            format!("prompt-{turn}"),
            EntryBody::Prompt {
                text: passage(text(PROMPT, turn)),
                origin: PromptOrigin::User,
            },
        ));
        for call in 0..3 {
            entries.push(entry(
                format!("tool-{turn}-{call}"),
                EntryBody::Tool(ToolEntry {
                    kind: CallKind::Read,
                    state: CallState::Completed,
                    summary: Arc::from(format!("src/module_{turn}/part_{call}.rs")),
                }),
            ));
        }
        if turn + 1 < TURNS {
            entries.push(entry(
                format!("agent-{turn}"),
                EntryBody::AgentText {
                    text: passage(text(
                        per_turn + if turn == 0 { remainder } else { 0 },
                        1_000 + turn,
                    )),
                    streaming: false,
                },
            ));
        }
    }
    entries
}

fn snapshot(
    session: &SessionKey,
    generation: u64,
    base: &[Entry],
    grown: usize,
) -> ConversationSnapshot {
    let mut entries = base.to_vec();
    entries.push(entry(
        format!("agent-{}", TURNS - 1),
        EntryBody::AgentText {
            text: passage(text(LAST_BASE + grown, 9_999)),
            streaming: true,
        },
    ));
    ConversationSnapshot {
        session: session.clone(),
        generation,
        agent: Agent::ClaudeCode,
        load: Load::Ready,
        link: Link::Working,
        can_prompt: Ok(()),
        context: None,
        earlier: 0,
        entries: Arc::from(entries),
        plan: None,
        permissions: Arc::from([]),
    }
}

/// The stream of 200 snapshots over a snapshot of 40 turns and 1 MiB.
pub struct StreamApply {
    window: WindowHandle<Stage>,
    model: Entity<MessagesModel>,
    /// The snapshots of one cycle, in order of the growth of the last passage.
    snapshots: Vec<ConversationSnapshot>,
    /// The step the next call of [`step`](StreamApply::step) takes, and the
    /// generation it gives its snapshot (always above the shown one).
    next: usize,
    generation: u64,
    /// Last: the test context checks for leaked handles when it drops, so
    /// the handles above go first.
    cx: TestAppContext,
}

impl StreamApply {
    /// Builds the window and shows the base snapshot, laid out at the end of
    /// the list. Not timed.
    pub fn new() -> StreamApply {
        let session = SessionKey {
            source: SourceId(Arc::from("fixture")),
            id: Arc::from("00000000-0000-4000-8000-000000000001"),
        };
        let base = base_entries();
        let snapshots: Vec<ConversationSnapshot> = (0..=STEPS)
            .map(|step| snapshot(&session, 0, &base, step * GROWTH))
            .collect();
        let mut cx = TestAppContext::single();
        cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
        let shared = Arc::new(Shared::new());
        let window = cx.add_window(|window, cx| {
            let model = cx.new(|cx| MessagesModel::new(shared, session, window, cx));
            Stage { model }
        });
        let model = window
            .read_with(&cx, |stage, _| stage.model.clone())
            .expect("the window is open");
        let mut fixture = StreamApply {
            cx,
            window,
            model,
            snapshots,
            next: 0,
            generation: 0,
        };
        // The base snapshot, then two frames: the second one lays out what
        // the first one's callbacks changed.
        fixture.show(0);
        fixture.draw();
        fixture.draw();
        fixture.next = 1;
        fixture
    }

    /// The next snapshot of the stream is applied and a frame is drawn. After
    /// the 200th the last passage goes back to its first length (the
    /// generation keeps rising).
    pub fn step(&mut self) {
        let step = self.next;
        self.next = if step >= STEPS { 1 } else { step + 1 };
        self.show(step);
        self.draw();
    }

    /// `items::build`, the comparison, the splice: the model applies the
    /// snapshot of `step` with a generation above the shown one.
    fn show(&mut self, step: usize) {
        self.generation += 1;
        let snapshot = Arc::new(ConversationSnapshot {
            generation: self.generation,
            ..self.snapshots[step].clone()
        });
        self.cx.update(|cx| {
            self.model.update(cx, |model, cx| model.apply(snapshot, cx));
        });
    }

    /// One frame: the list lays out the item that was spliced in and paints
    /// the rows on the screen.
    fn draw(&mut self) {
        // Not through `WindowHandle::update`: it holds the root view, which
        // the frame renders.
        self.cx
            .update_window(self.window.into(), |_, window, cx| {
                window.draw(cx).clear(cx)
            })
            .expect("the window is open");
    }

    /// How many items the page holds, and the bytes of text of its entries.
    pub fn size(&self) -> (usize, usize) {
        self.cx.read(|cx| {
            let model = self.model.read(cx);
            let bytes = model.snapshot().map_or(0, |snapshot| {
                snapshot
                    .entries
                    .iter()
                    .map(|entry| match &entry.body {
                        EntryBody::Prompt { text, .. } | EntryBody::AgentText { text, .. } => {
                            text.text.len()
                        }
                        _ => 0,
                    })
                    .sum()
            });
            (model.item_count(), bytes)
        })
    }

    /// The number of items the list replaced in all the snapshots after the
    /// first one: one per step if only the last item changes.
    pub fn spliced(&self) -> usize {
        self.cx.read(|cx| self.model.read(cx).spliced().0)
    }
}

impl Default for StreamApply {
    fn default() -> StreamApply {
        StreamApply::new()
    }
}
