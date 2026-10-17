//! Merges the benchmark targets' reports (docs/design/bench.md「报告」):
//! `lectern-bench-report merge [--require package] target/lectern-bench`.

use std::process::ExitCode;

fn main() -> ExitCode {
    lectern_bench::merge::main(std::env::args().skip(1))
}
