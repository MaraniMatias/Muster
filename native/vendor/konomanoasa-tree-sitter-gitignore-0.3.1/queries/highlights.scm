[
  (comment_marker)
  (comment_text)
] @comment

[
  (glob_literal)
  (set_text)
  (range_character)
] @string.special.path

(escape) @string.escape

[
  (negation)
  (set_negation)
  (range_operator)
] @operator

[
  (wildcard)
  (recursive_wildcard)
  (single_character)
] @character.special

[
  (path_separator)
  ":"
] @punctuation.delimiter

[
  (set_open)
  (set_close)
  "["
  "]"
] @punctuation.bracket

(class_name) @type
