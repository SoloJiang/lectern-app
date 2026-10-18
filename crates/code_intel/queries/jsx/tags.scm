; Lectern tags query for JSX, added to queries/ecmascript/tags.scm by the `js`
; and `tsx` languages.
;
; Written by Lectern (not in tree-sitter-javascript 0.25.0): capitalised
; JSX element names are component references; lowercase ones (`<label>`,
; `<my-tag>`) are intrinsic elements, which name nothing: ignored, so the
; catch-all reference does not take them (or drop them as locals).

(
  [
    (jsx_opening_element
      name: (identifier) @ignore)
    (jsx_closing_element
      name: (identifier) @ignore)
    (jsx_self_closing_element
      name: (identifier) @ignore)
  ]
  (#match? @ignore "^[a-z]")
)

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
