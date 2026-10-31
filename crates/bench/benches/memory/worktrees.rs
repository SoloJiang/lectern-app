//! `worktrees/resident_bytes` (docs/design/worktrees.md「性能」): what the
//! composed snapshot of the 17-worktree, 1000-session fixture holds, in live
//! bytes (`CountingAlloc`): the rows, the sessions they list, the overlap
//! items and every `NodeId`. The fixture's inputs (the services' snapshots
//! the composing reads) are made before the count and stay alive, so their
//! bytes, which belong to `project`, `agents`, `review` and `usage`, are not
//! counted; the snapshot's own `Arc` clones of them are. The views' state
//! (the model's selection and open items, the list widgets) is built in a
//! window and is not in the count (docs/design/worktrees.md「与设计的差异」,
//! step 3). The first sample is a warm-up and is not reported. GPUI only
//! builds on macOS in this workspace, so this module is compiled there alone.

use std::sync::Arc;

use lectern_bench::{Bench, BenchError, CountingAlloc, Ctx, Measured, Value};
use lectern_worktrees::fixture::fixture_1000;
use lectern_worktrees::snapshot::{Built, build};

const SAMPLES: usize = 3;

pub const RESIDENT: Bench = Bench {
    id: "worktrees/resident_bytes",
    run: resident,
};

fn resident(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let fixture = fixture_1000();
    let mut samples = Vec::new();
    // Round 0 warms up lazily initialised state and is not reported.
    for round in 0..=ctx.scale(SAMPLES, 1) {
        let before = CountingAlloc::live();
        let Built {
            snapshot,
            pending,
            queries,
        } = build(&fixture.inputs());
        // The queries go to `UsageLink` and the pending sessions to the
        // plugin's one request; neither stays in the snapshot.
        drop((pending, queries));
        let snapshot = Arc::new(snapshot);
        let held = CountingAlloc::live().saturating_sub(before);
        if snapshot.active_rows.len() + snapshot.idle_rows.len() != 17 {
            return Err(BenchError::new("the fixture should give 17 worktrees"));
        }
        if round > 0 {
            samples.push(held as u64);
        }
        drop(snapshot);
    }
    Ok(vec![Measured::new(
        "worktrees/resident_bytes",
        Value::Bytes(samples),
    )])
}
