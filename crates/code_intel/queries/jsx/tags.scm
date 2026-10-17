; Lectern tags query for JSX, added to queries/ecmascript/tags.scm by the `js`
; and `tsx` languages.
;
; Written by Lectern (not in tree-sitter-javascript 0.25.0): capitalised
; JSX element names are component references.

(
  (jsx_opening_element
    name: (identifier) @name) @reference.class
  (#match? @name "^[A-Z]")
  (#is-not? local)
)

(
  (jsx_self_closing_element
    name: (identifier) @name) @reference.class
  (#match? @name "^[A-Z]")
  (#is-not? local)
)
