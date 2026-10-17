//! Process-level benchmarks: `app/*`, `plugin/*`, `lsp/*`, `baseline/*`
//! (docs/design/bench.md「进程级基准」). They launch `lectern` or the empty
//! GPUI window and run only on macOS; the launcher and the probe come with
//! bench steps 4 and 5. Until then every budget of this target is planned.

use std::process::ExitCode;

use lectern_bench::{Bench, Target};

const BENCHES: &[Bench] = &[];

fn main() -> ExitCode {
    lectern_bench::main(Target::Process, env!("CARGO_TARGET_TMPDIR"), BENCHES)
}
