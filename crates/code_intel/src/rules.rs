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

/// `local_scopes`: the ranges of the file's local scopes (functions and
/// blocks, not class or namespace bodies: `locals::ScopeKind::is_local`), by
/// start and outermost first; a definition strictly inside one is
/// `Visibility::File` in every language. None for a language pack, whose
/// scopes Lectern does not classify: a definition inside a function or
/// method definition is `File`.
pub(crate) fn finish(
    dialect: Dialect,
    source: &[u8],
    tags: Vec<RawTag>,
    local_scopes: Option<&[Range<usize>]>,
) -> Finished {
    finish_inner(dialect, source, tags, None, local_scopes, usize::MAX)
}

/// `finish` for the walkers (`crate::walk`): `tags` holds definitions and
/// markers only, `refs` the reference names, deduplicated, in byte order,
/// without the definitions' own names. The names markers keep as
/// references count only when they start before `limit` (a window's cut,
/// as the walk's own `limit`).
pub(crate) fn finish_with_refs(
    dialect: Dialect,
    source: &[u8],
    tags: Vec<RawTag>,
    refs: Vec<Box<str>>,
    local_scopes: &[Range<usize>],
    limit: usize,
) -> Finished {
    finish_inner(dialect, source, tags, Some(refs), Some(local_scopes), limit)
}

/// The Rust `impl` blocks as containers: an `Impl` marker names the
/// implemented type, an `ImplTrait` marker (on the `for` token) on the same
/// block makes it a trait implementation. A trait implementation whose type
/// has no name the queries take (`impl Trait for fn()`) is still one, with
/// no name to give as `scope`.
fn impl_containers(markers: &[(Marker, RawTag, Box<str>)]) -> Vec<Container> {
    let trait_impls: HashSet<(usize, usize)> = markers
        .iter()
        .filter(|(marker, _, _)| *marker == Marker::ImplTrait)
        .map(|(_, tag, _)| (tag.range.start, tag.range.end))
        .collect();
    let mut named: HashSet<(usize, usize)> = HashSet::new();
    let mut out: Vec<Container> = markers
        .iter()
        .filter(|(marker, _, _)| *marker == Marker::Impl)
        .map(|(_, tag, name)| {
            let key = (tag.range.start, tag.range.end);
            named.insert(key);
            Container {
                range: tag.range.clone(),
                name: name.clone(),
                def: None,
                trait_impl: trait_impls.contains(&key),
                parent: None,
            }
        })
        .collect();
    out.extend(
        markers
            .iter()
            .filter(|(marker, tag, _)| {
                *marker == Marker::ImplTrait && !named.contains(&(tag.range.start, tag.range.end))
            })
            .map(|(_, tag, _)| Container {
                range: tag.range.clone(),
                name: "".into(),
                def: None,
                trait_impl: true,
                parent: None,
            }),
    );
    out
}

/// For each container, whether its node lies strictly inside one of
/// `scopes` (nested ranges, by start and outermost first); a scope with the
/// container's own range is the scope the definition opens.
fn inside_scopes(containers: &[Container], scopes: &[Range<usize>]) -> Vec<bool> {
    let mut order: Vec<usize> = (0..containers.len()).collect();
    order.sort_by_key(|&i| {
        (
            containers[i].range.start,
            std::cmp::Reverse(containers[i].range.end),
        )
    });
    let mut out = vec![false; containers.len()];
    let mut open: Vec<&Range<usize>> = Vec::new();
    let mut next = 0;
    for i in order {
        let r = &containers[i].range;
        // Open the scopes that start before the definition, or at its start
        // and end after it.
        while let Some(s) = scopes.get(next)
            && (s.start < r.start || s.start == r.start && s.end > r.end)
        {
            while open.last().is_some_and(|t| t.end <= s.start) {
                open.pop();
            }
            open.push(s);
            next += 1;
        }
        while open.last().is_some_and(|t| t.end <= r.start) {
            open.pop();
        }
        out[i] = open
            .iter()
            .any(|t| t.start <= r.start && r.end <= t.end && *t != r);
    }
    out
}

fn finish_inner(
    dialect: Dialect,
    source: &[u8],
    tags: Vec<RawTag>,
    ref_names: Option<Vec<Box<str>>>,
    local_scopes: Option<&[Range<usize>]>,
    limit: usize,
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
        .chain(impl_containers(&markers))
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
    // Local to the file: a definition inside a function or block. A member
    // (a field or method, a TS parameter property inside its constructor
    // included) is as local as the class or `impl` block it belongs to; a
    // member whose nearest container is a function or method belongs to an
    // anonymous class there, and is local by its own position.
    let local: Vec<bool> = match local_scopes {
        Some(scopes) => {
            let inside = inside_scopes(&containers, scopes);
            let kind_of = |c: usize| containers[c].def.map(|d| defs[d].1);
            (0..defs.len())
                .map(|i| {
                    let mut c = i;
                    while matches!(kind_of(c), Some(DefKind::Field | DefKind::Method))
                        && let Some(p) = containers[c].parent
                        && !matches!(kind_of(p), Some(DefKind::Function | DefKind::Method))
                    {
                        c = p;
                    }
                    inside[c]
                })
                .collect()
        }
        None => (0..defs.len())
            .map(|i| {
                Ancestors {
                    containers: &containers,
                    next: parent_of_def[i],
                }
                .any(|c| {
                    c.def
                        .is_some_and(|d| matches!(defs[d].1, DefKind::Function | DefKind::Method))
                })
            })
            .collect(),
    };
    let mut out: Vec<FileDef> = defs
        .iter()
        .enumerate()
        .map(|(i, (tag, kind, name))| {
            let ancestors = Ancestors {
                containers: &containers,
                next: parent_of_def[i],
            };
            let mut scope = parent_of_def[i]
                .map(|p| containers[p].name.clone())
                .filter(|n| !n.is_empty());
            if dialect == Dialect::Go
                && let Some(owner) = rules.owner(&tag.range)
            {
                scope = Some(owner.into());
            }
            // Inside a function or block: local to the file, in every
            // language (docs/design/code_intel.md「抽取结果」).
            let visibility = if local[i] {
                Visibility::File
            } else {
                rules.visibility(
                    tag,
                    *kind,
                    name,
                    ancestors.map(|c| (c, c.def.map(|d| (&defs[d].0, defs[d].1)))),
                )
            };
            let arity = matches!(kind, DefKind::Function | DefKind::Method)
                .then(|| tag.params.as_deref().map(|p| count_params(p, dialect)))
                .flatten();
            FileDef {
                def: Def {
                    name: name.clone(),
                    kind: *kind,
                    line: tag.row,
                    column: tag.column,
                    byte: tag.name_range.start as u64,
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
    let mut refs = refs;
    if dialect == Dialect::Python {
        let kind_of = |c: usize| containers[c].def.map(|d| defs[d].1);
        // Module level is no function or class scope around: an assignment
        // elsewhere binds a local or a class member, not a module constant.
        // A chained assignment (`A = B = None`) is no one's scope.
        let mut keep = vec![true; out.len()];
        for i in 0..out.len() {
            if defs[i].1 != DefKind::Constant {
                continue;
            }
            out[i].def.scope = None;
            let in_class = Ancestors {
                containers: &containers,
                next: parent_of_def[i],
            }
            .any(|c| c.def.is_some_and(|d| defs[d].1 == DefKind::Class));
            keep[i] = !local[i] && !in_class;
        }
        // The class attributes already defined in a class body.
        let mut fields: HashSet<(usize, &str)> = (0..defs.len())
            .filter(|&i| defs[i].1 == DefKind::Field && keep[i])
            .filter_map(|i| Some((parent_of_def[i]?, &*defs[i].2)))
            .collect();
        let mut i = 0;
        out.retain(|_| {
            i += 1;
            keep[i - 1]
        });
        // Instance attributes: `obj.name = …` in a method whose first
        // parameter is `obj` (also in a function nested in it) is a field of
        // the method's class; any other attribute assignment is a member
        // reference, as without the marker.
        let receivers: std::collections::HashMap<(usize, usize), &str> = markers
            .iter()
            .filter(|(marker, _, _)| *marker == Marker::Receiver)
            .map(|(_, tag, name)| ((tag.range.start, tag.range.end), &**name))
            .collect();
        let starts: Vec<usize> = order.iter().map(|&c| containers[c].range.start).collect();
        let innermost = |range: &Range<usize>| -> Option<usize> {
            let j = starts.partition_point(|&s| s <= range.start);
            let mut c = order[j.checked_sub(1)?];
            loop {
                let r = &containers[c].range;
                if r.start <= range.start && range.end <= r.end {
                    return Some(c);
                }
                c = containers[c].parent?;
            }
        };
        for (marker, tag, name) in &markers {
            if *marker != Marker::Attribute {
                continue;
            }
            let class = innermost(&tag.range).and_then(|mut m| {
                while kind_of(m) == Some(DefKind::Function) {
                    m = containers[m].parent?;
                }
                let receiver = (kind_of(m) == Some(DefKind::Method))
                    .then(|| receivers.get(&(containers[m].range.start, containers[m].range.end)))
                    .flatten()?;
                let object = source
                    .get(tag.range.start..tag.name_range.start)
                    .and_then(|b| std::str::from_utf8(b).ok())?
                    .trim_end()
                    .strip_suffix('.')?
                    .trim();
                let class = containers[m].parent?;
                (object == *receiver && kind_of(class) == Some(DefKind::Class)).then_some(class)
            });
            let Some(class) = class else {
                continue;
            };
            if !fields.insert((class, &**name)) {
                continue;
            }
            out.push(FileDef {
                def: Def {
                    name: name.clone(),
                    kind: DefKind::Field,
                    line: tag.row,
                    column: tag.column,
                    byte: tag.name_range.start as u64,
                    end_line: lines.last_row(tag.range.start, tag.range.end),
                    scope: Some(containers[class].name.clone()),
                    visibility: if local[class] {
                        Visibility::File
                    } else {
                        Visibility::Unknown
                    },
                    arity: None,
                },
                name_range: tag.name_range.clone(),
                range: tag.range.clone(),
            });
        }
    }
    // A marker that takes a name node from the reference patterns (an
    // exported name, an implemented or receiver type, an attribute write)
    // leaves the name a reference name, unless it became the definition at
    // that position (`Marker::consumes_name`).
    let def_names: HashSet<(usize, usize)> = out
        .iter()
        .map(|d| (d.name_range.start, d.name_range.end))
        .collect();
    let before = refs.len();
    refs.extend(
        markers
            .iter()
            .filter(|(marker, tag, _)| {
                marker.consumes_name()
                    && tag.name_range.start < limit
                    && !def_names.contains(&(tag.name_range.start, tag.name_range.end))
            })
            .map(|(_, _, name)| name.clone()),
    );
    if refs.len() != before {
        refs.sort_unstable();
        refs.dedup();
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
    /// JS / TS export statements and `declare global` blocks, by start;
    /// they do not overlap (all are at program level).
    exports: Vec<Range<usize>>,
    export_names: HashSet<&'a str>,
    /// JS / TS: the file has import or export statements.
    es_module: bool,
    /// Go: method range → receiver type.
    owners: std::collections::HashMap<(usize, usize), &'a str>,
}

impl<'a> FileRules<'a> {
    fn new(dialect: Dialect, markers: &'a [(Marker, RawTag, Box<str>)]) -> FileRules<'a> {
        let mut rules = FileRules {
            dialect,
            modifier_starts: HashSet::new(),
            exports: Vec::new(),
            export_names: HashSet::new(),
            es_module: false,
            owners: std::collections::HashMap::new(),
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
                // `declare global { … }`: its declarations are global, as
                // visible as exported ones.
                Marker::Global => rules.exports.push(tag.range.clone()),
                Marker::ExportName => {
                    rules.export_names.insert(name);
                    rules.es_module = true;
                }
                Marker::Module => rules.es_module = true,
                Marker::Owner => {
                    rules
                        .owners
                        .insert((tag.range.start, tag.range.end), &**name);
                }
                Marker::Impl | Marker::ImplTrait | Marker::Receiver | Marker::Attribute => {}
            }
        }
        rules.exports.sort_by_key(|r| r.start);
        rules
    }

    fn owner(&self, range: &Range<usize>) -> Option<&'a str> {
        self.owners.get(&(range.start, range.end)).copied()
    }

    /// Whether `range` lies in an export statement or `declare global`.
    fn in_export(&self, range: &Range<usize>) -> bool {
        let i = self.exports.partition_point(|r| r.start <= range.start);
        i > 0 && range.end <= self.exports[i - 1].end
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
                let exported = self.in_export(top.0) || self.export_names.contains(top.1);
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
