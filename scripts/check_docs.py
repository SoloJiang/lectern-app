#!/usr/bin/env python3
"""Check that relative links in the repository's Markdown files resolve.

Every tracked *.md file is scanned. For each relative link `[text](target)`:
  - the target file or directory must exist;
  - a `#fragment` must match a heading anchor in the target Markdown file,
    using GitHub's slug rules (see `slug`).

Links inside fenced code blocks and inline code are ignored, as are absolute
URLs (http, https, mailto) and links to the repository root. Symlinked files
(crates/*/CLAUDE.md) are checked once, through the file they point to.

Exit status is 1 when any link is broken.
"""

import os
import re
import subprocess
import sys
import unicodedata

LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)|!\[[^\]]*\]\(([^)\s]+)\)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
FENCE = re.compile(r"^\s*(```|~~~)")
INLINE_CODE = re.compile(r"`[^`]*`")
EXTERNAL = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")


def slug(heading):
    """GitHub's heading anchor: lowercase, drop punctuation, spaces to '-'."""
    text = re.sub(r"<[^>]+>", "", heading)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = text.replace("`", "").lower()
    out = []
    for ch in text:
        if ch == " ":
            out.append("-")
        elif ch in "-_" or unicodedata.category(ch)[0] in "LNM":
            out.append(ch)
    return "".join(out)


def strip_code(lines):
    """Yield (line_number, text) outside fenced blocks, with inline code removed."""
    fenced = False
    for i, line in enumerate(lines, 1):
        if FENCE.match(line):
            fenced = not fenced
            continue
        if not fenced:
            yield i, INLINE_CODE.sub("", line)


def anchors(path, cache):
    if path not in cache:
        seen = {}
        found = set()
        with open(path, encoding="utf-8") as f:
            for _, line in strip_code(f.read().splitlines()):
                m = HEADING.match(line)
                if not m:
                    continue
                base = slug(m.group(2))
                n = seen.get(base, 0)
                seen[base] = n + 1
                found.add(base if n == 0 else f"{base}-{n}")
        cache[path] = found
    return cache[path]


def check(root, files):
    errors = []
    cache = {}
    done = set()
    for rel in files:
        path = os.path.realpath(os.path.join(root, rel))
        if path in done or not os.path.isfile(path):
            continue
        done.add(path)
        shown = os.path.relpath(path, root)
        with open(path, encoding="utf-8") as f:
            lines = f.read().splitlines()
        for lineno, line in strip_code(lines):
            for m in LINK.finditer(line):
                target = m.group(1) or m.group(2)
                if EXTERNAL.match(target) or target.startswith("/"):
                    continue
                file_part, _, fragment = target.partition("#")
                dest = os.path.normpath(os.path.join(os.path.dirname(path), file_part)) if file_part else path
                if not os.path.exists(dest):
                    errors.append(f"{shown}:{lineno}: {target}: file not found")
                    continue
                if fragment and dest.endswith(".md") and os.path.isfile(dest):
                    if fragment.lower() not in anchors(dest, cache):
                        errors.append(f"{shown}:{lineno}: {target}: no heading with anchor #{fragment}")
    return errors


def main():
    root = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], check=True, capture_output=True, text=True
    ).stdout.strip()
    files = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.md"], cwd=root, check=True, capture_output=True, text=True
    ).stdout.split("\0")
    errors = check(root, [f for f in files if f])
    if errors:
        print("Doc link check failed:")
        for e in errors:
            print(f"  - {e}")
        return 1
    print("Doc link check passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
