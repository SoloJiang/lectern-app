; Lectern locals query for Java.
;
; Written by Lectern: tree-sitter-java 0.23.5 ships no locals query. Same
; capture names as the other locals queries (tree-sitter-highlight's
; convention). Parameters, local variables, catch and for-each variables,
; lambda parameters, local classes and records.

; Scopes
;-------

[
  (method_declaration)
  (constructor_declaration)
  (lambda_expression)
  (block)
  (enhanced_for_statement)
  (catch_clause)
] @local.scope

; Definitions
;------------

(formal_parameter
  name: (identifier) @local.definition)

(spread_parameter
  (variable_declarator
    name: (identifier) @local.definition))

(catch_formal_parameter
  name: (identifier) @local.definition)

(local_variable_declaration
  declarator: (variable_declarator
    name: (identifier) @local.definition))

(enhanced_for_statement
  name: (identifier) @local.definition)

(lambda_expression
  parameters: (identifier) @local.definition)

(inferred_parameters
  (identifier) @local.definition)

; Local classes and records, declared in a block.
(block
  [
    (class_declaration
      name: (identifier) @local.definition)
    (record_declaration
      name: (identifier) @local.definition)
  ])

; References
;------------

(identifier) @local.reference
