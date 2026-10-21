//! The module specifiers of a TS / JS file (「依赖层」的声明闭包): the edges
//! the closure follows, read from the tree the extraction's walk parsed, so
//! a dependency file is parsed once. Top-level `import`, `export … from`
//! and `import x = require(…)` statements, `/// <reference path>` and
//! `/// <reference types>` comments, and `import("…")` calls anywhere (the
//! types of `typeof import("…")`), the latter only when the source has one.

use tree_sitter::{Node, Tree};

use crate::extract::{SpecKind, Specifier};

/// Specifiers kept per file: a declaration file has a few dozen.
const MAX_SPECS: usize = 4096;

/// The file's specifiers in the order they first appear, each (kind, text)
/// once.
pub(crate) fn specifiers(tree: &Tree, source: &[u8]) -> Vec<Specifier> {
    let mut out = Specs::default();
    let root = tree.root_node();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        match node.kind() {
            "import_statement" => {
                let source_node = node.child_by_field_name("source").or_else(|| {
                    let mut c = node.walk();
                    let clause = node
                        .named_children(&mut c)
                        .find(|n| n.kind() == "import_require_clause");
                    clause.and_then(|n| n.child_by_field_name("source"))
                });
                if let Some(s) = source_node {
                    out.push(SpecKind::Module, string_text(s, source));
                }
            }
            "export_statement" => {
                if let Some(s) = node.child_by_field_name("source") {
                    out.push(SpecKind::Module, string_text(s, source));
                }
            }
            "comment" => {
                if let Some((kind, text)) = reference(&source[node.byte_range()]) {
                    out.push(kind, Some(text));
                }
            }
            _ => {}
        }
    }
    if memchr::memmem::find(source, b"import").is_some() && has_import_call(source) {
        let mut cursor = root.walk();
        'walk: loop {
            let node = cursor.node();
            if node.kind() == "call_expression"
                && node
                    .child_by_field_name("function")
                    .is_some_and(|f| f.kind() == "import")
                && let Some(arguments) = node.child_by_field_name("arguments")
            {
                let mut c = arguments.walk();
                if let Some(first) = arguments.named_children(&mut c).next() {
                    out.push(SpecKind::Module, string_text(first, source));
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    continue 'walk;
                }
                if !cursor.goto_parent() {
                    break 'walk;
                }
            }
        }
    }
    out.list
}

#[derive(Default)]
struct Specs {
    list: Vec<Specifier>,
}

impl Specs {
    fn push(&mut self, kind: SpecKind, text: Option<&str>) {
        let Some(text) = text.filter(|t| !t.is_empty()) else {
            return;
        };
        if self.list.len() >= MAX_SPECS
            || self.list.iter().any(|s| s.kind == kind && &*s.text == text)
        {
            return;
        }
        self.list.push(Specifier {
            kind,
            text: text.into(),
        });
    }
}

/// `import` followed by optional blanks and `(`.
fn has_import_call(source: &[u8]) -> bool {
    memchr::memmem::find_iter(source, b"import").any(|at| {
        source[at + 6..]
            .iter()
            .find(|b| !b.is_ascii_whitespace())
            .is_some_and(|b| *b == b'(')
    })
}

/// A string literal's text without its quotes; None for anything else.
fn string_text<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    if node.kind() != "string" {
        return None;
    }
    let raw = std::str::from_utf8(&source[node.byte_range()]).ok()?;
    let quote = raw.chars().next()?;
    if !matches!(quote, '"' | '\'') || raw.len() < 2 || !raw.ends_with(quote) {
        return None;
    }
    Some(&raw[1..raw.len() - 1])
}

/// A triple-slash directive: `/// <reference path="…" />` or `types="…"`.
fn reference(comment: &[u8]) -> Option<(SpecKind, &str)> {
    let text = std::str::from_utf8(comment).ok()?;
    let rest = text.strip_prefix("///")?.trim_start();
    let rest = rest.strip_prefix("<reference")?;
    for (attr, kind) in [("path", SpecKind::Path), ("types", SpecKind::Types)] {
        let mut from = 0;
        while let Some(i) = rest[from..].find(attr) {
            let at = from + i;
            from = at + attr.len();
            // A whole attribute name: preceded by a blank.
            if !rest[..at].ends_with(|c: char| c.is_ascii_whitespace()) {
                continue;
            }
            let after = rest[at + attr.len()..].trim_start();
            let Some(after) = after.strip_prefix('=') else {
                continue;
            };
            let after = after.trim_start();
            let Some(quote) = after.chars().next().filter(|q| matches!(q, '"' | '\'')) else {
                continue;
            };
            let value = &after[1..];
            let end = value.find(quote)?;
            return Some((kind, &value[..end]));
        }
    }
    None
}
