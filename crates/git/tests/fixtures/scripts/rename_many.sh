#!/usr/bin/env bash
# Fixture `rename_many`: 40 files renamed (new basenames) and edited a little,
# plus 5 exact renames. 40 x 40 = 1600 candidate pairs: within the default
# diff.renameLimit (1000, i.e. 1 000 000 pairs) and within 40 (1600 pairs),
# over 39 (1521 pairs), where only the exact renames remain.
# Generated with git version 2.43.0 (recorded in rename_many/expected/git-version).
# usage: rename_many.sh <scratch-dir>
name=rename_many
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
start "$@"

body() { # body FILE-NUMBER
  local i
  for i in $(seq 1 30); do printf 'module %02d: statement %d of the body\n' "$1" "$i"; done
}

mkdir -p "$work/src"
for n in $(seq 0 39); do
  body "$n" > "$work/src/f$(printf %02d "$n").rs"
done
for n in $(seq 0 4); do
  put "src/exact$n.txt" "exact rename $n\n"
done
g add -A
old=$(g write-tree)

rm -rf "$work/src"
mkdir -p "$work/lib"
for n in $(seq 0 39); do
  body "$n" | sed -e "s/statement 7 of/STATEMENT SEVEN of/" -e "s/statement 21 of/line 21 of/" \
    > "$work/lib/module_$(printf %02d "$n").rs"
done
for n in $(seq 0 4); do
  put "lib/moved$n.txt" "exact rename $n\n"
done
g add -A
new=$(g write-tree)

export_fixture "$old" "$new" renameLimit=39 renameLimit=40
