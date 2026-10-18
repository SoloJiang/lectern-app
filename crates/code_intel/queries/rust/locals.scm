; Lectern locals query for Rust.
;
; Written by Lectern: tree-sitter-rust 0.24.2 ships no locals query. Same
; capture names as the other locals queries (tree-sitter-highlight's
; convention), with the binding rules of docs/design/code_intel.md
; 「绑定规则」 as properties that tree-sitter-tags ignores:
; - `local.kind "function"`: functions and closures, and signatures and
;   function types, whose parameters bind nothing outside them;
; - `local.visible`: "scope" binds the whole scope (parameters, match arms,
;   items in a block); "end:KIND" binds from the end of the declaring KIND
;   node, so `let x = x + 1;` reads the earlier `x`; "body:KIND" binds from
;   the start of KIND's body (`for x in x.iter() { … }`);
; - `local.until "FIELD:KIND|KIND"`: out of scope from the start of that
;   field of the nearest node of those kinds, or from its end when it has no
;   such field (the `else` of an `if let`, the end of a `while let`);
; - `@definition.lectern.local`: a local definition Lectern reads and
;   tree-sitter-tags ignores (it acts only on `@local.scope` and
;   `@local.definition` here);
; - `local.namespace`: "type" or "both" for names that type positions use
;   (default: values); "owner:KIND" in `local.visible` binds only within the
;   parent of the nearest KIND (a type parameter in its item);
; - `local.scope-inherits false` on function scopes is for tree-sitter-tags
;   only: it filters references against the enclosing function's own locals.
; Every name-introducing form is in docs/design/code_intel.md「定义形式」.
; A captured pattern (`(a, b)`, `Some(v)`, `Point { x, y: z }`) is expanded
; by Lectern into the names it binds, leaving out the constructor and path
; (`Some`, `Point`) and capitalised names inside a pattern (unit variants
; and constants such as `None`).

; Scopes
;-------

[
  (block)
  (for_expression)
  (match_arm)
] @local.scope

(
  [
    (function_item)
    (closure_expression)
    (function_signature_item)
    (function_type)
  ] @local.scope
  (#set! local.kind "function")
  (#set! local.scope-inherits false)
)

; Definitions
;------------

(parameter
  pattern: (_) @local.definition
  (#set! local.visible "scope"))

(closure_parameters
  (_) @local.definition
  (#set! local.visible "scope"))

(let_declaration
  pattern: (_) @local.definition
  (#set! local.visible "end:let_declaration"))

; `if let` / `while let`: the rest of the condition and the body, not the
; `else` branch. `if` and `while` are not scopes (tree-sitter-tags checks
; every scope for each reference it filters, and most bind nothing): the
; binding is bounded in the enclosing scope instead, and hidden from
; tree-sitter-tags, which never filters the name.
(let_condition
  pattern: (_) @definition.lectern.local
  (#set! local.visible "end:let_condition")
  (#set! local.until "alternative:if_expression|while_expression"))

(for_expression
  pattern: (_) @local.definition
  (#set! local.visible "body:for_expression"))

(match_arm
  pattern: (match_pattern) @local.definition
  (#set! local.visible "scope"))

; Items in a block are visible in the whole block; a function's own scope
; starts inside it (Lectern resolves a definition that is the `name` of a
; scope node in the parent scope). Module-level and `impl` items are not
; locals: they are reached through paths or method syntax.
(block
  (function_item
    name: (identifier) @local.definition)
  (#set! local.visible "scope"))

(block
  [
    (const_item
      name: (identifier) @local.definition)
    (static_item
      name: (identifier) @local.definition)
  ]
  (#set! local.visible "scope"))

; Type items in a block: types, and for structs also values (`S(1)`).
; Hidden from tree-sitter-tags, which compares names only.
(block
  (struct_item
    name: (type_identifier) @definition.lectern.local)
  (#set! local.namespace "both")
  (#set! local.visible "scope"))

(block
  [
    (enum_item
      name: (type_identifier) @definition.lectern.local)
    (union_item
      name: (type_identifier) @definition.lectern.local)
    (trait_item
      name: (type_identifier) @definition.lectern.local)
    (type_item
      name: (type_identifier) @definition.lectern.local)
  ]
  (#set! local.namespace "type")
  (#set! local.visible "scope"))

; Type and const parameters, in the item that declares them.
(type_parameters
  (type_parameter
    name: (type_identifier) @definition.lectern.local)
  (#set! local.namespace "type")
  (#set! local.visible "owner:type_parameters"))

(type_parameters
  (const_parameter
    name: (identifier) @definition.lectern.local)
  (#set! local.visible "owner:type_parameters"))

; References
;------------

(identifier) @local.reference
