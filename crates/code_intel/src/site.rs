//! One file parsed on demand, and what is at a position in it: the
//! identifier, its reference syntax, the local definition it binds to, and
//! the definition whose name it is (docs/design/code_intel.md「抽取结果」
//! `ParsedFile`, 「跳转到定义」 step 1, 「候选的分层与过滤」).
//!
//! Unlike [`crate::extract::extract`], this keeps the syntax tree, so it
//! parses with its own parser and checks the deadline in the progress
//! callback instead of through the watchdog.

use std::collections::HashMap;
use std::ops::{ControlFlow, Range};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, ParseOptions, Parser, QueryCursor, QueryCursorOptions, Tree};

pub use crate::Position;
use crate::extract::{Def, Lang};
use crate::lang::{Capture, Dialect, TagClass};
use crate::locals::{Collector, LocalDef, Locals, Namespace};
use crate::rules::{self, FileDef, RawTag};
use crate::util::LineIndex;
use crate::watchdog::Clock;
use crate::{DefKind, RefSyntax};

/// Why parsing gave up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseError {
    TimedOut,
    Cancelled,
    /// The grammar or its queries do not compile.
    Unsupported,
}

/// What is at a position.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Site {
    pub name: Box<str>,
    pub syntax: RefSyntax,
    /// Where the identifier starts.
    pub at: Position,
    /// The local declaration this reference binds to under the binding rules
    /// (`Binding::Local`): certain, so a direct jump that hides the index
    /// candidates. None on a definition's own name.
    pub binding: Option<LocalBinding>,
    /// Same-named declarations in the scopes around the reference that may
    /// be the target, with no certain binding: declared later in a
    /// non-hoisting form, flow-scoped, or behind `global` / `nonlocal`.
    /// When this is not empty there is no direct jump, to this file or
    /// another: the query lists these first, then the index candidates
    /// (docs/design/code_intel.md「绑定规则」).
    pub possible: Vec<LocalBinding>,
    /// The definition whose name this is, when the position is on one.
    pub defines: Option<SiteDef>,
}

impl Site {
    /// Whether the query may jump directly to a definition in another file:
    /// no local binding, and no same-file declaration that might be it.
    pub fn may_jump_elsewhere(&self) -> bool {
        self.binding.is_none() && self.possible.is_empty()
    }
}

/// A definition named at the position.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SiteDef {
    pub def: Def,
    /// A `@local.definition` in a non-root scope: visible in that scope only
    /// (`DefinitionRef::local`).
    pub local: bool,
}

/// A local definition a reference binds to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LocalBinding {
    pub name: Box<str>,
    pub at: Position,
    /// The definition when tags captured it too; a plain local otherwise.
    pub def: Option<Def>,
    /// Visible in one function or block only: not at file level, not a
    /// class member.
    pub local: bool,
}

/// A reference position: tags references and `@local.reference`, merged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RefSite {
    pub at: Position,
    pub start: usize,
    pub end: usize,
}

/// The local scopes of `source` by the language's locals query
/// ([`Locals::local_scopes`]), for `rules::finish` on the tags path; None
/// once `stop` returns true, or when the grammar or its queries do not
/// compile.
pub(crate) fn local_scopes(
    lang: &Lang,
    source: &[u8],
    stop: &dyn Fn() -> bool,
) -> Option<Vec<Range<usize>>> {
    let compiled = lang.compiled()?;
    let mut parser = Parser::new();
    parser.set_language(lang.language()).ok()?;
    let mut progress = |_: &tree_sitter::ParseState| {
        if stop() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let tree = parser.parse_with_options(
        &mut |i, _| source.get(i..).unwrap_or(&[]),
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    )?;
    let query = &compiled.tags.query;
    let mut cursor = QueryCursor::new();
    let mut query_progress = |_: &tree_sitter::QueryCursorState| {
        if stop() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut matches = cursor.matches_with_options(
        query,
        tree.root_node(),
        source,
        QueryCursorOptions::new().progress_callback(&mut query_progress),
    );
    let mut collector = Collector::new(lang.dialect(), source.len());
    while let Some(m) = matches.next() {
        if m.pattern_index >= compiled.locals_patterns {
            continue;
        }
        let props = query.property_settings(m.pattern_index);
        for capture in m.captures() {
            if matches!(
                compiled.captures[capture.index as usize],
                Capture::LocalScope
            ) {
                collector.scope(capture.node, props);
            }
        }
    }
    drop(matches);
    if stop() {
        return None;
    }
    Some(collector.finish(source).local_scopes())
}

/// One parsed file, kept while a query uses it and then dropped; never
/// cached. Holds the grammar's lease through its [`Lang`].
///
/// Fields drop in declaration order: `lang` (and with the last one the
/// grammar's lease) comes after `tree`, so a language pack's library is never
/// unloaded while the tree built from it is being freed.
pub struct ParsedFile {
    tree: Tree,
    #[cfg(test)]
    _tree_dropped: crate::testing::DropTrace,
    source: Arc<[u8]>,
    lines: LineIndex,
    defs: Vec<FileDef>,
    locals: Locals,
    refs: Vec<RefSite>,
    lang: Lang,
}

impl ParsedFile {
    /// Parses `source` and runs the language's locals and tags query on the
    /// tree. Blocking; any thread but the UI thread. Gives up once `clock`
    /// reaches `at` or `cancel` is set.
    pub fn parse(
        lang: &Lang,
        source: Arc<[u8]>,
        clock: &dyn Clock,
        at: Instant,
        cancel: &AtomicBool,
    ) -> Result<ParsedFile, ParseError> {
        let compiled = lang.compiled().ok_or(ParseError::Unsupported)?;
        let dialect = lang.dialect();
        let stop = || cancel.load(Ordering::Acquire) || clock.now() >= at;
        let interrupted = || {
            if cancel.load(Ordering::Acquire) {
                ParseError::Cancelled
            } else {
                ParseError::TimedOut
            }
        };
        // A deadline already past or a cancellation already set: no work. A
        // small file can be parsed before the progress callback runs.
        if stop() {
            return Err(interrupted());
        }
        let mut parser = Parser::new();
        parser
            .set_language(lang.language())
            .map_err(|_| ParseError::Unsupported)?;
        let text = &*source;
        let mut progress = |_: &tree_sitter::ParseState| {
            if stop() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let tree = parser
            .parse_with_options(
                &mut |i, _| text.get(i..).unwrap_or(&[]),
                None,
                Some(ParseOptions::new().progress_callback(&mut progress)),
            )
            .ok_or_else(interrupted)?;

        let query = &compiled.tags.query;
        let mut cursor = QueryCursor::new();
        let mut query_progress = |_: &tree_sitter::QueryCursorState| {
            if stop() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut matches = cursor.matches_with_options(
            query,
            tree.root_node(),
            text,
            QueryCursorOptions::new().progress_callback(&mut query_progress),
        );
        let mut collector = Collector::new(dialect, text.len());
        let mut ref_ranges: Vec<Range<usize>> = Vec::new();
        // One tag per name node, from the lowest pattern (tree-sitter-tags).
        let mut tags: HashMap<(usize, usize), (usize, RawTag)> = HashMap::new();
        while let Some(m) = matches.next() {
            if m.pattern_index < compiled.locals_patterns {
                let props = query.property_settings(m.pattern_index);
                for capture in m.captures() {
                    match compiled.captures[capture.index as usize] {
                        Capture::LocalScope => collector.scope(capture.node, props),
                        Capture::LocalDefinition => {
                            collector.definition(capture.node, props, text);
                        }
                        // `(identifier) @local.reference` also matches member
                        // and path names (`items.add`, `Queue::new`), which
                        // never refer to a local.
                        Capture::LocalReference
                            if collector.reference(capture.node, props)
                                && is_variable_position(dialect, capture.node) =>
                        {
                            ref_ranges.push(capture.node.byte_range());
                        }
                        _ => {}
                    }
                }
                continue;
            }
            let mut name: Option<Node<'_>> = None;
            let mut tag: Option<(Node<'_>, TagClass)> = None;
            let mut params: Option<Node<'_>> = None;
            let mut ignored = false;
            for capture in m.captures() {
                match compiled.captures[capture.index as usize] {
                    Capture::Name => name = Some(capture.node),
                    Capture::Doc => params = Some(capture.node),
                    Capture::Ignore => ignored = true,
                    Capture::Tag(class) => tag = Some((capture.node, class)),
                    _ => {}
                }
            }
            let (Some(name), Some((node, class)), false) = (name, tag, ignored) else {
                continue;
            };
            if name.has_error() {
                continue;
            }
            let name_range = name.byte_range();
            let node_range = node.byte_range();
            let raw = RawTag {
                class,
                range: node_range.start.min(name_range.start)..node_range.end.max(name_range.end),
                name_range: name_range.clone(),
                row: u32::try_from(name.start_position().row).unwrap_or(u32::MAX),
                column: u32::try_from(name.start_position().column).unwrap_or(u32::MAX),
                params: params
                    .and_then(|p| p.utf8_text(text).ok())
                    .map(str::to_owned),
            };
            let key = (name_range.start, name_range.end);
            match tags.get(&key) {
                Some((pattern, _)) if *pattern <= m.pattern_index => {}
                _ => {
                    tags.insert(key, (m.pattern_index, raw));
                }
            }
        }
        drop(matches);
        if stop() {
            return Err(interrupted());
        }

        let locals = collector.finish(text);

        let mut tags: Vec<RawTag> = tags.into_values().map(|(_, t)| t).collect();
        // A core language's definitions and markers come from its walker,
        // as in `extract` (ADR 0031); the query's tags give the reference
        // positions.
        if lang.lease().is_static() && crate::walk::supports(dialect, lang.language()) {
            // The walk stops with the same deadline and cancellation as the
            // parse and the query run.
            let walked = crate::walk::walk_tree(dialect, lang.language(), &tree, text, &stop)
                .ok_or_else(interrupted)?;
            tags.retain(|t| t.class == TagClass::Ref);
            tags.extend(walked.tags);
        }
        tags.sort_by_key(|t| t.name_range.start);
        ref_ranges.extend(
            tags.iter()
                .filter(|t| t.class == TagClass::Ref)
                .map(|t| t.name_range.clone()),
        );
        let lines = LineIndex::new(text);
        ref_ranges.sort_by_key(|r| (r.start, r.end));
        ref_ranges.dedup();
        // `(identifier) @local.reference` matches definition names too.
        let named: std::collections::HashSet<(usize, usize)> = tags
            .iter()
            .filter(|t| matches!(t.class, TagClass::Def(_)))
            .map(|t| (t.name_range.start, t.name_range.end))
            .chain(locals.defs().iter().map(|l| (l.range.start, l.range.end)))
            .collect();
        let refs = ref_ranges
            .into_iter()
            .filter(|r| !named.contains(&(r.start, r.end)))
            .map(|r| RefSite {
                at: position(&lines, r.start, text),
                start: r.start,
                end: r.end,
            })
            .collect();
        // A language pack's scopes are not classified (`rules::finish`).
        let local_scopes = lang.lease().is_static().then(|| locals.local_scopes());
        let finished = rules::finish(lang.dialect(), text, tags, local_scopes.as_deref());
        // Checked once more before returning: the rules and the binding model
        // run after the last check above.
        if stop() {
            return Err(interrupted());
        }
        Ok(ParsedFile {
            tree,
            #[cfg(test)]
            _tree_dropped: crate::testing::DropTrace("tree"),
            source,
            lines,
            defs: finished.defs,
            locals,
            refs,
            lang: lang.clone(),
        })
    }

    pub fn lang(&self) -> &Lang {
        &self.lang
    }

    pub fn source(&self) -> &Arc<[u8]> {
        &self.source
    }

    /// The definitions with their node ranges, and the local scopes of the
    /// binding model, for the tests.
    #[cfg(test)]
    pub(crate) fn defs_and_local_scopes(&self) -> (&[FileDef], Vec<Range<usize>>) {
        (&self.defs, self.locals.local_scopes())
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// The definitions, in name position order.
    pub fn defs(&self) -> impl Iterator<Item = &Def> + '_ {
        self.defs.iter().map(|d| &d.def)
    }

    /// Every reference position: tags references and `@local.reference`,
    /// deduplicated by position. Tags leaves out references that locals
    /// resolves; the union keeps them.
    pub fn references(&self) -> &[RefSite] {
        &self.refs
    }

    /// The identifier at a position (or just before it, with the cursor at
    /// its end) and what it is; None when there is no identifier there
    /// (`QueryError::NoSymbol`).
    pub fn site_at(&self, at: Position) -> Option<Site> {
        let text = &*self.source;
        let offset = self.lines.offset(at.line, at.column, text.len())?;
        let dialect = self.lang.dialect();
        let root = self.tree.root_node();
        let node = [Some(offset), offset.checked_sub(1)]
            .into_iter()
            .flatten()
            .filter_map(|o| root.descendant_for_byte_range(o, o))
            .find(|n| is_identifier(dialect, n.kind()))?;
        let range = node.byte_range();
        let name: Box<str> = node.utf8_text(text).ok()?.into();
        let syntax = syntax_at(dialect, node);
        let start = position(&self.lines, range.start, text);
        if let Some(defines) = self.defines(&range, &name, start) {
            return Some(Site {
                name,
                syntax,
                at: start,
                binding: None,
                possible: Vec::new(),
                defines: Some(defines),
            });
        }
        // A member or path name (`items.add`, `obj.x`, `Queue::new`,
        // `a::B`) never binds to a same-named local. A type name looks for
        // local types (type parameters, local type declarations), any other
        // name for local values.
        let namespace = if node.kind() == "type_identifier" {
            Namespace::Type
        } else {
            Namespace::Value
        };
        let (binding, possible) = if is_variable_position(dialect, node)
            && !matches!(syntax, RefSyntax::Member | RefSyntax::MemberCall)
        {
            let resolution = self.locals.resolve(text, &range, namespace);
            (
                resolution.binding.map(|d| self.local_binding(d, &name)),
                resolution
                    .possible
                    .into_iter()
                    .map(|d| self.local_binding(d, &name))
                    .collect(),
            )
        } else {
            (None, Vec::new())
        };
        Some(Site {
            name,
            syntax,
            at: start,
            binding,
            possible,
            defines: None,
        })
    }

    /// The definition named by `range`: a tags definition, or a plain local.
    fn defines(&self, range: &Range<usize>, name: &str, at: Position) -> Option<SiteDef> {
        let local = self.locals.def_at(range);
        let local_flag = local.is_some_and(|l| self.locals.is_local(l));
        if let Some(def) = self.defs.iter().find(|d| d.name_range == *range) {
            return Some(SiteDef {
                def: def.def.clone(),
                local: local_flag,
            });
        }
        let local = local?;
        let scope = self
            .defs
            .iter()
            .filter(|d| d.range.start <= range.start && range.end <= d.range.end)
            .max_by_key(|d| d.range.start)
            .map(|d| d.def.name.clone());
        Some(SiteDef {
            def: Def {
                name: name.into(),
                // A local type (a type parameter, a local interface) or a
                // local value.
                kind: if local.namespace == Namespace::Value {
                    DefKind::Variable
                } else {
                    DefKind::Type
                },
                line: at.line,
                column: at.column,
                end_line: at.line,
                scope,
                visibility: crate::extract::Visibility::File,
                arity: None,
            },
            local: self.locals.is_local(local),
        })
    }

    fn local_binding(&self, local: &LocalDef, name: &str) -> LocalBinding {
        let def = self
            .defs
            .iter()
            .find(|d| d.name_range == local.range)
            .map(|d| d.def.clone());
        LocalBinding {
            name: name.into(),
            at: position(&self.lines, local.range.start, &self.source),
            def,
            local: self.locals.is_local(local),
        }
    }
}

fn position(lines: &LineIndex, offset: usize, text: &[u8]) -> Position {
    let line = lines.row(offset);
    let start = lines.offset(line, 0, text.len()).unwrap_or(0);
    Position {
        line,
        column: u32::try_from(offset - start).unwrap_or(u32::MAX),
    }
}

// ---- the site tables -------------------------------------------------------

/// One row of a language's site table: an identifier of kind `node` in the
/// field `field` of a `parent` node — and, with `outer`, that parent in the
/// field `outer.1` of an `outer.0` node; with `parent_has`, a parent that has
/// that field — is a reference of `syntax`. The first matching row wins;
/// type identifiers that no row matches are `Type`, the rest `Plain`.
struct SiteRule {
    node: &'static str,
    parent: &'static str,
    field: &'static str,
    outer: Option<(&'static str, &'static str)>,
    parent_has: Option<&'static str>,
    syntax: RefSyntax,
}

const fn rule(
    node: &'static str,
    parent: &'static str,
    field: &'static str,
    syntax: RefSyntax,
) -> SiteRule {
    SiteRule {
        node,
        parent,
        field,
        outer: None,
        parent_has: None,
        syntax,
    }
}

const fn rule_in(
    node: &'static str,
    parent: &'static str,
    field: &'static str,
    outer: (&'static str, &'static str),
    syntax: RefSyntax,
) -> SiteRule {
    SiteRule {
        node,
        parent,
        field,
        outer: Some(outer),
        parent_has: None,
        syntax,
    }
}

use RefSyntax::{Call, Construct, Member, MemberCall};

const RUST_SITES: &[SiteRule] = &[
    rule("identifier", "call_expression", "function", Call),
    rule_in(
        "identifier",
        "scoped_identifier",
        "name",
        ("call_expression", "function"),
        Call,
    ),
    rule("identifier", "macro_invocation", "macro", Call),
    rule_in(
        "identifier",
        "scoped_identifier",
        "name",
        ("macro_invocation", "macro"),
        Call,
    ),
    rule_in(
        "field_identifier",
        "field_expression",
        "field",
        ("call_expression", "function"),
        MemberCall,
    ),
    rule("field_identifier", "field_expression", "field", Member),
    rule("type_identifier", "struct_expression", "name", Construct),
    // `Message::Move { x, y }` and `crate::Point { .. }`.
    rule_in(
        "type_identifier",
        "scoped_type_identifier",
        "name",
        ("struct_expression", "name"),
        Construct,
    ),
];

const ECMASCRIPT_SITES: &[SiteRule] = &[
    rule("identifier", "call_expression", "function", Call),
    rule_in(
        "property_identifier",
        "member_expression",
        "property",
        ("call_expression", "function"),
        MemberCall,
    ),
    rule_in(
        "private_property_identifier",
        "member_expression",
        "property",
        ("call_expression", "function"),
        MemberCall,
    ),
    rule("identifier", "new_expression", "constructor", Construct),
    rule_in(
        "property_identifier",
        "member_expression",
        "property",
        ("new_expression", "constructor"),
        Construct,
    ),
    rule(
        "property_identifier",
        "member_expression",
        "property",
        Member,
    ),
    rule(
        "private_property_identifier",
        "member_expression",
        "property",
        Member,
    ),
];

const JAVA_SITES: &[SiteRule] = &[
    SiteRule {
        node: "identifier",
        parent: "method_invocation",
        field: "name",
        outer: None,
        parent_has: Some("object"),
        syntax: MemberCall,
    },
    rule("identifier", "method_invocation", "name", Call),
    rule(
        "type_identifier",
        "object_creation_expression",
        "type",
        Construct,
    ),
    rule("identifier", "field_access", "field", Member),
];

const PYTHON_SITES: &[SiteRule] = &[
    rule("identifier", "call", "function", Call),
    rule_in(
        "identifier",
        "attribute",
        "attribute",
        ("call", "function"),
        MemberCall,
    ),
    rule("identifier", "attribute", "attribute", Member),
];

const GO_SITES: &[SiteRule] = &[
    rule("identifier", "call_expression", "function", Call),
    rule_in(
        "field_identifier",
        "selector_expression",
        "field",
        ("call_expression", "function"),
        MemberCall,
    ),
    rule("field_identifier", "selector_expression", "field", Member),
];

fn site_rules(dialect: Dialect) -> &'static [SiteRule] {
    match dialect {
        Dialect::Rust => RUST_SITES,
        Dialect::TypeScript | Dialect::Tsx | Dialect::JavaScript => ECMASCRIPT_SITES,
        Dialect::Java => JAVA_SITES,
        Dialect::Python => PYTHON_SITES,
        Dialect::Go => GO_SITES,
        Dialect::Pack => &[],
    }
}

/// The node kinds that are identifiers in a language.
fn is_identifier(dialect: Dialect, kind: &str) -> bool {
    let kinds: &[&str] = match dialect {
        Dialect::Rust => &["identifier", "field_identifier", "type_identifier"],
        Dialect::TypeScript | Dialect::Tsx | Dialect::JavaScript => &[
            "identifier",
            "property_identifier",
            "private_property_identifier",
            "shorthand_property_identifier",
            "shorthand_property_identifier_pattern",
            "type_identifier",
        ],
        Dialect::Java => &["identifier", "type_identifier"],
        Dialect::Python => &["identifier"],
        Dialect::Go => &[
            "identifier",
            "field_identifier",
            "type_identifier",
            "package_identifier",
        ],
        Dialect::Pack => return kind.ends_with("identifier"),
    };
    kinds.contains(&kind)
}

/// Identifier positions that name a member, a path segment or a keyword
/// argument rather than a variable: (parent kind, field). The locals
/// queries' `(identifier) @local.reference` matches these too; they are not
/// local references and never bind to a local. JS, TS and Go name members
/// with their own node kinds (`property_identifier`, `field_identifier`).
const NON_VARIABLE_POSITIONS: &[(Dialect, &str, &str)] = &[
    (Dialect::Rust, "scoped_identifier", "name"),
    (Dialect::Rust, "scoped_identifier", "path"),
    (Dialect::Rust, "macro_invocation", "macro"),
    (Dialect::Java, "method_invocation", "name"),
    (Dialect::Java, "field_access", "field"),
    (Dialect::Java, "scoped_identifier", "name"),
    (Dialect::Java, "scoped_identifier", "scope"),
    (Dialect::Python, "attribute", "attribute"),
    (Dialect::Python, "keyword_argument", "name"),
    // The last segment of a qualified type name (`a::B`, `ns.T`, `pkg.T`).
    (Dialect::Rust, "scoped_type_identifier", "name"),
    (Dialect::TypeScript, "nested_type_identifier", "name"),
    (Dialect::Tsx, "nested_type_identifier", "name"),
    (Dialect::Go, "qualified_type", "name"),
];

/// Whether an identifier can refer to a variable or a local type (see
/// [`NON_VARIABLE_POSITIONS`]).
fn is_variable_position(dialect: Dialect, node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return true;
    };
    // Java's `Outer.Inner` has no fields: only the first segment can be a
    // local type.
    if dialect == Dialect::Java
        && parent.kind() == "scoped_type_identifier"
        && parent
            .named_child(0)
            .is_some_and(|first| first.id() != node.id())
    {
        return false;
    }
    !NON_VARIABLE_POSITIONS.iter().any(|(d, kind, field)| {
        *d == dialect && parent.kind() == *kind && in_field(node, parent, field)
    })
}

/// Whether `node` is the child in `field` of its parent.
fn in_field(node: Node<'_>, parent: Node<'_>, field: &str) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|c| c.id() == node.id())
}

fn syntax_at(dialect: Dialect, node: Node<'_>) -> RefSyntax {
    let parent = node.parent();
    for rule in site_rules(dialect) {
        let Some(parent) = parent else { break };
        if rule.node != node.kind() || rule.parent != parent.kind() {
            continue;
        }
        if !in_field(node, parent, rule.field) {
            continue;
        }
        if let Some(field) = rule.parent_has
            && parent.child_by_field_name(field).is_none()
        {
            continue;
        }
        if let Some((outer, field)) = rule.outer {
            match parent.parent() {
                Some(o) if o.kind() == outer && in_field(parent, o, field) => {}
                _ => continue,
            }
        }
        return rule.syntax;
    }
    if node.kind() == "type_identifier" && dialect != Dialect::Pack {
        RefSyntax::Type
    } else {
        RefSyntax::Plain
    }
}

// ---- the syntax-kind table -------------------------------------------------

const ALL_KINDS: u16 = (1 << crate::DEF_KINDS.len()) - 1;
const CALLABLE: u16 = crate::def_kind_bit(DefKind::Function)
    | crate::def_kind_bit(DefKind::Method)
    | crate::def_kind_bit(DefKind::Macro)
    | crate::def_kind_bit(DefKind::Class)
    | crate::def_kind_bit(DefKind::Struct);
const CONSTRUCTIBLE: u16 =
    crate::def_kind_bit(DefKind::Class) | crate::def_kind_bit(DefKind::Struct);
const TYPES: u16 = crate::def_kind_bit(DefKind::Class)
    | crate::def_kind_bit(DefKind::Interface)
    | crate::def_kind_bit(DefKind::Struct)
    | crate::def_kind_bit(DefKind::Enum)
    | crate::def_kind_bit(DefKind::Trait)
    | crate::def_kind_bit(DefKind::Type)
    | crate::def_kind_bit(DefKind::Module);

/// The definition kinds a reference syntax accepts in a language, as a
/// [`DefKind::bit`] set (docs/design/code_intel.md「候选的分层与过滤」).
/// For a core language they come from the definition forms
/// (`crate::forms`): a kind is accepted where its row says the syntax can
/// name it. `Plain` accepts every kind, and so does `Member` outside Rust:
/// the left side of `a.b` can be a namespace, a module, a package or a class
/// (`utils.handle`, `models.User`, `pkg.Var`, `Outer.Inner`).
pub fn accepted_kinds(lang: &Lang, syntax: RefSyntax) -> u16 {
    let core = match lang.dialect() {
        Dialect::TypeScript => Some("ts"),
        Dialect::Tsx => Some("tsx"),
        Dialect::JavaScript => Some("js"),
        Dialect::Rust => Some("rust"),
        Dialect::Java => Some("java"),
        Dialect::Python => Some("python"),
        Dialect::Go => Some("go"),
        Dialect::Pack => None,
    };
    if let Some(name) = core {
        return match syntax {
            RefSyntax::Plain => ALL_KINDS,
            RefSyntax::Member if lang.dialect() != Dialect::Rust => ALL_KINDS,
            _ => crate::forms::kinds_for(name, syntax),
        };
    }
    pack_kinds(syntax)
}

/// A language pack has no forms: the general rules.
fn pack_kinds(syntax: RefSyntax) -> u16 {
    match syntax {
        RefSyntax::Call | RefSyntax::MemberCall => CALLABLE,
        RefSyntax::Construct => CONSTRUCTIBLE,
        RefSyntax::Type => TYPES,
        RefSyntax::Member | RefSyntax::Plain => ALL_KINDS,
    }
}

/// Whether a reference of `syntax` in `lang` can name a definition of `kind`.
pub fn accepts(lang: &Lang, syntax: RefSyntax, kind: DefKind) -> bool {
    accepted_kinds(lang, syntax) & crate::def_kind_bit(kind) != 0
}
