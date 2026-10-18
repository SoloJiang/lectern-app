; Lectern locals query shared by JavaScript, TypeScript and TSX (`ts` and
; `tsx` add queries/ts/locals.scm).
;
; Based on queries/locals.scm of tree-sitter-javascript 0.25.0
; (commit 44c892e0be055ac465d5eeddae6d3e194424e7de), MIT.
; Lectern's changes: declarations are captured by form instead of as every
; `pattern/identifier` (which also matched destructuring assignments), with
; the binding rules of docs/design/code_intel.md「绑定规则」 as properties
; that tree-sitter-tags ignores:
; - `local.kind`: a scope's kind ("function", "dynamic" for a `with` body,
;   "namespace" in ts/locals.scm; blocks by default);
; - `local.scope`: "function" puts a definition in the nearest function
;   scope (`var`) instead of the innermost scope;
; - `local.visible`: "scope" binds the whole scope, including references
;   before the declaration (hoisting, and let/const/class, which closures may
;   use before the declaration runs);
; - `local.scope-inherits false` on function scopes is for tree-sitter-tags
;   only: it filters references against the enclosing function's own locals,
;   so fewer names are dropped from the index, never more.
; A captured pattern (`{a, b: [c]}`) is expanded by Lectern into the names
; it binds.

; Scopes
;-------

(statement_block) @local.scope

(for_statement) @local.scope

(for_in_statement) @local.scope

; The catch parameter belongs to the clause; a declaration in one `case`
; is in scope in the whole switch body.
(catch_clause) @local.scope

(switch_body) @local.scope

; A name that a `with` body does not declare may be a property of the
; object: a declaration found outside the body is only possible.
(
  (with_statement
    body: (_) @local.scope)
  (#set! local.kind "dynamic")
)

(
  [
    (function_expression)
    (arrow_function)
    (function_declaration)
    (generator_function)
    (generator_function_declaration)
    (method_definition)
  ] @local.scope
  (#set! local.kind "function")
  (#set! local.scope-inherits false)
)

; Definitions
;------------

; let / const: the enclosing block.
(lexical_declaration
  (variable_declarator
    name: (_) @local.definition)
  (#set! local.visible "scope"))

; var: the enclosing function, hoisted.
(variable_declaration
  (variable_declarator
    name: (_) @local.definition)
  (#set! local.scope "function")
  (#set! local.visible "scope"))

(for_in_statement
  kind: "var"
  left: (_) @local.definition
  (#set! local.scope "function")
  (#set! local.visible "scope"))

(for_in_statement
  kind: ["let" "const"]
  left: (_) @local.definition
  (#set! local.visible "scope"))

; Parameters: the whole function.
(formal_parameters
  (_) @local.definition
  (#set! local.visible "scope"))

(arrow_function
  parameter: (identifier) @local.definition
  (#set! local.visible "scope"))

(catch_clause
  parameter: (_) @local.definition
  (#set! local.visible "scope"))

; Function declarations are hoisted; classes bind the block like let. The
; name belongs to the scope around the declaration (Lectern resolves a
; definition that is the `name` of a scope node in the parent scope).
(function_declaration
  name: (identifier) @local.definition
  (#set! local.visible "scope"))

(generator_function_declaration
  name: (identifier) @local.definition
  (#set! local.visible "scope"))

(class_declaration
  name: (_) @local.definition
  (#set! local.namespace "both")
  (#set! local.visible "scope"))

; References
;------------

(identifier) @local.reference

; `{ a }` in an object literal uses the variable `a`. Only these captures
; resolve to locals (`ParsedFile::site_at`).
(shorthand_property_identifier) @local.reference
