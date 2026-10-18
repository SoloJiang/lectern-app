//! Extraction for the core languages by one walk over the syntax tree
//! (ADR 0031), instead of tree-sitter-tags' query run.
//!
//! The tags and locals queries stay the specification: each walker applies,
//! at every name node, the patterns of its language's tags query in query
//! order (the lowest pattern wins, as in tree-sitter-tags), and filters
//! `#is-not? local` references the way tree-sitter-tags does: by name,
//! against the `@local.definition` captures seen so far in the scopes around
//! the name, stopping at a scope that does not inherit. The output is the
//! raw tag stream that `rules::finish` turns into definitions, so the
//! visibility, scope and arity rules are shared with the tags path.
//! `extract_walker_matches_tags` checks that both paths give the same
//! extraction for every fixture.
//!
//! Language packs and `ParsedFile` still run the queries.

mod ecma;
mod go;
mod java;
mod python;
mod rust;

use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use tree_sitter::{Language, Node, ParseOptions, Parser, Tree};

use crate::DefKind;
use crate::lang::{Dialect, Marker, TagClass};
use crate::rules::RawTag;

/// The walk checks the cancellation flag every this many named nodes.
const CHECK_EVERY: u32 = 1024;

/// A node on the path from the root to the node being visited.
#[derive(Clone, Copy)]
pub(crate) struct Frame<'t> {
    pub node: Node<'t>,
    pub kind: u16,
    /// The field of this node in its parent; 0 for none.
    pub field: u16,
}

/// The walk's output and tree-sitter-tags' local scopes.
pub(crate) struct Ctx<'s> {
    src: &'s [u8],
    tags: Vec<RawTag>,
    refs: HashSet<&'s [u8], crate::util::FastBuild>,
    scopes: Vec<Scope<'s>>,
}

struct Scope<'s> {
    /// Index of the scope's node in the frame stack.
    depth: usize,
    inherits: bool,
    defs: Vec<&'s [u8]>,
}

impl<'s> Ctx<'s> {
    fn new(src: &'s [u8]) -> Ctx<'s> {
        Ctx {
            src,
            tags: Vec::new(),
            refs: HashSet::default(),
            // The file: tree-sitter-tags' root scope does not inherit.
            scopes: vec![Scope {
                depth: 0,
                inherits: false,
                defs: Vec::new(),
            }],
        }
    }

    pub fn text(&self, node: Node<'_>) -> &'s [u8] {
        &self.src[node.byte_range()]
    }

    /// A `@local.scope` capture at `depth` in the frame stack.
    pub fn scope(&mut self, depth: usize, inherits: bool) {
        self.scopes.push(Scope {
            depth,
            inherits,
            defs: Vec::new(),
        });
    }

    /// A `@local.definition` capture: its text joins the innermost scope.
    pub fn local(&mut self, node: Node<'_>) {
        let text = self.text(node);
        if let Some(scope) = self.scopes.last_mut() {
            scope.defs.push(text);
        }
    }

    fn is_local(&self, text: &[u8]) -> bool {
        for scope in self.scopes.iter().rev() {
            if scope.defs.contains(&text) {
                return true;
            }
            if !scope.inherits {
                return false;
            }
        }
        false
    }

    /// A reference name from a pattern without `#is-not? local`.
    pub fn reference(&mut self, name: Node<'_>) {
        if !name.has_error() {
            self.refs.insert(self.text(name));
        }
    }

    /// A reference name from a pattern with `#is-not? local`.
    pub fn nonlocal(&mut self, name: Node<'_>) {
        let text = self.text(name);
        if !name.has_error() && !self.is_local(text) {
            self.refs.insert(text);
        }
    }

    /// A `@definition.<kind>` tag on `tag` named by `name`, with the `@doc`
    /// capture `params`.
    pub fn def(&mut self, name: Node<'_>, tag: Node<'_>, kind: DefKind, params: Option<Node<'_>>) {
        let params = params
            .and_then(|p| std::str::from_utf8(self.text(p)).ok())
            .map(str::to_owned);
        self.push(name, tag, TagClass::Def(kind), params);
    }

    /// A `@reference.lectern.*` marker.
    pub fn marker(&mut self, name: Node<'_>, tag: Node<'_>, marker: Marker) {
        self.push(name, tag, TagClass::Marker(marker), None);
    }

    fn push(&mut self, name: Node<'_>, tag: Node<'_>, class: TagClass, params: Option<String>) {
        if name.has_error() {
            return;
        }
        let name_range = name.byte_range();
        let node_range = tag.byte_range();
        let start = name.start_position();
        self.tags.push(RawTag {
            class,
            range: node_range.start.min(name_range.start)..node_range.end.max(name_range.end),
            name_range,
            row: u32::try_from(start.row).unwrap_or(u32::MAX),
            column: u32::try_from(start.column).unwrap_or(u32::MAX),
            params,
        });
    }
}

/// One language's walker.
pub(crate) trait Walker: Send + Sync {
    /// Visits the last frame of `stack`, a named node; the frames before it
    /// are its ancestors. Called in document order (pre-order).
    fn visit(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]);
}

/// The frame `n` levels above the last one, if any.
pub(crate) fn up<'a, 't>(stack: &'a [Frame<'t>], n: usize) -> Option<&'a Frame<'t>> {
    stack.len().checked_sub(n + 1).map(|i| &stack[i])
}

/// The first child of `node` in `field`, when it has the kind `kind`.
pub(crate) fn field_of<'t>(node: Node<'t>, field: u16, kind: u16) -> Option<Node<'t>> {
    node.child_by_field_id(field)
        .filter(|c| c.kind_id() == kind)
}

pub(crate) fn kind(language: &Language, name: &str) -> Result<u16, String> {
    match language.id_for_node_kind(name, true) {
        0 => Err(format!("no node kind {name}")),
        id => Ok(id),
    }
}

pub(crate) fn anon(language: &Language, name: &str) -> Result<u16, String> {
    match language.id_for_node_kind(name, false) {
        0 => Err(format!("no token {name:?}")),
        id => Ok(id),
    }
}

pub(crate) fn field(language: &Language, name: &str) -> Result<u16, String> {
    language
        .field_id_for_name(name)
        .map(|f| f.get())
        .ok_or_else(|| format!("no field {name}"))
}

/// Declares a struct of node kind, token and field ids, resolved from a
/// language by name; resolution fails when the grammar lacks one.
macro_rules! ids {
    (
        $name:ident {
            kinds { $($k:ident = $ks:literal),* $(,)? }
            tokens { $($t:ident = $ts:literal),* $(,)? }
            fields { $($f:ident = $fs:literal),* $(,)? }
        }
    ) => {
        pub(super) struct $name {
            $(pub $k: u16,)*
            $(pub $t: u16,)*
            $(pub $f: u16,)*
        }

        impl $name {
            pub(super) fn new(language: &tree_sitter::Language) -> Result<$name, String> {
                Ok($name {
                    $($k: super::kind(language, $ks)?,)*
                    $($t: super::anon(language, $ts)?,)*
                    $($f: super::field(language, $fs)?,)*
                })
            }
        }
    };
}
pub(crate) use ids;

/// The walker of a core dialect, built on first use; None for a language
/// pack, or when the grammar lacks a node kind the walker names (the tags
/// path is used instead).
fn walker(dialect: Dialect, language: &Language) -> Option<&'static dyn Walker> {
    type Slot = OnceLock<Option<Box<dyn Walker>>>;
    static TS: Slot = OnceLock::new();
    static TSX: Slot = OnceLock::new();
    static JS: Slot = OnceLock::new();
    static RUST: Slot = OnceLock::new();
    static JAVA: Slot = OnceLock::new();
    static PYTHON: Slot = OnceLock::new();
    static GO: Slot = OnceLock::new();
    fn boxed<W: Walker + 'static>(
        w: Result<W, String>,
        dialect: Dialect,
    ) -> Option<Box<dyn Walker>> {
        match w {
            Ok(w) => Some(Box::new(w)),
            Err(error) => {
                crate::util::log(format_args!("no walker for {dialect:?}: {error}"));
                None
            }
        }
    }
    let slot = match dialect {
        Dialect::TypeScript => &TS,
        Dialect::Tsx => &TSX,
        Dialect::JavaScript => &JS,
        Dialect::Rust => &RUST,
        Dialect::Java => &JAVA,
        Dialect::Python => &PYTHON,
        Dialect::Go => &GO,
        Dialect::Pack => return None,
    };
    slot.get_or_init(|| match dialect {
        Dialect::TypeScript | Dialect::Tsx | Dialect::JavaScript => {
            boxed(ecma::Ecma::new(language, dialect), dialect)
        }
        Dialect::Rust => boxed(rust::Rust::new(language), dialect),
        Dialect::Java => boxed(java::Java::new(language), dialect),
        Dialect::Python => boxed(python::Python::new(language), dialect),
        Dialect::Go => boxed(go::Go::new(language), dialect),
        Dialect::Pack => None,
    })
    .as_deref()
}

/// Whether `dialect` has a walker for `language`.
pub(crate) fn supports(dialect: Dialect, language: &Language) -> bool {
    walker(dialect, language).is_some()
}

thread_local! {
    /// One parser per thread for the core languages (linked in, so no
    /// grammar lease is involved).
    static PARSER: RefCell<Parser> = RefCell::new(Parser::new());
}

/// The walk's result: definitions and markers, and reference names.
pub(crate) struct Walked {
    pub tags: Vec<RawTag>,
    pub refs: Vec<Box<str>>,
}

/// Parses `source` and walks it. None once `stop` is set (the watchdog's
/// flag: the deadline, cancellation, a pause request), checked while parsing
/// and every [`CHECK_EVERY`] nodes of the walk; None also when the dialect
/// has no walker.
pub(crate) fn extract(
    dialect: Dialect,
    language: &Language,
    source: &[u8],
    stop: &AtomicUsize,
) -> Option<Walked> {
    let walker = walker(dialect, language)?;
    let tree = PARSER.with(|parser| {
        let mut parser = parser.borrow_mut();
        parser.set_language(language).ok()?;
        let mut progress = |_: &tree_sitter::ParseState| {
            if stop.load(Ordering::Acquire) != 0 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let tree = parser.parse_with_options(
            &mut |i, _| source.get(i..).unwrap_or(&[]),
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        if tree.is_none() {
            // An interrupted parse leaves state for resuming it.
            parser.reset();
        }
        tree
    })?;
    walk(walker, &tree, source, &|| stop.load(Ordering::Acquire) != 0)
}

/// Walks a tree parsed elsewhere (`ParsedFile`, with its own deadline and
/// cancellation); None when the dialect has no walker or once `stop` returns
/// true, checked every [`CHECK_EVERY`] nodes and before returning.
pub(crate) fn walk_tree(
    dialect: Dialect,
    language: &Language,
    tree: &Tree,
    source: &[u8],
    stop: &dyn Fn() -> bool,
) -> Option<Walked> {
    walk(walker(dialect, language)?, tree, source, stop)
}

fn walk(
    walker: &dyn Walker,
    tree: &Tree,
    source: &[u8],
    stop: &dyn Fn() -> bool,
) -> Option<Walked> {
    let mut walk = Walk {
        walker,
        stop,
        cx: Ctx::new(source),
        stack: Vec::with_capacity(64),
        steps: 0,
    };
    let mut cursor = tree.walk();
    if !walk.enter(&cursor) {
        return None;
    }
    // Only named nodes are entered: tokens are leaves, and the walkers read
    // the few they need (`export`, `private`) from their parent.
    'outer: loop {
        if cursor.goto_first_child() {
            if to_named(&mut cursor) {
                if !walk.enter(&cursor) {
                    return None;
                }
                continue;
            }
            cursor.goto_parent();
        }
        loop {
            walk.leave();
            if cursor.goto_next_sibling() && to_named(&mut cursor) {
                if !walk.enter(&cursor) {
                    return None;
                }
                continue 'outer;
            }
            if !cursor.goto_parent() {
                break 'outer;
            }
        }
    }
    if stop() {
        return None;
    }
    let cx = walk.cx;
    let mut refs: Vec<Box<str>> = cx
        .refs
        .into_iter()
        .filter_map(|r| std::str::from_utf8(r).ok())
        .map(Box::from)
        .collect();
    refs.sort_unstable();
    Some(Walked {
        tags: cx.tags,
        refs,
    })
}

/// Moves the cursor to the first named node from its position among its
/// siblings; false when there is none.
fn to_named(cursor: &mut tree_sitter::TreeCursor<'_>) -> bool {
    loop {
        if cursor.node().is_named() {
            return true;
        }
        if !cursor.goto_next_sibling() {
            return false;
        }
    }
}

struct Walk<'w, 's, 't> {
    walker: &'w dyn Walker,
    stop: &'w dyn Fn() -> bool,
    cx: Ctx<'s>,
    stack: Vec<Frame<'t>>,
    steps: u32,
}

impl<'t> Walk<'_, '_, 't> {
    /// Pushes the cursor's node and visits it when it is named; false once
    /// the walk is to stop.
    fn enter(&mut self, cursor: &tree_sitter::TreeCursor<'t>) -> bool {
        let node = cursor.node();
        self.stack.push(Frame {
            node,
            kind: node.kind_id(),
            field: cursor.field_id().map_or(0, |f| f.get()),
        });
        self.walker.visit(&mut self.cx, &self.stack);
        self.steps += 1;
        !(self.steps.is_multiple_of(CHECK_EVERY) && (self.stop)())
    }

    /// Pops the last node, and the scopes it opened.
    fn leave(&mut self) {
        self.stack.pop();
        let depth = self.stack.len();
        while self
            .cx
            .scopes
            .last()
            .is_some_and(|s| s.depth >= depth && s.depth > 0)
        {
            self.cx.scopes.pop();
        }
    }
}
