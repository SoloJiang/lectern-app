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
use std::ops::{ControlFlow, Range};
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
    /// Names starting at or after this byte are not recorded as
    /// definitions or references (a window's cut, 「大文件」); markers are
    /// kept, as they may decide something about a definition before it.
    limit: usize,
    tags: Vec<RawTag>,
    refs: HashSet<&'s [u8], crate::util::FastBuild>,
    scopes: Vec<Scope<'s>>,
    /// The ranges of the scopes opened so far whose definitions are local
    /// to the file (`locals::ScopeKind::is_local`), in document order.
    local_scopes: Vec<Range<usize>>,
}

struct Scope<'s> {
    /// Index of the scope's node in the frame stack.
    depth: usize,
    inherits: bool,
    defs: Vec<&'s [u8]>,
}

impl<'s> Ctx<'s> {
    fn new(src: &'s [u8], limit: usize) -> Ctx<'s> {
        Ctx {
            src,
            limit,
            tags: Vec::new(),
            refs: HashSet::default(),
            // The file: tree-sitter-tags' root scope does not inherit.
            scopes: vec![Scope {
                depth: 0,
                inherits: false,
                defs: Vec::new(),
            }],
            local_scopes: Vec::new(),
        }
    }

    pub fn text(&self, node: Node<'_>) -> &'s [u8] {
        &self.src[node.byte_range()]
    }

    /// A `@local.scope` capture of the last frame: a function or block,
    /// whose definitions are local to the file.
    pub fn scope(&mut self, stack: &[Frame<'_>], inherits: bool) {
        self.local_scopes
            .push(stack[stack.len() - 1].node.byte_range());
        self.member_scope(stack, inherits);
    }

    /// A `@local.scope` capture of the last frame whose definitions are
    /// members (a Python class body, a TS namespace body).
    pub fn member_scope(&mut self, stack: &[Frame<'_>], inherits: bool) {
        self.scopes.push(Scope {
            depth: stack.len() - 1,
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
        if !name.has_error() && name.start_byte() < self.limit {
            self.refs.insert(self.text(name));
        }
    }

    /// A reference name from a pattern with `#is-not? local`.
    pub fn nonlocal(&mut self, name: Node<'_>) {
        let text = self.text(name);
        if !name.has_error() && name.start_byte() < self.limit && !self.is_local(text) {
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
        if name.has_error()
            || (matches!(class, TagClass::Def(_)) && name.start_byte() >= self.limit)
        {
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

/// A node kind, token or field a walker names and the grammar lacks: the
/// walker's own literals, which the log may keep.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Missing {
    /// `node_kind`, `token` or `field`.
    pub what: &'static str,
    pub name: &'static str,
}

pub(crate) fn kind(language: &Language, name: &'static str) -> Result<u16, Missing> {
    match language.id_for_node_kind(name, true) {
        0 => Err(Missing {
            what: "node_kind",
            name,
        }),
        id => Ok(id),
    }
}

pub(crate) fn anon(language: &Language, name: &'static str) -> Result<u16, Missing> {
    match language.id_for_node_kind(name, false) {
        0 => Err(Missing {
            what: "token",
            name,
        }),
        id => Ok(id),
    }
}

pub(crate) fn field(language: &Language, name: &'static str) -> Result<u16, Missing> {
    language
        .field_id_for_name(name)
        .map(|f| f.get())
        .ok_or(Missing {
            what: "field",
            name,
        })
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
            pub(super) fn new(language: &tree_sitter::Language) -> Result<$name, super::Missing> {
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
        w: Result<W, Missing>,
        dialect: Dialect,
    ) -> Option<Box<dyn Walker>> {
        match w {
            Ok(w) => Some(Box::new(w)),
            Err(missing) => {
                lectern_logging::warn!(
                    "the grammar lacks a node the walker names; its files use the tags queries";
                    dialect = dialect.name(),
                    missing = missing.what,
                    name = missing.name,
                );
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
    /// grammar lease is involved). Taken out while it parses; a parse
    /// stopped to yield keeps it (`Parse::Halted`), and the thread makes a
    /// new one.
    static PARSER: RefCell<Option<Parser>> = const { RefCell::new(None) };
}

/// The walk's result: definitions and markers, reference names, and the
/// local scopes for `rules::finish`.
pub(crate) struct Walked {
    pub tags: Vec<RawTag>,
    pub refs: Vec<Box<str>>,
    pub local_scopes: Vec<Range<usize>>,
}

/// Whether `dialect` has a walker for `language` and parsing can start:
/// the thread's parser (or `resumed`, a parser stopped to yield) for
/// [`parse`].
pub(crate) fn parser_for(language: &Language, resumed: Option<Parser>) -> Option<Parser> {
    if let Some(parser) = resumed {
        return Some(parser);
    }
    let mut parser = PARSER.with(|p| p.borrow_mut().take()).unwrap_or_default();
    parser.set_language(language).ok()?;
    Some(parser)
}

/// A parser done with: back to the thread for the next extraction.
pub(crate) fn give_back(mut parser: Parser) {
    parser.reset();
    PARSER.with(|p| *p.borrow_mut() = Some(parser));
}

/// What one call of [`parse`] did.
pub(crate) enum Parse {
    Tree(Tree),
    /// `yields` said so at a progress check: the parser keeps its state,
    /// and the next `parse` with the same source goes on from there (the
    /// tree is the one an uninterrupted parse gives, ADR 0034's research).
    Halted(Parser),
    /// `stop` was set (the deadline, cancellation, a pause): the parse is
    /// given up.
    Stopped,
}

/// Parses `source` with `parser` (from [`parser_for`]), checking `stop` and
/// then `yields` at every progress callback (about every 100 parse
/// operations).
pub(crate) fn parse(
    mut parser: Parser,
    source: &[u8],
    stop: &AtomicUsize,
    yields: &dyn Fn() -> bool,
) -> Parse {
    let mut halted = false;
    let tree = {
        let mut progress = |_: &tree_sitter::ParseState| {
            if stop.load(Ordering::Acquire) != 0 {
                ControlFlow::Break(())
            } else if yields() {
                halted = true;
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        parser.parse_with_options(
            &mut |i, _| source.get(i..).unwrap_or(&[]),
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        )
    };
    match tree {
        Some(tree) => {
            give_back(parser);
            Parse::Tree(tree)
        }
        None if halted && stop.load(Ordering::Acquire) == 0 => Parse::Halted(parser),
        None => {
            // An interrupted parse leaves state for resuming it.
            give_back(parser);
            Parse::Stopped
        }
    }
}

/// A walk stopped at a check every [`CHECK_EVERY`] nodes to yield: where
/// the cursor was and what was collected, without borrows of the tree or
/// the source (positions in the source instead), so it can wait in a
/// batch task with the tree and the source it was taken from.
pub(crate) struct WalkResume {
    /// The cursor's node, as `TreeCursor::descendant_index` from the root.
    descendant: usize,
    steps: u32,
    tags: Vec<RawTag>,
    refs: Vec<Range<usize>>,
    scopes: Vec<(usize, bool, Vec<Range<usize>>)>,
    local_scopes: Vec<Range<usize>>,
}

/// What one call of [`walk_from`] did.
pub(crate) enum WalkStep {
    Done(Walked),
    /// `yields` said so at a check; go on with [`walk_from`].
    Halted(WalkResume),
    /// `stop` said so, or the dialect has no walker.
    Stopped,
}

/// Walks `tree` from the start, or from where `resume` stopped, checking
/// `stop` and then `yields` every [`CHECK_EVERY`] nodes. Definitions and
/// references whose name starts at or after `limit` are left out (a
/// window's cut; `usize::MAX` for a whole file).
#[allow(
    clippy::too_many_arguments,
    reason = "the tree, its source, the checks and where to stop"
)]
pub(crate) fn walk_from(
    dialect: Dialect,
    language: &Language,
    tree: &Tree,
    source: &[u8],
    stop: &dyn Fn() -> bool,
    yields: &dyn Fn() -> bool,
    resume: Option<WalkResume>,
    limit: usize,
) -> WalkStep {
    match walker(dialect, language) {
        Some(walker) => walk(walker, tree, source, stop, yields, resume, limit),
        None => WalkStep::Stopped,
    }
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
    match walk(
        walker(dialect, language)?,
        tree,
        source,
        stop,
        &|| false,
        None,
        usize::MAX,
    ) {
        WalkStep::Done(walked) => Some(walked),
        WalkStep::Halted(_) | WalkStep::Stopped => None,
    }
}

fn walk(
    walker: &dyn Walker,
    tree: &Tree,
    source: &[u8],
    stop: &dyn Fn() -> bool,
    yields: &dyn Fn() -> bool,
    resume: Option<WalkResume>,
    limit: usize,
) -> WalkStep {
    let mut walk = Walk {
        walker,
        stop,
        yields,
        cx: Ctx::new(source, limit),
        stack: Vec::with_capacity(64),
        steps: 0,
    };
    let mut cursor = tree.walk();
    match resume {
        // The resumed node was entered and visited before the stop.
        Some(resume) => walk.restore(resume, &mut cursor),
        None => match walk.enter(&cursor) {
            Check::Go => {}
            Check::Stop => return WalkStep::Stopped,
            Check::Yield => return WalkStep::Halted(walk.save(&cursor)),
        },
    }
    // Only named nodes are entered: tokens are leaves, and the walkers read
    // the few they need (`export`, `private`) from their parent.
    'outer: loop {
        if cursor.goto_first_child() {
            if to_named(&mut cursor) {
                match walk.enter(&cursor) {
                    Check::Go => {}
                    Check::Stop => return WalkStep::Stopped,
                    Check::Yield => return WalkStep::Halted(walk.save(&cursor)),
                }
                continue;
            }
            cursor.goto_parent();
        }
        loop {
            walk.leave();
            if cursor.goto_next_sibling() && to_named(&mut cursor) {
                match walk.enter(&cursor) {
                    Check::Go => {}
                    Check::Stop => return WalkStep::Stopped,
                    Check::Yield => return WalkStep::Halted(walk.save(&cursor)),
                }
                continue 'outer;
            }
            if !cursor.goto_parent() {
                break 'outer;
            }
        }
    }
    if stop() {
        return WalkStep::Stopped;
    }
    let cx = walk.cx;
    let mut refs: Vec<Box<str>> = cx
        .refs
        .into_iter()
        .filter_map(|r| std::str::from_utf8(r).ok())
        .map(Box::from)
        .collect();
    refs.sort_unstable();
    WalkStep::Done(Walked {
        tags: cx.tags,
        refs,
        local_scopes: cx.local_scopes,
    })
}

/// What a check of the walk says.
enum Check {
    Go,
    Stop,
    Yield,
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
    yields: &'w dyn Fn() -> bool,
    cx: Ctx<'s>,
    stack: Vec<Frame<'t>>,
    steps: u32,
}

impl<'s, 't> Walk<'_, 's, 't> {
    /// Pushes the cursor's node and visits it when it is named; then, every
    /// [`CHECK_EVERY`] nodes, whether to stop or yield.
    fn enter(&mut self, cursor: &tree_sitter::TreeCursor<'t>) -> Check {
        let node = cursor.node();
        self.stack.push(Frame {
            node,
            kind: node.kind_id(),
            field: cursor.field_id().map_or(0, |f| f.get()),
        });
        self.walker.visit(&mut self.cx, &self.stack);
        self.steps += 1;
        if !self.steps.is_multiple_of(CHECK_EVERY) {
            Check::Go
        } else if (self.stop)() {
            Check::Stop
        } else if (self.yields)() {
            Check::Yield
        } else {
            Check::Go
        }
    }

    /// The walk's state at the cursor's node, just entered.
    fn save(self, cursor: &tree_sitter::TreeCursor<'t>) -> WalkResume {
        let base = self.cx.src.as_ptr() as usize;
        let range = |s: &[u8]| {
            let start = s.as_ptr() as usize - base;
            start..start + s.len()
        };
        WalkResume {
            descendant: cursor.descendant_index(),
            steps: self.steps,
            refs: self.cx.refs.iter().map(|r| range(r)).collect(),
            scopes: self
                .cx
                .scopes
                .iter()
                .map(|s| {
                    (
                        s.depth,
                        s.inherits,
                        s.defs.iter().map(|d| range(d)).collect(),
                    )
                })
                .collect(),
            tags: self.cx.tags,
            local_scopes: self.cx.local_scopes,
        }
    }

    /// Back to the state `save` took: the cursor on that node, the stack of
    /// its ancestors rebuilt from the tree.
    fn restore(&mut self, resume: WalkResume, cursor: &mut tree_sitter::TreeCursor<'t>) {
        cursor.goto_descendant(resume.descendant);
        let mut up = cursor.clone();
        let mut frames = Vec::new();
        loop {
            let node = up.node();
            frames.push(Frame {
                node,
                kind: node.kind_id(),
                field: up.field_id().map_or(0, |f| f.get()),
            });
            if !up.goto_parent() {
                break;
            }
        }
        frames.reverse();
        self.stack = frames;
        self.steps = resume.steps;
        let src: &'s [u8] = self.cx.src;
        self.cx.tags = resume.tags;
        self.cx.local_scopes = resume.local_scopes;
        self.cx.refs = resume.refs.into_iter().map(|r| &src[r]).collect();
        self.cx.scopes = resume
            .scopes
            .into_iter()
            .map(|(depth, inherits, defs)| Scope {
                depth,
                inherits,
                defs: defs.into_iter().map(|r| &src[r]).collect(),
            })
            .collect();
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
