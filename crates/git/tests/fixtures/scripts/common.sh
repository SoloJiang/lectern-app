# Shared by the fixture scripts; sourced, not run. See ../README.md.
#
# Each script builds two trees with the real git in a scratch repository and
# exports them as a directory fixture next to this directory:
#
#   <name>/trees                old and new tree OIDs
#   <name>/old.tree, new.tree   `git ls-tree -r --full-tree` of each tree
#   <name>/blobs/<oid>          every blob either tree names
#   <name>/expected/git-version `git --version` that produced the outputs
#   <name>/expected/<variant>/numstat  git diff ... --numstat -z
#   <name>/expected/<variant>/raw      git diff ... --raw --no-abbrev -z
#
# <variant> is `default` or `renameLimit=<n>` (run with -c diff.renameLimit=<n>).
# No commits are made, so commit signing and identities don't matter. Every
# setting that changes these outputs is passed with -c, so the user's
# configuration doesn't either; --no-ext-diff --no-textconv keep external
# programs out.

set -euo pipefail

fixtures_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GIT_OPTS=(
  -c core.autocrlf=false -c core.safecrlf=false -c core.eol=lf
  -c core.fileMode=true -c core.symlinks=true -c core.quotePath=false
  -c diff.renames=true -c diff.renameLimit=1000 -c diff.algorithm=histogram
  -c commit.gpgsign=false
)

# usage: <script> <scratch-dir>; the scratch repository is <scratch-dir>/<name>.
start() {
  if [ $# -ne 1 ]; then
    echo "usage: $0 <scratch-dir>" >&2
    exit 2
  fi
  work="$1/$name"
  rm -rf "$work"
  mkdir -p "$1"
  git init -q "$work"
}

g() { git -C "$work" "${GIT_OPTS[@]}" "$@"; }

# Writes a file under the scratch worktree, creating directories.
put() {
  mkdir -p "$(dirname "$work/$1")"
  printf '%b' "$2" > "$work/$1"
}

# Records the worktree as a tree: prints its OID.
snapshot() {
  g add -A
  g write-tree
}

# export_fixture OLD NEW [VARIANT...]
export_fixture() {
  old=$1
  new=$2
  shift 2
  out="$fixtures_dir/$name"
  rm -rf "$out"
  mkdir -p "$out/blobs" "$out/expected"
  printf 'old %s\nnew %s\n' "$old" "$new" > "$out/trees"
  g ls-tree -r --full-tree "$old" > "$out/old.tree"
  g ls-tree -r --full-tree "$new" > "$out/new.tree"
  cat "$out/old.tree" "$out/new.tree" | awk '$2 == "blob" { print $3 }' | sort -u |
    while read -r oid; do g cat-file blob "$oid" > "$out/blobs/$oid"; done
  git --version > "$out/expected/git-version"
  record default
  for variant in "$@"; do record "$variant"; done
}

record() {
  local dir="$out/expected/$1"
  local extra=()
  if [ "$1" != default ]; then
    extra=(-c "diff.$1")
  fi
  mkdir -p "$dir"
  g "${extra[@]}" diff --no-ext-diff --no-textconv -M --histogram --numstat -z \
    "$old" "$new" > "$dir/numstat"
  g "${extra[@]}" diff --no-ext-diff --no-textconv -M --histogram --raw --no-abbrev -z \
    "$old" "$new" > "$dir/raw"
}
