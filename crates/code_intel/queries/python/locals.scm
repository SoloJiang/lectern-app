; Lectern locals query for Python.
;
; Written by Lectern: tree-sitter-python 0.25.0 ships no locals query. Same
; capture names as the other locals queries (tree-sitter-highlight's
; convention). Parameters, assignments to a plain name, `for` targets,
; lambda parameters, and `def` / `class` names. Class bodies are not scopes:
; their names are not visible inside methods.

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

; A `def` or `class` name belongs to the scope around the declaration; the
; function's own scope starts inside it (Lectern resolves a definition that
; is the `name` of a scope node in the parent scope).
(function_definition
  name: (identifier) @local.definition)

(class_definition
  name: (identifier) @local.definition)

; References
;------------

(identifier) @local.reference
