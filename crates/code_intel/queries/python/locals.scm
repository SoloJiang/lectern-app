; Lectern locals query for Python.
;
; Written by Lectern: tree-sitter-python 0.25.0 ships no locals query. Same
; capture names as the other locals queries (tree-sitter-highlight's
; convention). Parameters, assignments to a plain name, `for` targets and
; lambda parameters. Class bodies are not scopes: their names are not
; visible inside methods.

; Scopes
;-------

[
  (function_definition)
  (lambda)
] @local.scope

; Definitions
;------------

(parameters
  (identifier) @local.definition)

(default_parameter
  name: (identifier) @local.definition)

(typed_parameter
  (identifier) @local.definition)

(typed_default_parameter
  name: (identifier) @local.definition)

(list_splat_pattern
  (identifier) @local.definition)

(dictionary_splat_pattern
  (identifier) @local.definition)

(lambda_parameters
  (identifier) @local.definition)

(assignment
  left: (identifier) @local.definition)

(for_statement
  left: (identifier) @local.definition)

; References
;------------

(identifier) @local.reference
