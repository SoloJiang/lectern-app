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
    cargo nextest run --locked "${selection[@]}" --all-features --no-tests=pass "$@"

# Doctests (nextest does not run them).
doctest:
    cargo test --locked --doc {{packages}} --all-features

# Performance budget benchmarks.
bench:
    cargo bench --locked -p lectern-bench --all-features

# Licenses, sources and advisories.
deny:
    cargo deny --locked check

# Everything CI runs on this platform.
ci: check deny (test "--profile" "ci") doctest
    {{ if os() == "macos" { "cargo build --locked -p lectern-app" } else { "true" } }}
    {{ if os() == "macos" { "just bench" } else { "true" } }}

# Format the code.
fmt:
    cargo fmt --all
