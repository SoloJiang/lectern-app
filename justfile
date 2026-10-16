# Lectern task runner. These are the only entry points agents should use
# (AGENTS.md). Linux runs everything that builds without GPUI; macOS runs the
# whole workspace.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Use sccache when it is installed; it shares builds across worktrees.
export RUSTC_WRAPPER := `command -v sccache || true`

# Crates that build without GPUI (all of them on macOS).
packages := if os() == "macos" { "--workspace" } else { `python3 scripts/check_architecture.py --gpui-free-packages` }

default: check

# Format check, architecture rules, clippy with warnings as errors.
check:
    cargo fmt --all --check
    python3 scripts/check_architecture.py
    cargo clippy --locked {{packages}} --all-targets -- -D warnings

# Run tests with cargo-nextest.
test *args:
    cargo nextest run --locked {{packages}} --no-tests=pass {{args}}

# Performance budget benchmarks.
bench:
    cargo bench --locked -p lectern-bench

# Licenses, sources and advisories.
deny:
    cargo deny --locked check

# Everything CI runs on this platform.
ci: check deny test
    {{ if os() == "macos" { "cargo build --locked -p lectern-app" } else { "true" } }}

# Format the code.
fmt:
    cargo fmt --all
