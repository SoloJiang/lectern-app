#!/usr/bin/env bash
# Fixture `ref_reflog`: the reflogs git writes under `logs/refs/` for a
# branch created, committed on, renamed and reset with `branch -f`, a branch
# created with `checkout -b`, a deleted branch, remote-tracking branches
# updated by `fetch`, and `refs/stash` through pushes and drops, at fixed
# times. Each step is a snapshot of `logs/refs/` and git's own reading of
# every reflog in it; see ../README.md「ref_reflog 夹具」.
# Generated with git version 2.43.0 (recorded in ref_reflog/expected/git-version).
# usage: ref_reflog.sh <scratch-dir>
name=ref_reflog
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

# Neither the user's nor the system's configuration (templates, default
# branch, identity) changes the reflogs.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=Fixture GIT_AUTHOR_EMAIL=fixture@example.com
export GIT_COMMITTER_NAME=Fixture GIT_COMMITTER_EMAIL=fixture@example.com
start "$@"
GIT_OPTS+=(-c core.logAllRefUpdates=true)
g symbolic-ref HEAD refs/heads/main

# at SECONDS COMMAND...: runs a git command whose reflog entries (the
# committer date) and commit dates are SECONDS.
at() {
  local when="@$1 +0100"
  shift
  GIT_AUTHOR_DATE="$when" GIT_COMMITTER_DATE="$when" g "$@"
}

out="$fixtures_dir/$name"
rm -rf "$out"
mkdir -p "$out/expected"
git --version > "$out/expected/git-version"

# snapshot STEP PREFIX ABSENT...: copies the reflogs under `logs/PREFIX`
# (a directory or one file) into STEP/logs/, records `git log -g` of each
# (newest first: `<seconds> <new OID> <message>`, the `<ref>@{<seconds>} `
# prefix cut to `<seconds> `) as STEP/expected/<ref>, and lists the ABSENT
# references, which have no reflog file, in STEP/absent.
snapshot() {
  local step="$out/$1" prefix=$2
  shift 2
  mkdir -p "$step"
  (cd "$work/.git/logs" && { find "$prefix" -type f 2>/dev/null || true; } | sort) |
    while read -r ref; do
      mkdir -p "$(dirname "$step/logs/$ref")" "$(dirname "$step/expected/$ref")"
      cp "$work/.git/logs/$ref" "$step/logs/$ref"
      g log -g --date=unix --format='%gd %H %gs' "$ref" |
        sed -e 's/^[^ ]*@{\([0-9]*\)} /\1 /' > "$step/expected/$ref"
    done
  : > "$step/absent"
  for ref in "$@"; do echo "$ref" >> "$step/absent"; done
}

t0=1700000000
put a.txt 'one\n'
g add -A
at $t0 commit -q -m first
at $((t0 + 100)) branch feat
at $((t0 + 200)) checkout -q feat
put a.txt 'one\nfeat\n'
g add -A
at $((t0 + 300)) commit -q -m 'on feat'
# Renaming moves the reflog to the new name and appends the rename.
at $((t0 + 400)) branch -m feat topic
at $((t0 + 500)) checkout -q main
put b.txt 'b\n'
g add -A
at $((t0 + 600)) commit -q -m second
# `branch -f` drops topic's own commit: the record is `branch: Reset to …`.
at $((t0 + 700)) branch -f topic main
at $((t0 + 800)) checkout -q -b other
at $((t0 + 900)) checkout -q main
# A deleted branch has no reflog file.
at $((t0 + 1000)) branch gone
at $((t0 + 1100)) branch -D -q gone
# Remote-tracking branches, fetched from this repository itself (`.`, so
# no scratch path ends up in a reflog message).
g remote add origin .
at $((t0 + 1200)) fetch -q origin
put b.txt 'b\nthird\n'
g add -A
at $((t0 + 1300)) commit -q -m third
at $((t0 + 1400)) fetch -q origin
snapshot branches refs refs/heads/feat refs/heads/gone refs/stash

# Three stash entries.
put a.txt 'one\nstash 1\n'
at $((t0 + 2000)) stash push -q -m 'stash one'
put a.txt 'one\nstash 2\n'
at $((t0 + 2100)) stash push -q -m 'stash two'
put a.txt 'one\nstash 3\n'
at $((t0 + 2200)) stash push -q -m 'stash three'
snapshot stash refs/stash
# Dropping the middle entry rewrites the stash reflog without it.
at $((t0 + 2300)) stash drop -q 'stash@{1}'
snapshot stash-drop refs/stash
# Dropping the last entries deletes `refs/stash` and its reflog.
at $((t0 + 2400)) stash drop -q
at $((t0 + 2500)) stash drop -q
snapshot stash-cleared refs/stash refs/stash
