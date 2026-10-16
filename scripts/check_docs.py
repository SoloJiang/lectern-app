#!/usr/bin/env python3
"""Check that relative links in the repository's Markdown files resolve.

Every tracked *.md file is scanned. For each relative link `[text](target)`:
  - the target file or directory must exist;
  - a `#fragment` must match a heading anchor in the target Markdown file,
    using GitHub's slug rules (see `slug`).

Inline links (including `<angle bracket>` destinations) and reference
definitions (`[label]: target`, not `[^1]:` footnotes) are checked;
percent-encoded paths and fragments are decoded first, and fragments match
case-sensitively. HTML comments and code spans of any backtick length are
skipped. Headings are ATX
(`## Title`) or setext (a line underlined with === or ---). Links inside fenced
code blocks and inline code are ignored, as are absolute URLs (http, https,
mailto) and links to the repository root. Symlinked files
(crates/*/CLAUDE.md) are checked once, through the file they point to.

Exit status is 1 when any link is broken.
"""

import os
import re
import subprocess
import sys
import unicodedata
from urllib.parse import unquote

LINK = re.compile(r"!?\[[^\]]*\]\(\s*(<[^>\n]*>|[^)\s]+)(?:\s+\"[^\"]*\")?\s*\)")
# Reference definitions; `[^1]:` footnotes are prose, not links.
REFERENCE = re.compile(r"^ {0,3}\[(?!\^)[^\]]+\]:\s*(<[^>\n]*>|\S+)")
HEADING = re.compile(r"^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$")
SETEXT = re.compile(r"^ {0,3}(=+|-+)\s*$")
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})(.*)$")
# A code span opens and closes with backtick runs of the same length.
INLINE_CODE = re.compile(r"(?<!`)(`+)(?!`).*?(?<!`)\1(?!`)")
COMMENT_OPEN = "<!--"
COMMENT_CLOSE = "-->"
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


def outside_fences(lines):
    """Yield (line_number, line) for lines outside fenced code blocks.

    A fence closes only on the same character repeated at least as many times
    as the opening, with nothing after it, as in CommonMark."""
    fence = None
    for i, line in enumerate(lines, 1):
        m = FENCE.match(line)
        if fence is None:
            if m and not (m.group(1)[0] == "`" and "`" in m.group(2)):
                fence = m.group(1)
                continue
            yield i, line
        elif m and m.group(1)[0] == fence[0] and len(m.group(1)) >= len(fence) and not m.group(2).strip():
            fence = None


def visible(lines):
    """Lines outside fenced blocks, with HTML comments (single or multi-line) removed."""
    in_comment = False
    for i, line in outside_fences(lines):
        out = []
        rest = line
        while rest:
            if in_comment:
                end = rest.find(COMMENT_CLOSE)
                if end < 0:
                    rest = ""
                else:
                    rest = rest[end + len(COMMENT_CLOSE):]
                    in_comment = False
            else:
                start = rest.find(COMMENT_OPEN)
                if start < 0:
                    out.append(rest)
                    rest = ""
                else:
                    out.append(rest[:start])
                    rest = rest[start + len(COMMENT_OPEN):]
                    in_comment = True
        yield i, "".join(out)


def strip_code(lines):
    """Visible lines with inline code spans removed."""
    for i, line in visible(lines):
        yield i, INLINE_CODE.sub("", line)


def headings(lines):
    """Heading texts in document order: ATX headings and setext headings."""
    out = []
    prev = None
    for _, line in visible(lines):
        m = HEADING.match(line)
        if m:
            out.append(m.group(2))
            prev = None
            continue
        if SETEXT.match(line) and prev and prev.strip():
            out.append(prev.strip())
            prev = None
            continue
        prev = line
    return out


def unique_slugs(texts):
    """GitHub's anchors for headings in order: a repeated slug gets the first
    free -N suffix, counting slugs that already end in -N."""
    seen = {}
    found = []
    for text in texts:
        base = slug(text)
        result = base
        while result in seen:
            seen[base] += 1
            result = f"{base}-{seen[base]}"
        seen[result] = 0
        found.append(result)
    return found


def anchors(path, cache):
    if path not in cache:
        with open(path, encoding="utf-8") as f:
            cache[path] = set(unique_slugs(headings(f.read().splitlines())))
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
            targets = [m.group(1) for m in LINK.finditer(line)]
            ref = REFERENCE.match(line)
            if ref:
                targets.append(ref.group(1))
            for target in targets:
                if target.startswith("<") and target.endswith(">"):
                    target = target[1:-1]
                if EXTERNAL.match(target) or target.startswith("/"):
                    continue
                file_part, _, fragment = target.partition("#")
                file_part, fragment = unquote(file_part), unquote(fragment)
                dest = os.path.normpath(os.path.join(os.path.dirname(path), file_part)) if file_part else path
                if not os.path.exists(dest):
                    errors.append(f"{shown}:{lineno}: {target}: file not found")
                    continue
                if fragment and dest.endswith(".md") and os.path.isfile(dest):
                    # Fragments match case-sensitively, like the browser does.
                    if fragment not in anchors(dest, cache):
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
