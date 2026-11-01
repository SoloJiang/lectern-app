//! The fixture of `conversation/stream_apply`
//! (docs/design/conversation.md「性能」): it must hold what the benchmark
//! says it measures. No real window is opened: the page is in a window of
//! GPUI's test platform.

use crate::fixture::StreamApply;

/// 40 turns and 1 MiB of text, and each snapshot of the stream replaces the
/// last item of the list and nothing else, through the end of a cycle and the
/// start of the next one (the last passage goes back to its first length).
#[test]
fn stream_apply_changes_only_the_last_item() {
    let mut fixture = StreamApply::new();
    let (items, bytes) = fixture.size();
    // A prompt, a row of tool calls and a passage in each of 40 turns.
    assert_eq!(items, 120);
    assert_eq!(bytes, 1 << 20);
    let first = fixture.spliced();
    for step in 1..=450 {
        fixture.step();
        assert_eq!(fixture.spliced(), first + step, "step {step}");
    }
    assert_eq!(fixture.size().0, 120);
}
