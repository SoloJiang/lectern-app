#!/usr/bin/env bash
# Regenerates every fixture. usage: generate-all.sh <scratch-dir>
set -euo pipefail
here="$(dirname "${BASH_SOURCE[0]}")"
for script in basic renames rename_many real worktree_conversions reflog ref_reflog patch_id; do
  bash "$here/$script.sh" "$1"
done
