; Lectern highlight query for TypeScript, appended to the ECMAScript query
; (and the JSX query, for TSX) as upstream intends (src/lang.rs).
;
; Based on queries/highlights.scm of tree-sitter-typescript 0.23.2
; (commit f975a621f4e7f532fe322e13c4f79495e0a7b2e7).
; Copyright (c) the tree-sitter-typescript authors (Max Brunsfeld, Amaan
; Qureshi; the crate ships no licence file). MIT; the permission notice is
; in ../NOTICE.
; Precedence: the last matching pattern colours a node.
; Lectern's changes:
; - The capitalised-identifier type pattern leaves all-caps names to the
;   ECMAScript query's @constant.
; - Adds the brackets of `type_parameters` next to `type_arguments`.

; Types

(type_identifier) @type
(predefined_type) @type.builtin

((identifier) @type
 (#match? @type "^[A-Z]")
 (#not-match? @type "^[A-Z_][A-Z\\d_]+$"))

(type_arguments
  "<" @punctuation.bracket
  ">" @punctuation.bracket)
(type_parameters
  "<" @punctuation.bracket
  ">" @punctuation.bracket)

; Variables

(required_parameter (identifier) @variable.parameter)
(optional_parameter (identifier) @variable.parameter)

; Keywords

[ "abstract"
  "declare"
  "enum"
  "export"
  "implements"
  "interface"
  "keyof"
  "namespace"
  "private"
  "protected"
  "public"
  "type"
  "readonly"
  "override"
  "satisfies"
] @keyword
