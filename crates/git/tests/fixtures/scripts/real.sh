#!/usr/bin/env bash
# Fixture `real`: line diffs of real edits, to compare the histogram counts with
# git's. The files are this repository's own documents and code, each before
# and after one commit of its history; plus a synthetic file whose lines
# repeat more than 64 times, histogram's degenerate case.
# Generated with git version 2.43.0 (recorded in real/expected/git-version).
# usage: real.sh <scratch-dir>
name=real
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
start "$@"

src="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
# commit path: the file before (commit~) and after (commit).
edits=(
  "45219ab README.md"
  "4165839 docs/adr/0020.md"
  "07542c1 docs/design/interaction.md"
  "192746c scripts/check_architecture.py"
  "ab0e4d0 .github/workflows/ci.yml"
  "58cd778 crates/project/src/lib.rs"
)

repeated() { # repeated SEED
  local i
  for i in $(seq 1 200); do
    if [ $((i % 3)) -eq 0 ]; then printf '}\n'; elif [ $((i % 7)) -eq "$1" ]; then printf 'x = %d;\n' "$i"; else printf '\n'; fi
  done
}

for edit in "${edits[@]}"; do
  read -r commit path <<< "$edit"
  mkdir -p "$work/$(dirname "$path")"
  git -C "$src" show "$commit~:$path" > "$work/$path"
done
repeated 1 > "$work/repeated.txt"
g add -A
old=$(g write-tree)

for edit in "${edits[@]}"; do
  read -r commit path <<< "$edit"
  git -C "$src" show "$commit:$path" > "$work/$path"
done
repeated 2 > "$work/repeated.txt"
g add -A
new=$(g write-tree)

export_fixture "$old" "$new"
