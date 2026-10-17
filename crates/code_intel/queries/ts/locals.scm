; Lectern locals query for TypeScript, added to queries/ecmascript/locals.scm
; by the `ts` and `tsx` languages.
;
; queries/locals.scm of tree-sitter-typescript 0.23.2
; (commit f975a621f4e7f532fe322e13c4f79495e0a7b2e7), MIT, unchanged.

(required_parameter (identifier) @local.definition)
(optional_parameter (identifier) @local.definition)
