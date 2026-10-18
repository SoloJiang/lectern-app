; Lectern locals query shared by JavaScript, TypeScript and TSX (`ts` and
; `tsx` add queries/ts/locals.scm).
; Based on queries/locals.scm of tree-sitter-javascript 0.25.0
; (commit 44c892e0be055ac465d5eeddae6d3e194424e7de), MIT.
; Lectern's changes: catch parameters, the bound name of a single-parameter
; arrow function, and function and class declaration names are local
; definitions.

; Scopes
;-------

[
  (statement_block)
  (function_expression)
  (arrow_function)
  (function_declaration)
  (method_definition)
] @local.scope

; Definitions
;------------

(pattern/identifier) @local.definition

(variable_declarator
  name: (identifier) @local.definition)

(arrow_function
  parameter: (identifier) @local.definition)

(catch_clause
  parameter: (identifier) @local.definition)

; Function and class declaration names belong to the scope around the
; declaration; a function's own scope starts inside it (Lectern resolves a
; definition that is the `name` of a scope node in the parent scope).
(function_declaration
  name: (identifier) @local.definition)

(generator_function_declaration
  name: (identifier) @local.definition)

(class_declaration
  name: (_) @local.definition)

; References
;------------

(identifier) @local.reference
