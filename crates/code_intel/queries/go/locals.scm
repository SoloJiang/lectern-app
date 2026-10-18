; Lectern locals query for Go.
;
; Written by Lectern: tree-sitter-go 0.25.0 ships no locals query. Same
; capture names as the other locals queries (tree-sitter-highlight's
; convention), with the binding rules of docs/design/code_intel.md
; 「绑定规则」 as properties that tree-sitter-tags ignores:
; - `local.kind "function"`: functions, methods and function literals, and
;   interface methods and function types, whose parameters bind nothing
;   outside them;
; - `local.visible`: "scope" binds the whole scope (parameters, results,
;   receivers, and package-level names, which do not depend on order);
;   "end:KIND" binds from the end of the declaring KIND node, so
;   `x := x + 1` reads the earlier `x`; "body:KIND" binds from the start of
;   KIND's body (range variables);
; - `local.redeclare "true"`: a `:=` name already declared in the same
;   scope (or the function's parameters and results, for its body) is a use
;   of the existing variable;
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
  (default_case)
  (communication_case)
] @local.scope

(
  [
    (function_declaration)
    (method_declaration)
    (func_literal)
    (method_elem)
    (function_type)
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

; `:=` may redeclare: a name already declared earlier in the same block
; (or among the parameters and results when the block is the function
; body) is a use of that variable, not a new one (`local.redeclare`; Go
; spec, Short variable declarations). tree-sitter-tags compares names only,
; and such a name is already a local of the scope, so it filters the same.
(short_var_declaration
  left: (expression_list
    (identifier) @local.definition)
  (#set! local.visible "end:short_var_declaration")
  (#set! local.redeclare "true"))

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
      (identifier) @local.definition))
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
      (identifier) @local.definition))
  (#set! local.visible "scope"))

; Only the `:=` forms declare: `for i = range xs` and `case v = <-ch`
; assign to existing variables. A range clause starts a new for scope, so
; its `:=` names are always new. Hidden from tree-sitter-tags: it handles
; matches in the order they complete, and these complete at the `:=` after
; the name, so the catch-all reference on the name would come first; not
; filtering these names only adds reference names.
(range_clause
  left: (expression_list
    (identifier) @definition.lectern.local)
  ":="
  (#set! local.visible "body:for_statement"))

(receive_statement
  left: (expression_list
    (identifier) @definition.lectern.local)
  ":="
  (#set! local.visible "end:receive_statement")
  (#set! local.redeclare "true"))

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

; Only these captures resolve to locals (`ParsedFile::site_at`): values
; and, for type parameters, type names.
(identifier) @local.reference

(type_identifier) @local.reference
