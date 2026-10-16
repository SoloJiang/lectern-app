# Lectern task runner. These are the only entry points agents should use
# (AGENTS.md). Linux runs everything that builds without GPUI; macOS runs the
# whole workspace.

set shell := ["bash", "-euo", "pipefail", "-c"]
# Evaluate variables only when a recipe uses them, so `just fmt` and
# `just --list` work even when Cargo.lock is out of date.
set lazy
# Pass recipe arguments as "$@" so arguments with spaces stay whole.
set positional-arguments

# Use sccache when it is installed and the caller has not set a wrapper of
# their own; it shares builds across worktrees. Never export an empty
# RUSTC_WRAPPER: Cargo reads that as "no wrapper" and drops a configured one.
sccache := `command -v sccache || true`
cargo := if env("RUSTC_WRAPPER", "") != "" { "cargo" } else if sccache != "" { "RUSTC_WRAPPER=sccache cargo" } else { "cargo" }

# Crates that build without GPUI (all of them on macOS).
packages := if os() == "macos" { "--workspace" } else { `python3 scripts/check_architecture.py --gpui-free-packages` }

default: check

# Format check, architecture rules, clippy with warnings as errors.
check:
    {{cargo}} fmt --all --check
    python3 scripts/check_architecture.py
    {{cargo}} clippy --locked {{packages}} --all-targets --all-features -- -D warnings

# Run tests with cargo-nextest. `-p` (also `-pNAME`), `--package` or
# `--workspace` in the arguments replace the default package set.
test *args:
    {{cargo}} nextest run --locked {{ if args =~ '(^|\s)(-p|--package|--workspace)' { "" } else { packages } }} --all-features --no-tests=pass "$@"

# Performance budget benchmarks.
bench:
    {{cargo}} bench --locked -p lectern-bench --all-features

# Licenses, sources and advisories.
deny:
    {{cargo}} deny --locked check

# Everything CI runs on this platform.
ci: check deny (test "--profile" "ci")
    {{ if os() == "macos" { cargo + " build --locked -p lectern-app" } else { "true" } }}
    {{ if os() == "macos" { "just bench" } else { "true" } }}

# Format the code.
fmt:
    {{cargo}} fmt --all
