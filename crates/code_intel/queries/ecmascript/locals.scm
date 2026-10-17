; Lectern locals query shared by JavaScript, TypeScript and TSX (`ts` and
; `tsx` add queries/ts/locals.scm).
; Based on queries/locals.scm of tree-sitter-javascript 0.25.0
; (commit 44c892e0be055ac465d5eeddae6d3e194424e7de), MIT.
; Lectern's changes: catch parameters and the bound name of a single-parameter
; arrow function are local definitions.

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

; References
;------------

(identifier) @local.reference
