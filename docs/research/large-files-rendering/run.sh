#!/usr/bin/env bash
# Parse, slice, window, chunked, query, text and contention measurements for
# docs/research/large-files-rendering.md. Usage:
#   bash docs/research/large-files-rendering/run.sh <work-dir> <stages> [real-files...]
# <stages> is a comma-separated subset of parse,slice,window,chunked,query,text,contend
# or "all". <work-dir> receives the build, the synthetic fixtures and the
# results; real files (downloaded data, see the research doc) are measured
# after the synthetic ones.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mkdir -p "$1" && cd "$1" && pwd)"
stages=",$2,"
shift 2
[[ "$stages" == ",all," ]] && stages=",parse,slice,window,chunked,query,text,contend,"
cargo build --release --locked --manifest-path "$here/bench/Cargo.toml" --target-dir "$work/cargo-target"
bin="$work/cargo-target/release/large-files-rendering-bench"
fx="$work/fixtures"
out="$work/results"
mkdir -p "$out"
[[ -f "$fx/synthetic_64m.json" ]] || "$bin" gen "$fx" > "$out/gen.txt"

synthetic=()
for size in 1 4 16 64; do
    for ext in ts rs json; do synthetic+=("$fx/synthetic_${size}m.$ext"); done
done
synthetic+=("$fx/minified_1m.js" "$fx/minified_4m.js")
all=("${synthetic[@]}" "$@")

has() { [[ "$stages" == *",$1,"* ]]; }
has parse && "$bin" parse "${all[@]}" | tee "$out/parse.tsv"
has slice && "$bin" slice "${all[@]}" | tee "$out/slice.tsv"
has window && "$bin" window "${all[@]}" | tee "$out/window.tsv"
if has chunked; then
    "$bin" chunked 64 "${all[@]}" | tee "$out/chunked64.tsv"
    "$bin" chunked-tree 64 "${all[@]}" | tee "$out/chunked64tree.tsv"
    "$bin" chunked 256 "$fx/synthetic_4m.ts" "$fx/synthetic_4m.rs" "$fx/synthetic_4m.json" "$fx/minified_4m.js" | tee "$out/chunked256.tsv"
fi
has query && "$bin" query "${all[@]}" | tee "$out/query.tsv"
has text && "$bin" text "${all[@]}" | tee "$out/text.tsv"
if has contend; then "$bin" contend "$fx/synthetic_1m.ts" | tee "$out/contend.tsv"; fi
