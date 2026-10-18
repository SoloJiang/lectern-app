; Lectern locals query for Java.
;
; Written by Lectern: tree-sitter-java 0.23.5 ships no locals query. Same
; capture names as the other locals queries (tree-sitter-highlight's
; convention), with the binding rules of docs/design/code_intel.md
; 「绑定规则」 as properties that tree-sitter-tags ignores:
; - `local.kind "function"`: methods, constructors and lambdas;
; - `local.visible`: "scope" binds the whole scope (parameters); "end:KIND"
;   binds from the end of the declaring KIND node (a local variable is in
;   scope after its declarator, so an earlier use names a field);
;   "body:KIND" binds from the start of KIND's body (for-each variables);
;   without it, from the name on (local classes);
; - `local.certain "false"`: pattern variables (`instanceof`, record and
;   type patterns) have flow-dependent scopes: they offer a same-file
;   candidate, never a direct jump;
; - `local.namespace`: "type" or "both" for names that type positions use
;   (default: values); "owner:KIND" in `local.visible` binds only within the
;   parent of the nearest KIND (a type parameter in its class or method);
; - `local.scope-inherits false` on function scopes is for tree-sitter-tags
;   only: it filters references against the enclosing function's own locals.
; Fields and members, record components included, are not locals: they are
; definitions in the index. Every name-introducing form is in
; docs/design/code_intel.md「定义形式」.

; Scopes
;-------

[
  (block)
  (for_statement)
  (enhanced_for_statement)
  (catch_clause)
  (try_with_resources_statement)
  (switch_block_statement_group)
] @local.scope

(
  [
    (method_declaration)
    (constructor_declaration)
    (lambda_expression)
  ] @local.scope
  (#set! local.kind "function")
  (#set! local.scope-inherits false)
)

; Definitions
;------------

; Parameters of methods, constructors and lambdas. A record header's
; components are not locals: they are the record's fields (tags.scm).
(
  [
    (method_declaration
      parameters: (formal_parameters
        [
          (formal_parameter
            name: (identifier) @local.definition)
          (spread_parameter
            (variable_declarator
              name: (identifier) @local.definition))
        ]))
    (constructor_declaration
      parameters: (formal_parameters
        [
          (formal_parameter
            name: (identifier) @local.definition)
          (spread_parameter
            (variable_declarator
              name: (identifier) @local.definition))
        ]))
    (lambda_expression
      parameters: (formal_parameters
        [
          (formal_parameter
            name: (identifier) @local.definition)
          (spread_parameter
            (variable_declarator
              name: (identifier) @local.definition))
        ]))
  ]
  (#set! local.visible "scope"))

(catch_formal_parameter
  name: (identifier) @local.definition
  (#set! local.visible "scope"))

(lambda_expression
  parameters: (identifier) @local.definition
  (#set! local.visible "scope"))

(inferred_parameters
  (identifier) @local.definition
  (#set! local.visible "scope"))

(local_variable_declaration
  declarator: (variable_declarator
    name: (identifier) @local.definition)
  (#set! local.visible "end:variable_declarator"))

(enhanced_for_statement
  name: (identifier) @local.definition
  (#set! local.visible "body:enhanced_for_statement"))

(resource
  name: (identifier) @local.definition
  (#set! local.visible "end:resource"))

; Local classes, records, enums and interfaces, from their name on, as
; values (`Local.of()`) and types (`new Local()`).
(block
  [
    (class_declaration
      name: (identifier) @local.definition)
    (record_declaration
      name: (identifier) @local.definition)
    (enum_declaration
      name: (identifier) @local.definition)
    (interface_declaration
      name: (identifier) @local.definition)
  ]
  (#set! local.namespace "both"))

; Type parameters, in the class or method that declares them (hidden from
; tree-sitter-tags, which compares names only).
(type_parameter
  (type_identifier) @definition.lectern.local
  (#set! local.namespace "type")
  (#set! local.visible "owner:type_parameters"))

(instanceof_expression
  name: (identifier) @local.definition
  (#set! local.certain "false"))

(type_pattern
  (identifier) @local.definition
  (#set! local.certain "false"))

(record_pattern_component
  (identifier) @local.definition
  (#set! local.certain "false"))

; References
;------------

(identifier) @local.reference
