//! The local binding model of one parsed file: scopes, the names each
//! declaration binds, from where, and how a reference resolves
//! (docs/design/code_intel.md「绑定规则」).
//!
//! The locals queries carry the per-language rules as pattern properties,
//! which tree-sitter-tags ignores: `local.kind` on scopes; `local.scope`,
//! `local.visible`, `local.until` and `local.certain` on definitions; `local.nonlocal` on
//! the references of Python's `global` / `nonlocal`. A captured pattern is
//! expanded here into the identifiers it binds.

use std::ops::Range;

use tree_sitter::{Node, QueryProperty};

use crate::lang::Dialect;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ScopeKind {
    Block,
    /// A function, lambda or comprehension, and the file itself.
    Function,
    /// A Python class body: visible to references directly in it, not to
    /// the functions inside it.
    Class,
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
    /// From the identifier on.
    Name,
}

struct RawDef {
    range: Range<usize>,
    visible_from: Option<usize>,
    visible_to: usize,
    function_scope: bool,
    certain: bool,
}

/// Collects scopes and definitions while the query runs.
pub(crate) struct Collector {
    dialect: Dialect,
    scopes: Vec<Scope>,
    defs: Vec<RawDef>,
    nonlocal: Vec<Range<usize>>,
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
            _ => ScopeKind::Block,
        };
        self.scopes.push(Scope {
            range: node.byte_range(),
            kind,
            parent: None,
            name: node.child_by_field_name("name").map(|n| n.byte_range()),
            comprehension: local_kind == Some("comprehension"),
        });
    }

    pub(crate) fn definition(&mut self, node: Node<'_>, props: &[QueryProperty], text: &[u8]) {
        let visible_from = match visible_rule(props) {
            Visible::Scope => Some(0),
            Visible::End(kind) => ancestor_or_self(node, kind).map(|n| n.end_byte()),
            Visible::Body(kind) => ancestor_or_self(node, kind)
                .and_then(|n| n.child_by_field_name("body"))
                .map(|b| b.start_byte()),
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
        let function_scope = property(props, "local.scope") == Some("function");
        let certain = property(props, "local.certain") != Some("false");
        let mut names = Vec::new();
        bindings(self.dialect, node, text, &mut names);
        for name in names {
            self.defs.push(RawDef {
                range: name.byte_range(),
                visible_from,
                visible_to,
                function_scope,
                certain,
            });
        }
    }

    /// A reference capture; Python's `global` / `nonlocal` names carry
    /// `local.nonlocal`.
    pub(crate) fn reference(&mut self, node: Node<'_>, props: &[QueryProperty]) -> bool {
        if property(props, "local.nonlocal") == Some("true") {
            self.nonlocal.push(node.byte_range());
            return false;
        }
        true
    }

    pub(crate) fn finish(self, text: &[u8]) -> Locals {
        let mut scopes = self.scopes;
        let root = scopes.remove(0);
        scopes.sort_by_key(|s| (s.range.start, std::cmp::Reverse(s.range.end)));
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
            scopes[i].parent = stack.last().copied();
            stack.push(i);
        }
        let mut raw = self.defs;
        raw.sort_by_key(|d| (d.range.start, d.range.end));
        raw.dedup_by(|b, a| a.range == b.range);
        let mut defs: Vec<LocalDef> = raw
            .into_iter()
            .map(|d| {
                let mut scope = innermost(&scopes, &d.range);
                if scopes[scope].name.as_ref() == Some(&d.range) {
                    scope = scopes[scope].parent.unwrap_or(0);
                }
                if d.function_scope {
                    while scopes[scope].kind != ScopeKind::Function || scopes[scope].comprehension {
                        scope = scopes[scope].parent.unwrap_or(0);
                    }
                }
                LocalDef {
                    visible_from: d.visible_from.unwrap_or(d.range.start),
                    visible_to: d.visible_to,
                    range: d.range,
                    scope,
                    certain: d.certain,
                }
            })
            .collect();
        defs.sort_by_key(|d| d.range.start);
        let nonlocal = self
            .nonlocal
            .into_iter()
            .filter_map(|r| {
                let mut scope = innermost(&scopes, &r);
                while scopes[scope].kind != ScopeKind::Function {
                    scope = scopes[scope].parent?;
                }
                Some((scope, text.get(r)?.to_vec()))
            })
            .collect();
        Locals {
            scopes,
            defs,
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
    /// (function scope, name) taken out by `global` / `nonlocal`.
    nonlocal: Vec<(usize, Vec<u8>)>,
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
        self.defs.iter().find(|d| d.range == *range)
    }

    /// Visible in one function or block only (`DefinitionRef::local`): not
    /// at file level, not a class member.
    pub(crate) fn is_local(&self, def: &LocalDef) -> bool {
        def.scope != 0 && self.scopes[def.scope].kind != ScopeKind::Class
    }

    /// The safety invariant (docs/design/code_intel.md「绑定规则」): a
    /// declaration binds only when the rules put it in scope at the
    /// reference; any other same-named declaration in an enclosing scope is
    /// only `possible`.
    pub(crate) fn resolve(&self, text: &[u8], reference: &Range<usize>) -> Resolution<'_> {
        let mut possible: Vec<&LocalDef> = Vec::new();
        let Some(name) = text.get(reference.clone()) else {
            return Resolution {
                binding: None,
                possible,
            };
        };
        let mut scope = Some(innermost(&self.scopes, reference));
        // Whether the walk has left a function or class body.
        let mut nested = false;
        while let Some(s) = scope {
            let kind = self.scopes[s].kind;
            // A class body is visible only to code directly in it, not to
            // the functions or classes inside it.
            let skip = kind == ScopeKind::Class && nested;
            let hidden = self.nonlocal.iter().any(|(f, n)| *f == s && n == name);
            if !skip {
                let mut before: Option<&LocalDef> = None;
                let mut after: Option<&LocalDef> = None;
                for def in self.defs.iter().filter(|d| d.scope == s) {
                    if text.get(def.range.clone()) != Some(name)
                        || def.range == *reference
                        || reference.start >= def.visible_to
                    {
                        // Another name, the declaration itself, or past
                        // the end of its scope.
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
                    if def.certain {
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
            if kind != ScopeKind::Block {
                nested = true;
            }
            scope = self.scopes[s].parent;
        }
        Resolution {
            binding: None,
            possible,
        }
    }
}

/// The innermost scope containing `range` (the file when no other does).
fn innermost(scopes: &[Scope], range: &Range<usize>) -> usize {
    scopes
        .iter()
        .enumerate()
        .rev()
        .find(|(_, s)| s.range.start <= range.start && range.end <= s.range.end)
        .map_or(0, |(i, _)| i)
}

// ---- pattern expansion -----------------------------------------------------

/// The node kinds that are bound names in a pattern.
fn is_binding_kind(dialect: Dialect, kind: &str) -> bool {
    match dialect {
        Dialect::Rust => matches!(kind, "identifier" | "shorthand_field_identifier"),
        Dialect::TypeScript | Dialect::Tsx | Dialect::JavaScript => {
            matches!(kind, "identifier" | "shorthand_property_identifier_pattern")
        }
        Dialect::Java | Dialect::Python | Dialect::Go | Dialect::Pack => kind == "identifier",
    }
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
    if is_binding_kind(dialect, node.kind()) {
        out.push(node);
    } else {
        nested(dialect, node, text, out);
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
