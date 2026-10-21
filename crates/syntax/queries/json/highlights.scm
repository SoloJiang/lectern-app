; Lectern highlight query for JSON.
;
; Based on queries/highlights.scm of tree-sitter-json 0.24.8
; (commit ee35a6ebefcef0c5c416c0d1ccec7370cfca5a24).
; Copyright (c) the tree-sitter-json authors (Max Brunsfeld, Amaan Qureshi;
; the crate ships no licence file). MIT; the permission notice is in
; ../NOTICE.
; Precedence: the first matching pattern colours a node.
; Lectern's changes: none.

(pair
  key: (_) @string.special.key)

(string) @string

(number) @number

[
  (null)
  (true)
  (false)
] @constant.builtin

(escape_sequence) @escape

(comment) @comment
