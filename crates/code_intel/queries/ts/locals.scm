; Lectern locals query for TypeScript, added to queries/ecmascript/locals.scm
; by the `ts` and `tsx` languages.
;
; Based on queries/locals.scm of tree-sitter-typescript 0.23.2
; (commit f975a621f4e7f532fe322e13c4f79495e0a7b2e7), MIT.
; Lectern's changes: the parameter's pattern (an identifier or a
; destructuring pattern) instead of a bare identifier child, and the binding
; rule of queries/ecmascript/locals.scm (the whole function).

(required_parameter
  pattern: (_) @local.definition
  (#set! local.visible "scope"))

(optional_parameter
  pattern: (_) @local.definition
  (#set! local.visible "scope"))
