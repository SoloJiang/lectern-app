//! `worktrees/*` (docs/design/worktrees.md「性能」): `worktrees/build_1000`,
//! `snapshot::build` over 1000 sessions in 17 worktrees, 8 overlaps and a
//! branch usage for each. The crate depends on GPUI, which only builds on
//! macOS in this workspace, so this module is compiled there alone; the
//! budget is `macos_only` and skipped elsewhere.

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_worktrees::fixture::fixture_1000;
use lectern_worktrees::snapshot::{Overlaps, RowUsage, build};

pub const BENCHES: &[Bench] = &[BUILD_1000];

/// `snapshot::build` of `fixture::fixture_1000`. In process, no window.
pub const BUILD_1000: Bench = Bench {
    id: "worktrees/build_1000",
    run: build_1000,
};

fn build_1000(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let fixture = fixture_1000();
    let built = build(&fixture.inputs());
    let snapshot = &built.snapshot;
    let rows: Vec<_> = snapshot
        .active_rows
        .iter()
        .chain(snapshot.idle_rows.iter())
        .collect();
    let sessions: u32 = rows.iter().map(|row| row.sessions).sum();
    let overlapping = rows
        .iter()
        .filter(|row| matches!(&row.overlaps, Overlaps::List(list) if list.len() == 8))
        .count();
    let with_usage = rows
        .iter()
        .filter(|row| matches!(row.usage, RowUsage::Branch(_)))
        .count();
    if rows.len() != 17 || sessions != 1000 || overlapping != 17 || with_usage != 17 {
        return Err(BenchError::new(format!(
            "the fixture should give 17 worktrees with 1000 sessions, 8 overlaps and a branch \
             usage each; it gave {} worktrees, {sessions} sessions, {overlapping} with 8 \
             overlaps and {with_usage} with a usage",
            rows.len()
        )));
    }
    let time = ctx.time(|| build(&fixture.inputs()));
    Ok(vec![Measured::new(
        "worktrees/build_1000",
        Value::Time(time),
    )])
}
