; Lectern highlight query for Markdown, block structure only (ADR 0041:
; no inline grammar, no injected languages).
;
; Based on tree-sitter-markdown/queries/highlights.scm of tree-sitter-md
; 0.5.3 (commit f969cd3ae3f9fbd4e43205431d0ae286014c05b5), which upstream
; took from nvim-treesitter.
; Copyright (c) the tree-sitter-md authors (MDeiml; the crate ships no
; licence file). MIT; the permission notice is in ../NOTICE.
; Precedence: the last matching pattern colours a node.
; Lectern's changes:
; - Drops `(code_fence_content) @none`: without injected languages a fenced
;   code block's content keeps the block's @text.literal.

(atx_heading
  (inline) @text.title)

(setext_heading
  (paragraph) @text.title)

[
  (atx_h1_marker)
  (atx_h2_marker)
  (atx_h3_marker)
  (atx_h4_marker)
  (atx_h5_marker)
  (atx_h6_marker)
  (setext_h1_underline)
  (setext_h2_underline)
] @punctuation.special

[
  (link_title)
  (indented_code_block)
  (fenced_code_block)
] @text.literal

(fenced_code_block_delimiter) @punctuation.delimiter

(link_destination) @text.uri

(link_label) @text.reference

[
  (list_marker_plus)
  (list_marker_minus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
  (thematic_break)
] @punctuation.special

[
  (block_continuation)
  (block_quote_marker)
] @punctuation.special

(backslash_escape) @string.escape
