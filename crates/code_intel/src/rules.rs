//! From tags to definitions: scopes, visibility and parameter counts, by
//! per-language rules (docs/design/code_intel.md「抽取结果」).
//!
//! tree-sitter-tags hands out tags, not the syntax tree, so the rules read
//! what the queries capture for them: `reference.lectern.*` markers (`pub`,
//! `private`, export statements and names, `impl` blocks, Go receivers) and
//! the parameter list captured as `@doc`.

use std::collections::HashSet;
use std::ops::Range;

use crate::DefKind;
use crate::extract::{Def, Visibility};
use crate::lang::{Dialect, Marker, TagClass};
use crate::util::LineIndex;

/// One tag, from tree-sitter-tags or from `ParsedFile`'s own query run.
#[derive(Clone, Debug)]
pub(crate) struct RawTag {
    pub class: TagClass,
    /// The definition or reference node, widened to cover the name.
    pub range: Range<usize>,
    pub name_range: Range<usize>,
    /// Row and byte column of the name's start.
    pub row: u32,
    pub column: u32,
    /// The text of the `@doc` capture: a definition's parameter list.
    pub params: Option<String>,
}

/// A definition with what the rules worked out, and where its name is.
#[derive(Clone, Debug)]
pub(crate) struct FileDef {
    pub def: Def,
    pub name_range: Range<usize>,
    pub range: Range<usize>,
}

/// The rules' output for one file.
pub(crate) struct Finished {
    /// By name position.
    pub defs: Vec<FileDef>,
    /// Reference names, deduplicated, in byte order.
    pub refs: Vec<Box<str>>,
}

/// A container of definitions: a definition, or an `impl` block marker.
struct Container {
    range: Range<usize>,
    name: Box<str>,
    /// Index into `defs` for a definition; None for an `impl` block.
    def: Option<usize>,
    /// A trait `impl` block: its methods are public.
    trait_impl: bool,
    parent: Option<usize>,
}

pub(crate) fn finish(dialect: Dialect, source: &[u8], tags: Vec<RawTag>) -> Finished {
    finish_inner(dialect, source, tags, None)
}

/// `finish` for the walkers (`crate::walk`): `tags` holds definitions and
/// markers only, `refs` the reference names, deduplicated, in byte order,
/// without the definitions' own names.
pub(crate) fn finish_with_refs(
    dialect: Dialect,
    source: &[u8],
    tags: Vec<RawTag>,
    refs: Vec<Box<str>>,
) -> Finished {
    finish_inner(dialect, source, tags, Some(refs))
}

fn finish_inner(
    dialect: Dialect,
    source: &[u8],
    tags: Vec<RawTag>,
    ref_names: Option<Vec<Box<str>>>,
) -> Finished {
    let text = |r: &Range<usize>| {
        source
            .get(r.clone())
            .and_then(|b| std::str::from_utf8(b).ok())
    };
    let mut defs: Vec<(RawTag, DefKind, Box<str>)> = Vec::new();
    let mut refs: Vec<(Range<usize>, Box<str>)> = Vec::new();
    let mut markers: Vec<(Marker, RawTag, Box<str>)> = Vec::new();
    for tag in tags {
        let Some(name) = text(&tag.name_range) else {
            continue;
        };
        let name: Box<str> = name.into();
        match tag.class {
            TagClass::Def(kind) => defs.push((tag, kind, name)),
            TagClass::Ref => refs.push((tag.name_range, name)),
            TagClass::Marker(marker) => markers.push((marker, tag, name)),
        }
    }
    // One definition per name node. tree-sitter-tags keeps the tag of the
    // first pattern only while both matches are still queued, so a later
    // match can add a second one (a method also matched as a function); the
    // more specific kind wins, as the earlier pattern would.
    defs.sort_by_key(|(tag, kind, _)| {
        (
            tag.name_range.start,
            tag.name_range.end,
            std::cmp::Reverse(specificity(*kind)),
        )
    });
    defs.dedup_by(|b, a| a.0.name_range == b.0.name_range);
    let refs = ref_names.unwrap_or_else(|| {
        // A definition's own name is not a reference to it.
        let def_names: HashSet<(usize, usize)> = defs
            .iter()
            .map(|(tag, _, _)| (tag.name_range.start, tag.name_range.end))
            .collect();
        let mut refs: Vec<Box<str>> = refs
            .into_iter()
            .filter(|(r, _)| !def_names.contains(&(r.start, r.end)))
            .map(|(_, name)| name)
            .collect();
        refs.sort_unstable();
        refs.dedup();
        refs
    });

    // Containers: definitions and `impl` blocks, outermost first.
    let mut containers: Vec<Container> = defs
        .iter()
        .enumerate()
        .map(|(i, (tag, _, name))| Container {
            range: tag.range.clone(),
            name: name.clone(),
            def: Some(i),
            trait_impl: false,
            parent: None,
        })
        .chain(
            markers
                .iter()
                .filter(|(marker, _, _)| matches!(marker, Marker::Impl | Marker::ImplTrait))
                .map(|(marker, tag, name)| Container {
                    range: tag.range.clone(),
                    name: name.clone(),
                    def: None,
                    trait_impl: *marker == Marker::ImplTrait,
                    parent: None,
                }),
        )
        .collect();
    let mut order: Vec<usize> = (0..containers.len()).collect();
    order.sort_by_key(|&i| {
        let c = &containers[i];
        (
            c.range.start,
            std::cmp::Reverse(c.range.end),
            c.def.is_some(),
        )
    });
    let mut stack: Vec<usize> = Vec::new();
    let mut parent_of_def = vec![None; defs.len()];
    for &i in &order {
        let range = containers[i].range.clone();
        while let Some(&top) = stack.last() {
            // Definitions on one node (`const A, B = 1, 2`, `int a, b;`) are
            // siblings, not each other's scope.
            let sibling = containers[top].range == range
                && containers[top].def.is_some()
                && containers[i].def.is_some();
            if containers[top].range.end >= range.end
                && containers[top].range.start <= range.start
                && !sibling
            {
                break;
            }
            stack.pop();
        }
        let parent = stack.last().copied();
        containers[i].parent = parent;
        if let Some(d) = containers[i].def {
            parent_of_def[d] = parent;
        }
        stack.push(i);
    }

    let rules = FileRules::new(dialect, &markers);
    let lines = LineIndex::new(source);
    let mut out: Vec<FileDef> = defs
        .iter()
        .enumerate()
        .map(|(i, (tag, kind, name))| {
            let ancestors = Ancestors {
                containers: &containers,
                next: parent_of_def[i],
            };
            let mut scope = parent_of_def[i].map(|p| containers[p].name.clone());
            if dialect == Dialect::Go
                && let Some(owner) = rules.owner(&tag.range)
            {
                scope = Some(owner.into());
            }
            let visibility = rules.visibility(
                tag,
                *kind,
                name,
                ancestors.map(|c| (c, c.def.map(|d| (&defs[d].0, defs[d].1)))),
            );
            let arity = matches!(kind, DefKind::Function | DefKind::Method)
                .then(|| tag.params.as_deref().map(|p| count_params(p, dialect)))
                .flatten();
            FileDef {
                def: Def {
                    name: name.clone(),
                    kind: *kind,
                    line: tag.row,
                    column: tag.column,
                    end_line: lines.last_row(tag.range.start, tag.range.end),
                    scope,
                    visibility,
                    arity,
                },
                name_range: tag.name_range.clone(),
                range: tag.range.clone(),
            }
        })
        .collect();
    // Enum members (Rust variants, Java constants) are as visible as their
    // enum; JS / TS members already take the outermost definition's.
    if matches!(dialect, Dialect::Rust | Dialect::Java) {
        for i in 0..out.len() {
            let parent = parent_of_def[i].and_then(|p| containers[p].def);
            if let Some(p) = parent
                && defs[i].1 == DefKind::Constant
                && defs[p].1 == DefKind::Enum
            {
                out[i].def.visibility = out[p].def.visibility;
            }
        }
    }
    out.sort_by_key(|d| (d.def.line, d.def.column, d.range.start));
    Finished { defs: out, refs }
}

/// Which of two definitions of one name node to keep: the queries list the
/// specific patterns (methods, classes) before the general ones (functions,
/// then constants and variables).
fn specificity(kind: DefKind) -> u8 {
    match kind {
        DefKind::Constant | DefKind::Variable | DefKind::Other => 0,
        DefKind::Function => 1,
        _ => 2,
    }
}

struct Ancestors<'a> {
    containers: &'a [Container],
    next: Option<usize>,
}

impl<'a> Iterator for Ancestors<'a> {
    type Item = &'a Container;

    fn next(&mut self) -> Option<&'a Container> {
        let c = &self.containers[self.next?];
        self.next = c.parent;
        Some(c)
    }
}

/// The markers of one file, indexed for the visibility rules.
struct FileRules<'a> {
    dialect: Dialect,
    /// Rust `pub`, Java `private`: where the modifier starts.
    modifier_starts: HashSet<usize>,
    /// JS / TS export statements.
    exports: Vec<Range<usize>>,
    export_names: HashSet<&'a str>,
    /// JS / TS: the file has import or export statements.
    es_module: bool,
    /// Go: method range → receiver type.
    owners: Vec<(Range<usize>, &'a str)>,
}

impl<'a> FileRules<'a> {
    fn new(dialect: Dialect, markers: &'a [(Marker, RawTag, Box<str>)]) -> FileRules<'a> {
        let mut rules = FileRules {
            dialect,
            modifier_starts: HashSet::new(),
            exports: Vec::new(),
            export_names: HashSet::new(),
            es_module: false,
            owners: Vec::new(),
        };
        for (marker, tag, name) in markers {
            match marker {
                Marker::Pub | Marker::Private => {
                    rules.modifier_starts.insert(tag.range.start);
                }
                Marker::Export => {
                    rules.exports.push(tag.range.clone());
                    rules.es_module = true;
                }
                Marker::ExportName => {
                    rules.export_names.insert(name);
                    rules.es_module = true;
                }
                Marker::Module => rules.es_module = true,
                Marker::Owner => rules.owners.push((tag.range.clone(), name)),
                Marker::Impl | Marker::ImplTrait => {}
            }
        }
        rules
    }

    fn owner(&self, range: &Range<usize>) -> Option<&'a str> {
        self.owners
            .iter()
            .find(|(r, _)| r == range)
            .map(|(_, name)| *name)
    }

    fn visibility<'c>(
        &self,
        tag: &RawTag,
        kind: DefKind,
        name: &str,
        ancestors: impl Iterator<Item = (&'c Container, Option<(&'c RawTag, DefKind)>)>,
    ) -> Visibility {
        match self.dialect {
            Dialect::Python | Dialect::Pack => Visibility::Unknown,
            Dialect::Go => {
                if name.chars().next().is_some_and(char::is_uppercase) {
                    Visibility::Public
                } else {
                    Visibility::Package
                }
            }
            Dialect::Java => {
                if self.modifier_starts.contains(&tag.range.start) {
                    Visibility::File
                } else {
                    Visibility::Public
                }
            }
            Dialect::Rust => {
                if self.modifier_starts.contains(&tag.range.start) || kind == DefKind::Macro {
                    return Visibility::Public;
                }
                // Members of a trait, or of a trait impl, are as visible as
                // the trait.
                for (container, def) in ancestors {
                    if container.trait_impl || def.is_some_and(|(_, k)| k == DefKind::Trait) {
                        return Visibility::Public;
                    }
                }
                Visibility::Module
            }
            Dialect::TypeScript | Dialect::Tsx | Dialect::JavaScript => {
                // Inside a function body: local to the file. Otherwise as
                // visible as the outermost definition around it.
                let mut top = (&tag.range, name);
                for (container, def) in ancestors {
                    if let Some((def_tag, def_kind)) = def {
                        if matches!(def_kind, DefKind::Function | DefKind::Method) {
                            return Visibility::File;
                        }
                        top = (&def_tag.range, &container.name);
                    }
                }
                let exported = self
                    .exports
                    .iter()
                    .any(|r| r.start <= top.0.start && top.0.end <= r.end)
                    || self.export_names.contains(top.1);
                if exported || !self.es_module {
                    // A script or CommonJS file: its top level is shared.
                    Visibility::Public
                } else {
                    Visibility::File
                }
            }
        }
    }
}

/// The number of parameters in a parameter list's text, up to 255. A list
/// without parentheses is a single parameter (`x => x`). Rust `self`
/// receivers and Python's `*` and `/` separators do not count.
pub(crate) fn count_params(text: &str, dialect: Dialect) -> u8 {
    let text = text.trim();
    let Some(inner) = text.strip_prefix('(') else {
        return u8::from(!text.is_empty());
    };
    let angles = matches!(
        dialect,
        Dialect::Rust | Dialect::Java | Dialect::TypeScript | Dialect::Tsx
    );
    let hash_comments = dialect == Dialect::Python;
    let quotes: &[u8] = match dialect {
        Dialect::Rust => b"\"",
        Dialect::Python | Dialect::Java => b"\"'",
        _ => b"\"'`",
    };
    let bytes = inner.as_bytes();
    let mut segments: Vec<&str> = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if quotes.contains(&b) {
            i += 1;
            while i < bytes.len() && bytes[i] != b {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
        } else if hash_comments && b == b'#'
            || !hash_comments && b == b'/' && bytes.get(i + 1) == Some(&b'/')
        {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        } else if !hash_comments && b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 1;
        } else if matches!(b, b'(' | b'[' | b'{') || angles && b == b'<' {
            depth += 1;
        } else if matches!(b, b')' | b']' | b'}')
            || angles
                && b == b'>'
                && !matches!(i.checked_sub(1).map(|j| bytes[j]), Some(b'-' | b'='))
        {
            if depth == 0 {
                // The closing parenthesis of the list.
                segments.push(&inner[start..i]);
                start = bytes.len();
                break;
            }
            depth -= 1;
        } else if b == b',' && depth == 0 {
            segments.push(&inner[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    if start < bytes.len() {
        segments.push(&inner[start..]);
    }
    let count = segments
        .iter()
        .map(|s| strip_comments(s, hash_comments))
        .filter(|s| !s.is_empty())
        .filter(|s| match dialect {
            Dialect::Rust => !is_self_receiver(s),
            Dialect::Python => *s != "*" && *s != "/",
            _ => true,
        })
        .count();
    u8::try_from(count).unwrap_or(u8::MAX)
}

/// A segment without leading line comments, trimmed (enough to tell an
/// empty trailing segment from a parameter).
fn strip_comments(segment: &str, hash_comments: bool) -> &str {
    let mut s = segment.trim();
    loop {
        let marker = if hash_comments { "#" } else { "//" };
        if let Some(rest) = s.strip_prefix(marker) {
            s = rest.split_once('\n').map_or("", |(_, r)| r).trim();
        } else if let Some(rest) = s.strip_prefix("/*").filter(|_| !hash_comments) {
            s = rest.split_once("*/").map_or("", |(_, r)| r).trim();
        } else {
            return s;
        }
    }
}

/// `self`, `mut self`, `&self`, `&'a mut self`, `self: Box<Self>`.
fn is_self_receiver(segment: &str) -> bool {
    let mut s = segment.trim_start();
    if let Some(rest) = s.strip_prefix('&') {
        s = rest.trim_start();
        if let Some(rest) = s.strip_prefix('\'') {
            s = rest
                .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_')
                .trim_start();
        }
    }
    if let Some(rest) = s.strip_prefix("mut ") {
        s = rest.trim_start();
    }
    s.strip_prefix("self")
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}
