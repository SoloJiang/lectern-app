; Lectern tags query for Java.
;
; Based on queries/tags.scm of tree-sitter-java 0.23.5
; (commit 94703d5a6bed02b98e438d7cad1136c01a60ba2c), MIT.
; Lectern's changes:
; - @doc captures the parameter list (Lectern counts parameters from it).
; - Adds records and their components, enums, annotation types and their
;   elements, fields, interface constants and enum constants
;   (docs/design/code_intel.md「定义形式」).
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

; A record's components are its fields (with accessors of the same name).
(record_declaration
  parameters: (formal_parameters
    [
      (formal_parameter
        name: (identifier) @name)
      (spread_parameter
        (variable_declarator
          name: (identifier) @name))
    ] @definition.field))

(annotation_type_element_declaration
  name: (identifier) @name) @definition.method

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

; Catch-all, last so that definitions win for their own names. Field names
; never refer to a local, so they carry no locals predicate; every other
; identifier that locals does not resolve is a reference name too. `ci_ref`
; must list each file that may use a name (`int v = MAX_ITEMS;`,
; `inventory.counts`), since only unindexed files are searched byte by byte.
(field_access
  field: (identifier) @name) @reference.member

(
  (identifier) @name @reference.name
  (#is-not? local)
)
