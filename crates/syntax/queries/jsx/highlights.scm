; Lectern highlight query for JSX, appended to the ECMAScript query for
; JavaScript and TSX (src/lang.rs).
;
; Based on queries/highlights-jsx.scm of tree-sitter-javascript 0.25.0
; (commit 44c892e0be055ac465d5eeddae6d3e194424e7de).
; Copyright (c) 2014 Max Brunsfeld. MIT; the permission notice is in
; ../NOTICE.
; Precedence: the last matching pattern colours a node.
; Lectern's changes: none.

(jsx_opening_element (identifier) @tag (#match? @tag "^[a-z][^.]*$"))
(jsx_closing_element (identifier) @tag (#match? @tag "^[a-z][^.]*$"))
(jsx_self_closing_element (identifier) @tag (#match? @tag "^[a-z][^.]*$"))

(jsx_attribute (property_identifier) @attribute)
(jsx_opening_element (["<" ">"]) @punctuation.bracket)
(jsx_closing_element (["</" ">"]) @punctuation.bracket)
(jsx_self_closing_element (["<" "/>"]) @punctuation.bracket)
