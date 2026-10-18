; Lectern tags query for Python.
;
; Based on queries/tags.scm of tree-sitter-python 0.25.0
; (commit 293fdc02038ee2bf0e2e206711b69c90ac0d413f), MIT.
; Lectern's changes:
; - Functions directly in a class body (decorated or not) are methods.
; - @doc captures the parameter list (Lectern counts parameters from it).
; - Adds superclasses as class references, class attributes (including
;   `Enum` members) as fields, and `type` aliases
;   (docs/design/code_intel.md「定义形式」).
; - Module-level constants in any statement and chain, and instance
;   attributes (`self.name = …` in a method) through the
;   `reference.lectern.receiver` and `reference.lectern.attribute` markers.
; - Identifier calls carry (#is-not? local).

; ---- definitions -------------------------------------------------------

; Class attributes, including `Enum` members (`Color.RED`), also unpacked
; (`left, right = 1, 2`): first, so that the general assignment pattern
; below does not take them.
(class_definition
  body: (block
    (expression_statement
      (assignment
      left: [
        (identifier) @name
        (pattern_list
          [
            (identifier) @name
            (list_splat_pattern (identifier) @name)
            (tuple_pattern
              [
                (identifier) @name
                (list_splat_pattern (identifier) @name)
              ])
            (list_pattern
              [
                (identifier) @name
                (list_splat_pattern (identifier) @name)
              ])
          ])
        (tuple_pattern
          [
            (identifier) @name
            (list_splat_pattern (identifier) @name)
            (tuple_pattern
              [
                (identifier) @name
                (list_splat_pattern (identifier) @name)
              ])
            (list_pattern
              [
                (identifier) @name
                (list_splat_pattern (identifier) @name)
              ])
          ])
        (list_pattern
          [
            (identifier) @name
            (list_splat_pattern (identifier) @name)
            (tuple_pattern
              [
                (identifier) @name
                (list_splat_pattern (identifier) @name)
              ])
            (list_pattern
              [
                (identifier) @name
                (list_splat_pattern (identifier) @name)
              ])
          ])
      ]) @definition.field)))

(class_definition
  name: (identifier) @name) @definition.class

; Module-level names, also unpacked (`A, B = load()`, `first, *rest = xs`,
; `(p, (q, r)) = …`): every name the target binds, two levels deep, in any
; statement (`if`, `try`, `with`, `for` bodies) and in a chain
; (`A = B = None`, where the inner assignment is the outer one's right
; side). Only module level counts: `rules::finish` drops the ones inside a
; function or class scope.
      (assignment
        left: [
          (identifier) @name
          (pattern_list
            [
              (identifier) @name
              (list_splat_pattern (identifier) @name)
              (tuple_pattern
                [
                  (identifier) @name
                  (list_splat_pattern (identifier) @name)
                ])
              (list_pattern
                [
                  (identifier) @name
                  (list_splat_pattern (identifier) @name)
                ])
            ])
          (tuple_pattern
            [
              (identifier) @name
              (list_splat_pattern (identifier) @name)
              (tuple_pattern
                [
                  (identifier) @name
                  (list_splat_pattern (identifier) @name)
                ])
              (list_pattern
                [
                  (identifier) @name
                  (list_splat_pattern (identifier) @name)
                ])
            ])
          (list_pattern
            [
              (identifier) @name
              (list_splat_pattern (identifier) @name)
              (tuple_pattern
                [
                  (identifier) @name
                  (list_splat_pattern (identifier) @name)
                ])
              (list_pattern
                [
                  (identifier) @name
                  (list_splat_pattern (identifier) @name)
                ])
            ])
        ]) @definition.constant

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

; `type Alias = …` and `type Alias[T] = …` (Python 3.12).
(type_alias_statement
  left: (type
    [
      (identifier) @name
      (generic_type
        (identifier) @name)
    ])) @definition.type

; ---- markers -----------------------------------------------------------

; A function's first parameter: a method's `self`.
(function_definition
  parameters: (parameters
    .
    (identifier) @name)) @reference.lectern.receiver

; An assignment to an attribute (`self.name = name`, also in a chain): a
; field of the class when the object is the receiver of the method around
; it (`rules::finish`), a member reference otherwise.
(assignment
  left: (attribute
    object: (identifier)
    attribute: (identifier) @name)) @reference.lectern.attribute

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
