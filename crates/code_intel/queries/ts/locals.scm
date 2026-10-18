; Lectern locals query for TypeScript, added to queries/ecmascript/locals.scm
; by the `ts` and `tsx` languages.
;
; Based on queries/locals.scm of tree-sitter-typescript 0.23.2
; (commit f975a621f4e7f532fe322e13c4f79495e0a7b2e7), MIT.
; Lectern's changes: the parameter's pattern (an identifier or a
; destructuring pattern) instead of a bare identifier child, and the binding
; rule of queries/ecmascript/locals.scm (the whole function); signatures and
; function types are function scopes, so that their parameters bind nothing
; outside them; namespace and module bodies are "namespace" scopes, whose
; declarations are members (docs/design/code_intel.md「作用域形式」).

(
  [
    (function_signature)
    (method_signature)
    (abstract_method_signature)
    (call_signature)
    (construct_signature)
    (function_type)
    (constructor_type)
  ] @local.scope
  (#set! local.kind "function")
  (#set! local.scope-inherits false)
)

(
  [
    (internal_module
      body: (statement_block) @local.scope)
    (module
      body: (statement_block) @local.scope)
    (ambient_declaration
      (statement_block) @local.scope)
  ]
  (#set! local.kind "namespace")
)

(required_parameter
  pattern: (_) @local.definition
  (#set! local.visible "scope"))

(optional_parameter
  pattern: (_) @local.definition
  (#set! local.visible "scope"))

; Types, hidden from tree-sitter-tags (it compares names only): type
; parameters in the declaration that owns them, mapped type keys in their
; mapped type, and interfaces, type aliases, enums and abstract classes in
; their block.
(type_parameter
  name: (type_identifier) @definition.lectern.local
  (#set! local.namespace "type")
  (#set! local.visible "owner:type_parameters"))

(mapped_type_clause
  name: (type_identifier) @definition.lectern.local
  (#set! local.namespace "type")
  (#set! local.visible "owner:mapped_type_clause"))

([
  (interface_declaration
    name: (type_identifier) @definition.lectern.local)
  (type_alias_declaration
    name: (type_identifier) @definition.lectern.local)
 ]
  (#set! local.namespace "type")
  (#set! local.visible "scope"))

([
  (enum_declaration
    name: (identifier) @definition.lectern.local)
  (abstract_class_declaration
    name: (type_identifier) @definition.lectern.local)
 ]
  (#set! local.namespace "both")
  (#set! local.visible "scope"))
