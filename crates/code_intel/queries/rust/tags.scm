; Lectern tags query for Rust.
;
; Based on queries/tags.scm of tree-sitter-rust 0.24.2
; (commit e2bee853694a1d3e0f6ef308fe3674542fec95d7), MIT.
; Lectern's changes:
; - Structs, enums, traits and type aliases use their own kinds
;   (struct, enum, trait, type) instead of class and interface.
; - @doc captures the parameter list (Lectern counts parameters from it).
; - Methods are the functions of `impl` and `trait` bodies only (upstream
;   took any declaration list, so functions of inline modules too); adds
;   trait method signatures, constants, statics, struct fields and enum
;   variants (constants).
; - Adds calls through paths (`Type::new()`, `module::f()`) and path macros,
;   struct literals, and every type identifier as a type reference (last, so
;   that definitions win for their own names).
; - Identifier calls carry (#is-not? local).
; - The implementation reference names the trait only; the implemented type
;   of an `impl` block is a `reference.lectern.impl*` marker instead.
; - `reference.lectern.*` captures are markers read by the extractor, never
;   stored: `pub` and the type an `impl` block belongs to.

; ---- definitions -------------------------------------------------------

(struct_item
    name: (type_identifier) @name) @definition.struct

(enum_item
    name: (type_identifier) @name) @definition.enum

; Variants: constants, as visible as their enum.
(enum_variant
    name: (identifier) @name) @definition.constant

(union_item
    name: (type_identifier) @name) @definition.struct

(type_item
    name: (type_identifier) @name) @definition.type

; A trait's associated type (`type Item;`).
(associated_type
    name: (type_identifier) @name) @definition.type

[
    (impl_item
        body: (declaration_list
            [
                (function_item
                    name: (identifier) @name
                    parameters: (parameters) @doc)
                (function_signature_item
                    name: (identifier) @name
                    parameters: (parameters) @doc)
            ] @definition.method))
    (trait_item
        body: (declaration_list
            [
                (function_item
                    name: (identifier) @name
                    parameters: (parameters) @doc)
                (function_signature_item
                    name: (identifier) @name
                    parameters: (parameters) @doc)
            ] @definition.method))
]

(function_item
    name: (identifier) @name
    parameters: (parameters) @doc) @definition.function

(function_signature_item
    name: (identifier) @name
    parameters: (parameters) @doc) @definition.function

(trait_item
    name: (type_identifier) @name) @definition.trait

(mod_item
    name: (identifier) @name) @definition.module

(macro_definition
    name: (identifier) @name) @definition.macro

(const_item
    name: (identifier) @name) @definition.constant

(static_item
    name: (identifier) @name) @definition.constant

(field_declaration
    name: (field_identifier) @name) @definition.field

; ---- markers -----------------------------------------------------------

(visibility_modifier) @name @reference.lectern.pub

; An `impl` block and the name of the type it implements: through
; references, pointers, arrays, slices and tuples (the first element), two
; levels deep (`&[T]`); a generic or path type gives its own name
; (`Box<T>` is `Box`, `a::B` is `B`); a primitive type names itself.
(impl_item
    type: [
        (type_identifier) @name
        (primitive_type) @name
        (scoped_type_identifier name: (type_identifier) @name)
        (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])
        (reference_type
            type: [
                (type_identifier) @name
                (primitive_type) @name
                (scoped_type_identifier name: (type_identifier) @name)
                (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])
                (reference_type type: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (pointer_type type: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (array_type element: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (tuple_type . [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
            ])
        (pointer_type
            type: [
                (type_identifier) @name
                (primitive_type) @name
                (scoped_type_identifier name: (type_identifier) @name)
                (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])
                (reference_type type: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (pointer_type type: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (array_type element: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (tuple_type . [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
            ])
        (array_type
            element: [
                (type_identifier) @name
                (primitive_type) @name
                (scoped_type_identifier name: (type_identifier) @name)
                (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])
                (reference_type type: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (pointer_type type: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (array_type element: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (tuple_type . [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
            ])
        (tuple_type
            .
            [
                (type_identifier) @name
                (primitive_type) @name
                (scoped_type_identifier name: (type_identifier) @name)
                (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])
                (reference_type type: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (pointer_type type: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (array_type element: [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
                (tuple_type . [(type_identifier) @name (primitive_type) @name (scoped_type_identifier name: (type_identifier) @name) (generic_type type: [(type_identifier) @name (scoped_type_identifier name: (type_identifier) @name)])])
            ])
    ]) @reference.lectern.impl

; A trait `impl` block (`impl Trait for T`), whatever its type: the `for`
; token, so the trait's own name stays a reference.
(impl_item
    "for" @name) @reference.lectern.impl-trait

; ---- references --------------------------------------------------------

(
    (call_expression
        function: (identifier) @name) @reference.call
    (#is-not? local)
)

(call_expression
    function: (field_expression
        field: (field_identifier) @name)) @reference.call

(call_expression
    function: (scoped_identifier
        name: (identifier) @name)) @reference.call

(macro_invocation
    macro: (identifier) @name) @reference.call

(macro_invocation
    macro: (scoped_identifier
        name: (identifier) @name)) @reference.call

(impl_item
    trait: [
        (type_identifier) @name
        (generic_type
            type: (type_identifier) @name)
        (scoped_type_identifier
            name: (type_identifier) @name)
    ]) @reference.implementation

(struct_expression
    name: (type_identifier) @name) @reference.class

(type_identifier) @name @reference.type

; Catch-all, last so that definitions win for their own names: every
; identifier that locals does not resolve, and every field name. `ci_ref`
; must list each file that may use a name (`let v = PUBLIC_CONSTANT;`,
; `object.field`), since only unindexed files are searched byte by byte.
(
    (identifier) @name @reference.name
    (#is-not? local)
)

(field_identifier) @name @reference.member
