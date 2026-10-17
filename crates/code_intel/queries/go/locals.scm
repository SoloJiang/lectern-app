; Lectern locals query for Go.
;
; Written by Lectern: tree-sitter-go 0.25.0 ships no locals query. Same
; capture names as the other locals queries (tree-sitter-highlight's
; convention). Parameters, `:=` and `var` bindings, range variables.

; Scopes
;-------

[
  (function_declaration)
  (method_declaration)
  (func_literal)
  (block)
  (for_statement)
  (if_statement)
] @local.scope

; Definitions
;------------

(parameter_declaration
  name: (identifier) @local.definition)

(variadic_parameter_declaration
  name: (identifier) @local.definition)

(short_var_declaration
  left: (expression_list
    (identifier) @local.definition))

(var_spec
  name: (identifier) @local.definition)

(range_clause
  left: (expression_list
    (identifier) @local.definition))

; References
;------------

(identifier) @local.reference
