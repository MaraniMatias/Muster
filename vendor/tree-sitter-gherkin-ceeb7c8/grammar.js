module.exports = grammar({
  name: 'gherkin',

  extras: $ => [/[ \t\r\n]/, $.comment],

  rules: {
    source_file: $ => seq(
      optional($.tag_list),
      $.feature
    ),

    comment: $ => token(prec(1, seq('#', /[^\n]*/))),

    tag_list: $ => repeat1($.tag),
    tag: $ => token(prec(2, /@\S+/)),

    feature: $ => seq(
      'Feature', ':', field('name', $.name_line),
      optional($.description),
      optional($.background),
      repeat(choice($.rule_header, $.scenario))
    ),

    // A Rule is a flat header: the scenarios that follow it are its
    // siblings in `feature`'s repeat, not its children. Nesting scenarios
    // inside `rule` made "does this tag start a new scenario in the rule,
    // or end the rule" ambiguous at the same lookahead as the outer
    // feature-level choice, which produced unstable GLR parses on real
    // files. Flattening removes the ambiguity outright.
    rule_header: $ => seq(
      optional($.tag_list),
      token(prec(2, 'Rule')), ':', field('name', $.name_line),
      optional($.description)
    ),

    background: $ => seq(
      token(prec(2, 'Background')), ':', optional(field('name', $.name_line)),
      optional($.description),
      repeat($.step)
    ),

    // optional(description) here reuses the same free-text-between-header-
    // and-content shape as feature/rule_header. No new ambiguity: step
    // keywords (Given/When/...) are prec(2), strictly above description's
    // prec(-1)/prec(-2) fallbacks, so the lexer always prefers "this line
    // starts a step" over "this line is more description" the moment a
    // step keyword appears — unlike the Rule-nesting bug, there's no
    // shared low-precedence prefix for two repeat-continuing alternatives
    // to fight over here.
    scenario: $ => seq(
      optional($.tag_list),
      choice(token(prec(2, 'Scenario')), token(prec(2, 'Scenario Outline'))),
      ':', field('name', $.name_line),
      optional($.description),
      repeat($.step),
      optional($.examples)
    ),

    // No leading tag_list here: allowing tags on Examples too would make
    // tag_list ambiguous between "belongs to this Examples" and "belongs
    // to the next scenario/rule". Untagged Examples covers v1's scope.
    examples: $ => seq(
      token(prec(2, 'Examples')), ':', optional(field('name', $.name_line)),
      $.data_table
    ),

    step: $ => seq(
      field('keyword', $.step_keyword),
      field('text', $.step_text),
      optional(choice($.data_table, $.doc_string))
    ),

    step_keyword: $ => choice(
      token(prec(2, 'Given')),
      token(prec(2, 'When')),
      token(prec(2, 'Then')),
      token(prec(2, 'And')),
      token(prec(2, 'But')),
      token(prec(2, '*')),
    ),

    step_text: $ => repeat1(choice($.quoted_string, $.parameter, $.text_fragment)),

    quoted_string: $ => token(prec(2, /"[^"\n]*"/)),

    parameter: $ => token(prec(2, /<[^<>\n]*>/)),

    text_fragment: $ => token(prec(-1, /[^"<\n]+/)),

    name_line: $ => token(prec(-1, /[^\n]*/)),

    description: $ => repeat1(choice($.narrative_line, $.description_line)),

    // "As a/I want/So that" is the conventional Gherkin user-story preamble.
    // Higher precedence than description_line so these specific lines win
    // when they match, even though both tokens would otherwise match the
    // same span (tree-sitter resolves same-position ambiguity by
    // precedence first, then length — same scheme used everywhere else in
    // this grammar).
    narrative_line: $ => token(prec(-1, seq(choice('As a', 'I want', 'So that'), /[^\n]*/))),

    description_line: $ => token(prec(-2, /[^\n@#|"][^\n]*/)),

    data_table: $ => repeat1($.table_row),

    table_row: $ => seq('|', repeat1(seq($.table_cell, '|'))),

    table_cell: $ => token(prec(-1, /[^|\n]*/)),

    doc_string: $ => token(prec(2, seq('"""', /[\s\S]*?/, '"""'))),
  }
});
