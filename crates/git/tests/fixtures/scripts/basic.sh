#!/usr/bin/env bash
# Fixture `basic`: every kind of change between two trees except renames.
# Generated with git version 2.43.0 (recorded in basic/expected/git-version).
# usage: basic.sh <scratch-dir>
name=basic
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
start "$@"

lines() { # lines PREFIX COUNT
  local i
  for i in $(seq 1 "$2"); do printf '%s %d\n' "$1" "$i"; done
}

put .gitattributes '*.bin binary\n*.dat -diff\n'
lines "readme line" 30 > "$work/README.md"
put src/main.rs 'fn main() {\n    println!("hello");\n}\n\nfn helper() -> u32 {\n    1\n}\n'
put src/gone.rs 'pub fn gone() {}\n'
put run.sh '#!/bin/sh\necho run\n'
put tool.sh '#!/bin/sh\necho tool\n'
ln -s target-a "$work/link"
put conf 'a = 1\nb = 2\n'
ln -s elsewhere "$work/alias"
put data.bin 'plain text\nbut binary by attribute\n'
put notes.dat 'notes\nwithout diff\n'
put image.png '\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x01'
{ head -c 8500 /dev/zero | tr '\0' 'x'; printf '\n\0tail\n'; } > "$work/late-nul.txt"
put crlf.txt 'one\r\ntwo\r\nthree\r\n'
put noeol.txt 'a\nb'
put thing 'a file that becomes a directory\n'
put gen/schema.rs '// generated\npub const A: u32 = 1;\n'
# Gitlinks without a checkout: added after `git add -A`, which would drop them.
g add -A
g update-index --add --cacheinfo 160000,1111111111111111111111111111111111111111,sub
g update-index --add --cacheinfo 160000,2222222222222222222222222222222222222222,oldsub
old=$(g write-tree)

put .gitattributes '*.bin binary\n*.dat -diff\ngen/** linguist-generated\n'
lines "readme line" 30 | sed -e 's/^readme line 10$/readme line ten/' -e '/^readme line 20$/d' \
  > "$work/README.md"
printf 'readme line 31\n' >> "$work/README.md"
put src/main.rs 'fn main() {\n    println!("hello, world");\n}\n\nfn helper() -> u32 {\n    2\n}\n'
rm "$work/src/gone.rs"
put src/new.rs 'pub fn new() {}\n'
chmod +x "$work/run.sh"
put tool.sh '#!/bin/sh\necho tool v2\n'
chmod +x "$work/tool.sh"
rm "$work/link"
ln -s target-b "$work/link"
rm "$work/conf"
ln -s conf.d "$work/conf"
rm "$work/alias"
put alias 'now a file\n'
put data.bin 'plain text\nstill binary by attribute\n'
put notes.dat 'notes\nwithout diff, changed\n'
put image.png '\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x02'
{ head -c 8500 /dev/zero | tr '\0' 'x'; printf '\n\0tail changed\n'; } > "$work/late-nul.txt"
put crlf.txt 'one\r\nTWO\r\nthree\r\n'
put noeol.txt 'a\nb\nc'
rm "$work/thing"
put thing/inner.txt 'now a directory\n'
put gen/schema.rs '// generated\npub const A: u32 = 2;\n'
put docs/added.md '# Added\n'
put empty.txt ''
g add -A
g update-index --add --cacheinfo 160000,3333333333333333333333333333333333333333,sub
g update-index --add --cacheinfo 160000,4444444444444444444444444444444444444444,newsub
new=$(g write-tree)

export_fixture "$old" "$new"
