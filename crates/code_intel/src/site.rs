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
use crate::util::{FastBuild, LineIndex};
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
    /// The locals query's `@local.reference` captures in variable
    /// positions, by position, with whether the position is uncertain (a Go
    /// composite literal key): the only names `site_at` resolves locally.
    local_refs: Vec<(Range<usize>, bool)>,
    refs: Vec<RefSite>,
    /// Parsed for one site (`parse_at`): `local_refs` is complete only at
    /// it and `refs` is empty.
    only_site: Option<Position>,
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
        ParsedFile::parse_inner(lang, source, clock, at, cancel, false, None)
    }

    /// [`parse`](Self::parse) for what a jump asks at `site`: `site_at`
    /// there, the definitions, the binding model; no
    /// [`references`](Self::references). On a core language the query run
    /// over the whole tree has only the locals patterns that build the
    /// binding model (`SiteQuery::jump`); the locals query's verdict on the
    /// site's own name comes from the full query run on the site's bytes.
    /// `site_at(site)` and the definitions are what `parse` gives
    /// (`site_parse_at_matches_parse`).
    pub fn parse_at(
        lang: &Lang,
        source: Arc<[u8]>,
        clock: &dyn Clock,
        at: Instant,
        cancel: &AtomicBool,
        site: Position,
    ) -> Result<ParsedFile, ParseError> {
        ParsedFile::parse_inner(lang, source, clock, at, cancel, false, Some(site))
    }

    /// `parse`; with `full_query`, every tags pattern stays enabled even
    /// when the walker gives the definitions (the tests' reference:
    /// `site_reference_query_matches_full`).
    #[cfg(test)]
    pub(crate) fn parse_with(
        lang: &Lang,
        source: Arc<[u8]>,
        clock: &dyn Clock,
        at: Instant,
        cancel: &AtomicBool,
        full_query: bool,
    ) -> Result<ParsedFile, ParseError> {
        ParsedFile::parse_inner(lang, source, clock, at, cancel, full_query, None)
    }

    fn parse_inner(
        lang: &Lang,
        source: Arc<[u8]>,
        clock: &dyn Clock,
        at: Instant,
        cancel: &AtomicBool,
        full_query: bool,
        site: Option<Position>,
    ) -> Result<ParsedFile, ParseError> {
        let dialect = lang.dialect();
        // A core language's definitions and markers come from its walker,
        // as in `extract` (ADR 0031): its query only has the locals
        // patterns and the tags patterns that give references enabled.
        let walking = lang.walks();
        // One site on a core language: the jump's query (`SiteQuery::jump`).
        let for_site = walking && !full_query && site.is_some();
        let (query, captures, locals_patterns) = if walking && !full_query {
            let q = if for_site {
                lang.jump_query()
            } else {
                lang.site_query()
            };
            let q = q.ok_or(ParseError::Unsupported)?;
            (&q.query, &q.captures[..], q.locals_patterns)
        } else {
            let compiled = lang.compiled().ok_or(ParseError::Unsupported)?;
            (
                &compiled.tags.query,
                &compiled.captures[..],
                compiled.locals_patterns,
            )
        };
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
        let ts_parse = crate::phases::span("1a tree-sitter parse");
        let tree = parser
            .parse_with_options(
                &mut |i, _| text.get(i..).unwrap_or(&[]),
                None,
                Some(ParseOptions::new().progress_callback(&mut progress)),
            )
            .ok_or_else(interrupted)?;
        drop(ts_parse);
        let query_run = crate::phases::span("1b site query (locals + refs)");

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
        let parents = Parents::new(&tree);
        let mut collector = Collector::new(dialect, text.len());
        let mut ref_ranges: Vec<Range<usize>> = Vec::new();
        let mut local_refs: Vec<(Range<usize>, bool)> = Vec::new();
        // One tag per name node, from the lowest pattern (tree-sitter-tags).
        let mut tags: HashMap<(usize, usize), (usize, RawTag), FastBuild> = HashMap::default();
        while let Some(m) = matches.next() {
            crate::phases::count("site query matches", 1);
            if m.pattern_index < locals_patterns {
                let props = query.property_settings(m.pattern_index);
                for capture in m.captures() {
                    match captures[capture.index as usize] {
                        Capture::LocalScope => collector.scope(capture.node, props),
                        Capture::LocalDefinition => {
                            collector.definition(capture.node, props, text);
                        }
                        // `(identifier) @local.reference` also matches member
                        // and path names (`items.add`, `Queue::new`), keys
                        // and tag names, which never refer to a local
                        // (the forms table's positions).
                        Capture::LocalReference if collector.reference(capture.node, props) => {
                            let at = variable_position(dialect, capture.node, &parents, text);
                            if at != Variable::No {
                                let range = capture.node.byte_range();
                                local_refs.push((range.clone(), at == Variable::Uncertain));
                                ref_ranges.push(range);
                            }
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
                match captures[capture.index as usize] {
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
        drop(query_run);
        if stop() {
            return Err(interrupted());
        }

        let lines = LineIndex::new(text);
        if let (true, Some(site)) = (for_site, site) {
            // The locals query's verdict on the site's own name: the full
            // query's captures over the bytes around the site (`site_at`
            // looks at the offset and the byte before it). Directives were
            // taken by the run above, so only plain references count.
            let _t = crate::phases::span("1b' site query at the site");
            let full = lang.site_query().ok_or(ParseError::Unsupported)?;
            if let Some(offset) = lines.offset(site.line, site.column, text.len()) {
                let mut cursor = QueryCursor::new();
                cursor.set_byte_range(offset.saturating_sub(1)..(offset + 1).min(text.len()));
                let mut query_progress = |_: &tree_sitter::QueryCursorState| {
                    if stop() {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                };
                let mut matches = cursor.matches_with_options(
                    &full.query,
                    tree.root_node(),
                    text,
                    QueryCursorOptions::new().progress_callback(&mut query_progress),
                );
                while let Some(m) = matches.next() {
                    if m.pattern_index >= full.locals_patterns {
                        continue;
                    }
                    let props = full.query.property_settings(m.pattern_index);
                    if !crate::locals::is_plain_reference(props) {
                        continue;
                    }
                    for capture in m.captures() {
                        if full.captures[capture.index as usize] != Capture::LocalReference {
                            continue;
                        }
                        let at = variable_position(dialect, capture.node, &parents, text);
                        if at != Variable::No {
                            local_refs.push((capture.node.byte_range(), at == Variable::Uncertain));
                        }
                    }
                }
            }
            if stop() {
                return Err(interrupted());
            }
        }

        let finish = crate::phases::span("1c locals finish");
        let locals = collector.finish(text);
        drop(finish);

        let mut tags: Vec<RawTag> = tags.into_values().map(|(_, t)| t).collect();
        // The query's tags give the reference positions.
        if walking {
            // The walk stops with the same deadline and cancellation as the
            // parse and the query run.
            let _t = crate::phases::span("1d walker");
            let walked = crate::walk::walk_tree(dialect, lang.language(), &tree, text, &stop)
                .ok_or_else(interrupted)?;
            tags.retain(|t| t.class == TagClass::Ref);
            tags.extend(walked.tags);
        }
        let rest = crate::phases::span("1e rules + reference sites");
        tags.sort_by_key(|t| t.name_range.start);
        // Tags references, and the names markers take from the reference
        // patterns (`Marker::consumes_name`): an exported name, an
        // implemented or receiver type, an attribute write. Not for one
        // site: it has no `references`.
        if !for_site {
            ref_ranges.extend(
                tags.iter()
                    .filter(|t| match t.class {
                        TagClass::Ref => true,
                        TagClass::Marker(marker) => marker.consumes_name(),
                        TagClass::Def(_) => false,
                    })
                    .map(|t| t.name_range.clone()),
            );
        } else {
            ref_ranges.clear();
        }
        ref_ranges.sort_by_key(|r| (r.start, r.end));
        ref_ranges.dedup();
        local_refs.sort_by_key(|(r, _)| (r.start, r.end));
        local_refs.dedup_by(|b, a| a.0 == b.0);
        let tag_defs: Vec<(usize, usize)> = tags
            .iter()
            .filter(|t| matches!(t.class, TagClass::Def(_)))
            .map(|t| (t.name_range.start, t.name_range.end))
            .collect();
        // A language pack's scopes are not classified (`rules::finish`).
        let local_scopes = lang.lease().is_static().then(|| locals.local_scopes());
        let finished = rules::finish(lang.dialect(), text, tags, local_scopes.as_deref());
        // `(identifier) @local.reference` matches definition names too; a
        // marker's name that became a definition (a Python field) is one.
        let named: std::collections::HashSet<(usize, usize), FastBuild> = tag_defs
            .into_iter()
            .chain(
                finished
                    .defs
                    .iter()
                    .map(|d| (d.name_range.start, d.name_range.end)),
            )
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
        drop(rest);
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
            local_refs,
            refs,
            only_site: site.filter(|_| for_site),
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

    /// The definitions, each with whether it is a local definition: a
    /// `@local.definition` in a non-root scope (`DefinitionRef::local`).
    pub fn defs_with_local(&self) -> impl Iterator<Item = (&Def, bool)> + '_ {
        self.defs.iter().map(|d| {
            let local = self
                .locals
                .def_at(&d.name_range)
                .is_some_and(|l| self.locals.is_local(l));
            (&d.def, local)
        })
    }

    /// The innermost definition whose node holds byte `offset` outside its
    /// own name: who a reference there is in (the callers' `enclosing`).
    pub fn enclosing(&self, offset: usize) -> Option<&Def> {
        self.defs
            .iter()
            .filter(|d| d.range.start <= offset && offset < d.range.end)
            .filter(|d| !(d.name_range.start <= offset && offset < d.name_range.end))
            .max_by_key(|d| (d.range.start, std::cmp::Reverse(d.range.end)))
            .map(|d| &d.def)
    }

    /// The source text of a reference position.
    pub fn text(&self, site: &RefSite) -> &[u8] {
        self.source.get(site.start..site.end).unwrap_or(&[])
    }

    /// Every reference position: tags references and `@local.reference`,
    /// deduplicated by position. Tags leaves out references that locals
    /// resolves; the union keeps them.
    pub fn references(&self) -> &[RefSite] {
        debug_assert!(
            self.only_site.is_none(),
            "parsed for one site: no references"
        );
        &self.refs
    }

    /// The identifier at a position (or just before it, with the cursor at
    /// its end) and what it is; None when there is no identifier there
    /// (`QueryError::NoSymbol`).
    pub fn site_at(&self, at: Position) -> Option<Site> {
        debug_assert!(
            self.only_site.is_none_or(|s| s == at),
            "parsed for another site"
        );
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
        // Only the locals query's references in variable positions resolve
        // locally (an allow-list): a member, key, attribute, path segment or
        // tag name never binds to a same-named local. A type name looks for
        // local types (type parameters, local type declarations), any other
        // name for local values.
        let namespace = if node.kind() == "type_identifier" {
            Namespace::Type
        } else {
            Namespace::Value
        };
        let i = self
            .local_refs
            .partition_point(|(r, _)| r.start < range.start);
        let local_ref = self
            .local_refs
            .get(i)
            .filter(|(r, _)| *r == range)
            .map(|(_, uncertain)| *uncertain);
        let (binding, possible) = match local_ref {
            Some(uncertain) if !matches!(syntax, RefSyntax::Member | RefSyntax::MemberCall) => {
                let resolution = self.locals.resolve(text, &range, namespace);
                let mut possible: Vec<LocalBinding> = resolution
                    .possible
                    .into_iter()
                    .map(|d| self.local_binding(d, &name))
                    .collect();
                let mut binding = resolution.binding.map(|d| self.local_binding(d, &name));
                if uncertain {
                    // A Go composite literal key may be a field name.
                    possible.extend(binding.take());
                }
                (binding, possible)
            }
            _ => (None, Vec::new()),
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

/// The node kinds that are names in a language: the binding and the
/// reference kinds of `locals::name_kinds`.
fn is_identifier(dialect: Dialect, kind: &str) -> bool {
    crate::locals::is_name_kind(dialect, kind)
}

/// The forms table's name for a core dialect.
fn core_name(dialect: Dialect) -> Option<&'static str> {
    match dialect {
        Dialect::TypeScript => Some("ts"),
        Dialect::Tsx => Some("tsx"),
        Dialect::JavaScript => Some("js"),
        Dialect::Rust => Some("rust"),
        Dialect::Java => Some("java"),
        Dialect::Python => Some("python"),
        Dialect::Go => Some("go"),
        Dialect::Pack => None,
    }
}

/// Whether a `@local.reference` can refer to a variable or a local type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Variable {
    Yes,
    /// A key, an attribute, a member or path segment, a tag name: never a
    /// local binding.
    No,
    /// A Go composite literal key: a local of that name is only possible.
    Uncertain,
}

/// What an identifier's position is, by the positions the forms table
/// gives its parent's row (`forms::Pos`); a language pack has none.
/// `Node::parent` over one tree, with the root's level found by binary
/// search. tree-sitter finds a parent by descending from the root and
/// scanning each level's children (`ts_node_parent`): on a file of many
/// top-level items every call scans all of them. Below the root this
/// descends as tree-sitter does (`child_with_descendant`); an empty node,
/// or one the root's children do not contain, falls back to
/// `Node::parent` (`site_parents_match_tree_sitter`).
pub(crate) struct Parents<'t> {
    root: Node<'t>,
    /// The root's children, in order: they do not overlap.
    children: Vec<Node<'t>>,
}

impl<'t> Parents<'t> {
    pub(crate) fn new(tree: &'t Tree) -> Parents<'t> {
        let root = tree.root_node();
        let mut cursor = root.walk();
        let children = root.children(&mut cursor).collect();
        Parents { root, children }
    }

    pub(crate) fn of(&self, node: Node<'t>) -> Option<Node<'t>> {
        let (start, end) = (node.start_byte(), node.end_byte());
        if start == end || node.id() == self.root.id() {
            return node.parent();
        }
        // The last child starting at or before the node: the only one that
        // can contain it.
        let at = self.children.partition_point(|c| c.start_byte() <= start);
        let Some(&top) = at.checked_sub(1).and_then(|i| self.children.get(i)) else {
            return node.parent();
        };
        if top.id() == node.id() {
            return Some(self.root);
        }
        if top.end_byte() < end {
            return node.parent();
        }
        let mut at = top;
        loop {
            match at.child_with_descendant(node) {
                Some(next) if next.id() == node.id() => return Some(at),
                Some(next) => at = next,
                None => return node.parent(),
            }
        }
    }
}

pub(crate) fn variable_position<'t>(
    dialect: Dialect,
    node: Node<'t>,
    parents: &Parents<'t>,
    text: &[u8],
) -> Variable {
    use crate::forms::Pos;
    let Some(lang) = core_name(dialect) else {
        return Variable::Yes;
    };
    let Some(parent) = parents.of(node) else {
        return Variable::Yes;
    };
    let rows = crate::forms::positions(lang);
    if let Some(positions) = rows.get(parent.kind()) {
        let first = parent.named_child(0).is_some_and(|f| f.id() == node.id());
        for pos in *positions {
            let not_variable = match pos {
                Pos::In(field) => in_field(node, parent, field),
                Pos::NotFirst => !first,
                Pos::First => first,
                Pos::Any => true,
                Pos::Lowercase(field) => {
                    in_field(node, parent, field)
                        && text
                            .get(node.start_byte())
                            .is_some_and(u8::is_ascii_lowercase)
                }
                Pos::Uncertain(_) => false,
            };
            if not_variable {
                return Variable::No;
            }
        }
    }
    if let Some(grand) = parents.of(parent)
        && let Some(positions) = rows.get(grand.kind())
        && positions
            .iter()
            .any(|p| matches!(p, Pos::Uncertain(field) if in_field(parent, grand, field)))
    {
        return Variable::Uncertain;
    }
    Variable::Yes
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
