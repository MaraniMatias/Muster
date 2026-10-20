#include "tree_sitter/alloc.h"
#include "tree_sitter/parser.h"
#include <string.h>

enum Token {
  BLANK_START,
  COMMENT_START,
  PATTERN_START,
  END_OF_FILE,
  GLOB_START,
  SET_ITEM_START,
  COMMENT_MARKER,
  COMMENT_TEXT,
  NEGATION,
  LITERAL,
  WILDCARD,
  RECURSIVE_WILDCARD,
  SINGLE_CHARACTER,
  PATH_SEPARATOR,
  ESCAPE,
  ESCAPE_PREFIX,
  SET_OPEN,
  SET_NEGATION,
  SET_TEXT,
  SET_CLOSE,
  RANGE_START,
  RANGE_CHARACTER,
  RANGE_OPERATOR,
  COMPOUND_OPEN,
  COMPOUND_CLOSE,
  CLASS_OPEN,
  CLASS_NAME,
  CLASS_CLOSE,
  TRAILING_SPACE,
  TRAILING_CARRIAGE_RETURN,
  IGNORED_START,
  IGNORED_TEXT,
  MISSING_SET_CLOSE,
  INCOMPLETE_SET_CLOSE,
  INVALID_ENCODING,
  ENDED_ESCAPE,
  INCOMPLETE_ESCAPE,
  ERROR_SENTINEL
};

#define NONE UINT32_MAX

// Positions are line-relative character offsets.
typedef struct {
  uint32_t position, content_end, line_end, ignored_start;
  uint32_t next, next_end;
  uint32_t set_end, class_start, class_end;
  uint32_t lower_end, range_end;
  uint8_t kind, component_start, after_separator, line_eof;
} Scanner;

typedef char scanner_fits_buffer
  [sizeof(Scanner) <= TREE_SITTER_SERIALIZATION_BUFFER_SIZE ? 1 : -1];

typedef struct {
  TSLexer *lexer;
  uint32_t position;
} Cursor;

typedef struct {
  uint32_t kind, end;
} Item;

static bool literal_boundary(int32_t c) {
  return c == -1 || c == '\\' || c == '*' || c == '?' || c == '/' || c == '[';
}

static bool emit(TSLexer *lexer, const bool *valid, enum Token token) {
  if (!valid[token]) return false;
  lexer->result_symbol = token;
  return true;
}

static enum Token
absent(const Scanner *s, enum Token missing, enum Token incomplete) {
  return s->line_eof && s->ignored_start == NONE ? incomplete : missing;
}

static uint32_t content_limit(const Scanner *s) {
  return s->ignored_start < s->line_end ? s->ignored_start : s->line_end;
}

static void advance(Scanner *s, TSLexer *lexer) {
  lexer->advance(lexer, false);
  s->position++;
}

// Include the next decoded character so edits invalidate the run.
static bool invalid_run(Scanner *s, TSLexer *lexer, const bool *valid) {
  do advance(s, lexer);
  while (!lexer->eof(lexer) && lexer->lookahead == -1);
  lexer->mark_end(lexer);
  if (!lexer->eof(lexer)) lexer->advance(lexer, false);
  return emit(lexer, valid, INVALID_ENCODING);
}

static bool consume(
  Scanner *s,
  TSLexer *lexer,
  const bool *valid,
  enum Token token,
  uint32_t end
) {
  if (token == INVALID_ENCODING) return invalid_run(s, lexer, valid);
  while (s->position < end) advance(s, lexer);
  lexer->mark_end(lexer);
  return emit(lexer, valid, token);
}

static bool text_run(
  Scanner *s,
  TSLexer *lexer,
  const bool *valid,
  enum Token token,
  uint32_t end
) {
  if (lexer->lookahead == -1) return invalid_run(s, lexer, valid);
  while (s->position < end && lexer->lookahead != -1) advance(s, lexer);
  lexer->mark_end(lexer);
  return emit(lexer, valid, token);
}

static void step(Cursor *c) {
  c->lexer->advance(c->lexer, false);
  c->position++;
}

static bool peek(const Cursor *c, uint32_t limit, int32_t ch) {
  return c->position < limit && c->lexer->lookahead == ch;
}

// The cursor has passed the backslash.
static enum Token escape_kind(const Scanner *s, Cursor *c, uint32_t limit) {
  if (c->position >= limit) return absent(s, ENDED_ESCAPE, INCOMPLETE_ESCAPE);
  const enum Token kind = c->lexer->lookahead == -1 ? ESCAPE_PREFIX : ESCAPE;
  step(c);
  return kind;
}

static bool start_line(Scanner *s, TSLexer *lexer, const bool *valid) {
  if (lexer->eof(lexer)) return false;
  memset(s, 0, sizeof(*s));
  s->ignored_start = NONE;
  s->class_start = NONE;
  s->component_start = true;
  s->kind = lexer->lookahead == '#' ? COMMENT_START : PATTERN_START;
  bool escaped = false;
  int32_t last = 0;
  uint32_t before_last = 0;
  lexer->mark_end(lexer);
  while (!lexer->eof(lexer) && lexer->lookahead != '\n') {
    last = lexer->lookahead;
    before_last = s->content_end;
    if (last == 0 && s->ignored_start == NONE) s->ignored_start = s->position;
    if (s->ignored_start == NONE && (last != ' ' || escaped))
      s->content_end = s->position + 1;
    escaped = !escaped && last == '\\';
    advance(s, lexer);
  }
  s->line_end = s->position;
  s->line_eof = lexer->eof(lexer);
  if (last == '\r') {
    s->line_end--;
    s->content_end = before_last;
  }
  if (s->kind == COMMENT_START) s->content_end = content_limit(s);
  else if (s->content_end == 0) s->kind = BLANK_START;
  s->position = 0;
  return emit(lexer, valid, (enum Token)s->kind);
}

static bool
skip_escaped(Cursor *c, uint32_t limit, int32_t *ch, uint32_t *candidate) {
  if (c->position >= limit) return false;
  *ch = c->lexer->lookahead;
  if (*ch == ']') *candidate = NONE;
  step(c);
  return true;
}

static bool next_class(
  Cursor *c,
  uint32_t limit,
  bool lower,
  uint32_t *start,
  uint32_t *end
) {
  uint32_t candidate = NONE;
  int32_t previous = 0;
  while (c->position < limit) {
    const int32_t here = c->lexer->lookahead;
    if (here == ']') {
      if (candidate == NONE || previous != ':' || c->position == candidate + 2)
        return false;
      step(c);
      *start = candidate;
      *end = c->position;
      return true;
    }
    step(c);
    previous = here;
    if (here == '\\') {
      lower = skip_escaped(c, limit, &previous, &candidate) && previous != -1;
      continue;
    }
    if (
      here == '-' && lower && c->position < limit && c->lexer->lookahead != ']'
    ) {
      previous = c->lexer->lookahead;
      step(c);
      if (previous == '\\') skip_escaped(c, limit, &previous, &candidate);
      lower = false;
      continue;
    }
    lower = here != -1;
    if (candidate == NONE && here == '[' && peek(c, limit, ':'))
      candidate = c->position - 1;
  }
  return false;
}

static void find_set(Scanner *s, Cursor *c) {
  s->set_end = s->content_end;
  s->class_start = NONE;
  if (peek(c, s->content_end, '!') || peek(c, s->content_end, '^')) step(c);
  bool lower = peek(c, s->content_end, ']');
  if (lower) step(c);
  uint32_t class_start, class_end;
  while (next_class(c, s->content_end, lower, &class_start, &class_end)) {
    lower = false;
    if (s->class_start == NONE) {
      s->class_start = class_start;
      s->class_end = class_end;
    }
  }
  if (c->position < s->content_end) {
    s->set_end = c->position;
    step(c);
  }
}

static Item item(const Scanner *s, Cursor *c) {
  Item result = {SET_TEXT, 0};
  const uint32_t start = c->position;
  const int32_t first = c->lexer->lookahead;
  step(c);
  if (first == -1) result.kind = INVALID_ENCODING;
  else if (first == '\\') result.kind = escape_kind(s, c, s->set_end);
  else if (first == '[' && start == s->class_start) {
    result.kind = CLASS_OPEN;
    while (c->position < s->class_end) step(c);
  }
  result.end = c->position;
  return result;
}

static bool endpoint(Item a) {
  return a.kind == SET_TEXT || a.kind == ESCAPE;
}

// Advance past whole characters so edits inside them invalidate lookahead.
static bool prepare_glob(Scanner *s, TSLexer *lexer, const bool *valid) {
  Cursor c = {lexer, s->position};
  const uint32_t start = s->position;
  s->next_end = start + 1;
  const int32_t first = lexer->lookahead;
  bool separator = first == '/';
  step(&c);
  if (first == -1) s->next = INVALID_ENCODING;
  else if (first == '?') s->next = SINGLE_CHARACTER;
  else if (first == '/') s->next = PATH_SEPARATOR;
  else if (first == '\\') {
    const int32_t escaped = lexer->lookahead;
    s->next = escape_kind(s, &c, s->content_end);
    s->next_end = c.position;
    separator = s->next == ESCAPE && escaped == '/';
  } else if (first == '*') {
    while (peek(&c, s->content_end, '*')) step(&c);
    const bool pair = c.position - start == 2 && s->component_start;
    const bool tail = c.position == s->content_end && s->after_separator;
    s->next_end = c.position;
    if (peek(&c, s->content_end, '\\')) step(&c);
    const bool slash = peek(&c, s->content_end, '/');
    s->next = pair && (slash || tail) ? RECURSIVE_WILDCARD : WILDCARD;
  } else if (first == '[') {
    s->next = SET_OPEN;
    find_set(s, &c);
  } else {
    while (c.position < s->content_end && !literal_boundary(lexer->lookahead))
      step(&c);
    s->position = c.position;
    lexer->mark_end(lexer);
    s->component_start = false;
    return emit(lexer, valid, LITERAL);
  }
  s->component_start = separator;
  if (separator) s->after_separator = true;
  return emit(lexer, valid, GLOB_START);
}

static bool prepare_set(Scanner *s, TSLexer *lexer, const bool *valid) {
  Cursor c = {lexer, s->position};
  const uint32_t start = s->position;
  s->next = SET_TEXT;
  s->next_end = start;
  Item current = item(s, &c);
  for (;;) {
    if (endpoint(current) && peek(&c, s->set_end - 1, '-')) {
      const uint32_t operator_end = c.position + 1;
      step(&c);
      const Item upper = item(s, &c);
      if (endpoint(upper)) {
        if (s->next_end == start) {
          s->next = RANGE_START;
          s->lower_end = current.end;
          s->range_end = upper.end;
        }
        break;
      }
      if (current.kind == SET_TEXT) {
        s->next_end = operator_end;
        current = upper;
        continue;
      }
    }
    if (current.kind != SET_TEXT) {
      if (s->next_end == start) {
        s->next = current.kind;
        s->next_end = current.end;
      }
      break;
    }
    s->next_end = current.end;
    if (s->next_end >= s->set_end) break;
    current = item(s, &c);
  }
  return emit(lexer, valid, SET_ITEM_START);
}

static bool class_piece(Scanner *s, TSLexer *lexer, const bool *valid) {
  if (valid[CLASS_OPEN])
    return consume(s, lexer, valid, CLASS_OPEN, s->position + 1);
  if (s->position == s->class_end - 1) {
    advance(s, lexer);
    lexer->mark_end(lexer);
    Cursor c = {lexer, s->position};
    if (!next_class(&c, s->set_end, false, &s->class_start, &s->class_end))
      s->class_start = NONE;
    return emit(lexer, valid, COMPOUND_CLOSE);
  }
  if (s->position == s->class_end - 2)
    return consume(s, lexer, valid, CLASS_CLOSE, s->position + 1);
  return text_run(s, lexer, valid, CLASS_NAME, s->class_end - 2);
}

static bool range_piece(Scanner *s, TSLexer *lexer, const bool *valid) {
  if (s->position == s->lower_end)
    return consume(s, lexer, valid, RANGE_OPERATOR, s->position + 1);
  const uint32_t end = s->position < s->lower_end ? s->lower_end : s->range_end;
  return consume(
    s,
    lexer,
    valid,
    lexer->lookahead == '\\' ? ESCAPE : RANGE_CHARACTER,
    end
  );
}

static bool scan(Scanner *s, TSLexer *lexer, const bool *valid) {
  if (valid[ERROR_SENTINEL]) return false;
  if (valid[BLANK_START] || valid[COMMENT_START] || valid[PATTERN_START])
    return start_line(s, lexer, valid);
  lexer->mark_end(lexer);
  if (
    (valid[SET_CLOSE] ||
      valid[MISSING_SET_CLOSE] ||
      valid[INCOMPLETE_SET_CLOSE]) &&
    s->position == s->set_end
  ) {
    if (s->set_end < s->content_end)
      return consume(s, lexer, valid, SET_CLOSE, s->position + 1);
    return emit(
      lexer,
      valid,
      absent(s, MISSING_SET_CLOSE, INCOMPLETE_SET_CLOSE)
    );
  }
  if (s->position >= s->line_end) {
    if (s->line_eof && lexer->lookahead == '\r')
      return consume(
        s,
        lexer,
        valid,
        TRAILING_CARRIAGE_RETURN,
        s->position + 1
      );
    return lexer->eof(lexer) && emit(lexer, valid, END_OF_FILE);
  }
  if (s->position >= s->ignored_start) {
    if (s->position == s->ignored_start && valid[IGNORED_START])
      return emit(lexer, valid, IGNORED_START);
    return text_run(s, lexer, valid, IGNORED_TEXT, s->line_end);
  }
  if (s->position >= s->content_end)
    return consume(s, lexer, valid, TRAILING_SPACE, content_limit(s));
  if (s->kind == COMMENT_START) {
    if (s->position == 0) return consume(s, lexer, valid, COMMENT_MARKER, 1);
    return text_run(s, lexer, valid, COMMENT_TEXT, s->content_end);
  }
  if (valid[NEGATION] && s->position == 0 && lexer->lookahead == '!')
    return consume(s, lexer, valid, NEGATION, 1);
  if (
    valid[SET_NEGATION] && (lexer->lookahead == '!' || lexer->lookahead == '^')
  )
    return consume(s, lexer, valid, SET_NEGATION, s->position + 1);
  if (valid[GLOB_START]) return prepare_glob(s, lexer, valid);
  if (valid[SET_ITEM_START] || valid[SET_NEGATION])
    return prepare_set(s, lexer, valid);
  if (s->class_start < s->position && s->position < s->class_end)
    return class_piece(s, lexer, valid);
  if (s->next == RANGE_START) {
    s->next = RANGE_CHARACTER;
    return emit(lexer, valid, RANGE_START);
  }
  if (s->range_end > s->position) return range_piece(s, lexer, valid);
  const enum Token token = (enum Token)s->next;
  if (token == CLASS_OPEN)
    return consume(s, lexer, valid, COMPOUND_OPEN, s->position + 1);
  if (token == ESCAPE_PREFIX) {
    s->next = INVALID_ENCODING;
    return consume(s, lexer, valid, token, s->position + 1);
  }
  return consume(s, lexer, valid, token, s->next_end);
}

void *tree_sitter_gitignore_external_scanner_create(void) {
  return ts_calloc(1, sizeof(Scanner));
}

void tree_sitter_gitignore_external_scanner_destroy(void *payload) {
  ts_free(payload);
}

unsigned
tree_sitter_gitignore_external_scanner_serialize(void *payload, char *buffer) {
  memcpy(buffer, payload, sizeof(Scanner));
  return sizeof(Scanner);
}

void tree_sitter_gitignore_external_scanner_deserialize(
  void *payload,
  const char *buffer,
  unsigned length
) {
  memset(payload, 0, sizeof(Scanner));
  if (length == sizeof(Scanner)) memcpy(payload, buffer, length);
}

bool tree_sitter_gitignore_external_scanner_scan(
  void *payload,
  TSLexer *lexer,
  const bool *valid
) {
  Scanner next = *(Scanner *)payload;
  if (!scan(&next, lexer, valid)) return false;
  *(Scanner *)payload = next;
  return true;
}
