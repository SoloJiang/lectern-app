; Lectern highlight query for JavaScript's parameters, appended to the
; ECMAScript query for JavaScript only: TypeScript wraps parameters in
; `required_parameter` and has its own patterns (src/lang.rs).
;
; Based on queries/highlights-params.scm of tree-sitter-javascript 0.25.0
; (commit 44c892e0be055ac465d5eeddae6d3e194424e7de).
; Copyright (c) 2014 Max Brunsfeld. MIT; the permission notice is in
; ../NOTICE.
; Precedence: the last matching pattern colours a node.
; Lectern's changes: none.

(formal_parameters
  [
    (identifier) @variable.parameter
    (array_pattern
      (identifier) @variable.parameter)
    (object_pattern
      [
        (pair_pattern value: (identifier) @variable.parameter)
        (shorthand_property_identifier_pattern) @variable.parameter
      ])
  ]
)
