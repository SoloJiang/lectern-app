; Lectern tags query shared by JavaScript, TypeScript and TSX: only nodes
; all three grammars have. `js` adds queries/js/tags.scm and
; queries/jsx/tags.scm, `ts` adds queries/ts/tags.scm, `tsx` adds the
; TypeScript and JSX files.
;
; Based on queries/tags.scm of tree-sitter-javascript 0.25.0
; (commit 44c892e0be055ac465d5eeddae6d3e194424e7de), MIT.
; Lectern's changes:
; - @doc captures the parameter list (Lectern counts parameters from it);
;   comment captures and their #strip! / #select-adjacent! are removed.
; - Adds class expressions bound to a name, top-level constants and
;   variables (also destructured), private methods.
; - Identifier references carry (#is-not? local).
; - `new` takes the identifier or the member name, not the whole expression.
; - `reference.lectern.*` captures are markers read by the extractor, never
;   stored: export statements and names, and ES module syntax.

; ---- definitions -------------------------------------------------------

(method_definition
  name: [(property_identifier) (private_property_identifier)] @name
  parameters: (formal_parameters) @doc
  (#not-eq? @name "constructor")) @definition.method

[
  (class
    name: (_) @name)
  (class_declaration
    name: (_) @name)
] @definition.class

[
  (function_expression
    name: (identifier) @name
    parameters: (formal_parameters) @doc)
  (function_declaration
    name: (identifier) @name
    parameters: (formal_parameters) @doc)
  (generator_function
    name: (identifier) @name
    parameters: (formal_parameters) @doc)
  (generator_function_declaration
    name: (identifier) @name
    parameters: (formal_parameters) @doc)
] @definition.function

(variable_declarator
  name: (identifier) @name
  value: [
    (arrow_function
      parameters: (formal_parameters) @doc)
    (arrow_function
      parameter: (identifier) @doc)
    (function_expression
      parameters: (formal_parameters) @doc)
    (generator_function
      parameters: (formal_parameters) @doc)
  ]) @definition.function

(variable_declarator
  name: (identifier) @name
  value: (class)) @definition.class

(assignment_expression
  left: [
    (identifier) @name
    (member_expression
      property: (property_identifier) @name)
  ]
  right: [
    (arrow_function
      parameters: (formal_parameters) @doc)
    (arrow_function
      parameter: (identifier) @doc)
    (function_expression
      parameters: (formal_parameters) @doc)
  ]) @definition.function

(pair
  key: (property_identifier) @name
  value: [
    (arrow_function
      parameters: (formal_parameters) @doc)
    (arrow_function
      parameter: (identifier) @doc)
    (function_expression
      parameters: (formal_parameters) @doc)
  ]) @definition.function

(export_statement
  value: (assignment_expression
    left: (identifier) @name
    right: [
      (number)
      (string)
      (identifier)
      (undefined)
      (null)
      (new_expression)
      (binary_expression)
      (call_expression)
    ])) @definition.constant

; Top-level `const`, `let` and `var`, also destructured
; (`const { a, b: renamed, ...rest } = obj`, `let [x, , y = 1] = arr`):
; every name the pattern binds, two levels deep. `const` comes first, so a
; `const` declarator is never also a variable: the second pattern's
; wildcard is a `lexical_declaration` or a `variable_declaration`.
(program
  [
    (lexical_declaration
      kind: "const"
      (variable_declarator
        name: [
          (identifier) @name
          (object_pattern
              [
                (shorthand_property_identifier_pattern) @name
                (pair_pattern
                  value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                (object_assignment_pattern
                  left: (shorthand_property_identifier_pattern) @name)
                (rest_pattern (identifier) @name)
                (pair_pattern value: (object_pattern
                      [
                        (shorthand_property_identifier_pattern) @name
                        (pair_pattern
                          value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                        (object_assignment_pattern
                          left: (shorthand_property_identifier_pattern) @name)
                        (rest_pattern (identifier) @name)
                      ]))
                (pair_pattern value: (array_pattern
                      [
                        (identifier) @name
                        (assignment_pattern left: (identifier) @name)
                        (rest_pattern (identifier) @name)
                      ]))
              ])
          (array_pattern
              [
                (identifier) @name
                (assignment_pattern left: (identifier) @name)
                (rest_pattern (identifier) @name)
                (object_pattern
                    [
                      (shorthand_property_identifier_pattern) @name
                      (pair_pattern
                        value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                      (object_assignment_pattern
                        left: (shorthand_property_identifier_pattern) @name)
                      (rest_pattern (identifier) @name)
                    ])
                (array_pattern
                    [
                      (identifier) @name
                      (assignment_pattern left: (identifier) @name)
                      (rest_pattern (identifier) @name)
                    ])
              ])
        ]) @definition.constant)
    (export_statement
      declaration: (lexical_declaration
        kind: "const"
        (variable_declarator
          name: [
            (identifier) @name
            (object_pattern
                [
                  (shorthand_property_identifier_pattern) @name
                  (pair_pattern
                    value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                  (object_assignment_pattern
                    left: (shorthand_property_identifier_pattern) @name)
                  (rest_pattern (identifier) @name)
                  (pair_pattern value: (object_pattern
                        [
                          (shorthand_property_identifier_pattern) @name
                          (pair_pattern
                            value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                          (object_assignment_pattern
                            left: (shorthand_property_identifier_pattern) @name)
                          (rest_pattern (identifier) @name)
                        ]))
                  (pair_pattern value: (array_pattern
                        [
                          (identifier) @name
                          (assignment_pattern left: (identifier) @name)
                          (rest_pattern (identifier) @name)
                        ]))
                ])
            (array_pattern
                [
                  (identifier) @name
                  (assignment_pattern left: (identifier) @name)
                  (rest_pattern (identifier) @name)
                  (object_pattern
                      [
                        (shorthand_property_identifier_pattern) @name
                        (pair_pattern
                          value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                        (object_assignment_pattern
                          left: (shorthand_property_identifier_pattern) @name)
                        (rest_pattern (identifier) @name)
                      ])
                  (array_pattern
                      [
                        (identifier) @name
                        (assignment_pattern left: (identifier) @name)
                        (rest_pattern (identifier) @name)
                      ])
                ])
          ]) @definition.constant))
  ])

(program
  [
    (_
      (variable_declarator
        name: [
          (identifier) @name
          (object_pattern
              [
                (shorthand_property_identifier_pattern) @name
                (pair_pattern
                  value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                (object_assignment_pattern
                  left: (shorthand_property_identifier_pattern) @name)
                (rest_pattern (identifier) @name)
                (pair_pattern value: (object_pattern
                      [
                        (shorthand_property_identifier_pattern) @name
                        (pair_pattern
                          value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                        (object_assignment_pattern
                          left: (shorthand_property_identifier_pattern) @name)
                        (rest_pattern (identifier) @name)
                      ]))
                (pair_pattern value: (array_pattern
                      [
                        (identifier) @name
                        (assignment_pattern left: (identifier) @name)
                        (rest_pattern (identifier) @name)
                      ]))
              ])
          (array_pattern
              [
                (identifier) @name
                (assignment_pattern left: (identifier) @name)
                (rest_pattern (identifier) @name)
                (object_pattern
                    [
                      (shorthand_property_identifier_pattern) @name
                      (pair_pattern
                        value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                      (object_assignment_pattern
                        left: (shorthand_property_identifier_pattern) @name)
                      (rest_pattern (identifier) @name)
                    ])
                (array_pattern
                    [
                      (identifier) @name
                      (assignment_pattern left: (identifier) @name)
                      (rest_pattern (identifier) @name)
                    ])
              ])
        ]) @definition.variable)
    (export_statement
      declaration: (_
        (variable_declarator
          name: [
            (identifier) @name
            (object_pattern
                [
                  (shorthand_property_identifier_pattern) @name
                  (pair_pattern
                    value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                  (object_assignment_pattern
                    left: (shorthand_property_identifier_pattern) @name)
                  (rest_pattern (identifier) @name)
                  (pair_pattern value: (object_pattern
                        [
                          (shorthand_property_identifier_pattern) @name
                          (pair_pattern
                            value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                          (object_assignment_pattern
                            left: (shorthand_property_identifier_pattern) @name)
                          (rest_pattern (identifier) @name)
                        ]))
                  (pair_pattern value: (array_pattern
                        [
                          (identifier) @name
                          (assignment_pattern left: (identifier) @name)
                          (rest_pattern (identifier) @name)
                        ]))
                ])
            (array_pattern
                [
                  (identifier) @name
                  (assignment_pattern left: (identifier) @name)
                  (rest_pattern (identifier) @name)
                  (object_pattern
                      [
                        (shorthand_property_identifier_pattern) @name
                        (pair_pattern
                          value: [(identifier) @name (assignment_pattern left: (identifier) @name)])
                        (object_assignment_pattern
                          left: (shorthand_property_identifier_pattern) @name)
                        (rest_pattern (identifier) @name)
                      ])
                  (array_pattern
                      [
                        (identifier) @name
                        (assignment_pattern left: (identifier) @name)
                        (rest_pattern (identifier) @name)
                      ])
                ])
          ]) @definition.variable))
  ])

; ---- references --------------------------------------------------------

(
  (call_expression
    function: (identifier) @name) @reference.call
  (#not-match? @name "^(require)$")
  (#is-not? local)
)

(call_expression
  function: (member_expression
    property: [(property_identifier) (private_property_identifier)] @name)
  arguments: (_) @reference.call)

(
  (new_expression
    constructor: (identifier) @name) @reference.class
  (#is-not? local)
)

(new_expression
  constructor: (member_expression
    property: (property_identifier) @name)) @reference.class

; ---- markers -----------------------------------------------------------

(export_statement
  "export" @name) @reference.lectern.export

(export_specifier
  name: (identifier) @name) @reference.lectern.export-name

(export_statement
  value: (identifier) @name) @reference.lectern.export-name

(import_statement
  "import" @name) @reference.lectern.module
