#!/usr/bin/env bash
# Installs Zig 0.15.2, the version libghostty-vt-sys builds Ghostty with
# (docs/design/terminal.md「依赖」), from the official release after checking
# its SHA-256. Usage: scripts/install_zig.sh <directory>. The directory must
# not exist yet, or hold this Zig already; zig is then at <directory>/zig.
set -euo pipefail

version=0.15.2
dir="${1:?usage: scripts/install_zig.sh <directory>}"

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target=aarch64-macos sha=3cc2bab367e185cdfb27501c4b30b1b0653c28d9f73df8dc91488e66ece5fa6b ;;
    Linux-x86_64) target=x86_64-linux sha=02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239 ;;
    *) echo "install_zig: no checksum for $(uname -s)-$(uname -m); install Zig $version from https://ziglang.org/download/" >&2; exit 1 ;;
esac

if [[ -x "$dir/zig" && "$("$dir/zig" version)" == "$version" ]]; then
    exit 0
fi
if [[ -e "$dir" ]]; then
    echo "install_zig: $dir exists and does not hold Zig $version" >&2
    exit 1
fi

name="zig-$target-$version"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl --fail --silent --show-error --location --retry 3 \
    -o "$tmp/$name.tar.xz" "https://ziglang.org/download/$version/$name.tar.xz"
echo "$sha  $tmp/$name.tar.xz" | shasum -a 256 -c - >&2
tar -xJf "$tmp/$name.tar.xz" -C "$tmp"
mkdir -p "$(dirname "$dir")"
mv "$tmp/$name" "$dir"
