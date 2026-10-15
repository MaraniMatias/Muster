# Typography, inspector columns and source highlighting

The record of one pass over three reported defects: the diff was drawn in the
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
| `native/Cargo.toml`, `Cargo.lock` | 22 grammar crates, plus three direct dependencies for the queries the toolkit omits. |
| `docs/native-rust-migration/{interface-parity,dark-colors}.md` | The delivered slice and the differences that remain. |

### The grammar set

Every grammar the toolkit ships and `syntax_language` names is compiled in, 32
languages in all: JSON, Rust, JavaScript, TypeScript, TSX, YAML, HTML, CSS/SCSS,
Markdown, TOML, Python, Bash, Go, C, C++, C#, CMake, Diff, EJS, ERB, Elixir,
Java, Kotlin, Lua, Make, PHP, Ruby, Scala, SQL, Svelte, Swift and Zig.

Two of those needed work beyond enabling a feature. The toolkit registers C++
with only the rules C++ *adds* to C, so `install` prepends C's query; and it
registers C#, CMake and Swift with empty queries, so their upstream queries are
taken from their own crates through three direct dependencies that add nothing
new to the graph.

`cc` moved from 1.8.0 to 1.2.67 in the lockfile. That is not churn:
`tree-sitter-sequel` 0.3.11 declares `cc = "~1.2.1"`, and SQL is in the set.

## How it is verified

`cargo test --locked --workspace`, `cargo clippy --locked --workspace
--all-targets -- -D warnings` and `cargo fmt --all --check` are the gate. The
native crate holds 241 default tests, seven of them new:

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
- Vue, XML, GraphQL, Protobuf, INI and Dockerfile still open as plain text, and
  there is no manual language picker. GraphQL and Protobuf are excluded on
  purpose: the toolkit registers both with empty queries.

## Security note

`node scripts/audit-rust.mjs` was run after the lockfile grew and **blocks** on
four advisories against `rustls` 0.23.45 (RUSTSEC-2026-0346/0347/0348/0349).
That version is pre-existing and untouched by this work; no grammar crate is
flagged. The gate is therefore red for a reason outside this change, and the
resolution belongs in its own change rather than here.