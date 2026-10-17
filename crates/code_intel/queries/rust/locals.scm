; Lectern locals query for Rust.
;
; Written by Lectern: tree-sitter-rust 0.24.2 ships no locals query. Same
; capture names as the other locals queries (tree-sitter-highlight's
; convention). Parameters, closure parameters, `let`, `for` and match-arm
; bindings that are plain identifiers.

; Scopes
;-------

[
  (function_item)
  (closure_expression)
  (block)
  (for_expression)
  (match_arm)
] @local.scope

; Definitions
;------------

(parameter
  pattern: (identifier) @local.definition)

(closure_parameters
  (identifier) @local.definition)

(let_declaration
  pattern: (identifier) @local.definition)

(for_expression
  pattern: (identifier) @local.definition)

(match_arm
  pattern: (match_pattern
    (identifier) @local.definition))

; References
;------------

(identifier) @local.reference
