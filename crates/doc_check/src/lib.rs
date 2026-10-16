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
use pulldown_cmark::{Event, Options, Parser, Tag};

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

/// Whether `path` has a Markdown extension (`.md` or `.markdown`, any case).
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

/// A document rendered to HTML the way GitHub's Markdown pipeline does, with
/// enough bookkeeping to map positions back to the source.
struct Rendered {
    html: String,
    /// `(html offset, source offset)` at the start of each Markdown event, in
    /// order.
    map: Vec<(usize, usize)>,
    /// HTML offsets where a Markdown heading starts (raw `<h2>` in the
    /// source does not get an automatic anchor on GitHub).
    headings: HashSet<usize>,
    /// HTML offsets of the generated end tags of Markdown headings, so a raw
    /// `</h1>` inside `# Hello </h1> world` does not end the heading.
    heading_ends: HashSet<usize>,
    /// Indices into `map` of raw HTML events, which are copied verbatim.
    raw: HashSet<usize>,
}

fn render(markdown: &str) -> Rendered {
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Sink(Rc<RefCell<String>>);
    impl std::fmt::Write for Sink {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            self.0.borrow_mut().push_str(s);
            Ok(())
        }
    }

    let html = Rc::new(RefCell::new(String::new()));
    let map = RefCell::new(Vec::new());
    let headings = RefCell::new(HashSet::new());
    let heading_ends = RefCell::new(HashSet::new());
    let raw = RefCell::new(HashSet::new());
    let events = Parser::new_ext(markdown, options())
        .into_offset_iter()
        .map(|(event, range)| {
            let at = html.borrow().len();
            if matches!(event, Event::Html(_) | Event::InlineHtml(_)) {
                raw.borrow_mut().insert(map.borrow().len());
            }
            map.borrow_mut().push((at, range.start));
            if matches!(event, Event::Start(Tag::Heading { .. })) {
                headings.borrow_mut().insert(at);
            }
            if matches!(event, Event::End(pulldown_cmark::TagEnd::Heading(_))) {
                heading_ends.borrow_mut().insert(at);
            }
            event
        });
    // Writing to a String cannot fail.
    let _ = pulldown_cmark::html::write_html_fmt(Sink(html.clone()), events);
    let html = html.borrow().clone();
    Rendered {
        html,
        map: map.into_inner(),
        headings: headings.into_inner(),
        heading_ends: heading_ends.into_inner(),
        raw: raw.into_inner(),
    }
}

/// Links, anchors and heading texts of a document, read from its rendered
/// HTML in one pass of an HTML-standard tokenizer (html5gum) with naive state
/// switching: character references are decoded, comments are skipped, and
/// everything inside `script`, `style` and other raw-text elements (including
/// an unclosed or self-closed one, and the escaped states of `script`) is text,
/// so links and headings there do not exist. `plaintext` swallows the rest of
/// the document. Tree construction is not modelled, so elements whose parsing
/// or meaning depends on it (foreign content, raw-text and template contents,
/// `select`, form controls) are reported instead of being read the wrong way
/// (see [`unsupported_html`]).
#[derive(Default)]
struct Scan {
    /// Destinations with source byte offsets.
    links: Vec<(usize, String)>,
    /// `id` attributes and `<a name>` values.
    ids: Vec<String>,
    /// Text of each Markdown heading, in order.
    headings: Vec<String>,
    /// Source byte offsets and names of raw HTML elements the checker cannot
    /// follow.
    unsupported: Vec<(usize, String)>,
}

fn scan(markdown: &str) -> Scan {
    let rendered = render(markdown);
    let event_at = |at: usize| {
        rendered
            .map
            .partition_point(|&(start, _)| start <= at)
            .checked_sub(1)
    };
    // Exact inside raw HTML, which is copied verbatim; elsewhere the rendered
    // text can be longer than the source (`&lt;`), so stay on a character.
    let source_offset = |at: usize| match event_at(at).map(|i| rendered.map[i]) {
        Some((start, source)) => {
            let mut offset = source + (at - start).min(markdown.len() - source);
            while !markdown.is_char_boundary(offset) {
                offset -= 1;
            }
            offset
        }
        None => 0,
    };
    // Generated HTML (Markdown tables, lists) is well formed; only raw HTML
    // from the source can use the unsupported elements.
    let is_raw = |at: usize| event_at(at).is_some_and(|i| rendered.raw.contains(&i));
    let is_heading = |name: &[u8]| matches!(name, [b'h', b'1'..=b'6']);
    let mut out = Scan::default();
    let mut heading: Option<(Vec<u8>, String)> = None;
    let mut emitter = DefaultEmitter::<usize>::new_with_span();
    emitter.naively_switch_states(true);
    for token in Tokenizer::new_with_emitter(rendered.html.as_str(), emitter) {
        match token {
            Ok(Token::StartTag(tag)) => {
                let at = tag.span.start;
                let unsupported = is_raw(at) && UNSUPPORTED_ELEMENTS.contains(&&tag.name[..]);
                if unsupported {
                    out.unsupported.push((
                        source_offset(at),
                        String::from_utf8_lossy(&tag.name).into_owned(),
                    ));
                }
                if is_heading(&tag.name)
                    && (rendered.headings.contains(&at)
                        || at
                            .checked_sub(1)
                            .is_some_and(|a| rendered.headings.contains(&a)))
                {
                    heading = Some((tag.name.to_vec(), String::new()));
                }
                for (attr, only_a) in [(&b"id"[..], false), (b"name", true)] {
                    // An ignored `<head id>` defines nothing.
                    if !unsupported
                        && (!only_a || &tag.name[..] == b"a")
                        && let Some(value) = tag.attributes.get(attr)
                        && !value.value.is_empty()
                    {
                        out.ids
                            .push(String::from_utf8_lossy(&value.value).into_owned());
                    }
                }
                // Only attributes HTML gives link or fetch semantics; an
                // `href` on a `div` is not a link.
                let link_attrs: &[&[u8]] = match &tag.name[..] {
                    b"a" | b"area" | b"link" => &[b"href"],
                    // The tree builder renames `<image>` to `<img>`.
                    // `source` and `track` only fetch `src` inside `audio` or
                    // `video`, which are unsupported; in `picture` a `source`
                    // selects from `srcset`.
                    b"img" | b"image" | b"embed" => &[b"src"],
                    // `src` applies only to the Image Button state; task list
                    // checkboxes and other inputs have no destination.
                    b"input"
                        if tag
                            .attributes
                            .get(&b"type"[..])
                            .is_some_and(|v| v.value.eq_ignore_ascii_case(b"image")) =>
                    {
                        &[b"src"]
                    }
                    _ => &[],
                };
                let mut urls: Vec<String> = link_attrs
                    .iter()
                    .filter_map(|&attr| tag.attributes.get(attr))
                    .map(|v| String::from_utf8_lossy(&v.value).into_owned())
                    .collect();
                // A browser may fetch any `srcset` candidate. A stray `source`
                // outside `picture` is inert but checked anyway.
                if matches!(&tag.name[..], b"img" | b"image" | b"source")
                    && let Some(value) = tag.attributes.get(&b"srcset"[..])
                {
                    urls.extend(srcset_urls(&String::from_utf8_lossy(&value.value)));
                }
                for raw in urls {
                    let url = clean_url(&raw);
                    if !url.is_empty() {
                        out.links.push((source_offset(at), url));
                    }
                }
            }
            Ok(Token::EndTag(tag)) => {
                // Only the generated end tag of the same level closes the
                // heading: a raw `</h2>` inside `# Hello </h2> world` is text
                // to the slug.
                // A raw `</h1>` inside `# Hello </h1> world` has the same name
                // but is not the end tag pulldown-cmark generated.
                let at = tag.span.start;
                if heading
                    .as_ref()
                    .is_some_and(|(name, _)| name[..] == tag.name[..])
                    && (rendered.heading_ends.contains(&at)
                        || at
                            .checked_sub(1)
                            .is_some_and(|a| rendered.heading_ends.contains(&a)))
                    && let Some((_, text)) = heading.take()
                {
                    out.headings.push(text);
                }
            }
            Ok(Token::String(text)) => {
                if let Some((_, h)) = heading.as_mut() {
                    h.push_str(&String::from_utf8_lossy(&text.value));
                }
            }
            _ => {}
        }
    }
    out
}

/// Raw HTML elements whose effect on parsing or rendering needs HTML tree
/// construction, which a tokenizer cannot follow: foreign content (`svg`,
/// `math`: CDATA, non-raw-text `style`), raw-text and RCDATA elements that
/// can swallow the rest of the document (`script`, `style`, `iframe`, ...),
/// inert `template` contents and `select` (which ignores some start tags),
/// `audio` and `video` (which `source` or `track` loads depends on the tree
/// and on end tags the parser may ignore), `form` (a nested form start tag
/// is dropped with its `id`), and the elements whose start tags the in-body
/// insertion mode ignores or merges (`html`, `head`, `body`, and table parts
/// outside a table; raw tables also foster-parent their contents). Documents
/// do not need them: diagrams are image files, recordings are links, and
/// tables are Markdown tables.
const UNSUPPORTED_ELEMENTS: &[&[u8]] = &[
    b"svg",
    b"math",
    b"script",
    b"style",
    b"iframe",
    b"noembed",
    b"noframes",
    b"noscript",
    b"xmp",
    b"plaintext",
    b"textarea",
    b"title",
    b"template",
    b"select",
    b"object",
    b"frame",
    b"frameset",
    b"audio",
    b"video",
    b"form",
    b"html",
    b"head",
    b"body",
    b"table",
    b"caption",
    b"colgroup",
    b"col",
    b"thead",
    b"tbody",
    b"tfoot",
    b"tr",
    b"th",
    b"td",
];

/// URL parsing strips leading and trailing C0 controls and spaces, and removes
/// tabs and newlines anywhere. A backslash is a slash for web URLs before the
/// fragment, so `\\host\x` is a network-path reference like `//host/x`; in the
/// fragment it stays a backslash.
fn clean_url(raw: &str) -> String {
    let cleaned: String = raw
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let (before, fragment) = match cleaned.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (cleaned.as_str(), None),
    };
    let mut url = before.replace('\\', "/");
    if let Some(f) = fragment {
        url.push('#');
        url.push_str(f);
    }
    url
}

/// URLs of the image candidates in a `srcset`, following the HTML parsing
/// rules: candidates are separated by commas, a URL runs to the next
/// whitespace (trailing commas end the candidate), and descriptors such as
/// `2x` or `100w` run to the next comma outside parentheses.
fn srcset_urls(srcset: &str) -> Vec<String> {
    let is_space = |c: char| matches!(c, ' ' | '\t' | '\n' | '\x0c' | '\r');
    let mut urls = Vec::new();
    let mut rest = srcset;
    loop {
        rest = rest.trim_start_matches(|c: char| is_space(c) || c == ',');
        if rest.is_empty() {
            return urls;
        }
        let end = rest.find(is_space).unwrap_or(rest.len());
        let (url, after) = rest.split_at(end);
        rest = after;
        if url.ends_with(',') {
            urls.push(url.trim_end_matches(',').to_string());
            continue;
        }
        urls.push(url.to_string());
        let mut depth = 0usize;
        let mut cut = rest.len();
        for (i, c) in rest.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    cut = i + 1;
                    break;
                }
                _ => {}
            }
        }
        rest = &rest[cut..];
    }
}

/// Source byte offsets and names of the [`UNSUPPORTED_ELEMENTS`] used as raw
/// HTML; the checker reports them so links around them cannot be misread.
pub fn unsupported_html(markdown: &str) -> Vec<(usize, String)> {
    scan(markdown).unsupported
}

/// Rendered text of every Markdown heading, in order: text, inline code and
/// link text count; markup and link destinations do not.
pub fn headings(markdown: &str) -> Vec<String> {
    scan(markdown).headings
}

/// Destinations of all links and images with their source byte offsets:
/// Markdown links and `href` / `src` attributes in raw HTML, as rendered.
pub fn links(markdown: &str) -> Vec<(usize, String)> {
    scan(markdown).links
}

/// Anchors a document defines: GitHub heading slugs plus `id` / `<a name>`
/// attributes in its HTML.
pub fn anchors(markdown: &str) -> HashSet<String> {
    let found = scan(markdown);
    let mut set: HashSet<String> = unique_slugs(found.headings).into_iter().collect();
    set.extend(found.ids);
    set
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

/// Whether every path met while resolving `path` (absolute, under `root`) is
/// in `tracked`: each lexical component, and for a symlink each component of
/// its target, as git checks them out. A tracked directory symlink
/// `docs -> real-docs` reaches the tracked `real-docs/guide.md` through
/// `docs/guide.md`, which git cannot list itself.
fn tracked_route(root: &Path, path: &Path, tracked: &HashSet<PathBuf>) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let mut todo: Vec<PathBuf> = rel
        .components()
        .rev()
        .map(|c| PathBuf::from(c.as_os_str()))
        .collect();
    let mut current = PathBuf::new();
    let mut hops = 0;
    while let Some(part) = todo.pop() {
        match part.components().next() {
            Some(Component::ParentDir) => {
                if !current.pop() {
                    return false;
                }
            }
            Some(Component::CurDir) | None => {}
            Some(_) => {
                current.push(&part);
                if !tracked.contains(&current) {
                    return false;
                }
                if let Ok(target) = fs::read_link(root.join(&current)) {
                    hops += 1;
                    if target.is_absolute() || hops > 40 {
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
    true
}

/// The Markdown README GitHub shows for `dir`: `README.md` or
/// `README.markdown` in any letter case, tracked when `tracked` is given; the
/// first in byte order when several exist.
fn directory_readme(
    root: &Path,
    dir: &Path,
    tracked: Option<&HashSet<PathBuf>>,
) -> Option<PathBuf> {
    let mut names: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.eq_ignore_ascii_case("readme.md") || n.eq_ignore_ascii_case("readme.markdown")
            })
        })
        .filter(|p| match tracked {
            Some(set) => p.strip_prefix(root).is_ok_and(|r| set.contains(r)),
            None => true,
        })
        .collect();
    names.sort();
    names.into_iter().next()
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

/// `base` (under `root`) joined with the relative `rel`, normalized
/// lexically; None as soon as a `..` would step above `root`.
fn resolve_within(root: &Path, base: &Path, rel: &Path) -> Option<PathBuf> {
    let mut parts: Vec<PathBuf> = base
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|c| PathBuf::from(c.as_os_str()))
        .collect();
    for c in rel.components() {
        match c {
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::CurDir => {}
            Component::Normal(name) => parts.push(PathBuf::from(name)),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    let mut out = root.to_path_buf();
    out.extend(parts);
    Some(out)
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
        // The name the input was given; a fragment-only link resolves against it.
        let input_name = root.join(rel);
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
        // A tracked Markdown symlink must reach a tracked file through tracked
        // paths only, or it is broken in a fresh checkout. Checked on the name
        // given, before deduplication, so an untracked hop is not hidden by
        // canonicalization.
        if let Some(set) = &tracked
            && !tracked_route(&root, &normalize(&root.join(rel)), set)
        {
            errors.push(BrokenLink {
                file: rel.clone(),
                line: 1,
                target: rel.display().to_string(),
                reason: "file is a symlink to a file not tracked by git".into(),
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
        for (offset, name) in unsupported_html(&text) {
            errors.push(BrokenLink {
                file: rel.clone(),
                line: text[..offset].matches('\n').count() + 1,
                target: format!("<{name}>"),
                reason: "raw HTML element the link checker cannot follow; leave it out (use an image file for diagrams)"
                    .into(),
            });
        }
        for (offset, target) in links(&text) {
            // `//host/x` is a network-path reference; a single leading slash
            // is relative to the repository root on GitHub.
            if is_external(&target) || target.starts_with("//") {
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
            // `..` must not leave the repository even if a later component
            // comes back in (`../<checkout name>/README.md`).
            let dest = if file_part.as_os_str().is_empty() {
                Some(path.clone())
            } else if let Ok(from_root) = file_part.strip_prefix("/") {
                resolve_within(&root, &root, from_root)
            } else {
                resolve_within(&root, &dir, &file_part)
            };
            let Some(dest) = dest.filter(|d| d.starts_with(&root)) else {
                errors.push(broken("points outside the repository".into()));
                continue;
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
            // A symlink inside the repository may point outside it: check the
            // real target too, and never read a file outside the repository.
            let real = match fs::canonicalize(&dest) {
                Ok(real) if real.starts_with(&root) => real,
                _ => {
                    errors.push(broken("points outside the repository".into()));
                    continue;
                }
            };
            // Every path the link passes through must be tracked: the
            // destination, each symlink on the way and what it points to.
            if let Some(set) = &tracked
                && !tracked_route(&root, &dest, set)
            {
                errors.push(broken("not tracked by git".into()));
                continue;
            }
            // Whether the target is Markdown follows the name the link uses
            // (`alias.md -> real.txt` is rendered as Markdown on GitHub).
            let mut lexical = if file_part.as_os_str().is_empty() {
                input_name.clone()
            } else {
                dest
            };
            let mut dest = real;
            let Some(fragment) = fragment else { continue };
            if fragment.is_empty() {
                continue;
            }
            // GitHub renders a directory's README below its listing, so
            // `docs/#section` points into that README.
            if dest.is_dir() {
                match directory_readme(&root, &dest, tracked.as_ref()) {
                    Some(readme) => {
                        // The README may itself be a symlink: same checks as
                        // any destination before reading its anchors.
                        if has_absolute_symlink(&readme) {
                            errors.push(broken(
                                "the directory's README is a symlink with an absolute target"
                                    .into(),
                            ));
                            continue;
                        }
                        let real = match fs::canonicalize(&readme) {
                            Ok(real) if real.starts_with(&root) => real,
                            _ => {
                                errors.push(broken(
                                    "the directory's README points outside the repository".into(),
                                ));
                                continue;
                            }
                        };
                        // A README symlink to a directory renders nothing, so
                        // it defines no anchors.
                        if !real.is_file() {
                            errors.push(broken("the directory's README is not a file".into()));
                            continue;
                        }
                        if let Some(set) = &tracked
                            && !tracked_route(&root, &readme, set)
                        {
                            errors.push(broken(
                                "the directory's README is not tracked by git".into(),
                            ));
                            continue;
                        }
                        lexical = readme;
                        dest = real;
                    }
                    // No README is rendered, so the directory page has no
                    // heading anchors.
                    None => {
                        errors.push(broken(format!(
                            "no heading with anchor #{fragment} (the directory has no Markdown README)"
                        )));
                        continue;
                    }
                }
            }
            if !is_markdown(&lexical) || !dest.is_file() {
                continue;
            }
            let set = anchors.entry(dest.clone()).or_insert_with(|| {
                fs::read_to_string(&dest)
                    .map(|t| self::anchors(&t))
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

        /// Link errors only: raw HTML the checker rejects is reported
        /// separately and covered by its own test.
        fn check_links(&self, files: &[&str]) -> Vec<BrokenLink> {
            self.check(files)
                .into_iter()
                .filter(|e| !e.reason.starts_with("raw HTML element"))
                .collect()
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
        let errors = repo.check_links(&["b.md"]);
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
        let errors = repo.check_links(&["b.md"]);
        let got: Vec<_> = errors.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(got, ["gone.md"]);
    }

    #[test]
    fn raw_text_end_tag_needs_a_boundary() {
        let repo = TempRepo::new(&[(
            "b.md",
            "Text <script>ignored </scriptx><a href=\"missing.md\"></a></script> then <a href=\"gone.md\">x</a>.\n",
        )]);
        let errors = repo.check_links(&["b.md"]);
        let got: Vec<_> = errors.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(got, ["gone.md"]);
    }

    #[test]
    fn plaintext_never_closes() {
        let repo = TempRepo::new(&[(
            "b.md",
            "Text <plaintext></plaintext><a href=\"missing.md\">x</a>\n",
        )]);
        assert_eq!(repo.check_links(&["b.md"]), []);
    }

    #[cfg(unix)]
    #[test]
    fn percent_encoded_non_utf8_bytes_reach_the_file_name() {
        use std::os::unix::ffi::OsStrExt;
        let repo = TempRepo::new(&[("b.md", "[x](asset-%FF.bin)\n")]);
        let name = std::ffi::OsStr::from_bytes(b"asset-\xff.bin");
        if let Err(e) = fs::write(repo.0.join("repo").join(name), "x") {
            // APFS and HFS+ store only UTF-8 names, so such a path cannot
            // exist on macOS; the byte handling is covered on Linux.
            if cfg!(target_vendor = "apple") {
                return;
            }
            panic!("{e}");
        }
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
    fn raw_text_stays_open_across_paragraphs() {
        let repo = TempRepo::new(&[(
            "b.md",
            "Text <script>\n\n<a href=\"missing.md\">x</a> [md](gone.md)\n\n</script>\n\n[after](nope.md)\n",
        )]);
        let errors = repo.check_links(&["b.md"]);
        let got: Vec<_> = errors.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(got, ["nope.md"]);
    }

    #[test]
    fn html_ids_are_anchors() {
        let repo = TempRepo::new(&[
            (
                "a.md",
                "<h2 id=\"setup\">Setup</h2>\n\nText <a name=\"old-name\"></a>.\n",
            ),
            (
                "b.md",
                "[s](a.md#setup) [o](a.md#old-name) [x](a.md#nope)\n",
            ),
        ]);
        let errors = repo.check(&["b.md"]);
        let got: Vec<_> = errors.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(got, ["a.md#nope"]);
    }

    #[test]
    fn self_closed_raw_text_stays_open() {
        let repo = TempRepo::new(&[(
            "b.md",
            "Text <script/>\n\n[example](missing.md)\n\n</script>\n\n[after](nope.md)\n",
        )]);
        let errors = repo.check_links(&["b.md"]);
        let got: Vec<_> = errors.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(got, ["nope.md"]);
    }

    #[test]
    fn raw_end_tags_of_another_level_stay_in_the_heading() {
        assert_eq!(headings("# Hello </h2> world\n"), ["Hello  world"]);
        assert!(anchors("# Hello </h2> world\n").contains("hello--world"));
    }

    #[test]
    fn swallowed_headings_are_not_anchors() {
        let md = "Text <script>\n\n# Fake\n\n</script>\n\n# Real\n";
        let set = anchors(md);
        assert!(set.contains("real"));
        assert!(!set.contains("fake"));
    }

    #[test]
    fn script_escaped_states_follow_the_tokenizer() {
        let repo = TempRepo::new(&[(
            "b.md",
            "<script>\n<!-- <script> x </script> <a href=\"missing.md\"> -->\n</script>\n\n<a href=\"nope.md\">x</a>\n",
        )]);
        let errors = repo.check_links(&["b.md"]);
        let got: Vec<_> = errors.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(got, ["nope.md"]);
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
    fn links_through_tracked_directory_symlinks_pass() {
        let repo = TempRepo::new(&[
            ("real-docs/guide.md", "# Guide\n"),
            ("loose/guide.md", "# Guide\n"),
            (
                "c.md",
                "[ok](docs/guide.md#guide)\n[hop](hop/guide.md)\n[bad](docs/guide.md#nope)\n",
            ),
        ]);
        let base = repo.0.join("repo");
        std::os::unix::fs::symlink("real-docs", base.join("docs")).unwrap();
        // `hop` is tracked but points through `stray`, an untracked symlink.
        std::os::unix::fs::symlink("loose", base.join("stray")).unwrap();
        std::os::unix::fs::symlink("stray", base.join("hop")).unwrap();
        let tracked: Vec<PathBuf> = [
            "c.md",
            "docs",
            "hop",
            "real-docs/guide.md",
            "loose/guide.md",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        let errors = check_tracked(&base, &[PathBuf::from("c.md")], Some(&tracked));
        let got: Vec<_> = errors
            .iter()
            .map(|e| (e.target.as_str(), e.reason.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("hop/guide.md", "not tracked by git"),
                ("docs/guide.md#nope", "no heading with anchor #nope"),
            ]
        );
    }

    #[test]
    fn root_relative_links_resolve_from_the_repository_root() {
        let repo = TempRepo::new(&[
            ("docs/guide.md", "# Guide\n"),
            (
                "sub/b.md",
                "[ok](/docs/guide.md#guide)\n[gone](/missing.md)\n[net](//example.com/x)\n",
            ),
        ]);
        let got: Vec<_> = repo
            .check(&["sub/b.md"])
            .iter()
            .map(|e| (e.target.clone(), e.reason.clone()))
            .collect();
        assert_eq!(
            got,
            [("/missing.md".to_string(), "file not found".to_string())]
        );
    }

    #[test]
    fn inline_svg_and_math_are_reported() {
        let repo = TempRepo::new(&[(
            "b.md",
            "Text\n\n<svg><![CDATA[x > <a href=\"missing.md\">]]></svg>\n\n<math></math>\n",
        )]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| (e.line, e.target.clone()))
            .collect();
        assert_eq!(
            got[..2],
            [(3, "<svg>".to_string()), (5, "<math>".to_string())]
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_readme_symlinks_are_checked() {
        let repo = TempRepo::new(&[
            ("local.md", "# Docs\n"),
            ("docs/a.txt", "x"),
            ("b.md", "[x](docs/#docs)\n"),
        ]);
        let base = repo.0.join("repo");
        std::os::unix::fs::symlink("../local.md", base.join("docs/README.md")).unwrap();
        let tracked: Vec<PathBuf> = ["b.md", "docs/a.txt", "docs/README.md"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let errors = check_tracked(&base, &[PathBuf::from("b.md")], Some(&tracked));
        let got: Vec<_> = errors.iter().map(|e| e.reason.as_str()).collect();
        assert_eq!(got, ["the directory's README is not tracked by git"]);
    }

    #[test]
    fn only_link_attributes_of_link_elements_count() {
        let repo = TempRepo::new(&[(
            "b.md",
            "<div href=\"missing.md\"></div><span src=\"missing.png\"></span>\n\n<img src=\"gone.png\">\n",
        )]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(got, ["gone.png"]);
    }

    #[test]
    fn fragments_keep_backslashes() {
        let repo = TempRepo::new(&[("b.md", "<span id=\"a\\b\"></span><a href=\"#a\\b\">x</a>\n")]);
        assert_eq!(repo.check(&["b.md"]), []);
    }

    #[test]
    fn unsupported_raw_html_is_reported() {
        let repo = TempRepo::new(&[(
            "b.md",
            "<template><a href=\"missing.md\">x</a></template>\n\n<select><iframe></select><a href=\"gone.md\">x</a>\n",
        )]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .filter(|e| e.reason.starts_with("raw HTML element"))
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(got, ["<template>", "<select>", "<iframe>"]);
    }

    #[test]
    fn media_and_forms_are_unsupported() {
        let repo = TempRepo::new(&[(
            "b.md",
            "<audio><p></audio></p><source src=\"a.mp3\"></audio>\n\n<video src=\"v.mp4\"></video>\n\n<form><form id=\"ghost\"></form></form>\n",
        )]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(got, ["<audio>", "<video>", "<form>", "<form>"]);
    }

    #[test]
    fn ignored_structure_and_raw_tables_are_unsupported() {
        let repo = TempRepo::new(&[(
            "b.md",
            "<head id=\"ghost\"></head><a href=\"#ghost\">x</a>\n\n<td id=\"cell\">x</td>\n\n| a |\n|---|\n| [ok](#x) |\n\n# X\n",
        )]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(got, ["<head>", "<td>", "#ghost"]);
    }

    #[test]
    fn markdown_tables_with_multibyte_text_are_supported() {
        let repo = TempRepo::new(&[("b.md", "| 文件 | 说明 |\n|---|---|\n| `a` | 改动 |\n")]);
        assert!(repo.check(&["b.md"]).is_empty());
    }

    #[test]
    fn source_and_track_src_are_not_links() {
        let repo = TempRepo::new(&[(
            "b.md",
            "<picture><source src=\"missing.png\"><img src=\"gone.png\"></picture>\n\n<div><track src=\"missing.vtt\"></div>\n",
        )]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(got, ["gone.png"]);
    }

    #[test]
    fn links_may_not_leave_and_reenter_the_repository() {
        let repo = TempRepo::new(&[
            ("a.md", "# A\n"),
            ("b.md", "[x](../repo/a.md)\n[y](sub/../a.md)\n"),
        ]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| (e.target.clone(), e.reason.clone()))
            .collect();
        assert_eq!(
            got,
            [(
                "../repo/a.md".to_string(),
                "points outside the repository".to_string()
            )]
        );
    }

    #[test]
    fn directory_fragments_without_readme_fail() {
        let repo = TempRepo::new(&[("plain/a.txt", "x"), ("b.md", "[x](plain/#missing)\n")]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(got, ["plain/#missing"]);
    }

    #[test]
    fn srcset_candidates_are_links() {
        let repo = TempRepo::new(&[
            ("ok.png", ""),
            (
                "b.md",
                "<picture><source media=\"(x)\" srcset=\"gone-dark.png\"><img src=\"ok.png\" srcset=\"ok.png 1x, gone@2x.png 2x,missing,, a.png?(1) 100w\"></picture>\n",
            ),
        ]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(
            got,
            ["gone-dark.png", "gone@2x.png", "missing", "a.png?(1)"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn readme_symlinks_to_directories_fail() {
        let repo = TempRepo::new(&[("docs/sub/a.txt", "x"), ("b.md", "[x](docs/#missing)\n")]);
        std::os::unix::fs::symlink("sub", repo.0.join("repo/docs/README.md")).unwrap();
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| (e.target.clone(), e.reason.clone()))
            .collect();
        assert_eq!(
            got,
            [(
                "docs/#missing".to_string(),
                "the directory's README is not a file".to_string()
            )]
        );
    }

    #[cfg(unix)]
    #[test]
    fn self_fragments_use_the_input_name() {
        let repo = TempRepo::new(&[("real.txt", "# Real\n\n[x](#missing)\n")]);
        let base = repo.0.join("repo");
        std::os::unix::fs::symlink("real.txt", base.join("alias.md")).unwrap();
        let got: Vec<_> = repo
            .check(&["alias.md"])
            .iter()
            .map(|e| e.reason.clone())
            .collect();
        assert_eq!(got, ["no heading with anchor #missing"]);
    }

    #[test]
    fn image_is_an_img() {
        let repo = TempRepo::new(&[("b.md", "<image src=\"missing.png\">\n")]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(got, ["missing.png"]);
    }

    #[cfg(unix)]
    #[test]
    fn non_markdown_names_skip_fragments_even_through_symlinks() {
        let repo = TempRepo::new(&[
            ("real.md", "# Real\n"),
            ("b.md", "[x](alias.txt#anything)\n"),
        ]);
        let base = repo.0.join("repo");
        std::os::unix::fs::symlink("real.md", base.join("alias.txt")).unwrap();
        assert_eq!(repo.check(&["b.md"]), []);
    }

    #[test]
    fn input_src_counts_only_for_image_buttons() {
        let repo = TempRepo::new(&[(
            "b.md",
            "<input src=\"missing.png\"> <input type=\"IMAGE\" src=\"gone.png\">\n\n- [x] done\n",
        )]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(got, ["gone.png"]);
    }

    #[test]
    fn raw_same_level_end_tags_stay_in_the_heading() {
        assert_eq!(headings("# Hello </h1> world\n"), ["Hello  world"]);
    }

    #[cfg(unix)]
    #[test]
    fn markdown_symlinks_to_other_names_check_fragments() {
        let repo = TempRepo::new(&[
            ("real.txt", "# Real\n"),
            ("b.md", "[x](alias.md#missing)\n"),
        ]);
        let base = repo.0.join("repo");
        std::os::unix::fs::symlink("real.txt", base.join("alias.md")).unwrap();
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| e.reason.clone())
            .collect();
        assert_eq!(got, ["no heading with anchor #missing"]);
    }

    #[test]
    fn directory_fragments_check_the_readme() {
        let repo = TempRepo::new(&[
            ("docs/README.md", "# Docs\n"),
            ("plain/a.txt", "x"),
            (
                "b.md",
                "[ok](docs/#docs)\n[bad](docs/#nope)\n[none](plain/#x)\n",
            ),
        ]);
        let got: Vec<_> = repo
            .check(&["b.md"])
            .iter()
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(got, ["docs/#nope", "plain/#x"]);
    }

    #[cfg(unix)]
    #[test]
    fn markdown_symlinks_through_untracked_hops_fail() {
        let repo = TempRepo::new(&[("real.md", "# Real\n")]);
        let base = repo.0.join("repo");
        std::os::unix::fs::symlink("real.md", base.join("intermediate.md")).unwrap();
        std::os::unix::fs::symlink("intermediate.md", base.join("alias.md")).unwrap();
        let tracked: Vec<PathBuf> = ["alias.md", "real.md"].iter().map(PathBuf::from).collect();
        let errors = check_tracked(
            &base,
            &[PathBuf::from("alias.md"), PathBuf::from("real.md")],
            Some(&tracked),
        );
        let got: Vec<_> = errors
            .iter()
            .map(|e| (e.file.display().to_string(), e.reason.as_str()))
            .collect();
        assert_eq!(
            got,
            [(
                "alias.md".to_string(),
                "file is a symlink to a file not tracked by git"
            )]
        );
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
