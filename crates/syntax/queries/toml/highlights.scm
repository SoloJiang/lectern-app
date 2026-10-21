; Lectern highlight query for TOML.
;
; Based on queries/highlights.scm of tree-sitter-toml-ng 0.7.0
; (commit 64b56832c2cffe41758f28e05c756a3a98d16f41).
; Copyright (c) the tree-sitter-toml authors (Ika, Amaan Qureshi; the crate
; ships no licence file). MIT; the permission notice is in ../NOTICE.
; Precedence: the last matching pattern colours a node.
; Lectern's changes:
; - `(pair (bare_key) @property)` captures the key, not the whole pair, so
;   the value's punctuation does not take the key's colour.

; Properties
;-----------

(bare_key) @type

(quoted_key) @string

(pair
  (bare_key) @property)

(pair
  (dotted_key
    (bare_key) @property))

; Literals
;---------

(boolean) @boolean

(comment) @comment

(string) @string

[
  (integer)
  (float)
] @number

[
  (offset_date_time)
  (local_date_time)
  (local_date)
  (local_time)
] @string.special

; Punctuation
;------------

[
  "."
  ","
] @punctuation.delimiter

"=" @operator

[
  "["
  "]"
  "[["
  "]]"
  "{"
  "}"
] @punctuation.bracket
