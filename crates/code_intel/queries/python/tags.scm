; Lectern tags query for Python.
;
; Based on queries/tags.scm of tree-sitter-python 0.25.0
; (commit 293fdc02038ee2bf0e2e206711b69c90ac0d413f), MIT.
; Lectern's changes:
; - Functions directly in a class body (decorated or not) are methods.
; - @doc captures the parameter list (Lectern counts parameters from it).
; - Adds superclasses as class references, and class attributes (including
;   `Enum` members) as fields.
; - Identifier calls carry (#is-not? local).

; ---- definitions -------------------------------------------------------

(module
  (expression_statement
    (assignment
      left: (identifier) @name) @definition.constant))

(class_definition
  name: (identifier) @name) @definition.class

; Class attributes, including `Enum` members (`Color.RED`).
(class_definition
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @name) @definition.field)))

(class_definition
  body: (block
    (function_definition
      name: (identifier) @name
      parameters: (parameters) @doc) @definition.method))

(class_definition
  body: (block
    (decorated_definition
      definition: (function_definition
        name: (identifier) @name
        parameters: (parameters) @doc) @definition.method)))

(function_definition
  name: (identifier) @name
  parameters: (parameters) @doc) @definition.function

; ---- references --------------------------------------------------------

(
  (call
    function: (identifier) @name) @reference.call
  (#is-not? local)
)

(call
  function: (attribute
    attribute: (identifier) @name)) @reference.call

(class_definition
  superclasses: (argument_list
    (identifier) @name) @reference.class)

; Catch-all, last so that definitions win for their own names. Attribute
; names never refer to a local, so they carry no locals predicate; every
; other identifier that locals does not resolve is a reference name too.
; `ci_ref` must list each file that may use a name (`rate = TAX_RATE`,
; `models.User`), since only unindexed files are searched byte by byte.
(attribute
  attribute: (identifier) @name) @reference.member

(
  (identifier) @name @reference.name
  (#is-not? local)
)
