; Lectern locals query for Go.
;
; Written by Lectern: tree-sitter-go 0.25.0 ships no locals query. Same
; capture names as the other locals queries (tree-sitter-highlight's
; convention), with the binding rules of docs/design/code_intel.md
; 「绑定规则」 as properties that tree-sitter-tags ignores:
; - `local.kind "function"`: functions, methods and function literals;
; - `local.visible`: "scope" binds the whole scope (parameters, results,
;   receivers, and package-level names, which do not depend on order);
;   "end:KIND" binds from the end of the declaring KIND node, so
;   `x := x + 1` reads the earlier `x`; "body:KIND" binds from the start of
;   KIND's body (range variables);
; - `local.scope-inherits false` on function scopes is for tree-sitter-tags
;   only: it filters references against the enclosing function's own locals.

; Scopes
;-------

[
  (block)
  (for_statement)
  (if_statement)
  (expression_switch_statement)
  (type_switch_statement)
  (expression_case)
  (type_case)
  (communication_case)
] @local.scope

(
  [
    (function_declaration)
    (method_declaration)
    (func_literal)
  ] @local.scope
  (#set! local.kind "function")
  (#set! local.scope-inherits false)
)

; Definitions
;------------

(parameter_declaration
  name: (identifier) @local.definition
  (#set! local.visible "scope"))

(variadic_parameter_declaration
  name: (identifier) @local.definition
  (#set! local.visible "scope"))

(short_var_declaration
  left: (expression_list
    (identifier) @local.definition)
  (#set! local.visible "end:short_var_declaration"))

(statement_list
  (var_declaration
    [
      (var_spec
        name: (identifier) @local.definition)
      (var_spec_list
        (var_spec
          name: (identifier) @local.definition))
    ])
  (#set! local.visible "end:var_spec"))

(statement_list
  (const_declaration
    (const_spec
      name: (identifier) @local.definition))
  (#set! local.visible "end:const_spec"))

(source_file
  (var_declaration
    [
      (var_spec
        name: (identifier) @local.definition)
      (var_spec_list
        (var_spec
          name: (identifier) @local.definition))
    ])
  (#set! local.visible "scope"))

(source_file
  (const_declaration
    (const_spec
      name: (identifier) @local.definition))
  (#set! local.visible "scope"))

(range_clause
  left: (expression_list
    (identifier) @local.definition)
  (#set! local.visible "body:for_statement"))

(receive_statement
  left: (expression_list
    (identifier) @local.definition)
  (#set! local.visible "end:receive_statement"))

(type_switch_statement
  alias: (expression_list
    (identifier) @local.definition)
  (#set! local.visible "scope"))

; Type parameters, in the function or type that declares them; Go has one
; namespace, and the uses are type names. Hidden from tree-sitter-tags,
; which compares names only. (A method receiver's type parameters,
; `(l *List[E])`, are not modelled.)
(type_parameter_declaration
  name: (identifier) @definition.lectern.local
  (#set! local.namespace "both")
  (#set! local.visible "owner:type_parameter_list"))

; References
;------------

(identifier) @local.reference
