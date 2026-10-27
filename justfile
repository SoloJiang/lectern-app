# Lectern task runner. These are the only entry points agents should use
# (AGENTS.md). Linux runs everything that builds without GPUI; macOS runs the
# whole workspace.

set shell := ["bash", "-euo", "pipefail", "-c"]
# Evaluate variables only when a recipe uses them, so `just fmt` and
# `just --list` work even when Cargo.lock is out of date.
set lazy
# Pass recipe arguments as "$@" so arguments with spaces stay whole.
set positional-arguments

# No compiler wrapper is set here, so a RUSTC_WRAPPER, CARGO_BUILD_RUSTC_WRAPPER
# or build.rustc-wrapper you configured is always respected. To share builds
# across worktrees, set `build.rustc-wrapper = "sccache"` in ~/.cargo/config.toml.

# Crates that build without GPUI (all of them on macOS).
packages := if os() == "macos" { "--workspace" } else { `python3 scripts/check_architecture.py --gpui-free-packages` }

default: check

# Format check, architecture rules, doc links, clippy with warnings as errors.
check:
    cargo fmt --all --check
    python3 scripts/test_check_architecture.py
    python3 scripts/check_architecture.py
    cargo run -q --locked -p lectern-doc-check
    cargo clippy --locked {{packages}} --all-targets --all-features -- -D warnings

# Run tests with cargo-nextest. `-p` (also `-pNAME`), `--package` or
# `--workspace` (or its alias `--all`) before any `--` replace the default package set; arguments
# after `--` go to the test binaries and are never treated as selectors.
test *args:
    #!/usr/bin/env bash
    set -euo pipefail
    selection=({{packages}})
    for arg in "$@"; do
        [[ $arg == -- ]] && break
        if [[ $arg == -p* || $arg == --package || $arg == --package=* || $arg == --workspace || $arg == --all ]]; then
            selection=()
            break
        fi
    done
    # bash 3.2 (macOS /bin/bash) treats an empty "${selection[@]}" as unbound under `set -u`.
    cargo nextest run --locked ${selection[@]+"${selection[@]}"} --all-features --no-tests=pass "$@"

# Doctests (nextest does not run them).
doctest:
    cargo test --locked --doc {{packages}} --all-features

# Performance budget benchmarks: gated on Apple silicon macOS, a smoke run
# elsewhere. `--measure` samples on Linux too (reported, not gated); other
# arguments filter IDs, e.g. `just bench agents/`. On macOS,
# `--shard <name>` runs one CI shard (code-intel, process or rest) and writes
# its reports without merging them: CI merges the shards' reports.
# See docs/design/bench.md.
bench *args:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -f target/lectern-bench/report-*.json target/lectern-bench/report.json
    report=(cargo run -q --locked -p lectern-bench --bin lectern-bench-report --)
    if [[ "$(uname -s)" == Darwin ]]; then
        if [[ "${1-}" == --measure ]]; then shift; fi
        if [[ "${1-}" == --shard ]]; then
            shard="${2-}"
            if [[ -z "$shard" ]]; then
                echo "just bench: --shard needs a shard name" >&2
                exit 2
            fi
            shift 2
            # The shard's benchmark targets, one per line; an unknown name
            # fails here, before anything is built.
            targets="$("${report[@]}" shard "$shard")"
            bench_args=()
            build_process=false
            for target in $targets; do
                bench_args+=(--bench "$target")
                if [[ "$target" == process ]]; then build_process=true; fi
            done
            if [[ "$build_process" == true ]]; then
                # The empty GPUI window of the baseline/* benchmarks.
                cargo build --release --locked -p lectern-bench --all-features --example empty_window
                # The measurement build of lectern (the release build plus the
                # probe) of the app/* and plugin/* benchmarks.
                cargo build --release --locked -p lectern-app --features bench-probe
                export LECTERN_BENCH_BASELINE="$PWD/target/release/examples/empty_window"
                export LECTERN_BENCH_APP="$PWD/target/release/lectern"
            fi
            cargo bench --locked --no-fail-fast -p lectern-bench --all-features "${bench_args[@]}" -- --shard "$shard" "$@" || true
            # A shard does not judge: the merge does. It succeeds once its
            # reports are written; a target that failed to build or crashed
            # leaves none.
            for target in $targets; do
                if [[ ! -f "target/lectern-bench/report-$target.json" ]]; then
                    echo "just bench: no report-$target.json: the $target benchmark target failed to build or crashed" >&2
                    exit 2
                fi
            done
            exit 0
        fi
        # The empty GPUI window of the baseline/* benchmarks.
        cargo build --release --locked -p lectern-bench --all-features --example empty_window
        # The measurement build of lectern (the release build plus the probe)
        # of the app/* and plugin/* benchmarks.
        cargo build --release --locked -p lectern-app --features bench-probe
        export LECTERN_BENCH_BASELINE="$PWD/target/release/examples/empty_window"
        export LECTERN_BENCH_APP="$PWD/target/release/lectern"
        cargo bench --locked --no-fail-fast -p lectern-bench --all-features -- "$@" || true
    elif [[ "${1-}" == --shard ]]; then
        echo "just bench: --shard runs only on macOS" >&2
        exit 2
    elif [[ "${1-}" == --measure ]]; then
        shift
        cargo bench --locked --no-fail-fast -p lectern-bench --all-features -- "$@" || true
    else
        cargo test --locked --no-fail-fast -p lectern-bench --all-features --benches -- "$@" || true
    fi
    # The merged report decides the result: a target that failed to build or crashed leaves no report.
    "${report[@]}" merge target/lectern-bench

# A UI probe session runs in the background (docs/design/ui_probe.md): a debug
# build with the `ui-probe` feature, a temporary HOME and a private probe
# directory, so the real HOME and the agents' session directories are not
# read. Read the window with
# `cargo run -p lectern-ui-probe --bin lectern-probe -- ui_tree`, and end the
# session with `lectern-probe quit`.

# Start a UI probe session; prints the export line and the pid.
probe:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --locked -p lectern-app --features ui-probe
    bin="$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/debug/lectern"
    # mktemp -d makes a directory only this user can use (mode 0700).
    dir="$(mktemp -d "${TMPDIR:-/tmp}/lectern-probe.XXXXXX")"
    mkdir "$dir/home"
    HOME="$dir/home" LECTERN_UI_PROBE_DIR="$dir" \
        nohup env -u CLAUDE_CONFIG_DIR -u CODEX_HOME -u LECTERN_LOG "$bin" \
        >"$dir/lectern.out" 2>&1 </dev/null &
    echo "export LECTERN_UI_PROBE_DIR=$dir"
    echo "pid $!"

# Licenses, sources and advisories.
deny:
    cargo deny --locked check

# Everything CI runs on this platform.
ci: check deny (test "--profile" "ci") doctest
    {{ if os() == "macos" { "cargo build --locked -p lectern-app" } else { "true" } }}
    just bench

# Format the code.
fmt:
    cargo fmt --all
