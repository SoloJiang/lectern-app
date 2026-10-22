#!/usr/bin/env bash
# Fixture `patch_id`: commits on one root, each with the trees git wrote and
# `git patch-id --stable` of its diff against the parent: hunk merging and
# context at the file's ends, a last line without a newline, CRLF, an
# insertion the indent heuristic slides, new, deleted, empty and executable
# files, symbolic links, mode changes, a file that becomes a symbolic link
# or a directory, submodules, paths git quotes, two changes that differ only
# in whitespace, one change listed in two file orders, and an empty commit.
# Not a tree fixture (`trees/` is a directory, not the `trees` file of two
# trees); see ../README.md「patch_id 夹具」.
# Generated with git version 2.43.0 (recorded in patch_id/expected/git-version).
# usage: patch_id.sh <scratch-dir>
name=patch_id
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=Fixture GIT_AUTHOR_EMAIL=fixture@example.com
export GIT_COMMITTER_NAME=Fixture GIT_COMMITTER_EMAIL=fixture@example.com
export GIT_AUTHOR_DATE='@1700000000 +0100' GIT_COMMITTER_DATE='@1700000000 +0100'
start "$@"
scratch="$1"

lines() { for i in $(seq -w 1 30); do printf 'l%s\n' "$i"; done; }

# The root.
lines > "$work/lines.txt"
put edge.txt 'first\nsecond\nthird\n'
put nonl.txt 'a\nb'
put addnl.txt 'x'
put crlf.txt 'one\r\ntwo\r\nthree\r\n'
put code.rs 'fn a() {\n    one();\n}\n\nfn c() {\n    three();\n}\n'
put mode.sh 'echo\n'
put modec.sh 'a\n'
put type.txt 'target\n'
put gone.txt 'bye\n'
put empty-gone.txt ''
put bin.dat 'bin\0ary\n'
put dirfile 'f\n'
put 'sp ace/é.txt' 'q\n'
put $'tab\there.txt' 'q\n'
put $'quo"te\\.txt' 'q\n'
put order/a/1.txt '1\n'
put order/b/2.txt '2\n'
put order/z.txt 'z\n'
put ws.txt 'call(a, b)\n'
g add -A
g commit -q -m root
c0=$(g rev-parse HEAD)

# from COMMIT: checks COMMIT out, untracked files removed.
# commit NAME PARENT: commits the worktree as NAME, recording PARENT's name.
declare -A commits parents
order=()
from() {
  g checkout -q -f --detach "$1"
  g clean -fdq
}
commit() {
  g add -A
  g commit -q --allow-empty -m "$1"
  commits[$1]=$(g rev-parse HEAD)
  parents[$1]=$2
  order+=("$1")
}
commits[root]=$c0
parents[root]=-
order+=(root)

from "$c0"
# Changes 6 lines apart share a hunk; 7 apart don't.
lines | sed -e 's/^l05$/L05/' -e 's/^l12$/L12/' -e 's/^l20$/L20/' -e 's/^l28$/L28/' > "$work/lines.txt"
put edge.txt 'FIRST\nsecond\nTHIRD\n'
put nonl.txt 'a\nB'
put addnl.txt 'x\n'
put crlf.txt 'one\r\n2\r\nthree\r\n'
commit modify root

from "$c0"
put code.rs 'fn a() {\n    one();\n}\n\nfn b() {\n    two();\n}\n\nfn c() {\n    three();\n}\n'
commit slide root

from "$c0"
put new.txt 'new\n'
put empty.txt ''
put tool.sh 'run\n'
chmod +x "$work/tool.sh"
ln -s target/path "$work/link"
rm "$work/gone.txt" "$work/empty-gone.txt"
commit add-delete root

from "$c0"
chmod +x "$work/mode.sh" "$work/modec.sh"
put modec.sh 'b\n'
commit mode root

from "$c0"
rm "$work/type.txt" "$work/dirfile"
ln -s elsewhere "$work/type.txt"
put dirfile/inner.txt 'inner\n'
commit type root

from "$c0"
g add -A
g update-index --add --cacheinfo "160000,1111111111111111111111111111111111111111,sub"
g commit -q -m submodule
commits[submodule]=$(g rev-parse HEAD)
parents[submodule]=root
order+=(submodule)
g update-index --cacheinfo "160000,2222222222222222222222222222222222222222,sub"
g commit -q -m submodule-moved
commits[submodule-moved]=$(g rev-parse HEAD)
parents[submodule-moved]=submodule
order+=(submodule-moved)

from "$c0"
put 'sp ace/é.txt' 'Q\n'
put $'tab\there.txt' 'Q\n'
put $'quo"te\\.txt' 'Q\n'
commit quoted root

from "$c0"
put ws.txt 'call(a,b)\n'
commit ws-a root
from "$c0"
put ws.txt 'call( a ,\tb )  \n'
commit ws-b root

from "$c0"
put order/a/1.txt 'one\n'
put order/b/2.txt 'two\n'
put order/z.txt 'zed\n'
commit order root

from "$c0"
put bin.dat 'bin\0ARY\n'
commit binary root

from "$c0"
commit empty root

out="$fixtures_dir/$name"
rm -rf "$out"
mkdir -p "$out/trees" "$out/blobs" "$out/expected"
git --version > "$out/expected/git-version"
printf 'order/z.txt\norder/b/*\norder/a/*\n' > "$scratch/orderfile"
# patch_id NAME [OPTIONS...]: git's stable patch ID of the commit against its
# parent with the given `git show` options, or `-` when git prints none.
patch_id() {
  local c=${commits[$1]}
  shift
  local id
  id=$(g -c core.quotePath=true show --format= --no-ext-diff --no-textconv --no-renames \
    --full-index "$@" "$c" | git patch-id --stable | cut -d' ' -f1)
  echo "${id:--}"
}
for n in "${order[@]}"; do
  c=${commits[$n]}
  p=${parents[$n]}
  tree=$(g rev-parse "$c^{tree}")
  echo "$n $tree $p" >> "$out/commits"
  if [ ! -e "$out/trees/$tree" ]; then
    g ls-tree -r -z --full-tree "$tree" > "$out/trees/$tree"
    g ls-tree -r -z --full-tree "$tree" | tr '\0' '\n' | awk '$2 == "blob" { print $3 }' |
      while read -r oid; do g cat-file blob "$oid" > "$out/blobs/$oid"; done
  fi
  # The root has no parent: `Repo::patch_id` gives None, whatever git says.
  [ "$p" = - ] && continue
  echo "$n $(patch_id "$n")" >> "$out/expected/patch-id"
done
echo "order -O $(patch_id order -O "$scratch/orderfile")" >> "$out/expected/patch-id"
