; Lectern locals query for Rust.
;
; Written by Lectern: tree-sitter-rust 0.24.2 ships no locals query. Same
; capture names as the other locals queries (tree-sitter-highlight's
; convention). Parameters, closure parameters, `let`, `for` and match-arm
; bindings that are plain identifiers, and functions nested in a block.

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

; A function nested in a block is visible in that block; its own scope
; starts inside it (Lectern resolves a definition that is the `name` of a
; scope node in the parent scope). Module-level and `impl` functions are not
; locals: they are only reached through paths or method syntax.
(block
  (function_item
    name: (identifier) @local.definition))

; References
;------------

(identifier) @local.reference
