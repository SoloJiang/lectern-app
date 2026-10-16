//! Checks that relative links in Markdown files resolve.
//!
//! Every link the CommonMark parser reports (inline, reference-style, images)
//! is checked when its destination is relative:
//!
//! - the target file or directory must exist and stay inside the repository;
//! - a `#fragment` on a Markdown target must equal one of its heading anchors,
//!   generated with GitHub's rules (see [`slug`]) and compared case-sensitively.
//!
//! `href` and `src` attributes in raw HTML count as links too; the HTML is
//! tokenized per the HTML standard. Query strings are dropped and
//! percent-encoding is decoded before the path is resolved. Code blocks, code
//! spans, HTML comments and `script` / `style` bodies contain no links, so they
//! are never checked.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use html5gum::{DefaultEmitter, Token, Tokenizer};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

mod slug_table;

/// A link that does not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenLink {
    /// File containing the link, relative to the repository root.
    pub file: PathBuf,
    /// 1-based line of the link.
    pub line: usize,
    /// The destination as written.
    pub target: String,
    pub reason: String,
}

impl std::fmt::Display for BrokenLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: {}: {}",
            self.file.display(),
            self.line,
            self.target,
            self.reason
        )
    }
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
}

/// GitHub's heading anchor (github-slugger 2.0.0): lowercase, drop every
/// character in [`slug_table::REMOVED`], turn spaces into `-`.
pub fn slug(text: &str) -> String {
    // Lowercase the whole string: some letters (e.g. final sigma) depend on
    // their neighbours.
    text.to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if is_removed(c) => None,
            c => Some(c),
        })
        .collect()
}

fn is_removed(c: char) -> bool {
    slug_table::REMOVED
        .binary_search_by(|&(lo, hi)| {
            if hi < c {
                std::cmp::Ordering::Less
            } else if lo > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Anchors for headings in document order. A repeated slug takes the first
/// free `-N` suffix, counting slugs that already end in `-N` (github-slugger).
pub fn unique_slugs<I: IntoIterator<Item = String>>(texts: I) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for text in texts {
        let base = slug(&text);
        let mut result = base.clone();
        while seen.contains_key(&result) {
            let n = seen.get_mut(&base).map_or(1, |n| {
                *n += 1;
                *n
            });
            result = format!("{base}-{n}");
        }
        seen.insert(result.clone(), 0);
        out.push(result);
    }
    out
}

/// Rendered text of every heading, in order: text, inline code and link text
/// count; markup and link destinations do not.
pub fn headings(markdown: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for event in Parser::new_ext(markdown, options()) {
        match event {
            Event::Start(Tag::Heading { .. }) => current = Some(String::new()),
            Event::End(TagEnd::Heading(_)) => out.extend(current.take()),
            Event::Text(t) | Event::Code(t) => {
                if let Some(h) = current.as_mut() {
                    h.push_str(&t);
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether `path` has a Markdown extension (`.md` or `.markdown`, any case).
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

/// `href` and `src` values in a piece of raw HTML, with their byte offsets
/// in `html`. html5gum tokenizes it as the HTML standard does: character
/// references are decoded, comments are skipped, and the bodies of `script`,
/// `style` and other raw-text elements are not read as tags.
fn html_destinations(html: &str) -> Vec<(usize, String)> {
    html_scan(html, None).0
}

/// Elements whose content the HTML tokenizer reads as text, not tags.
const RAW_TEXT: [&[u8]; 9] = [
    b"script",
    b"style",
    b"textarea",
    b"title",
    b"xmp",
    b"iframe",
    b"noembed",
    b"noframes",
    b"plaintext",
];

/// Byte offset of the end tag that closes the raw-text element `name` in
/// `html`: `</name` (any case) followed by whitespace, `/`, `>` or the end of
/// the input, as the HTML tokenizer requires. `</scriptx>` does not close
/// `script`.
fn raw_text_end(html: &str, name: &[u8]) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut from = 0;
    while let Some(i) = html[from..].find("</").map(|i| from + i) {
        let after = i + 2 + name.len();
        if bytes.len() >= after
            && bytes[i + 2..after].eq_ignore_ascii_case(name)
            && bytes
                .get(after)
                .is_none_or(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
        {
            return Some(i);
        }
        from = i + 2;
    }
    None
}

/// Like [`html_destinations`], starting inside the raw-text element `open`
/// (left open by an earlier piece of inline HTML) and returning the raw-text
/// element still open at the end.
fn html_scan(html: &str, open: Option<Vec<u8>>) -> (Vec<(usize, String)>, Option<Vec<u8>>) {
    let mut open = open;
    let html = match &open {
        // Inside an element opened by an earlier piece: skip to its end tag.
        // `plaintext` never ends: everything after it is text.
        Some(name) if name.as_slice() == b"plaintext" => return (Vec::new(), open),
        Some(name) => match raw_text_end(html, name) {
            Some(i) => {
                open = None;
                &html[i..]
            }
            None => return (Vec::new(), open),
        },
        None => html,
    };
    let mut emitter = DefaultEmitter::<usize>::new_with_span();
    emitter.naively_switch_states(true);
    let mut out = Vec::new();
    for token in Tokenizer::new_with_emitter(html, emitter) {
        let tag = match token {
            Ok(Token::StartTag(tag)) => tag,
            Ok(Token::EndTag(end)) => {
                if open.as_deref() == Some(&end.name[..]) {
                    open = None;
                }
                continue;
            }
            _ => continue,
        };
        if RAW_TEXT.contains(&&tag.name[..]) && !tag.self_closing {
            open = Some(tag.name.to_vec());
        }
        for name in [&b"href"[..], b"src"] {
            if let Some(value) = tag.attributes.get(name)
                && !value.value.is_empty()
            {
                // URL parsing strips leading and trailing C0 controls and
                // spaces, and removes tabs and newlines anywhere.
                // A backslash is a slash for web URLs, so `\\host\x` is a
                // network-path reference like `//host/x`.
                let url: String = String::from_utf8_lossy(&value.value)
                    .trim_matches(|c: char| c <= ' ')
                    .chars()
                    .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
                    .map(|c| if c == '\\' { '/' } else { c })
                    .collect();
                if !url.is_empty() {
                    out.push((tag.span.start, url));
                }
            }
        }
    }
    (out, open)
}

/// Destinations of all links and images, with their byte offsets: Markdown
/// links and `href` / `src` attributes in raw HTML.
pub fn links(markdown: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    // An HTML block arrives one line per event. Tokenize the whole block, so a
    // tag or comment spanning lines is read as one; `pieces` maps offsets in
    // `block` back to the source.
    let mut block: Option<(String, Vec<(usize, usize)>)> = None;
    // A raw-text element opened by inline HTML (`<script>` in a paragraph)
    // stays open across the following inline events until its end tag.
    let mut inline_open: Option<Vec<u8>> = None;
    for (event, range) in Parser::new_ext(markdown, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                out.push((range.start, dest_url.into_string()));
            }
            Event::Start(Tag::HtmlBlock) => block = Some(Default::default()),
            Event::Html(html) => match block.as_mut() {
                Some((text, pieces)) => {
                    pieces.push((text.len(), range.start));
                    text.push_str(&html);
                }
                None => out.extend(
                    html_destinations(&html)
                        .into_iter()
                        .map(|(at, d)| (range.start + at, d)),
                ),
            },
            Event::End(TagEnd::HtmlBlock) => {
                let Some((text, pieces)) = block.take() else {
                    continue;
                };
                for (at, dest) in html_destinations(&text) {
                    let i = pieces.partition_point(|&(start, _)| start <= at);
                    let (start, source) = pieces[i.saturating_sub(1)];
                    out.push((source + at - start, dest));
                }
            }
            Event::InlineHtml(html) => {
                let (found, open) = html_scan(&html, inline_open.take());
                inline_open = open;
                out.extend(found.into_iter().map(|(at, d)| (range.start + at, d)));
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::TableCell) => {
                inline_open = None;
            }
            _ => {}
        }
    }
    out
}

fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| (b as char).to_digit(16);
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// A path from raw bytes: on Unix any bytes are a valid file name.
#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Whether resolving `path` passes through a symlink whose target is
/// absolute. Such a link only works on the machine that created it.
fn has_absolute_symlink(path: &Path) -> bool {
    let mut todo: Vec<PathBuf> = path
        .components()
        .rev()
        .map(|c| PathBuf::from(c.as_os_str()))
        .collect();
    let mut current = PathBuf::new();
    let mut hops = 0;
    while let Some(part) = todo.pop() {
        match part.components().next() {
            Some(Component::ParentDir) => {
                current.pop();
            }
            Some(Component::CurDir) | None => {}
            Some(_) => {
                current.push(&part);
                if let Ok(target) = fs::read_link(&current) {
                    if target.is_absolute() {
                        return true;
                    }
                    hops += 1;
                    if hops > 40 {
                        return false;
                    }
                    current.pop();
                    todo.extend(
                        target
                            .components()
                            .rev()
                            .map(|c| PathBuf::from(c.as_os_str())),
                    );
                }
            }
        }
    }
    false
}

fn is_external(target: &str) -> bool {
    // A scheme is letters, digits, `+`, `-`, `.` before the first `:`, and
    // comes before any `/`, `?` or `#`.
    match target.find(':') {
        Some(i) => {
            let scheme = &target[..i];
            !scheme.is_empty()
                && scheme
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        }
        None => false,
    }
}

/// Lexically normalize `path` (no filesystem access), so `..` can be checked
/// against the repository root.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Check the given Markdown files (paths relative to `root`).
pub fn check(root: &Path, files: &[PathBuf]) -> Vec<BrokenLink> {
    check_tracked(root, files, None)
}

/// Like [`check`], and when `tracked` is given (every path git tracks,
/// relative to `root`), a destination must also be tracked: a file in the
/// list, or a directory containing one. Ignored and untracked files (build
/// output under `target/`, local notes) do not exist on GitHub.
pub fn check_tracked(
    root: &Path,
    files: &[PathBuf],
    tracked: Option<&[PathBuf]>,
) -> Vec<BrokenLink> {
    let tracked: Option<HashSet<PathBuf>> = tracked.map(|list| {
        let mut set = HashSet::new();
        for path in list {
            let mut p = path.clone();
            set.insert(p.clone());
            while p.pop() && !p.as_os_str().is_empty() {
                set.insert(p.clone());
            }
        }
        set
    });
    // Work with canonical paths throughout, so a symlinked file (crates/*/CLAUDE.md
    // -> AGENTS.md) resolves its links from where its target lives.
    let root = fs::canonicalize(root).unwrap_or_else(|_| {
        normalize(&std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf()))
    });
    let mut anchors: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    let mut done = HashSet::new();
    let mut errors = Vec::new();
    for rel in files {
        let joined = root.join(rel);
        if has_absolute_symlink(&joined) {
            errors.push(BrokenLink {
                file: rel.clone(),
                line: 1,
                target: rel.display().to_string(),
                reason:
                    "file is a symlink with an absolute target, which breaks in other checkouts"
                        .into(),
            });
            continue;
        }
        let path = fs::canonicalize(&joined).unwrap_or(joined);
        // A tracked Markdown symlink pointing outside the repository is not
        // read: its content would come from the machine, not the commit.
        if !path.starts_with(&root) {
            errors.push(BrokenLink {
                file: rel.clone(),
                line: 1,
                target: path.display().to_string(),
                reason: "file is a symlink that points outside the repository".into(),
            });
            continue;
        }
        // Each file once, whichever name reached it first.
        if !done.insert(path.clone()) {
            continue;
        }
        let rel = path
            .strip_prefix(&root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| rel.clone());
        let input_error = |reason: String| BrokenLink {
            file: rel.clone(),
            line: 1,
            target: rel.display().to_string(),
            reason,
        };
        // A tracked Markdown symlink must point at a tracked file, or it is
        // broken in a fresh checkout.
        if let Some(set) = &tracked
            && !set.contains(&rel)
        {
            errors.push(input_error(
                "file is a symlink to a file not tracked by git".into(),
            ));
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            // Deleted in the working tree but not yet committed: nothing to check.
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && !fs::symlink_metadata(&path).is_ok_and(|m| m.is_symlink()) =>
            {
                continue;
            }
            Err(e) => {
                errors.push(input_error(format!("cannot read file: {e}")));
                continue;
            }
        };
        let dir = path.parent().unwrap_or(&root).to_path_buf();
        for (offset, target) in links(&text) {
            if is_external(&target) || target.starts_with('/') {
                continue;
            }
            let line = text[..offset].matches('\n').count() + 1;
            let broken = |reason: String| BrokenLink {
                file: rel.clone(),
                line,
                target: target.clone(),
                reason,
            };
            let (without_fragment, fragment) = match target.split_once('#') {
                Some((p, f)) => (
                    p,
                    Some(String::from_utf8_lossy(&percent_decode(f)).into_owned()),
                ),
                None => (target.as_str(), None),
            };
            let file_part = path_from_bytes(&percent_decode(
                without_fragment.split('?').next().unwrap_or(""),
            ));
            let dest = if file_part.as_os_str().is_empty() {
                path.clone()
            } else {
                normalize(&dir.join(&file_part))
            };
            if !dest.starts_with(&root) {
                errors.push(broken("points outside the repository".into()));
                continue;
            }
            if !dest.exists() {
                errors.push(broken("file not found".into()));
                continue;
            }
            if has_absolute_symlink(&dest) {
                errors.push(broken(
                    "goes through a symlink with an absolute target, which breaks in other checkouts"
                        .into(),
                ));
                continue;
            }
            if let Some(set) = &tracked
                && let Ok(rel_dest) = dest.strip_prefix(&root)
                && !rel_dest.as_os_str().is_empty()
                && !set.contains(rel_dest)
            {
                errors.push(broken("not tracked by git".into()));
                continue;
            }
            // A symlink inside the repository may point outside it: check the
            // real target too, and never read a file outside the repository.
            let dest = match fs::canonicalize(&dest) {
                Ok(real) if real.starts_with(&root) => real,
                _ => {
                    errors.push(broken("points outside the repository".into()));
                    continue;
                }
            };
            // The symlink may be tracked while its target is not.
            if let Some(set) = &tracked
                && let Ok(rel_dest) = dest.strip_prefix(&root)
                && !rel_dest.as_os_str().is_empty()
                && !set.contains(rel_dest)
            {
                errors.push(broken("not tracked by git".into()));
                continue;
            }
            let Some(fragment) = fragment else { continue };
            if fragment.is_empty() || !is_markdown(&dest) || !dest.is_file() {
                continue;
            }
            let set = anchors.entry(dest.clone()).or_insert_with(|| {
                fs::read_to_string(&dest)
                    .map(|t| unique_slugs(headings(&t)).into_iter().collect())
                    .unwrap_or_default()
            });
            if !set.contains(&fragment) {
                errors.push(broken(format!("no heading with anchor #{fragment}")));
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRepo(PathBuf);

    impl TempRepo {
        fn new(files: &[(&str, &str)]) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("lectern-doc-check-{}-{n}", std::process::id()));
            let repo = root.join("repo");
            for (name, text) in files {
                let path = repo.join(name);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, text).unwrap();
            }
            TempRepo(root)
        }

        fn check(&self, files: &[&str]) -> Vec<BrokenLink> {
            let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
            check(&self.0.join("repo"), &files)
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn slugs_follow_github() {
        assert_eq!(slug("性能预算"), "性能预算");
        assert_eq!(slug("对 Lectern 的建议"), "对-lectern-的建议");
        assert_eq!(slug("交互规范「动效」"), "交互规范动效");
        assert_eq!(
            slug("Build and test (macOS arm64)"),
            "build-and-test-macos-arm64"
        );
        assert_eq!(slug("Cafe\u{301}"), "cafe\u{301}");
        let s = |v: &[&str]| unique_slugs(v.iter().map(|s| s.to_string()));
        assert_eq!(s(&["Foo", "Foo-1", "Foo"]), ["foo", "foo-1", "foo-2"]);
        assert_eq!(s(&["A", "A", "A"]), ["a", "a-1", "a-2"]);
    }

    #[test]
    fn heading_text_is_rendered_text() {
        let md = "# `kernel` 设计\n\n> # Quoted heading\n\n- # In a list\n\nSetext\n===\n\n# [Guide][docs]\n\n[docs]: x.md\n\n    # not a heading\n";
        let got = unique_slugs(headings(md));
        assert_eq!(
            got,
            [
                "kernel-设计",
                "quoted-heading",
                "in-a-list",
                "setext",
                "guide"
            ]
        );
    }

    #[test]
    fn valid_links_pass() {
        let repo = TempRepo::new(&[
            (
                "a.md",
                "# 标题\n\n## 二级 标题\n\n## 二级 标题\n\n# Performance Budget\n",
            ),
            ("my guide.md", "# 性能预算\n"),
            ("guide(v2).md", "# V2\n"),
            (
                "docs/b.md",
                concat!(
                    "[a](../a.md) [x](../a.md#二级-标题-1) [self](#c) [web](https://x.y/z)\n",
                    "[angle](<../my guide.md>) [enc](../my%20guide.md#%E6%80%A7%E8%83%BD%E9%A2%84%E7%AE%97)\n",
                    "[see [nested]](../a.md) [parens](../guide(v2).md) [q](../a.md?plain=1#performance-budget)\n",
                    "[ref][r] Text.[^1]\n\n[r]: ../a.md#标题\n[^1]: This is a note.\n\n",
                    "<!-- [retired](gone.md) -->\n\n`` `[x](missing.md)` ``\n\n",
                    "`multi\n[y](missing.md)\nline`\n\n    [indented](missing.md)\n\n",
                    "```\n[fenced](missing.md)\n```\n\n## C\n",
                ),
            ),
        ]);
        assert_eq!(repo.check(&["a.md", "docs/b.md"]), []);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_file_resolves_from_its_target() {
        let repo = TempRepo::new(&[
            ("docs/guide.md", "[child](child.md)\n"),
            ("a/child.md", "# Decoy\n"),
        ]);
        let base = repo.0.join("repo");
        std::os::unix::fs::symlink("../docs/guide.md", base.join("a/alias.md")).unwrap();
        // The alias comes first. Its link resolves next to docs/guide.md, where
        // child.md is missing, not next to the alias, where a decoy exists.
        let errors = repo.check(&["a/alias.md", "docs/guide.md"]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].file, PathBuf::from("docs/guide.md"));
    }

    #[test]
    fn raw_html_links_are_checked() {
        let repo = TempRepo::new(&[
            ("a.md", "# A\n"),
            (
                "b.md",
                "<p align=\"center\"><a href=\"a.md#a\">ok</a> <img src=\"missing.png\"></p>\n\n<!-- <a href=\"gone.md\">x</a> -->\n\nText <a href='nope.md'>inline</a>.\n",
            ),
        ]);
        let errors = repo.check(&["b.md"]);
        let targets: Vec<_> = errors.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(targets, ["missing.png", "nope.md"]);
    }

    #[test]
    fn html_attributes_respect_quotes() {
        let repo = TempRepo::new(&[(
            "b.md",
            "<div data-note=\"x href=missing.md\" title='src=gone.md'><a class=x href=nope.md>y</a></div>\n",
        )]);
        let errors = repo.check(&["b.md"]);
        let targets: Vec<_> = errors.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(targets, ["nope.md"]);
    }

    #[test]
    fn final_sigma_lowercases_like_github() {
        assert_eq!(slug("ΟΣ"), "ος");
    }

    #[cfg(unix)]
    #[test]
    fn markdown_symlink_out_of_the_repository_is_rejected() {
        let repo = TempRepo::new(&[("outside.md", "[x](nope.md)\n")]);
        let base = repo.0.join("repo");
        fs::rename(base.join("outside.md"), repo.0.join("outside.md")).unwrap();
        std::os::unix::fs::symlink("../outside.md", base.join("alias.md")).unwrap();
        let errors = repo.check(&["alias.md"]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].reason.contains("outside the repository"));
    }

    #[test]
    fn html_entities_are_decoded() {
        let repo = TempRepo::new(&[
            ("guide&notes.md", "# G\n"),
            (
                "b.md",
                "<a href=\"guide&amp;notes.md\">ok</a> <a href=\"x&#x2e;md\">bad</a>\n",
            ),
        ]);
        let errors = repo.check(&["b.md"]);
        let targets: Vec<_> = errors.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(targets, ["x.md"]);
    }

    #[test]
    fn markdown_extension_is_recognized() {
        assert!(is_markdown(Path::new("README.markdown")));
        assert!(is_markdown(Path::new("GUIDE.MD")));
        assert!(!is_markdown(Path::new("notes.txt")));
    }

    #[test]
    fn html_url_whitespace_is_stripped() {
        let repo = TempRepo::new(&[
            ("guide.md", "# G\n"),
            (
                "b.md",
                "<a href=\" guide.md \">ok</a> <a href=\"gu\nide.md\">ok</a>\n",
            ),
        ]);
        assert_eq!(repo.check(&["b.md"]), []);
    }

    #[test]
    fn all_named_entities_are_decoded() {
        let repo = TempRepo::new(&[
            ("café.md", "# C\n"),
            ("b.md", "<a href=\"caf&eacute;.md\">ok</a>\n"),
        ]);
        assert_eq!(repo.check(&["b.md"]), []);
    }

    #[test]
    fn html_blocks_are_tokenized_whole() {
        let repo = TempRepo::new(&[(
            "b.md",
            concat!(
                "<div>\n<a\nhref=\"missing.md\">x</a>\n<!--\n<a href=\"commented.md\">y</a>\n-->\n",
                "<script>const x = '<a href=\"script.md\">';</script>\n",
                "<style>a[href=\"style.md\"] {}</style>\n</div>\n",
            ),
        )]);
        let errors = repo.check(&["b.md"]);
        let got: Vec<_> = errors.iter().map(|e| (e.line, e.target.as_str())).collect();
        assert_eq!(got, [(2, "missing.md")]);
    }

    #[test]
    fn slugs_drop_what_github_drops() {
        assert_eq!(slug("x²"), "x");
        assert_eq!(slug("Ⅻ 章"), "ⅻ-章");
        assert_eq!(slug("a 😀 b"), "a--b");
        assert_eq!(slug("x—y"), "xy");
    }

    #[test]
    fn inline_raw_text_spans_events() {
        let repo = TempRepo::new(&[(
            "b.md",
            "Text <script>const x = '<a href=\"missing.md\">';</script> and <a href=\"gone.md\">x</a>.\n",
        )]);
        let errors = repo.check(&["b.md"]);
        let got: Vec<_> = errors.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(got, ["gone.md"]);
    }

    #[test]
    fn raw_text_end_tag_needs_a_boundary() {
        let repo = TempRepo::new(&[(
            "b.md",
            "Text <script>ignored </scriptx><a href=\"missing.md\"></a></script> then <a href=\"gone.md\">x</a>.\n",
        )]);
        let errors = repo.check(&["b.md"]);
        let got: Vec<_> = errors.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(got, ["gone.md"]);
    }

    #[test]
    fn plaintext_never_closes() {
        let repo = TempRepo::new(&[(
            "b.md",
            "Text <plaintext></plaintext><a href=\"missing.md\">x</a>\n",
        )]);
        assert_eq!(repo.check(&["b.md"]), []);
    }

    #[cfg(unix)]
    #[test]
    fn percent_encoded_non_utf8_bytes_reach_the_file_name() {
        use std::os::unix::ffi::OsStrExt;
        let repo = TempRepo::new(&[("b.md", "[x](asset-%FF.bin)\n")]);
        let name = std::ffi::OsStr::from_bytes(b"asset-\xff.bin");
        fs::write(repo.0.join("repo").join(name), "x").unwrap();
        assert_eq!(repo.check(&["b.md"]), []);
    }

    #[cfg(unix)]
    #[test]
    fn absolute_symlink_targets_fail() {
        let repo = TempRepo::new(&[("guide.md", "# G\n"), ("b.md", "[x](link.md)\n")]);
        let base = repo.0.join("repo");
        std::os::unix::fs::symlink(base.join("guide.md"), base.join("link.md")).unwrap();
        std::os::unix::fs::symlink(base.join("b.md"), base.join("alias.md")).unwrap();
        let errors = repo.check(&["b.md", "alias.md"]);
        let got: Vec<_> = errors
            .iter()
            .map(|e| {
                (
                    e.file.display().to_string(),
                    e.reason.contains("absolute target"),
                )
            })
            .collect();
        assert_eq!(
            got,
            [("b.md".to_string(), true), ("alias.md".to_string(), true)]
        );
    }

    #[test]
    fn backslash_network_reference_is_external() {
        let repo = TempRepo::new(&[("b.md", "<a href=\"\\\\example.com\\guide\">x</a>\n")]);
        assert_eq!(repo.check(&["b.md"]), []);
    }

    #[test]
    fn connector_punctuation_stays_in_slugs() {
        assert_eq!(slug("a‿b"), "a‿b");
        assert_eq!(slug("snake_case"), "snake_case");
    }

    #[cfg(unix)]
    #[test]
    fn tracked_symlink_to_untracked_file_fails() {
        let repo = TempRepo::new(&[("gen/out.md", "# Out\n"), ("c.md", "[x](link.md)\n")]);
        let base = repo.0.join("repo");
        std::os::unix::fs::symlink("gen/out.md", base.join("link.md")).unwrap();
        let tracked: Vec<PathBuf> = ["c.md", "link.md"].iter().map(PathBuf::from).collect();
        let errors = check_tracked(&base, &[PathBuf::from("c.md")], Some(&tracked));
        let got: Vec<_> = errors.iter().map(|e| e.reason.as_str()).collect();
        assert_eq!(got, ["not tracked by git"]);
    }

    #[cfg(unix)]
    #[test]
    fn markdown_symlink_to_untracked_file_fails() {
        let repo = TempRepo::new(&[("generated.md", "[x](nope.md)\n")]);
        let base = repo.0.join("repo");
        std::os::unix::fs::symlink("generated.md", base.join("alias.md")).unwrap();
        let tracked = vec![PathBuf::from("alias.md")];
        let errors = check_tracked(&base, &[PathBuf::from("alias.md")], Some(&tracked));
        let got: Vec<_> = errors.iter().map(|e| e.reason.as_str()).collect();
        assert_eq!(got, ["file is a symlink to a file not tracked by git"]);
    }

    #[cfg(unix)]
    #[test]
    fn markdown_symlink_with_missing_target_fails() {
        let repo = TempRepo::new(&[("a.md", "# A\n")]);
        let base = repo.0.join("repo");
        std::os::unix::fs::symlink("gone.md", base.join("alias.md")).unwrap();
        let errors = repo.check(&["alias.md"]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].reason.starts_with("cannot read file"));
    }

    #[test]
    fn untracked_destinations_fail_when_tracking_is_known() {
        let repo = TempRepo::new(&[
            ("a.md", "# A\n"),
            ("docs/b.md", "# B\n"),
            ("target/out.md", "# Out\n"),
            ("c.md", "[a](a.md) [dir](docs) [built](target/out.md)\n"),
        ]);
        let tracked: Vec<PathBuf> = ["a.md", "docs/b.md", "c.md"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let errors = check_tracked(
            &repo.0.join("repo"),
            &[PathBuf::from("c.md")],
            Some(&tracked),
        );
        let got: Vec<_> = errors
            .iter()
            .map(|e| (e.target.as_str(), e.reason.as_str()))
            .collect();
        assert_eq!(got, [("target/out.md", "not tracked by git")]);
    }

    #[test]
    fn uppercase_extension_is_markdown() {
        let repo = TempRepo::new(&[("GUIDE.MD", "# Guide\n"), ("b.md", "[x](GUIDE.MD#nope)\n")]);
        let errors = repo.check(&["b.md"]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].reason.contains("#nope"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_out_of_the_repository_is_rejected() {
        let repo = TempRepo::new(&[("b.md", "[out](docs/out.md)\n"), ("outside.md", "# Out\n")]);
        let base = repo.0.join("repo");
        fs::create_dir_all(base.join("docs")).unwrap();
        fs::rename(base.join("outside.md"), repo.0.join("outside.md")).unwrap();
        std::os::unix::fs::symlink("../../outside.md", base.join("docs/out.md")).unwrap();
        let errors = repo.check(&["b.md"]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].reason, "points outside the repository");
    }

    #[test]
    fn broken_links_fail() {
        let repo = TempRepo::new(&[
            ("a.md", "# Performance Budget\n"),
            (
                "b.md",
                "[missing](nope.md)\n[anchor](a.md#没有)\n[case](a.md#Performance-Budget)\n[outside](../outside.md)\n[nested [x]](nope2.md)\n\n[ref][r]\n\n[r]: gone.md\n",
            ),
            ("../outside.md", "# Outside\n"),
        ]);
        let errors = repo.check(&["b.md"]);
        let reasons: Vec<_> = errors.iter().map(|e| (e.line, e.reason.as_str())).collect();
        assert_eq!(
            reasons,
            [
                (1, "file not found"),
                (2, "no heading with anchor #没有"),
                (3, "no heading with anchor #Performance-Budget"),
                (4, "points outside the repository"),
                (5, "file not found"),
                (7, "file not found"),
            ]
        );
    }
}
