#!/usr/bin/env bash
# Fixture `renames`: exact and similar renames, git's tie-breaks, the basename
# pass (75 %) and the matrix (50 %), and what never pairs.
# Generated with git version 2.43.0 (recorded in renames/expected/git-version).
# usage: renames.sh <scratch-dir>
name=renames
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
start "$@"

lines() { # lines PREFIX FROM TO
  local i
  for i in $(seq "$2" "$3"); do printf '%s %d\n' "$1" "$i"; done
}

# Exact: identical content, the source with the same basename wins; empty
# files pair too (git's rename_empty is on for `git diff`).
put old/a.txt 'same\n'
put old/b.txt 'same\n'
put keep/c.txt 'same\n'
put old/empty1 ''
# Same basename, similar: 79 % pairs in the basename pass, 59 % only in the matrix.
lines "hi line" 1 10 > "$work/old/hi.txt"
lines "mid line" 1 10 > "$work/old/mid.txt"
# Different basenames, similar.
lines "other" 1 10 > "$work/old/q1"
# Exact rename with a mode change.
put old/script.sh '#!/bin/sh\necho script\n'
# Symbolic links pair only exactly; so do gitlinks.
ln -s some/target "$work/old/link"
ln -s first/target "$work/old/link2"
# Below 50 %: stays a deletion and an addition.
lines "below" 1 10 > "$work/old/below.txt"
# CRLF in the source, LF in the destination: spanhash skips the CR in text.
lines "crlf line" 1 12 | sed 's/$/\r/' > "$work/old/crlf.txt"
# Binary content is compared too (no CR skipping).
{ printf 'BIN\0'; lines "binary row" 1 20; } > "$work/old/blob.bin"
# A modified file is not a rename source (no copy detection).
lines "modified" 1 10 > "$work/keep/modified.txt"
g add -A
g update-index --add --cacheinfo 160000,5555555555555555555555555555555555555555,old/sub
old=$(g write-tree)

rm -rf "$work/old"
put new/b.txt 'same\n'
put new/z.txt 'same\n'
put new/empty2 ''
{ lines "hi line" 1 8; printf 'x\ny\n'; } > "$work/new/hi.txt"
{ lines "mid line" 1 6; lines "changed" 7 10; } > "$work/new/mid.txt"
lines "other" 1 9 > "$work/new/q2"
put new/script.sh '#!/bin/sh\necho script\n'
chmod +x "$work/new/script.sh"
ln -s some/target "$work/new/link"
ln -s second/target "$work/new/link2"
{ lines "below" 1 4; lines "fresh" 5 10; } > "$work/new/below2.txt"
lines "crlf line" 1 12 > "$work/new/lf.txt"
{ printf 'BIN\0'; lines "binary row" 1 19; } > "$work/new/blob2.bin"
lines "modified" 1 11 > "$work/keep/modified.txt"
lines "modified" 1 10 > "$work/new/copy.txt"
g add -A
g update-index --add --cacheinfo 160000,5555555555555555555555555555555555555555,new/sub
new=$(g write-tree)

export_fixture "$old" "$new" renameLimit=1
