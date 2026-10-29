#!/usr/bin/env bash
# Prints the gpui-mcp endpoint directory of the automation build after
# checking it (docs/design/app.md「自动化构建的装配」). A Unix socket path holds
# at most 104 bytes on macOS, so it is a short directory in /tmp, not under
# the automation HOME. /tmp is shared: a symbolic link, another owner or a
# mode other than 0700 is an error. `--clean` empties it when this user runs
# no lectern-automation, so killed runs leave no stale endpoints.
set -euo pipefail
dir="/tmp/lectern-mcp-$(id -u)"
fail() {
    echo "error: $dir $1; remove it and run again" >&2
    exit 1
}
[[ -L $dir ]] && fail "is a symbolic link"
[[ -e $dir ]] || mkdir -m 0700 "$dir"
[[ -d $dir && -O $dir ]] || fail "is not a directory of this user"
[[ $(stat -f %Lp "$dir") == 700 ]] || fail "does not have mode 0700"
if [[ ${1:-} == --clean ]] && ! pgrep -u "$(id -u)" -f '/lectern-automation( |$)' >/dev/null; then
    find "$dir" -mindepth 1 -delete
fi
echo "$dir"
