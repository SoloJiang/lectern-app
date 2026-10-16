//! Checks that relative links in Markdown files resolve.
//!
//! Every link the CommonMark parser reports (inline, reference-style, images)
//! is checked when its destination is relative:
//!
//! - the target file or directory must exist and stay inside the repository;
//! - a `#fragment` on a Markdown target must equal one of its heading anchors,
//!   generated with GitHub's rules (see [`slug`]) and compared case-sensitively.
//!
//! Query strings are dropped and percent-encoding is decoded before the path
//! is resolved. Code blocks, code spans and HTML comments contain no links, so
//! they are never checked.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use unicode_normalization::char::is_combining_mark;

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

/// GitHub's heading anchor: lowercase, keep letters, digits, combining marks,
/// `-` and `_`, turn spaces into `-`, drop everything else.
pub fn slug(text: &str) -> String {
    // Lowercase the whole string: some letters (e.g. final sigma) depend on
    // their neighbours.
    text.to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() || is_combining_mark(c) => Some(c),
            _ => None,
        })
        .collect()
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

/// Whether `path` has a Markdown extension (`.md`, any case).
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

/// `href` and `src` values in a piece of raw HTML, with HTML comments removed.
fn html_destinations(html: &str) -> Vec<String> {
    let mut text = String::new();
    let mut rest = html;
    while let Some(start) = rest.find("<!--") {
        text.push_str(&rest[..start]);
        rest = rest[start..]
            .find("-->")
            .map_or("", |end| &rest[start + end + 3..]);
    }
    text.push_str(rest);
    // Walk the tags and their attributes, so a quoted value that merely
    // contains `href=` is not mistaken for an attribute.
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c != '<' {
            continue;
        }
        // Tag name; skip closing tags, declarations and processing instructions.
        match chars.peek() {
            Some((_, c)) if c.is_ascii_alphabetic() => {}
            _ => continue,
        }
        while chars
            .next_if(|(_, c)| !c.is_whitespace() && *c != '>' && *c != '/')
            .is_some()
        {}
        loop {
            while chars
                .next_if(|(_, c)| c.is_whitespace() || *c == '/')
                .is_some()
            {}
            match chars.peek() {
                None => break,
                Some((_, '>')) => {
                    chars.next();
                    break;
                }
                _ => {}
            }
            let mut name = String::new();
            while let Some((_, c)) =
                chars.next_if(|(_, c)| !c.is_whitespace() && !matches!(c, '=' | '>' | '/'))
            {
                name.push(c.to_ascii_lowercase());
            }
            while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
            if chars.next_if(|(_, c)| *c == '=').is_none() {
                continue;
            }
            while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
            let mut value = String::new();
            match chars.peek().map(|(_, c)| *c) {
                Some(q @ ('"' | '\'')) => {
                    chars.next();
                    for (_, c) in chars.by_ref() {
                        if c == q {
                            break;
                        }
                        value.push(c);
                    }
                }
                _ => {
                    while let Some((_, c)) = chars.next_if(|(_, c)| !c.is_whitespace() && *c != '>')
                    {
                        value.push(c);
                    }
                }
            }
            if (name == "href" || name == "src") && !value.is_empty() {
                out.push(value);
            }
        }
    }
    out
}

/// Destinations of all links and images, with their byte offsets: Markdown
/// links and `href` / `src` attributes in raw HTML.
pub fn links(markdown: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (event, range) in Parser::new_ext(markdown, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                out.push((range.start, dest_url.into_string()));
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                out.extend(
                    html_destinations(&html)
                        .into_iter()
                        .map(|d| (range.start, d)),
                );
            }
            _ => {}
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
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
    String::from_utf8_lossy(&out).into_owned()
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
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
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
                Some((p, f)) => (p, Some(percent_decode(f))),
                None => (target.as_str(), None),
            };
            let file_part = percent_decode(without_fragment.split('?').next().unwrap_or(""));
            let dest = if file_part.is_empty() {
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
            // A symlink inside the repository may point outside it: check the
            // real target too, and never read a file outside the repository.
            let dest = match fs::canonicalize(&dest) {
                Ok(real) if real.starts_with(&root) => real,
                _ => {
                    errors.push(broken("points outside the repository".into()));
                    continue;
                }
            };
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
        std::os::unix::fs::symlink(repo.0.join("outside.md"), base.join("alias.md")).unwrap();
        let errors = repo.check(&["alias.md"]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].reason.contains("outside the repository"));
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
        std::os::unix::fs::symlink(repo.0.join("outside.md"), base.join("docs/out.md")).unwrap();
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
