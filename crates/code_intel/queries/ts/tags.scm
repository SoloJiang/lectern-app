; Lectern tags query for TypeScript, added to queries/ecmascript/tags.scm by
; the `ts` and `tsx` languages, as upstream overlays the JavaScript query.
;
; Based on queries/tags.scm of tree-sitter-typescript 0.23.2
; (commit f975a621f4e7f532fe322e13c4f79495e0a7b2e7), MIT.
; Lectern's changes:
; - @doc captures the parameter list (Lectern counts parameters from it).
; - Adds namespaces (`namespace A.B` by its last segment), type aliases,
;   enums and their members (constants), fields, property signatures,
;   parameter properties and top-level `declare` variables
;   (docs/design/code_intel.md「定义形式」).
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

; Parameter properties: a constructor parameter with an accessibility
; modifier, `override` or `readonly` is also a field of the class.
[
  (required_parameter
    (accessibility_modifier)
    pattern: (identifier) @name)
  (required_parameter
    (override_modifier)
    pattern: (identifier) @name)
  (required_parameter
    "readonly"
    pattern: (identifier) @name)
  (optional_parameter
    (accessibility_modifier)
    pattern: (identifier) @name)
  (optional_parameter
    (override_modifier)
    pattern: (identifier) @name)
  (optional_parameter
    "readonly"
    pattern: (identifier) @name)
] @definition.field

; `namespace A.B`: the last segment.
[
  (module
    name: (nested_identifier
      property: (property_identifier) @name))
  (internal_module
    name: (nested_identifier
      property: (property_identifier) @name))
] @definition.module

; Top-level `declare const` / `let` / `var`, also exported.
(program
  [
    (ambient_declaration
      (lexical_declaration
        kind: "const"
        (variable_declarator
          name: (identifier) @name) @definition.constant))
    (export_statement
      declaration: (ambient_declaration
        (lexical_declaration
          kind: "const"
          (variable_declarator
            name: (identifier) @name) @definition.constant)))
  ])

; ---- markers (TS) ------------------------------------------------------

; `export = X;` (the grammar has no field for X) exports X.
(program
  (export_statement
    "="
    (identifier) @name) @reference.lectern.export-name)

; `declare global { … }`: its declarations are global, as visible as
; exported ones.
(program
  (ambient_declaration
    "global" @name) @reference.lectern.global)

; Exported `const`, `let` and `var` in a namespace or module body
; (`namespace Config { export const version = 1 }`): members, used as
; `Config.version`. Unexported ones are locals of the body; destructuring
; there is not indexed.
[
  (internal_module
    body: (statement_block
      (export_statement
        declaration: (lexical_declaration
          kind: "const"
          (variable_declarator
            name: (identifier) @name) @definition.constant))))
  (module
    body: (statement_block
      (export_statement
        declaration: (lexical_declaration
          kind: "const"
          (variable_declarator
            name: (identifier) @name) @definition.constant))))
]

[
  (internal_module
    body: (statement_block
      (export_statement
        declaration: (_
          (variable_declarator
            name: (identifier) @name) @definition.variable))))
  (module
    body: (statement_block
      (export_statement
        declaration: (_
          (variable_declarator
            name: (identifier) @name) @definition.variable))))
]

(program
  [
    (ambient_declaration
      [
        (lexical_declaration
          kind: "let"
          (variable_declarator
            name: (identifier) @name) @definition.variable)
        (variable_declaration
          (variable_declarator
            name: (identifier) @name) @definition.variable)
      ])
    (export_statement
      declaration: (ambient_declaration
        [
          (lexical_declaration
            kind: "let"
            (variable_declarator
              name: (identifier) @name) @definition.variable)
          (variable_declaration
            (variable_declarator
              name: (identifier) @name) @definition.variable)
        ]))
  ])

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
