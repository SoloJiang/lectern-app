; Lectern tags query for JavaScript only, added to queries/ecmascript/tags.scm
; by the `js` language (the TypeScript grammars name these nodes
; differently; see queries/ts/tags.scm).
;
; Written by Lectern (not in tree-sitter-javascript 0.25.0): class fields,
; and the class an `extends` clause names as a class reference.

(field_definition
  property: [(property_identifier) (private_property_identifier)] @name) @definition.field

(
  (class_heritage
    (identifier) @name) @reference.class
  (#is-not? local)
)

(class_heritage
  (member_expression
    property: (property_identifier) @name)) @reference.class
