#!/usr/bin/env bash
# Fixture `reflog`: the HEAD reflog git writes for commits, a `checkout -b`
# (old = new), switching branches, a reset, an amend and a merge, at fixed
# times. Not a tree fixture (no `trees` file); see ../README.md「reflog 夹具」.
# Generated with git version 2.43.0 (recorded in reflog/expected/git-version).
# usage: reflog.sh <scratch-dir>
name=reflog
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

# Neither the user's nor the system's configuration (templates, default
# branch, identity) changes the reflog.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=Fixture GIT_AUTHOR_EMAIL=fixture@example.com
export GIT_COMMITTER_NAME=Fixture GIT_COMMITTER_EMAIL=fixture@example.com
start "$@"
GIT_OPTS+=(-c core.logAllRefUpdates=true -c merge.ff=false)
g symbolic-ref HEAD refs/heads/main

# at SECONDS COMMAND...: runs a git command whose reflog entry (the
# committer date) and commit dates are SECONDS.
at() {
  local when="@$1 +0100"
  shift
  GIT_AUTHOR_DATE="$when" GIT_COMMITTER_DATE="$when" g "$@"
}

t0=1700000000
put a.txt 'one\n'
put dir/b.txt 'b\n'
g add -A
at $t0 commit -q -m first
put a.txt 'one\ntwo\n'
g add -A
at $((t0 + 100)) commit -q -m second
at $((t0 + 200)) checkout -q -b feature
put dir/b.txt 'b on feature\n'
g add -A
at $((t0 + 300)) commit -q -m 'on feature'
at $((t0 + 400)) checkout -q main
put a.txt 'one\ntwo\nthree\n'
g add -A
at $((t0 + 450)) commit -q -m third
at $((t0 + 500)) reset -q --hard HEAD~1
put c.txt 'c\n'
g add -A
at $((t0 + 600)) commit -q --amend -m 'second, amended'
at $((t0 + 700)) merge -q -m 'merge feature' feature

out="$fixtures_dir/$name"
rm -rf "$out"
mkdir -p "$out/logs" "$out/expected"
cp "$work/.git/logs/HEAD" "$out/logs/HEAD"
git --version > "$out/expected/git-version"
# Newest first: `<seconds> <new OID> <message>`, git's own reading.
g log -g --date=unix --format='%gd %H %gs' HEAD |
  sed -e 's/^HEAD@{\([0-9]*\)} /\1 /' > "$out/expected/entries"
