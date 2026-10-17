; Lectern tags query for Java.
;
; Based on queries/tags.scm of tree-sitter-java 0.23.5
; (commit 94703d5a6bed02b98e438d7cad1136c01a60ba2c), MIT.
; Lectern's changes:
; - @doc captures the parameter list (Lectern counts parameters from it).
; - Adds records, enums, annotation types, fields, interface constants and
;   enum constants.
; - Adds every type identifier as a type reference (last, so that
;   definitions win for their own names).
; - `reference.lectern.private` is a marker read by the extractor, never
;   stored: the `private` modifier.

; ---- definitions -------------------------------------------------------

(class_declaration
  name: (identifier) @name) @definition.class

(record_declaration
  name: (identifier) @name) @definition.class

(enum_declaration
  name: (identifier) @name) @definition.enum

(interface_declaration
  name: (identifier) @name) @definition.interface

(annotation_type_declaration
  name: (identifier) @name) @definition.interface

(method_declaration
  name: (identifier) @name
  parameters: (formal_parameters) @doc) @definition.method

(field_declaration
  declarator: (variable_declarator
    name: (identifier) @name)) @definition.field

(constant_declaration
  declarator: (variable_declarator
    name: (identifier) @name)) @definition.constant

(enum_constant
  name: (identifier) @name) @definition.constant

; ---- markers -----------------------------------------------------------

(modifiers
  "private" @name) @reference.lectern.private

; ---- references --------------------------------------------------------

(method_invocation
  name: (identifier) @name
  arguments: (argument_list) @reference.call)

(type_list
  (type_identifier) @name) @reference.implementation

(object_creation_expression
  type: (type_identifier) @name) @reference.class

(superclass
  (type_identifier) @name) @reference.class

(type_identifier) @name @reference.type
