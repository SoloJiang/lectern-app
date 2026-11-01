//! `conversation/*` (docs/design/conversation.md「性能」):
//! `conversation/stream_apply`, a stream of snapshots over a conversation of
//! 40 turns and 1 MiB of text, in which the last agent passage grows by 200
//! bytes per snapshot. GPUI only builds on macOS in this workspace, so this
//! module is compiled there alone; the budget is `macos_only` and skipped
//! elsewhere. No real window is opened: the page is in a window of GPUI's
//! test platform.

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_conversation::fixture::StreamApply;

pub const BENCHES: &[Bench] = &[STREAM_APPLY];

/// One snapshot of the stream: `items::build`, the comparison with the shown
/// items, the splice of the last item, and a frame in which the list lays out
/// that item and paints the rows on the screen. In process; the frame is the
/// test platform's (no GPU, the test text system), so the layout of the
/// text is cheaper than the real one.
pub const STREAM_APPLY: Bench = Bench {
    id: "conversation/stream_apply",
    run: stream_apply,
};

fn stream_apply(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let mut fixture = StreamApply::new();
    let (items, bytes) = fixture.size();
    // 40 turns of a prompt, a row of tool calls and a passage, and 1 MiB of
    // text: a fixture that is smaller measures nothing.
    if items < 120 || bytes < 1 << 20 {
        return Err(BenchError::new(format!(
            "the fixture should give 120 items or more and 1 MiB of text; it has {items} items \
             and {bytes} bytes"
        )));
    }
    let before = fixture.spliced();
    fixture.step();
    // A stream changes the last item and nothing else.
    if fixture.spliced() != before + 1 {
        return Err(BenchError::new(
            "one snapshot of the stream should replace one item of the list",
        ));
    }
    let time = ctx.time(|| fixture.step());
    Ok(vec![Measured::new(
        "conversation/stream_apply",
        Value::Time(time),
    )])
}
