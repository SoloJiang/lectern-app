//! `project_home/*` (docs/design/project_home.md「性能」):
//! `project_home/build_1000`, `snapshot::build` over 1000 sessions in 17
//! worktrees, 50 overview entries and 2000 sub-projects. The crate depends on
//! GPUI, which only builds on macOS in this workspace, so this module is
//! compiled there alone; the budget is `macos_only` and skipped elsewhere.

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_project_home::fixture::{BuildFixture, OVERVIEW, PACKAGES, SESSIONS};
use lectern_project_home::snapshot::build;

pub const BENCHES: &[Bench] = &[BUILD_1000];

/// `snapshot::build` of `fixture::BuildFixture`. In process, no window.
pub const BUILD_1000: Bench = Bench {
    id: "project_home/build_1000",
    run: build_1000,
};

fn build_1000(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let fixture = BuildFixture::new();
    let built = build(&fixture.inputs());
    let main = &built.snapshot.main;
    let sidebar = &built.snapshot.sidebar;
    if sidebar.session_rows as usize != SESSIONS
        || sidebar.packages.len() != PACKAGES + 1
        || main.needs_review.len() + main.done.len() != OVERVIEW
    {
        return Err(BenchError::new(format!(
            "the fixture should give {SESSIONS} session rows, {} packages and {OVERVIEW} cards; \
             it gave {}, {} and {}",
            PACKAGES + 1,
            sidebar.session_rows,
            sidebar.packages.len(),
            main.needs_review.len() + main.done.len()
        )));
    }
    let time = ctx.time(|| build(&fixture.inputs()));
    Ok(vec![Measured::new(
        "project_home/build_1000",
        Value::Time(time),
    )])
}
