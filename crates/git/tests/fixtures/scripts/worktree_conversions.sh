#!/usr/bin/env bash
# Fixture `worktree_conversions`: worktree files and the blob OIDs `git add`
# stores for them after git's built-in conversions (`text` / `eol` and
# `core.autocrlf`, `ident`, `working-tree-encoding`).
# Generated with git version 2.43.0 (recorded in
# worktree_conversions/expected/git-version).
# usage: worktree_conversions.sh <scratch-dir>
#
# Unlike the tree fixtures this one exports the worktree itself:
#
#   <name>/files/<path>                 the worktree files, byte for byte
#                                       (`.git*` stored as `dot-git*`)
#   <name>/expected/git-version         `git --version`
#   <name>/expected/<variant>/ls-files  `git ls-files -s -z` after `git add -A`
#
# <variant> is `autocrlf=<value>`: `git add` runs with -c core.autocrlf=<value>
# (the test writes the same value into the repository's configuration).
name=worktree_conversions
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
start "$@"

put .gitattributes '*.crlf text eol=crlf
*.lf text eol=lf
*.auto text=auto
*.text text
*.raw -text
*.id ident
*.u16 text working-tree-encoding=UTF-16LE
*.u16bom text working-tree-encoding=UTF-16
'
put plain.txt 'one\r\ntwo\r\n'
put mixed.txt 'one\r\ntwo\nthree\r\n'
put lone-cr.txt 'a\rb\r\n'
put lf.txt 'one\ntwo\n'
put a.crlf 'one\r\ntwo\r\n'
put b.crlf 'one\ntwo\n'
put a.lf 'one\r\ntwo\r\n'
put a.auto 'one\r\ntwo\r\n'
put bin.auto 'one\r\n\x00two\r\n'
put a.text 'one\r\ntwo\r\n'
put a.raw 'one\r\ntwo\r\n'
put x.id 'version $Id: 0123456789abcdef $\r\nplain $Id$\n'
put u.u16 'h\x00\xe9\x00l\x00l\x00o\x00\r\x00\n\x00w\x00o\x00r\x00l\x00d\x00\n\x00'
put v.u16bom '\xff\xfeh\x00\xe9\x00\r\x00\n\x00'

out="$fixtures_dir/$name"
rm -rf "$out"
mkdir -p "$out/files" "$out/expected"
(cd "$work" && find . -path ./.git -prune -o -type f -print | sed 's|^\./||' | sort) |
  while read -r path; do
    # `.gitattributes` would apply to the fixture in this repository.
    stored="${path/#.git/dot-git}"
    mkdir -p "$(dirname "$out/files/$stored")"
    cp "$work/$path" "$out/files/$stored"
  done
git --version > "$out/expected/git-version"
for autocrlf in false true input; do
  rm -f "$work/.git/index"
  dir="$out/expected/autocrlf=$autocrlf"
  mkdir -p "$dir"
  g -c "core.autocrlf=$autocrlf" add -A
  g ls-files -s -z > "$dir/ls-files"
done
