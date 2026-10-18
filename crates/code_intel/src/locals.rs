//! The local binding model of one parsed file: scopes, the names each
//! declaration binds, from where, and how a reference resolves
//! (docs/design/code_intel.md「绑定规则」).
//!
//! The locals queries carry the per-language rules as pattern properties,
//! which tree-sitter-tags ignores: `local.kind` on scopes; `local.scope`,
//! `local.visible`, `local.until`, `local.certain` and `local.namespace` on
//! definitions; `local.nonlocal` (`"global"` or `"true"`) on the names of
//! Python's `global` / `nonlocal`. A captured pattern is expanded here into the identifiers it
//! binds. The parts of a scope's node that are evaluated in the enclosing
//! scope (parameter defaults, base classes, …) are in [`ENCLOSING`].

use std::ops::Range;

use tree_sitter::{Node, QueryProperty};

use crate::lang::Dialect;

/// Which names a local binds: values (variables, functions), types (type
/// parameters, local type declarations), or both (a class name).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Namespace {
    Value,
    Type,
    Both,
}

impl Namespace {
    /// Whether a binding in `self` answers a reference in `wanted`.
    fn holds(self, wanted: Namespace) -> bool {
        self == Namespace::Both || self == wanted
    }
}

/// One step of a path from a scope's node to a part evaluated outside it.
#[derive(Clone, Copy)]
enum Step {
    /// The children in this field.
    Field(&'static str),
    /// Every named child.
    Any,
    /// The first named child of this kind.
    First(&'static str),
}

use Step::{Any, Field, First};

/// The parts of a scope's node that are evaluated in the scope around it
/// (docs/design/code_intel.md「绑定规则」, 作用域入口): references there,
/// and scopes and definitions inside them, resolve from the enclosing scope.
/// TSX follows the TypeScript rows.
const ENCLOSING: &[(Dialect, &str, &[Step])] = &[
    // Python: defaults and annotations of parameters, the return type, base
    // classes, and the first iterable of a comprehension.
    (
        Dialect::Python,
        "function_definition",
        &[Field("parameters"), Any, Field("value")],
    ),
    (
        Dialect::Python,
        "function_definition",
        &[Field("parameters"), Any, Field("type")],
    ),
    (
        Dialect::Python,
        "function_definition",
        &[Field("return_type")],
    ),
    (
        Dialect::Python,
        "lambda",
        &[Field("parameters"), Any, Field("value")],
    ),
    (
        Dialect::Python,
        "class_definition",
        &[Field("superclasses")],
    ),
    (
        Dialect::Python,
        "list_comprehension",
        &[First("for_in_clause"), Field("right")],
    ),
    (
        Dialect::Python,
        "set_comprehension",
        &[First("for_in_clause"), Field("right")],
    ),
    (
        Dialect::Python,
        "dictionary_comprehension",
        &[First("for_in_clause"), Field("right")],
    ),
    (
        Dialect::Python,
        "generator_expression",
        &[First("for_in_clause"), Field("right")],
    ),
    // JS / TS: a computed method name (`[key]() {}`); TS: parameter
    // decorators (`constructor(@Inject(TOKEN) x)`).
    (Dialect::JavaScript, "method_definition", &[Field("name")]),
    (Dialect::TypeScript, "method_definition", &[Field("name")]),
    (
        Dialect::TypeScript,
        "method_definition",
        &[Field("parameters"), Any, Field("decorator")],
    ),
];

/// The ranges of `node`'s parts that [`ENCLOSING`] lists.
fn enclosing_parts(dialect: Dialect, node: Node<'_>) -> Vec<Range<usize>> {
    let dialect = if dialect == Dialect::Tsx {
        Dialect::TypeScript
    } else {
        dialect
    };
    let mut out = Vec::new();
    for (d, kind, path) in ENCLOSING {
        if *d != dialect || node.kind() != *kind {
            continue;
        }
        let mut nodes = vec![node];
        for step in *path {
            let mut next = Vec::new();
            for n in nodes {
                let mut cursor = n.walk();
                match step {
                    Field(field) => next.extend(n.children_by_field_name(field, &mut cursor)),
                    Any => next.extend(n.named_children(&mut cursor)),
                    First(kind) => {
                        next.extend(n.named_children(&mut cursor).find(|c| c.kind() == *kind));
                    }
                }
            }
            nodes = next;
        }
        out.extend(nodes.iter().map(Node::byte_range));
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ScopeKind {
    Block,
    /// A function, lambda or comprehension, and the file itself.
    Function,
    /// A Python class body: visible to references directly in it, not to
    /// the functions inside it.
    Class,
    /// A TS namespace or module body (`local.kind "namespace"`): a block
    /// whose declarations are members, not locals.
    Namespace,
    /// A JS `with` body (`local.kind "dynamic"`): a name it does not declare
    /// may be a property of the object, so a declaration found outside it
    /// is only possible.
    Dynamic,
}

impl ScopeKind {
    /// Whether a definition inside a scope of this kind is local to the
    /// file (`Visibility::File`): every scope but a class or namespace body,
    /// whose definitions are members.
    pub(crate) fn is_local(self) -> bool {
        !matches!(self, ScopeKind::Class | ScopeKind::Namespace)
    }
}

/// A Python scope directive on a name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Directive {
    /// `global x`: the name is the module's, past every enclosing function.
    Global,
    /// `nonlocal x`: the name is an enclosing function's, never the
    /// module's.
    Nonlocal,
}

pub(crate) struct Scope {
    pub range: Range<usize>,
    pub kind: ScopeKind,
    pub parent: Option<usize>,
    /// The `name` field of the scope's node (a function or class that also
    /// opens this scope): a definition there belongs to the parent.
    name: Option<Range<usize>>,
    /// A Python comprehension: a function scope that `local.scope
    /// "function"` (`:=`) skips.
    comprehension: bool,
    /// Parts of the scope's node evaluated in the enclosing scope.
    enclosing: Vec<Range<usize>>,
    /// A function's `body` field: a declaration scoped to the function
    /// (`var`) lives there, so parameter defaults do not see it.
    body: Option<Range<usize>>,
    /// The innermost scope containing this one by range (`parent` skips
    /// a scope whose part evaluated outside contains this one); 0 for the
    /// file.
    outer: usize,
}

impl Scope {
    fn encloses(&self, range: &Range<usize>) -> bool {
        self.enclosing
            .iter()
            .any(|o| o.start <= range.start && range.end <= o.end)
    }
}

pub(crate) struct LocalDef {
    /// The bound identifier.
    pub range: Range<usize>,
    pub scope: usize,
    /// References at or after this offset are in the declaration's scope;
    /// 0 for the whole scope (hoisting).
    pub visible_from: usize,
    /// References at or after this offset are out of scope again (the
    /// `else` of a Rust `if let`); `usize::MAX` for the end of the scope.
    pub visible_to: usize,
    /// False for forms whose scope depends on control flow: they offer a
    /// candidate, never a binding.
    pub certain: bool,
    pub namespace: Namespace,
    /// Visible only within its owner (`local.visible "owner:…"`): before
    /// `visible_from` it is out of scope, not merely declared later.
    pub owned: bool,
}

/// Where a definition binds from.
#[derive(Clone, Copy)]
enum Visible {
    /// The whole scope.
    Scope,
    /// From the end of the nearest node of this kind around the capture.
    End(&'static str),
    /// From the start of the `body` of the nearest node of this kind.
    Body(&'static str),
    /// Within the parent of the nearest node of this kind (the declaration
    /// that owns a type parameter list), and nowhere else.
    Owner(&'static str),
    /// Within the nearest node of this kind itself.
    Within(&'static str),
    /// From the identifier on.
    Name,
}

struct RawDef {
    range: Range<usize>,
    visible_from: Option<usize>,
    visible_to: usize,
    function_scope: bool,
    certain: bool,
    namespace: Namespace,
    /// The owner's range, for `local.visible "owner:…"`.
    owner: Option<Range<usize>>,
    /// `local.redeclare "true"` (Go's `:=`): a use of an earlier
    /// declaration in the same scope, when there is one.
    redeclare: bool,
}

/// Collects scopes and definitions while the query runs.
pub(crate) struct Collector {
    dialect: Dialect,
    scopes: Vec<Scope>,
    defs: Vec<RawDef>,
    nonlocal: Vec<(Range<usize>, Directive)>,
}

/// The kinds in `local.visible "end:KIND"` / `"body:KIND"` values, so that
/// the parsed rule can hold a `&'static str`.
const DECLARATION_KINDS: &[&str] = &[
    "let_declaration",
    "let_condition",
    "for_expression",
    "variable_declarator",
    "enhanced_for_statement",
    "resource",
    "short_var_declaration",
    "var_spec",
    "const_spec",
    "for_statement",
    "receive_statement",
    "type_parameters",
    "type_parameter_list",
    "type_parameter",
    "mapped_type_clause",
    "type_alias_statement",
];

fn property<'p>(props: &'p [QueryProperty], key: &str) -> Option<&'p str> {
    props
        .iter()
        .find(|p| &*p.key == key)
        .and_then(|p| p.value.as_deref())
}

fn visible_rule(props: &[QueryProperty]) -> Visible {
    let Some(value) = property(props, "local.visible") else {
        return Visible::Name;
    };
    let kind = |k: &str| DECLARATION_KINDS.iter().copied().find(|d| *d == k);
    if value == "scope" {
        Visible::Scope
    } else if let Some(k) = value.strip_prefix("end:").and_then(kind) {
        Visible::End(k)
    } else if let Some(k) = value.strip_prefix("body:").and_then(kind) {
        Visible::Body(k)
    } else if let Some(k) = value.strip_prefix("owner:").and_then(kind) {
        Visible::Owner(k)
    } else if let Some(k) = value.strip_prefix("within:").and_then(kind) {
        Visible::Within(k)
    } else {
        Visible::Name
    }
}

fn ancestor_or_self<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut node = Some(node);
    while let Some(n) = node {
        if n.kind() == kind {
            return Some(n);
        }
        node = n.parent();
    }
    None
}

impl Collector {
    pub(crate) fn new(dialect: Dialect, len: usize) -> Collector {
        Collector {
            dialect,
            scopes: vec![Scope {
                range: 0..len,
                kind: ScopeKind::Function,
                parent: None,
                name: None,
                comprehension: false,
                enclosing: Vec::new(),
                body: None,
                outer: 0,
            }],
            defs: Vec::new(),
            nonlocal: Vec::new(),
        }
    }

    pub(crate) fn scope(&mut self, node: Node<'_>, props: &[QueryProperty]) {
        let local_kind = property(props, "local.kind");
        let kind = match local_kind {
            Some("function" | "comprehension") => ScopeKind::Function,
            Some("class") => ScopeKind::Class,
            Some("namespace") => ScopeKind::Namespace,
            Some("dynamic") => ScopeKind::Dynamic,
            _ => ScopeKind::Block,
        };
        self.scopes.push(Scope {
            range: node.byte_range(),
            kind,
            parent: None,
            // A function's or class's own name; a block's `name` field is
            // what it binds (Java's for-each variable).
            name: matches!(kind, ScopeKind::Function | ScopeKind::Class)
                .then(|| node.child_by_field_name("name"))
                .flatten()
                .map(|n| n.byte_range()),
            comprehension: local_kind == Some("comprehension"),
            enclosing: enclosing_parts(self.dialect, node),
            body: (kind == ScopeKind::Function)
                .then(|| node.child_by_field_name("body"))
                .flatten()
                .map(|b| b.byte_range()),
            outer: 0,
        });
    }

    pub(crate) fn definition(&mut self, node: Node<'_>, props: &[QueryProperty], text: &[u8]) {
        let rule = visible_rule(props);
        let owner = match rule {
            Visible::Owner(kind) => ancestor_or_self(node, kind).and_then(|n| n.parent()),
            Visible::Within(kind) => ancestor_or_self(node, kind),
            _ => None,
        };
        let visible_from = match rule {
            Visible::Scope => Some(0),
            Visible::End(kind) => ancestor_or_self(node, kind).map(|n| n.end_byte()),
            Visible::Body(kind) => ancestor_or_self(node, kind)
                .and_then(|n| n.child_by_field_name("body"))
                .map(|b| b.start_byte()),
            Visible::Owner(_) | Visible::Within(_) => owner.map(|o| o.start_byte()),
            Visible::Name => None,
        };
        // `local.until "FIELD:KIND|KIND…"`: up to the start of that field of
        // the nearest node of one of the kinds, or to that node's end when it
        // has no such field.
        let visible_to = property(props, "local.until")
            .and_then(|v| v.split_once(':'))
            .and_then(|(field, kinds)| {
                let mut n = Some(node);
                while let Some(a) = n {
                    if kinds.split('|').any(|k| k == a.kind()) {
                        return Some(
                            a.child_by_field_name(field)
                                .map_or(a.end_byte(), |f| f.start_byte()),
                        );
                    }
                    n = a.parent();
                }
                None
            })
            .unwrap_or(usize::MAX);
        let visible_to = owner.map_or(visible_to, |o| o.end_byte());
        let function_scope = property(props, "local.scope") == Some("function");
        let certain = property(props, "local.certain") != Some("false");
        let redeclare = property(props, "local.redeclare") == Some("true");
        let namespace = match property(props, "local.namespace") {
            Some("type") => Namespace::Type,
            Some("both") => Namespace::Both,
            _ => Namespace::Value,
        };
        let mut names = Vec::new();
        bindings(self.dialect, node, text, &mut names);
        for name in names {
            self.defs.push(RawDef {
                range: name.byte_range(),
                visible_from,
                visible_to,
                function_scope,
                certain,
                namespace,
                owner: owner.map(|o| o.byte_range()),
                redeclare,
            });
        }
    }

    /// A reference capture; Python's `global` / `nonlocal` names carry
    /// `local.nonlocal "global"` / `"true"`.
    pub(crate) fn reference(&mut self, node: Node<'_>, props: &[QueryProperty]) -> bool {
        let directive = match property(props, "local.nonlocal") {
            Some("global") => Directive::Global,
            Some("true") => Directive::Nonlocal,
            _ => return true,
        };
        self.nonlocal.push((node.byte_range(), directive));
        false
    }

    pub(crate) fn finish(self, text: &[u8]) -> Locals {
        let mut scopes = self.scopes;
        let root = scopes.remove(0);
        // Two captures of one node (a block that is also a namespace or a
        // `with` body) keep the more specific kind.
        scopes.sort_by_key(|s| {
            (
                s.range.start,
                std::cmp::Reverse(s.range.end),
                s.kind == ScopeKind::Block,
            )
        });
        scopes.dedup_by(|b, a| a.range == b.range);
        scopes.insert(0, root);
        let mut stack: Vec<usize> = vec![0];
        for i in 1..scopes.len() {
            let range = scopes[i].range.clone();
            while let Some(&top) = stack.last() {
                let r = &scopes[top].range;
                if r.start <= range.start && range.end <= r.end {
                    break;
                }
                stack.pop();
            }
            // A scope inside a part evaluated outside its container (a
            // lambda in a default value) has the container's parent.
            let mut parent = stack.last().copied().unwrap_or(0);
            scopes[i].outer = parent;
            while parent != 0 && scopes[parent].encloses(&range) {
                parent = scopes[parent].parent.unwrap_or(0);
            }
            scopes[i].parent = Some(parent);
            stack.push(i);
        }
        let by_range: std::collections::HashMap<(usize, usize), usize> = scopes
            .iter()
            .enumerate()
            .map(|(i, s)| ((s.range.start, s.range.end), i))
            .collect();
        let mut raw = self.defs;
        raw.sort_by_key(|d| (d.range.start, d.range.end));
        raw.dedup_by(|b, a| a.range == b.range);
        let mut defs: Vec<(LocalDef, bool)> = raw
            .into_iter()
            .map(|d| {
                let mut scope = home(&scopes, &d.range);
                if scopes[scope].name.as_ref() == Some(&d.range) {
                    scope = scopes[scope].parent.unwrap_or(0);
                }
                // An owned definition (a type parameter) lives in the scope
                // around its owner, bounded to the owner's range, so that
                // what the owner contains (a class's methods) sees it.
                if let Some(owner) = &d.owner {
                    scope = home(&scopes, owner);
                    if scopes[scope].range == *owner {
                        scope = scopes[scope].parent.unwrap_or(0);
                    }
                }
                if d.function_scope {
                    while scopes[scope].kind != ScopeKind::Function || scopes[scope].comprehension {
                        scope = scopes[scope].parent.unwrap_or(0);
                    }
                    // In the function's body block when it is a scope (JS):
                    // parameter defaults are evaluated before the body's
                    // `var` declarations exist.
                    if let Some(body) = &scopes[scope].body
                        && let Some(&b) = by_range.get(&(body.start, body.end))
                    {
                        scope = b;
                    }
                }
                let def = LocalDef {
                    visible_from: d.visible_from.unwrap_or(d.range.start),
                    visible_to: d.visible_to,
                    range: d.range,
                    scope,
                    certain: d.certain,
                    namespace: d.namespace,
                    owned: d.owner.is_some(),
                };
                (def, d.redeclare)
            })
            .collect();
        defs.sort_by_key(|(d, _)| d.range.start);
        // Go's `:=` declares only the names new to its scope: one declared
        // earlier in the same scope, or among the parameters and results
        // when the scope is the function's body, is a use of that variable
        // (a reference, resolved like any other).
        let mut declared: std::collections::HashSet<(usize, &[u8])> =
            std::collections::HashSet::new();
        let defs: Vec<LocalDef> = defs
            .into_iter()
            .filter_map(|(def, redeclare)| {
                let name = text.get(def.range.clone()).unwrap_or_default();
                if redeclare {
                    let function = scopes[def.scope]
                        .parent
                        .filter(|f| scopes[*f].body.as_ref() == Some(&scopes[def.scope].range));
                    if declared.contains(&(def.scope, name))
                        || function.is_some_and(|f| declared.contains(&(f, name)))
                    {
                        return None;
                    }
                }
                declared.insert((def.scope, name));
                Some(def)
            })
            .collect();
        let nonlocal = self
            .nonlocal
            .into_iter()
            .filter_map(|(r, directive)| {
                let mut scope = home(&scopes, &r);
                while scopes[scope].kind != ScopeKind::Function {
                    scope = scopes[scope].parent?;
                }
                // `global` at module level changes nothing.
                if scope == 0 && directive == Directive::Global {
                    return None;
                }
                Some((scope, text.get(r)?.to_vec(), directive))
            })
            .collect();
        let mut scope_defs = vec![Vec::new(); scopes.len()];
        for (i, def) in defs.iter().enumerate() {
            scope_defs[def.scope].push(i);
        }
        Locals {
            scopes,
            defs,
            scope_defs,
            nonlocal,
        }
    }
}

/// The resolved binding model of one file.
pub(crate) struct Locals {
    /// Index 0 is the file; outermost first.
    scopes: Vec<Scope>,
    /// By position.
    defs: Vec<LocalDef>,
    /// For each scope, the indices of its definitions in `defs`, by
    /// position.
    scope_defs: Vec<Vec<usize>>,
    /// (function scope, name, directive) taken out by `global` /
    /// `nonlocal`.
    nonlocal: Vec<(usize, Vec<u8>, Directive)>,
}

/// How a reference resolves.
pub(crate) struct Resolution<'a> {
    /// The declaration in scope at the reference, when the binding rules make
    /// it certain: a direct jump that hides the index candidates.
    pub binding: Option<&'a LocalDef>,
    /// Same-named declarations in the scopes around the reference that may be
    /// the target but are not certain (declared later in a non-hoisting
    /// form, flow-scoped, or behind `global` / `nonlocal`).
    pub possible: Vec<&'a LocalDef>,
}

impl Locals {
    pub(crate) fn defs(&self) -> &[LocalDef] {
        &self.defs
    }

    pub(crate) fn def_at(&self, range: &Range<usize>) -> Option<&LocalDef> {
        // By position: definitions do not overlap.
        let i = self.defs.partition_point(|d| d.range.start < range.start);
        self.defs.get(i).filter(|d| d.range == *range)
    }

    /// Visible in one function or block only (`DefinitionRef::local`): not
    /// at file level, not a class or namespace member.
    pub(crate) fn is_local(&self, def: &LocalDef) -> bool {
        def.scope != 0 && self.scopes[def.scope].kind.is_local()
    }

    /// The ranges of the scopes whose definitions are local to the file
    /// ([`ScopeKind::is_local`]), without the file itself, by start and
    /// outermost first: `rules::finish` makes a definition inside one
    /// `Visibility::File`.
    pub(crate) fn local_scopes(&self) -> Vec<Range<usize>> {
        self.scopes
            .iter()
            .skip(1)
            .filter(|s| s.kind.is_local())
            .map(|s| s.range.clone())
            .collect()
    }

    /// The safety invariant (docs/design/code_intel.md「绑定规则」): a
    /// declaration binds only when the rules put it in scope at the
    /// reference; any other same-named declaration in an enclosing scope is
    /// only `possible`. `namespace` is the reference's: a type position looks
    /// for types, any other position for values.
    pub(crate) fn resolve(
        &self,
        text: &[u8],
        reference: &Range<usize>,
        namespace: Namespace,
    ) -> Resolution<'_> {
        let mut possible: Vec<&LocalDef> = Vec::new();
        let Some(name) = text.get(reference.clone()) else {
            return Resolution {
                binding: None,
                possible,
            };
        };
        // Parts evaluated outside their scope (defaults, base classes)
        // resolve from the scope around it.
        let mut scope = Some(home(&self.scopes, reference));
        // Whether the walk has left a function or class body.
        let mut nested = false;
        // Whether it has left a `with` body: what it finds is only possible.
        let mut dynamic = false;
        // Whether it has passed a `nonlocal` for the name: the module's
        // declarations are not the target.
        let mut enclosing_only = false;
        while let Some(s) = scope {
            let kind = self.scopes[s].kind;
            if s == 0 && enclosing_only {
                break;
            }
            // A class body is visible only to code directly in it, not to
            // the functions or classes inside it.
            let skip = kind == ScopeKind::Class && nested;
            let directive = self
                .nonlocal
                .iter()
                .find(|(f, n, _)| *f == s && n == name)
                .map(|(_, _, d)| *d);
            let hidden = directive.is_some();
            if !skip {
                let mut before: Option<&LocalDef> = None;
                let mut after: Option<&LocalDef> = None;
                for def in self.scope_defs[s].iter().map(|&i| &self.defs[i]) {
                    if text.get(def.range.clone()) != Some(name)
                        || def.range == *reference
                        || !def.namespace.holds(namespace)
                        || reference.start >= def.visible_to
                        || def.owned && reference.start < def.visible_from
                    {
                        // Another name or namespace, the declaration itself,
                        // or outside the part of the scope it covers.
                        continue;
                    }
                    if hidden || def.visible_from > reference.start {
                        // Not in scope here (declared later in a
                        // non-hoisting form), or taken out by `global`.
                        possible.push(def);
                    } else if def.range.start < reference.start {
                        // The last declaration before the reference.
                        before = Some(def);
                    } else if after.is_none() {
                        // A hoisted declaration after the reference counts
                        // only when none comes before.
                        after = Some(def);
                    }
                }
                if let Some(def) = before.or(after) {
                    if def.certain && !dynamic {
                        return Resolution {
                            binding: Some(def),
                            possible: Vec::new(),
                        };
                    }
                    possible.push(def);
                    return Resolution {
                        binding: None,
                        possible,
                    };
                }
            }
            if matches!(kind, ScopeKind::Function | ScopeKind::Class) {
                nested = true;
            }
            if kind == ScopeKind::Dynamic {
                dynamic = true;
            }
            scope = match directive {
                // Past every enclosing function, to the module.
                Some(Directive::Global) => Some(0),
                Some(Directive::Nonlocal) => {
                    enclosing_only = true;
                    self.scopes[s].parent
                }
                None => self.scopes[s].parent,
            };
        }
        Resolution {
            binding: None,
            possible,
        }
    }
}

/// The scope `range` is evaluated in: the innermost scope containing it,
/// or the scope around that one when `range` is in a part of its node
/// evaluated outside it ([`ENCLOSING`]).
fn home(scopes: &[Scope], range: &Range<usize>) -> usize {
    let mut scope = innermost(scopes, range);
    while scope != 0 && scopes[scope].encloses(range) {
        scope = scopes[scope].parent.unwrap_or(0);
    }
    scope
}

/// The innermost scope containing `range` (the file when no other does).
/// The scopes are nested and ordered by start, outermost first: the last
/// one starting at or before `range` either contains it or has it among
/// the scopes around it, so a binary search and a walk outwards find it.
fn innermost(scopes: &[Scope], range: &Range<usize>) -> usize {
    let mut i = scopes
        .partition_point(|s| s.range.start <= range.start)
        .saturating_sub(1);
    while i != 0 && !(scopes[i].range.start <= range.start && range.end <= scopes[i].range.end) {
        i = scopes[i].outer;
    }
    i
}

// ---- pattern expansion -----------------------------------------------------

/// The node kinds that are names in a dialect, with whether a pattern
/// binds them: the binding kinds, then the kinds that only name something
/// in a reference (members, types, packages). `site::is_identifier` (where
/// a position resolves) and the pattern expansion below both read it.
fn name_kinds(dialect: Dialect) -> &'static [(&'static str, bool)] {
    match dialect {
        Dialect::Rust => &[
            ("identifier", true),
            ("shorthand_field_identifier", true),
            ("field_identifier", false),
            ("type_identifier", false),
        ],
        Dialect::TypeScript | Dialect::Tsx | Dialect::JavaScript => &[
            ("identifier", true),
            ("shorthand_property_identifier_pattern", true),
            ("property_identifier", false),
            ("private_property_identifier", false),
            ("shorthand_property_identifier", false),
            ("type_identifier", false),
        ],
        Dialect::Java => &[("identifier", true), ("type_identifier", false)],
        Dialect::Python => &[("identifier", true)],
        Dialect::Go => &[
            ("identifier", true),
            ("field_identifier", false),
            ("type_identifier", false),
            ("package_identifier", false),
        ],
        Dialect::Pack => &[("identifier", true)],
    }
}

/// Whether `kind` is a name in `dialect`; in a language pack, any kind
/// ending in `identifier`.
pub(crate) fn is_name_kind(dialect: Dialect, kind: &str) -> bool {
    if dialect == Dialect::Pack {
        return kind.ends_with("identifier");
    }
    name_kinds(dialect).iter().any(|(k, _)| *k == kind)
}

/// The node kinds that are bound names in a pattern.
fn is_binding_kind(dialect: Dialect, kind: &str) -> bool {
    name_kinds(dialect)
        .iter()
        .any(|(k, binds)| *binds && *k == kind)
}

/// Parts of a pattern that bind nothing: (parent kind or "*", field or "*",
/// child kind or "*"). Constructors and paths (`Some(v)`, `Point { x }`),
/// default values, type annotations, keys, and attribute or subscript
/// targets.
const NON_BINDING: &[(Dialect, &str, &str, &str)] = &[
    (Dialect::Rust, "tuple_struct_pattern", "type", "*"),
    (Dialect::Rust, "struct_pattern", "type", "*"),
    (Dialect::Rust, "parameter", "type", "*"),
    (Dialect::Rust, "match_pattern", "condition", "*"),
    (Dialect::Rust, "*", "*", "scoped_identifier"),
    (Dialect::Rust, "*", "*", "range_pattern"),
    (Dialect::Rust, "*", "*", "macro_invocation"),
    (Dialect::JavaScript, "assignment_pattern", "right", "*"),
    (
        Dialect::JavaScript,
        "object_assignment_pattern",
        "right",
        "*",
    ),
    (Dialect::JavaScript, "pair_pattern", "key", "*"),
    (Dialect::JavaScript, "*", "*", "computed_property_name"),
    (Dialect::JavaScript, "*", "*", "member_expression"),
    (Dialect::JavaScript, "*", "*", "subscript_expression"),
    (Dialect::TypeScript, "assignment_pattern", "right", "*"),
    (
        Dialect::TypeScript,
        "object_assignment_pattern",
        "right",
        "*",
    ),
    (Dialect::TypeScript, "pair_pattern", "key", "*"),
    (Dialect::TypeScript, "required_parameter", "type", "*"),
    (Dialect::TypeScript, "required_parameter", "value", "*"),
    (Dialect::TypeScript, "required_parameter", "decorator", "*"),
    (Dialect::TypeScript, "optional_parameter", "type", "*"),
    (Dialect::TypeScript, "optional_parameter", "value", "*"),
    (Dialect::TypeScript, "optional_parameter", "decorator", "*"),
    (Dialect::TypeScript, "*", "*", "type_annotation"),
    (Dialect::TypeScript, "*", "*", "computed_property_name"),
    (Dialect::TypeScript, "*", "*", "member_expression"),
    (Dialect::TypeScript, "*", "*", "subscript_expression"),
    (Dialect::Python, "default_parameter", "value", "*"),
    (Dialect::Python, "typed_default_parameter", "value", "*"),
    (Dialect::Python, "typed_default_parameter", "type", "*"),
    (Dialect::Python, "typed_parameter", "type", "*"),
    (Dialect::Python, "*", "*", "attribute"),
    (Dialect::Python, "*", "*", "subscript"),
];

fn non_binding(dialect: Dialect, parent: &str, field: Option<&str>, child: &str) -> bool {
    // TSX follows the TypeScript rows.
    let dialect = if dialect == Dialect::Tsx {
        Dialect::TypeScript
    } else {
        dialect
    };
    NON_BINDING.iter().any(|(d, p, f, c)| {
        *d == dialect
            && (*p == "*" || *p == parent)
            && (*f == "*" || Some(*f) == field)
            && (*c == "*" || *c == child)
    })
}

/// The identifiers a captured definition binds: the node itself when it is a
/// name, otherwise the names in its pattern.
fn bindings<'t>(dialect: Dialect, node: Node<'t>, text: &[u8], out: &mut Vec<Node<'t>>) {
    // The captured node itself may bind nothing (`obj.count = 1`).
    if non_binding(dialect, "", None, node.kind()) {
        return;
    }
    // A captured type name (a type parameter, a local interface) binds
    // itself; inside patterns type names are never bindings.
    if is_binding_kind(dialect, node.kind()) || node.kind() == "type_identifier" {
        out.push(node);
    } else if dialect == Dialect::Python && node.kind() == "case_pattern" {
        case_captures(node, out);
    } else {
        nested(dialect, node, text, out);
    }
}

/// The names a Python `case` pattern captures: a bare name (`case x`, the
/// value of `y=yy`), `*rest`, `**rest` and `… as name`. A class name, a
/// keyword, a dotted value (`Color.RED`) and a mapping key are uses.
fn case_captures<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>) {
    let mut cursor = node.walk();
    let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
    match node.kind() {
        "dotted_name" => {
            if let [name] = children.as_slice()
                && name.kind() == "identifier"
                && node.parent().is_some_and(|p| p.kind() != "class_pattern")
            {
                out.push(*name);
            }
        }
        "splat_pattern" => out.extend(children.iter().filter(|c| c.kind() == "identifier")),
        "as_pattern" => {
            for child in children {
                if child.kind() == "identifier" {
                    out.push(child);
                } else {
                    case_captures(child, out);
                }
            }
        }
        "keyword_pattern" => {
            // The first identifier is the keyword.
            for child in children.into_iter().skip(1) {
                case_captures(child, out);
            }
        }
        "dict_pattern" => {
            let mut cursor = node.walk();
            for child in node.children_by_field_name("value", &mut cursor) {
                case_captures(child, out);
            }
            for child in children {
                if child.kind() == "splat_pattern" {
                    case_captures(child, out);
                }
            }
        }
        _ => {
            for child in children {
                case_captures(child, out);
            }
        }
    }
}

fn nested<'t>(dialect: Dialect, node: Node<'t>, text: &[u8], out: &mut Vec<Node<'t>>) {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        if child.is_named() && !non_binding(dialect, node.kind(), cursor.field_name(), child.kind())
        {
            if is_binding_kind(dialect, child.kind()) {
                // Inside a Rust pattern a capitalised name is a unit variant
                // or a constant (`None`, `MAX`), not a binding.
                let constant = dialect == Dialect::Rust
                    && child.kind() == "identifier"
                    && text
                        .get(child.start_byte())
                        .is_some_and(u8::is_ascii_uppercase);
                if !constant {
                    out.push(child);
                }
            } else {
                nested(dialect, child, text, out);
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}
