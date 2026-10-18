; Lectern tags query for Go.
;
; Based on queries/tags.scm of tree-sitter-go 0.25.0
; (commit 1547678a9da59885853f5f5cc8a99cc203fa2e2c), MIT.
; Lectern's changes:
; - @doc captures the parameter list (Lectern counts parameters from it);
;   comment captures and their #strip! / #set-adjacent! are removed.
; - Adds interface methods, type aliases, struct fields, and package-level
;   constants and variables; drops the patterns that had no definition or
;   reference capture (package clause, imports).
; - Identifier calls carry (#is-not? local).
; - `reference.lectern.owner` is a marker read by the extractor, never
;   stored: the receiver type of a method.

; ---- definitions -------------------------------------------------------

(function_declaration
  name: (identifier) @name
  parameters: (parameter_list) @doc) @definition.function

(method_declaration
  name: (field_identifier) @name
  parameters: (parameter_list) @doc) @definition.method

(method_elem
  name: (field_identifier) @name
  parameters: (parameter_list) @doc) @definition.method

(type_spec
  name: (type_identifier) @name) @definition.type

(type_alias
  name: (type_identifier) @name) @definition.type

; Every name of `const A, B = 1, 2`: the grammar's `name` field of a
; const_spec also spans the commas, and `name: (identifier)` in this nested
; pattern matches only the first name. An identifier child of a const_spec
; is always a name (types are type identifiers, values an expression list).
(source_file
  (const_declaration
    (const_spec
      (identifier) @name) @definition.constant))

(source_file
  (var_declaration
    [
      (var_spec
        name: (identifier) @name) @definition.variable
      (var_spec_list
        (var_spec
          name: (identifier) @name) @definition.variable)
    ]))

(field_declaration
  name: (field_identifier) @name) @definition.field

; ---- markers -----------------------------------------------------------

(method_declaration
  receiver: (parameter_list
    (parameter_declaration
      type: [
        (type_identifier) @name
        (pointer_type
          (type_identifier) @name)
        (generic_type
          type: (type_identifier) @name)
        (pointer_type
          (generic_type
            type: (type_identifier) @name))
      ]))) @reference.lectern.owner

; ---- references --------------------------------------------------------

(
  (call_expression
    function: [
      (identifier) @name
      (parenthesized_expression
        (identifier) @name)
    ]) @reference.call
  (#is-not? local)
)

(call_expression
  function: [
    (selector_expression
      field: (field_identifier) @name)
    (parenthesized_expression
      (selector_expression
        field: (field_identifier) @name))
  ]) @reference.call

(type_identifier) @name @reference.type

; Catch-all, last so that definitions win for their own names: every
; identifier that locals does not resolve, and every field or selector
; name. `ci_ref` must list each file that may use a name (`v := Precision`,
; `t.units`), since only unindexed files are searched byte by byte.
(
  (identifier) @name @reference.name
  (#is-not? local)
)

(field_identifier) @name @reference.member
