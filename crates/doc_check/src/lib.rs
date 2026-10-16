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
    text.chars()
        .flat_map(char::to_lowercase)
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

/// Destinations of all links and images, with their byte offsets.
pub fn links(markdown: &str) -> Vec<(usize, String)> {
    Parser::new_ext(markdown, options())
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                Some((range.start, dest_url.into_string()))
            }
            _ => None,
        })
        .collect()
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
            let Some(fragment) = fragment else { continue };
            if fragment.is_empty() || dest.extension().is_none_or(|e| e != "md") || !dest.is_file()
            {
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
