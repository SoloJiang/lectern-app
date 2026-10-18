; Lectern locals query for Python.
;
; Written by Lectern: tree-sitter-python 0.25.0 ships no locals query. Same
; capture names as the other locals queries (tree-sitter-highlight's
; convention), with the binding rules of docs/design/code_intel.md
; 「绑定规则」 as properties that tree-sitter-tags ignores:
; - `local.kind`: "function" (functions, lambdas), "comprehension" (a
;   function scope that `:=` skips) or "class" (a class body: its names are
;   visible in the body only, not in methods);
; - `local.visible "scope"`: a Python name binds its whole scope, also before
;   the assignment;
; - `local.scope "function"`: an assignment expression binds in the
;   enclosing function, also inside a comprehension (PEP 572);
; - `local.nonlocal`: names a `global` / `nonlocal` statement takes out of
;   the function's scope;
; - `local.scope-inherits false` on function scopes is for tree-sitter-tags
;   only: it filters references against the enclosing function's own locals
;   (so a class body never hides a module name from a method).
; A captured target (`a, (b, *c) = …`, a parameter with a default) is
; expanded by Lectern into the names it binds; attribute and subscript
; targets bind nothing. Imports are not locals: they are resolved through
; the index.

; Scopes
;-------

(
  [
    (function_definition)
    (lambda)
  ] @local.scope
  (#set! local.kind "function")
  (#set! local.scope-inherits false)
)

; A comprehension is a function scope that an assignment expression skips.
(
  [
    (list_comprehension)
    (set_comprehension)
    (dictionary_comprehension)
    (generator_expression)
  ] @local.scope
  (#set! local.kind "comprehension")
  (#set! local.scope-inherits false)
)

(
  (class_definition) @local.scope
  (#set! local.kind "class")
)

; Definitions
;------------

(parameters
  (_) @local.definition
  (#set! local.visible "scope"))

(lambda_parameters
  (_) @local.definition
  (#set! local.visible "scope"))

(assignment
  left: (_) @local.definition
  (#set! local.visible "scope"))

(augmented_assignment
  left: (identifier) @local.definition
  (#set! local.visible "scope"))

(for_statement
  left: (_) @local.definition
  (#set! local.visible "scope"))

(for_in_clause
  left: (_) @local.definition
  (#set! local.visible "scope"))

(named_expression
  name: (identifier) @local.definition
  (#set! local.scope "function")
  (#set! local.visible "scope"))

; `with … as name`, `except … as name`.
(as_pattern
  alias: (_) @local.definition
  (#set! local.visible "scope"))

; A `def` or `class` name belongs to the scope around the declaration
; (Lectern resolves a definition that is the `name` of a scope node in the
; parent scope).
(function_definition
  name: (identifier) @local.definition
  (#set! local.visible "scope"))

(class_definition
  name: (identifier) @local.definition
  (#set! local.visible "scope"))

; References
;------------

(global_statement
  (identifier) @local.reference
  (#set! local.nonlocal "true"))

(nonlocal_statement
  (identifier) @local.reference
  (#set! local.nonlocal "true"))

(identifier) @local.reference
