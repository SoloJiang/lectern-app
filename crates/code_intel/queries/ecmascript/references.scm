; Lectern catch-all references for JavaScript, TypeScript and TSX, appended
; after every other tags query of these languages: tree-sitter-tags keeps
; the earliest pattern for a name node, so these must come after the
; definition patterns.
;
; Written by Lectern. Every identifier that locals does not resolve, and
; every member name, is a reference name: `ci_ref` must list each file that
; may use a name (`const v = LIMIT`, `obj.field`), since only unindexed
; files are searched byte by byte.

(
  (identifier) @name @reference.name
  (#is-not? local)
)

(
  (shorthand_property_identifier) @name @reference.name
  (#is-not? local)
)

(property_identifier) @name @reference.member

(private_property_identifier) @name @reference.member
