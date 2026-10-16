# Typography, inspector columns and source highlighting

Historical pass records follow. Current inventory/evidence is summarized below;
current layout and behavior are in
[opening, reading and scrolling](opening-reading-and-scrolling.md#current-contract-after-ap-01ap-10).
Current suite results are canonical in [AP-11](audit-action-plan.md#ap-11).

## Current grammar inventory and evidence

Count the distinct non-`plaintext` return values of
`native/src/document.rs::syntax_language`, in both filename and extension arms:
**38** — bash, c, cmake, cpp, csharp, css, diff, dotenv, ejs, elixir, erb,
gitignore, go, graphql, html, ini, java, javascript, json, kotlin, lua, make,
markdown, php, proto, python, ruby, rust, scala, sql, svelte, swift, toml, tsx,
typescript, vue, yaml, zig. SCSS maps to `css`; filename aliases do not add
languages. This is a selector-output count, not a count of registry entries,
Cargo features or grammar packages. JSDoc supports injections without being a
selector output; the toolkit registers other names too. TypeScript/TSX can share
a package. Dotenv reuses Bash's parser with a file-specific query; Gitignore and
INI have their own registered parsers.
The earlier 32/33/34-crate narrative below is historical accounting, not an
inventory rule.

Evidence has distinct strengths:

- The path-table test in `document.rs` checks path-to-language mapping.
- `theme::tests::every_capture_a_grammar_can_emit_wears_a_legacy_token` and
  `every_grammar_has_had_its_captures_renamed` check registered query capture
  vocabulary/normalization, not visible syntax spans for every language.
- `additional_grammars_produce_legacy_token_colors` renders an editor harness,
  then checks standalone `SyntaxHighlighter::styles` output for its listed
  samples. It does not inspect the editor's painted token pixels or cover every
  selector output. JSON has a separate standalone style check.
- `a_vue_component_is_coloured_in_its_script_its_template_and_its_style` uses
  editor text but a standalone highlighter, asserting multiple colors separately
  in the script, template and style ranges. It is not a rendered pixel assertion.
- Headless UI bounds/click checks cover geometry and interaction decisions, not
  screenshot parity. No new manual rendering observation is claimed here.

The diff's injection limitation described at the end remains unchanged.

The historical record of one pass over three reported defects: the diff was drawn in the
wrong face and size, the Files/Changes panel did not stand on Legacy's columns,
and a document opened without colours.

The scope was presentation only. No stored shape, IPC contract or preference key
changed, so nothing here needed the `v0.*.*` migration rule.

## What was wrong, and what actually caused it

Two of the three reports had one cause each, and neither was where it looked.

**The face.** `theme.rs` installed `mono_font_family` but never `font_family`,
which stayed at the toolkit's `.SystemUIFont`. The toolkit's root refines every
window's surface with `theme.font_family` (`gpui-component/src/root.rs`), so the
chrome that names no family of its own — the diff's header, the inspector's rows,
both toolbars, and the toolkit's own menus and popovers — was drawn in the system
face. Legacy sets one family for the whole app (`legacy/src/style.css:4`).

**The row size.** A toolkit `Button` paints its label with `button_text_size`,
so `Size::Small` is `text_sm()` = 14px, and `content_style` is `pub(crate)`, so
an application cannot change it. The inspector's rows were `Button`s, which is
why they read at 14px whatever the panel said. They are now hand-drawn rows with
a transparent `Button` laid over each one — the pattern `sidebar.rs` already
used — which is what makes 13px reachable at all.

**The colours.** Not the theme. A probe against the installed `muster-content`
highlight theme resolved 3–5 distinct colours for JSON, Rust, JavaScript,
TypeScript, TSX, YAML, HTML and CSS, and **zero** for anything outside the nine
mapped languages, because `LanguageRegistry` held only those. `syntax_language`
answered `"plaintext"` for everything else, so `Cargo.toml`, `.py`, `.sh`, `.go`,
`.vue` and `.xml` all opened monochrome.

## What changed

| File | Change |
| --- | --- |
| `native/src/theme.rs` | Installs `font_family` and `mono_font_size` explicitly. `font_size` is left at the toolkit's 16px on purpose: the root derives the window's `rem` from it, so moving it would rescale every `text_*` in the window at once. Composes C and C++ highlight queries and installs the C#, CMake and Swift queries the toolkit registers empty. |
| `native/src/document.rs` | `syntax_language` answers from an extension table derived from Legacy's, plus a filename table for `Makefile`, `CMakeLists.txt` and the shell/JSON dotfiles. Unknown formats still open as plain text. |
| `native/src/diff.rs` | Names the diff chrome's face and size (11px), restores the status letter before the path at 10px, and moves the counts to Legacy's `--success-fg`/`--danger-fg` inks. |
| `native/src/inspector.rs` | Rows are hand-drawn: Legacy's columns, the 16px fold slot, 13px names, 10px status, 13px counts, the accent bar, and hover/selection/press ink. Tabs carry the accent underline instead of a wash. |
| `native/Cargo.toml`, `Cargo.lock` | Enables selected toolkit grammars, Vue, INI and the pinned Gitignore parser; Dotenv reuses Bash's parser. See the current selector-output inventory above rather than adding dependency counts. |
| `docs/native-rust-migration/{interface-parity,dark-colors}.md` | The delivered slice and the differences that remain. |

### The grammar set

The initial pass enabled the following selector languages through the toolkit
(the current inventory above includes later additions): JSON, Rust, JavaScript, TypeScript, TSX, YAML, HTML, CSS/SCSS,
Markdown, TOML, Python, Bash, Go, C, C++, C#, CMake, Diff, EJS, ERB, Elixir,
Java, Kotlin, Lua, Make, PHP, Ruby, Scala, SQL, Svelte, Swift and Zig.

Vue is an additional grammar the toolkit does not ship: it has no
Vue grammar and no feature to turn one on, so `theme::install` registers
`tree-sitter-vue-updated` in the toolkit's own `LanguageRegistry` with the
queries that crate exports — the same registry the toolkit grammars live in, reached
by the same name `syntax_language` answers. Its one capture the theme cannot
name, `@props`, is the attribute inside a directive (`v-if`, `@click`), so it
is renamed to `attribute`, and the pass that renames captures now walks every
*registered* grammar rather than the toolkit's enum, which is what lets it
reach a grammar the enum never heard of. That pass also now requires every name
a query emits to be paintable, which is what says a grammar is finished rather
than merely compiled.

Mermaid is the one Legacy highlighted that stays plain, and not for want of a
grammar: `tree-sitter-mermaid` needs `cc ^1.8`, and every `tree-sitter-sequel`
0.3.x — which is the toolkit's SQL grammar — pins `cc ~1.2.1`, so the two cannot
be resolved into one graph. Its older 0.1/0.2 releases pin `tree-sitter-language`
to `=0.1.7` while the graph runs 0.1.9, and the `LanguageFn` those releases
export would not convert.

Two of those needed work beyond enabling a feature. The toolkit registers C++
with only the rules C++ *adds* to C, so `install` prepends C's query; and it
registers C#, CMake and Swift with empty queries, so their upstream queries are
taken from their own crates through three direct dependencies that add nothing
new to the graph.

GraphQL and Protobuf complete the current selector set, and each needed a
query written here rather than lifted: the toolkit registers both empty, and
neither grammar crate exports a query constant. Protobuf's crate does ship a
`queries/highlights.scm`, but it names `returns`, `repeated` and `to`, which its
own grammar no longer declares, so copying it would not compile. `GRAPHQL_
HIGHLIGHTS` and `PROTO_HIGHLIGHTS` are written against the node names each
grammar does declare, in the capture vocabulary `SYNTAX_TOKENS` answers — which
is why every name either of them emits is paintable by construction rather than
by a rename after the fact.

`cc` moved from 1.8.0 to 1.2.67 in the lockfile. That is not churn:
`tree-sitter-sequel` 0.3.11 declares `cc = "~1.2.1"`, and SQL is in the set. It
is also why Mermaid is out: `tree-sitter-mermaid` wants `cc ^1.8`.

## How it is verified

`cargo test --locked --workspace`, `cargo clippy --locked --workspace
--all-targets -- -D warnings` and `cargo fmt --all --check` are the gate. The
historical pass added checks for:

- the diff header's column order, read off the drawn bounds;
- the installed theme's family and code size, and the deliberate 16px root;
- the inspector's fold slot at 8px and one 14px step per level, a change row one
  step in from its heading, and 26px rows;
- a folder's expanded state across a real click, and the collapse that follows;
- that `R`, `C`, `T` and `??` survive into both the change list and a diff
  header instead of being collapsed by the file tree's marker filter;
- that two rows agreeing about Git keep separate element identities, because an
  element id names an element for the whole window and a status or a count
  alone is not a name;
- that each new grammar resolves more than one colour through the installed
  Legacy token theme.

The bounds tests are differences between drawn rectangles, never glyph
advances: a text run's layout is rounded to whole pixels while a shaped advance
is not, and an absolute advance is not stable in a test process whose font state
belongs to the test that registered the faces. `sidebar_tests.rs` already
asserts only relative growth for the same reason.

**This is not a pixel comparison.** Nothing here renders a screenshot, so the
colours and geometry are verified as decisions and as measurements, not as
pixels. That gap is stated rather than closed.

## What is still different

- The toolkit's diff row is `font_size * 1.6` = 20.8px; Legacy rounds
  `13 * 1.55` to 20px.
- The `@@` band is fixed by the toolkit at 24px tall and 12px; Legacy draws it
  monospaced at the row height.
- The diff gutter is sized from its digit count; Legacy's are two fixed 48px
  columns.
- The diff's change wash comes from `theme.success`/`theme.danger`, where Legacy
  mixes its content-added and content-removed tokens.
- Inspector names truncate; Legacy's rows are `max-content` and scroll sideways.
  **Current:** Files measures its widest row and scrolls sideways; Changes fits
  panel-width rows and shortens names. See
  [opening, reading and scrolling](opening-reading-and-scrolling.md).
- XML and Dockerfile still open as plain text. Git ignore files use the
  dedicated Gitignore grammar; `.gitattributes`, `.gitconfig`, `.gitmodules` and
  other Legacy INI dotfiles use INI. `.env` and `.env.*` use a Dotenv-specific
  query over Bash's parser; `.envrc` remains plain text, matching Legacy. Other
  unsupported formats such as `.ps1`, `.less`, `.sass`, `.tf`, `.tex`, `.pl` and
  `.r` still open as plain text.

## Historical security note (not a current vulnerability lookup)

`node scripts/audit-rust.mjs` was run after the lockfile grew and **blocks** on
four advisories against `rustls` 0.23.45 (RUSTSEC-2026-0346/0347/0348/0349).
That version is pre-existing and untouched by this work; no grammar crate is
flagged. The gate is therefore red for a reason outside this change, and the
resolution belongs in its own change rather than here.
---

# One surface, every capture, and the whole header row

Three reports, one pass: the document and the diff were painted on a violet
well the terminal does not share, several tokens fell back to plain ink, and a
file in the change set could only be opened by clicking sixteen pixels of
chevron.

Presentation only. No stored shape, IPC contract or preference key changed.

## The violet was Legacy's, and this window is not a page

Legacy's `--muster-content-bg-0`, `#222436`, is the well a document sits in
inside a page. The terminal grid already refused it — see `terminal/view.rs`,
which wears `--muster-bg-0` and says why — and the editor and the diff kept it.
One background now serves all three: `highlight_theme.editor.background` is
`theme.background`, so `Roles::content` is the base the chrome is painted on
and the constant `CONTENT_SURFACE` is gone. Every syntax token reads better on
it than it did on `#222436` (the comment is 5.4:1 rather than 4.6:1), and
`theme::tests::content_surfaces_and_syntax_tokens_come_from_legacy` pins the
equality against the terminal's own palette rather than against a CSS file.

## A capture the toolkit cannot name is a token nobody painted

The mapping was already right for the names the toolkit's highlight theme holds
a key for. The gap was the other kind: `@escape` (Rust, Python, Ruby),
`@namespace` (Go), `@parameter`, `@method`, `@module`, `@delimiter`, `@float`,
`@field`, and Make's and SQL's directives are capture names that fall through
the theme entirely and land in the editor's own ink. A `\n` in the middle of a
green string was a different colour from the string.

A missing key cannot be added — `ThemeStyle`'s fields are private — so
`CAPTURE_ALIASES` renames those captures in each registered grammar's query at
install time, to the name that carries the same meaning (`@escape` →
`@string.escape`, `@namespace` → `@punctuation`). `@character.special` is the one
whole name answered before its segment: Swift spells its wildcard that way, and
a wildcard is punctuation where a Zig character literal is a string.

`every_grammar_has_had_its_captures_renamed` runs the rename over every
registered query and requires it to be a fixed point, so a grammar the pass
never reached fails. `a_renamed_capture_is_painted_as_legacy_paints_its_meaning`
checks five real sources through the highlighter, and reads every colour run
that touches the token rather than the run that starts on it — a run of its own
with no colour in it is exactly what the failure looked like.

## The row is the control

`Diff::render_header` replaces the toolkit's header content but keeps its shell
and its chevron, which is the only part of the row that answered a click. The
content is now one target across the row: `diff-file-<path>` carries the click,
the pointer, and `toggle_file_collapsed` plus the two state transitions the
window already had, extracted as `expand_diff_file`/`collapse_diff_file` so the
chevron and the row are one transition rather than two.

## How it is verified

`cargo test --locked --workspace`, `cargo clippy --locked --workspace
--all-targets -- -D warnings` and `cargo fmt --all --check`. Four new tests: the
three above, and `the_whole_diff_header_row_opens_and_closes_its_file`, which
clicks the row twice through a real pointer and reads the change set's own state
rather than the drawn frame.

## What is still different

- The toolkit paints the change wash from `theme.success`/`theme.danger`, where
  Legacy mixes its content-added and content-removed tokens; the two are the
  same *kind* of decision made from the chrome palette rather than the content
  one, and the toolkit offers no token for it.
- Files names scroll sideways; Changes names shorten inside panel-width rows.
  The earlier claim that both panels scroll sideways is superseded by
  [opening, reading and scrolling](opening-reading-and-scrolling.md).
- A Markdown heading is Legacy's `function` ink in bold and emphasis is the font
  alone, which the theme now says; strikethrough, which Legacy also draws, has
  no expression in the toolkit's `HighlightStyle`.

## A third pass: the size of the letter, and a stack that answers for itself

Two more defects from the same kind of report: the code read smaller than the
terminal beside it, and the folder stack did not work.

**The size.** The terminal draws at `FONT_SIZE` = 16px and the document and the
diff were reading `mono_font_size` = 13px, Legacy's editor size, so the same
code was a size smaller in this window than in the Legacy one it was ported
from. `theme::mono_font_size` is now `crate::terminal::FONT_SIZE`: the grid, a
file and a diff are one size of letter, and moving the grid moves all three
with it. The diff's *chrome* — the header at 11px and the status letter at
10px — is untouched, because it is metadata about the code and not the code.
The inspector's rows and the sidebar stay at 13px for the same reason they
always did: a 280px panel is not the place for the grid's size.

This is the one deliberate divergence from Legacy in this file. It is not a
token, so `dark-colors.md` has nothing to say about it.

**The stack.** The sticky ancestor rows were drawn over the rows they name with
no hitbox of their own, so a click on a stacked folder's name went *through*
the stack to the tree row hidden underneath: measured, it opened
`src/nested/f12.txt` while the tree was scrolled such that the file's own row
sat at y = −207, above the top of the tree. Only the small "Show in tree"
button worked, and Legacy has no such button.

A stacked row is now the tree's own folder row: the same 26px, the same 16px
fold slot, the name starting on the column the tree starts that folder's name
on. The fold closes the folder and the row brings it into view, and each stops
its click on the way out — the row because the rows it covers are still under
the pointer, the fold because the row would otherwise answer it too. The
"Show in tree" button is gone with the click it was carrying.

`stack_room` also stopped measuring the window. The stack is laid inside the
tree's box and clipped by it, so what it may cover is that box's height, and
the window's is taller: a stack sized to the window hides more of the tree
than it has room for. It reads the scroller's own bounds now, which is the
tree as it was a frame ago — the same one-frame-old reading its offset already
was.

### How it is verified

Two new tests in `inspector_tests.rs`, plus one in `theme.rs`:

- `a_stacked_folder_keeps_its_own_click` — the defect, measured the same way:
  it asserts the stacked name is drawn over a real row, clicks it, and reads
  that no file was selected and the tree came to the folder. It also clicks
  the fold and reads that the folder closed and its subtree was forgotten.
- `the_folder_stack_appears_over_a_scrolled_tree` — now also pins the stacked
  row's columns to the tree's.
- `the_document_and_the_diff_read_at_the_terminals_own_size`.

The gate is the same three commands, and they are clean.

---

# A single-file component, and the one place a diff cannot follow it

Two reports of the same kind: Vue highlighting "isn't working", and the file
viewer shows a `.vue` file "like HTML, not like Vue", with things missing.

Presentation only. No stored shape, IPC contract or preference key changed.

## Neither the theme nor the grammar was wrong

A probe against the installed `muster-content` theme resolves 2–7 distinct
colours for the samples probed then (not a registry census), and `syntax_language` answers
`"vue"` for `component.vue`. The theme and the registration were both already
right; so was `document::tests::additional_grammars_produce_legacy_token_colors`,
except for one row: **Vue was monochrome**, and it had been failing that way
with a debug probe left in the test.

A Vue SFC is three languages in one file. `<template>` is the Vue grammar's own
markup query — a tag is a tag, an attribute is an attribute — so it is *meant*
to read like HTML. `<script>` is reached through the grammar's **injections**,
and so is `<style>`. That is where the file was coming apart.

## The stylesheet had no language

`theme::install` registers the Vue grammar with
`tree_sitter_vue_updated::INJECTIONS_QUERY`, which injects into `<script>` and
into a `<style>` that **names** one — `lang="ts"`, `lang="tsx"`, `lang="css"`,
`lang="scss"`. A bare `<style>`, which is what most components write, matched
no rule: the whole block opened in the editor's own ink. `VUE_INJECTIONS` adds
the one rule that was missing, and CSS is what that block is without a `lang`.

The rule also matches the two blocks that do name one, and that is a harmless
overlap rather than a precedence question: `scss` and `css` resolve to the same
registered grammar, so either rule colours the block as CSS.

## The diff applies no injections at all

This is the half that is **not** fixed, because it is not ours to fix.
`gpui-component`'s `SyntaxHighlighter::capture_names` is the diff's only entry
point, and it takes the injections query out before parsing:

> Injected languages are skipped: building their layers compiles a query on
> every parse, which dominates the cost of many short fragments.

A diff parses each hunk as a short fragment, so the cost argument holds — and
the consequence is that **every embedded language is blank in a diff**: a Vue
`<script>` or `<style>`, an HTML `<script>` or `<style>`, a Svelte `<script>`,
a Markdown fence. The host language itself is highlighted normally; a `.vue`
file in the diff is tags in colour and everything else in plain ink, which is
exactly the "it's HTML, not Vue" report. The document viewer does not have this
problem, because it goes through `styles()`, which keeps its layers.

Closing it needs an upstream change in `gpui-component` (or a vendored patch of
it), not a line here. It is recorded rather than worked around.

### How it is verified

`cargo test --locked --workspace`, `cargo clippy --locked --workspace
--all-targets -- -D warnings` and `cargo fmt --all --check`.

One new test, `document::tests::a_vue_component_is_coloured_in_its_script_its_template_and_its_style`:
it opens an SFC in the editor harness, then asks a standalone highlighter for
each of the three blocks' styles separately, because a component whose `<style>` is uncoloured
still passes a check that only asks whether the file is coloured *somewhere*.
`additional_grammars_produce_legacy_token_colors` stops failing on
`component.vue`, and its two leftover `eprintln!` probes are gone.

---

# The template's own text wore the colour of a tag

The same file again, one report narrower: the `<template>` was coloured, the tags
in it were coloured, and the **text between the tags was coloured too** — in the
tag's own gold, so a label read `Preview Type` in exactly the colour of
`x-label`.

## The capture was wider than a token

`tree_sitter_vue_updated`'s `highlights.scm` opens with

```scm
[
  (template_element)
  (tag_name)
  (start_tag)
  ...
] @tag
```

`(template_element)` is not a tag name: it is **the whole `<template>` element**,
from the opening `<template>` to the closing one. `(element)` is the same idea
for one tag's body. A span that wide reaches every byte inside that the rest of
the query does not name, and what the rest does not name is the whitespace, the
indentation, and the prose between the tags.

The query does name the prose — `(text) @none` — but `@none` wears no colour, so
the container's gold underneath it is what the editor paints. Legacy's Shiki
grammar had no such span: template text there was `text`, which its tables leave
as plain foreground.

## One query says so

`VUE_HIGHLIGHTS` prepends `(template_element) @none` and `(element) @none` to the
grammar's own query, beside the `(attribute_name) @function` that is already
there. It is the same trick and the same reason: the highlighter takes the first
pattern that matches a node, and all three of the grammar's are further down its
file. A container then wears nothing, the tags inside it are still named by
`tag_name`, `start_tag`, `end_tag` and the four delimiter literals, and the text
between them falls to the editor's ink.

### How it is verified

`cargo test --locked --workspace`, `cargo clippy --locked --workspace
--all-targets -- -D warnings` and `cargo fmt --all --check`.

One new test, `document::tests::vue_template_text_is_not_painted_as_a_string`:
it opens an SFC, asks for the styles of the whole file, and reads the colours
that land on `Preview Type` and on `x-label` separately. The second half matters
as much as the first — a file that failed to parse, or a highlighter that found
nothing in it, wears no colour anywhere and would pass a check that only asked
about the text.
