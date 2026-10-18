; Lectern tags query for TypeScript, added to queries/ecmascript/tags.scm by
; the `ts` and `tsx` languages, as upstream overlays the JavaScript query.
;
; Based on queries/tags.scm of tree-sitter-typescript 0.23.2
; (commit f975a621f4e7f532fe322e13c4f79495e0a7b2e7), MIT.
; Lectern's changes:
; - @doc captures the parameter list (Lectern counts parameters from it).
; - Adds namespaces, type aliases, enums and their members (constants),
;   fields and property signatures.
; - The `new` reference is in queries/ecmascript/tags.scm; adds the class an
;   `extends` clause names as a class reference.
; - Type references cover every type identifier, not only annotations, and
;   come last so that definitions win for their own names.

; ---- definitions -------------------------------------------------------

(function_signature
  name: (identifier) @name
  parameters: (formal_parameters) @doc) @definition.function

(method_signature
  name: [(property_identifier) (private_property_identifier)] @name
  parameters: (formal_parameters) @doc) @definition.method

(abstract_method_signature
  name: [(property_identifier) (private_property_identifier)] @name
  parameters: (formal_parameters) @doc) @definition.method

(abstract_class_declaration
  name: (type_identifier) @name) @definition.class

(module
  name: (identifier) @name) @definition.module

(internal_module
  name: (identifier) @name) @definition.module

(interface_declaration
  name: (type_identifier) @name) @definition.interface

(type_alias_declaration
  name: (type_identifier) @name) @definition.type

(enum_declaration
  name: (identifier) @name) @definition.enum

; Members: constants, as visible as their enum.
(enum_body
  name: (property_identifier) @name @definition.constant)

(enum_assignment
  name: (property_identifier) @name) @definition.constant

(public_field_definition
  name: [(property_identifier) (private_property_identifier)] @name) @definition.field

(property_signature
  name: (property_identifier) @name) @definition.field

; ---- references --------------------------------------------------------

(
  (extends_clause
    value: (identifier) @name) @reference.class
  (#is-not? local)
)

(extends_clause
  value: (member_expression
    property: (property_identifier) @name)) @reference.class

(implements_clause
  (type_identifier) @name) @reference.implementation

(type_identifier) @name @reference.type
