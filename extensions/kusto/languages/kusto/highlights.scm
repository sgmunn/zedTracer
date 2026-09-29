(comment) @comment
(string) @string
(number) @number
(timespan) @number
(bool) @boolean
(null) @constant.builtin
(type) @type

(let_keyword) @keyword
(operator) @keyword
(join_operator) @keyword
(mv_apply_operator) @keyword
(range_operator) @keyword
(sub_operator) @keyword
(to_operator) @keyword
(compound_keywords) @keyword
(sort_keyword) @keyword
(join_types) @constant

(source (identifier) @type)
(let_statement (identifier) @variable)
(function_call (identifier) @function)
(typed_parameter (identifier) @variable.parameter)
(assignment (identifier) @variable)
(property_identifier (identifier) @property)
(property_index (identifier) @property)

(pipe) @operator
(binary_operator) @operator

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

"," @punctuation.delimiter
