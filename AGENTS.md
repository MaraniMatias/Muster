# AGENTS.md

Muster is being rebuilt as a native Rust desktop application. The root is a Cargo
workspace of two crates; the Tauri + Vue app it replaces is parked in `legacy/`.

## Layout

```text
Cargo.toml        workspace: core + native, and the [patch.crates-io] table
core/             muster-core   PTY + workspace shapes + Git discovery. No window, no toolkit.
native/           muster            the app: GPUI Kit, VT grid, keyboard, sidebar, lifecycle
legacy/           the Tauri 2 + Vue 3 app, frozen at v0.21.1. Not built here.
scripts/          the RustSec audit, the macOS bundle and the vendored-crate hash pins
docs/             the migration plan, and what the port has to satisfy
```

## Development and verification

- The toolchain is pinned in `rust-toolchain.toml`. Use that `cargo`, not another
  one that happens to be earlier in `PATH`. Put the directory returned by
  `dirname "$(rustup which --toolchain 1.97.1 rustc)"` first on `PATH`, then
  confirm both `rustc --version` and `cargo --version` are 1.97.1.
- `cargo test --workspace` covers both crates. `cargo clippy --locked --workspace
--all-targets -- -D warnings` and `cargo fmt --all --check` are both clean and
  are expected to stay that way.
- Current test counts and exact run evidence live in
  `docs/native-rust-migration/audit-action-plan.md` under AP-11; earlier task
  counts are historical. Ignored performance checks use `scripts/perf-native.sh`.
  The PTY tests belong to `core`, including its `core/tests/` instance-lock suite.
- A focused test: `cargo test --locked -p muster-core pty::`.
- The native app uses GPUI's `test-support` feature for headless UI coverage;
  `native/src/app.rs` uses it for the window, focus and close-path cases,
  `native/src/sidebar_tests.rs` for the sidebar rows, the worktree dialog and
  the directory picker, and `native/src/titlebar_tests.rs` for the title bar:
  its crumbs, its menus and the window it belongs to.
- `native/src/theme.rs` carries its own tests, so the roles are checked without
  a window. `native/src/state_writer.rs` carries its own too: what the slot
  keeps, drops and flushes is decided without one.
- The native workspace lives in `~/.muster/native-workspace.json`, written by
  `core::navigation`. It is **not** the Legacy SQLite database and nothing reads
  the old one. It holds roots, archived checkout ids, the active checkout, the
  sidebar width and the review export location. Live sessions and terminal
  layouts are never written; review notes stay in window memory until exported,
  except for the notes a review sent, which are captured in
  `~/.muster/opencode-rounds.json` so a sent review can be shown and settled
  later. That file is app state like the workspace file, under the same lock and
  the same bounded, non-symlink read.
  It is also capped, and a cap is not a wall: a write that gets close gives up
  what no reader can ask for again, which is the prompt of a settled round, and a
  person can forget delivered reviews from **Settings → Forget delivered
  reviews…** (from any window state; the store is one file). The ordinary action
  keeps SHA-256 fingerprints of canonical note snapshots as `id:digest` claims,
  so unchanged notes remain deduplicated with collision resistance, not mathematical
  certainty. The same confirmation also offers an explicit last-resort reset that
  removes settled rounds and their claims; unchanged delivered notes can then be
  sent again. Neither action touches queued, dispatching or unknown rounds.
  A restart creates a fresh shell in the selected checkout; it never revives
  processes that are gone.
- The preferences live in `~/.muster/settings.json`, written by `core::settings`.
  This is the one store in `~/.muster` with **no schema version**: every field
  defaults, a key this build does not have is ignored, and a size outside its
  documented range is clamped rather than refused, because a preference holds no
  work and the file is deliberately a document a person may open and edit. It
  carries no colour, no theme and no zoom -- one appearance is dark and
  `native/src/theme.rs` is its only authority -- and no review storage, which
  `core::navigation` already persists. A file that does not parse is never
  written over.
- Linux builds need the Vulkan, X11 and XKB development packages. See the
  `linux-build` job in `.github/workflows/ci.yml` for the current list.

## Colour: roles, not values

`native/src/theme.rs` is the only place this app names a chrome colour. Its
`install` function applies Legacy's dark tokens to GPUI Kit with `Theme::update`
before the first window opens; it must not use the toolkit's default dark
palette. Every surface, ink, hairline and state wash is a `Roles` field, so
application drawing and toolkit widgets share that installed palette.
`docs/native-rust-migration/dark-colors.md` records the token equivalence and
remaining component-level differences. Adding a `rgb(0x...)` anywhere
else is the thing to catch in review.

The states the roles have to cover, and what each one is for:

| role                                                         | state it paints                                                 |
| ------------------------------------------------------------ | --------------------------------------------------------------- |
| `surface_base`, `surface`, `surface_raised`, `surface_inset` | application/field, panel, overlay, recessed well                |
| `text`, `text_muted`, `text_disabled`                        | primary, secondary, not actionable                              |
| `border`, `border_strong`                                    | hairline, overlay edge                                          |
| `hover`, `pressed`                                           | pointer on, control held                                        |
| `selected`, `selected_hover`                                 | this window's open row, and the same row with the pointer on it |
| `focus`                                                      | the ring a focused control wears                                |
| `disabled_surface`                                           | a row that cannot be chosen                                     |
| `accent`, `drag`                                             | the selection bar and a chosen crumb; a slot being dropped into |
| `status.*`                                                   | added, removed, waiting, failed, done, worth reading            |

`Roles::row_rest` and `Roles::row_hover` are the one place a row's
state-to-colour combination is decided, because GPUI refines a hover style
after the rest style: a drawing that answers both in its own closure will paint
the open row's hover over its selection for exactly as long as the pointer is
resting on it.

## Shape: Zed's radii, the toolkit's sizes

GPUI Kit is **not** the widget library Zed draws with. Only the core is: the
toolkit depends on `gpui-pre`, a snapshot of Zed's `gpui`, so the primitives and
the focus handling are Zed's. The _widgets_ are longbridge's `gpui-component`,
a shadcn-flavoured design system, where Zed's buttons and overlays live in Zed's
own unpublished crate. So a control drawn here wears the toolkit's shape unless
this app says otherwise, and `theme::install` is where it says so:

| what                | where                             | value                                            |
| ------------------- | --------------------------------- | ------------------------------------------------ |
| an overlay's corner | `theme.radius`, `theme.radius_lg` | `8px`, Zed's `rounded_lg`                        |
| a button's corner   | `theme::zed_button`               | `4px`, Zed's `rounded_sm`                        |
| a button's outline  | `theme::zed_button`               | none; Zed fills a control and gives it no border |

The toolkit keeps **one** radius token for both roles, so `install` gives it the
overlay's `8px` — its dropdown menus and popovers read that one — and the
controls name Zed's `4px` where they are built. A control must therefore not be
left to the token: a button through `theme::zed_button`, and the dialog's own
fields where they are.

A button is built with `theme::zed_button(id)`, which is the toolkit's
`Secondary` variant: `install` gives `button`, `button_hover`, `button_active`
and their foregrounds to both `Default` and `Secondary`, so the fill, the
pointer ramp and the ink are the ones already installed and only the 1px
hairline box goes. A control that means to be one of Zed's other buttons says
so after it — `.ghost()`, `.text()`, `.custom(..)`, `.danger()` — which is why
those calls still exist at call sites.

What no token reaches is the size ladder: the toolkit hardcodes 20/24/32px
where Zed's is 22/28/32, so a `.small()` button here is 4px shorter than Zed's
`Medium`. Correcting that means pinning `.h(..)` per button, which is not done.

### Not the terminal's palette

`crate::terminal::Palette` is a separate contract and is not themed. It is what
a VT sequence asked for: ANSI indices, OSC colours, an explicit background. The
application draws it verbatim, and no role here reaches `TerminalHost::resolve_color`.
There is no terminal preference, no theme preference and no switcher: one
appearance, dark, chosen by `main.rs` before the first window opens.

The grid is the window's body, so the document and the diff share its
background: `highlight_theme.editor.background` is `theme.background`, and
Legacy's `--muster-content-bg-0` (`#222436`, a violet well for a document drawn
inside a page) has no role. One background for the terminal, the editor and the
diff is `theme::tests::content_surfaces_and_syntax_tokens_come_from_legacy`.

### Syntax: two tables, and why there are two

A grammar's capture is a Legacy meaning spelled its own way. `SYNTAX_TOKENS`
answers the names the toolkit's highlight theme holds a key for;
`CAPTURE_ALIASES` renames the ones it holds **no** key for (`@escape`,
`@namespace`, `@parameter`, `@module`, …) in each registered query at install
time. A capture that reaches neither table is painted in the editor's own ink,
so a new grammar's query is not done when it compiles — it is done when every
name it emits is in one of the two.
`theme::tests::every_capture_a_grammar_can_emit_wears_a_legacy_token` states the
contract from the grammar's side, and
`every_grammar_has_had_its_captures_renamed` fails if the pass missed one.

### File icons: their own palette, and their own generator

`theme::Ink` is the second contract that is not the app's chrome. A file icon is
a drawing wearing the colours Catppuccin drew it in, the way a VT sequence wears
what it asked for; `Roles::ink` holds the values, and nothing else in the crate
names one.

`native/src/file_icons/data.rs` is generated by
`scripts/generate-native-file-icons.mjs`, which reads Legacy's own generated
module rather than a Catppuccin checkout, and carries Catppuccin's MIT notice.
Do not hand-edit it; regenerate. The one thing that cannot be copied is colour:
GPUI rasterizes an SVG to a single-colour alpha mask, so a drawing that paints
with three inks is three layers, each a complete SVG, stacked in one box and
tinted per layer. The generator enforces that every layer carries a paint usvg can
parse, that every shape lands in exactly one layer, and that no rule key carries a
dot — a key with a dot is one no file name can be asked about, which is how Legacy
reaches a third fewer rules than the theme defines.

## On macOS, the app is the bundle

An executable has no icon of its own on macOS: the icon, the name Finder and
the menu bar show, the version in Get Info and the category are what
`Info.plist` says about the `.app` around it. `scripts/bundle-macos.sh` is that
`.app`, and it carries Legacy's identifier, name, copyright and artwork so the
OS files this build as the same application rather than a second one that looks
like it. `native/assets/icon.icns` is the bundle's icon and `icon.png` is the
same 512×512 the About panel draws; the artwork came out of `legacy/` and is not
hand-edited.

The menu bar is `native/src/menus.rs`, and it is macOS-only by call site: the
same two submenus Legacy built in `legacy/src-tauri/src/menus.rs`. The Edit
submenu is not decoration, and it is the one part of the menu that changes
behaviour rather than adding it — AppKit answers a key equivalent before the key
reaches the window, so `⌘C`/`⌘V` arrive as `input::Copy`/`input::Paste`, which is
why `TerminalView` listens for those two as well as answering the keys itself.

`LSMinimumSystemVersion 15.0` in that bundle is a claim; `.cargo/config.toml` is
what makes the build enforce it. The two are one decision: if the floor moves,
both move, and if a build fails with an unavailable API then that API is the real
floor rather than the number that was written down.

## The file tree

`native/src/inspector.rs` owns the Files tree and draws it the way Legacy does: a
scrolling box with a window of rows built either side of the top and the rest of the tree
accounted for as space above and below, so the scrollbar is the tree's and not the
window's. It is deliberately **not** `gpui::uniform_list`: that does not publish where it
is scrolled to, and the folder stack drawn over the rows it names is exactly that number.
`ScrollHandle::offset()` is.

Three rules the tree depends on, each pinned by a test:

- Opening a folder opens it **inside** the tree that is already open. A listing arriving
  used to clear every other expansion in the checkout, which collapsed everything above
  the folder being opened.
- Collapsing a folder forgets its **whole subtree** — listings, pending reads and
  failures — and `within` is what keeps a sibling like `src-tauri` out of it.
- Every row is `flex_none`. A flex column will shrink its children to fit, and the rows
  then stood 21 pixels apart in a tree that scrolls by 26.

## The document pane

`native/src/document.rs` draws the file in place of the terminal, and it is an
editor rather than a viewer: a typed character makes the document a draft, and a
draft owns the pane's last row until it is saved or cancelled.

Three rules it depends on, each pinned by a test:

- A **save compares before replacing**. `core::files::write_preview` compares
  against the text the document was opened with and rechecks target identity and
  bytes just before rename; a detected change is refused, but POSIX rename leaves
  a narrow race with a non-cooperating writer, so this is not an unconditional
  compare-and-swap guarantee. A pre-publication failure leaves the original in
  place. If directory sync fails after rename, the bytes are already visible but
  durability is uncertain: the pane keeps the draft/intent, updates its baseline,
  and offers a retry rather than navigating. A read-only file is refused because
  replacing it needs the directory's write access and would otherwise undo its bit.
- **A draft is edited in its source.** The reading is a parse of the text as it
  was read, so drawing it while the editor holds another one shows a document
  that is no longer there. The reading is rebuilt from the saved text, which is
  why a save and a new parse are one step rather than two.
- **Dirty is a comparison, not an event.** `refresh_document_draft` asks the
  editor's rope against the loaded text, so an undo back to what the file holds
  ends the draft instead of leaving a state a draft can be stuck in.

## The review composer

`native/src/diff.rs` draws the note being written as a row of the diff and
`native/src/review.rs` owns the draft. It is Legacy's composer: the range label
and then the picker, the field that turns the accent blue while it has focus (what
`.review-note-input:focus` did there), Legacy's sentence about what a range covers
and its two buttons with room around them. What is _not_ Legacy's is the margin
around it — the toolkit draws every annotation inside a `px_3 py_3` box, and the
composer takes that padding back with `m_neg_3` so the card sits flush between the
lines above and below it.

What it wears is the toolkit's own widgets rather than a hand-rolled imitation of
them: the range picker is a `Select` over the line numbers a range could start on
(the delegate is the toolkit's `Vec<SharedString>`, so a candidate is a line
number and nothing else), and the two keys the card answers are drawn with `Kbd`,
which is what keeps the hint correct off macOS without the card spelling out a
platform pair.

Both sides of a change are sides: a note opens on a line the diff **removed** as
readily as on one it added, widens over removed lines, is washed over them, and is
saved and exported against `ReviewSide::Old`. Nothing about the composer treats
the added side as the real one.

**A removed line is a negative number**, and the picker and the export say the
same thing: `review_line_label` writes `-3` for old line 3 and `3` for new line 3,
and the Markdown fence carries the same sign (` ```js{-3,-72} `). The sign is
the only thing in an exported document that says which side of the change a comment
is about, and the offer and the record cannot disagree because they are one
function's word.

Five rules it depends on, each pinned by a test:

- **A range is written the way GitLab writes one.** The `+` fixes where the note
  ends, and a `+` clicked on a line above an open draft says where it starts: that
  click widens the draft and keeps its text, because it is the same comment about
  more of the same change. A click at or below it is a different line and opens a
  new note. A note being edited is never widened this way — its range is what it
  was saved against.
- **Where the range ends was decided by the `+`.** Only the picker moves it, and
  it is offered from the moment the note opens rather than after a range exists.
  Its confirm is routed back through `set_review_note_start`, so the picker and
  the draft are one state rather than two.
- **The box does not move, and the lines do.** The card is attached to the
  composer's **end** line, so choosing another start leaves the box where the
  person is typing; and the range is driven onto `DiffState::set_selected_lines`
  by one helper, so the lines the note covers are washed in the diff's own
  selection — visible while the note is open, re-marked when the diff is rebuilt
  underneath it, and cleared when it is saved, cancelled or deleted.
- **Both sides are sides.** `begin_review_note` opens a draft on a `−` line as
  readily as on a `+` one, the picker offers the removed lines above it, the wash
  is drawn over them, and the saved note and its export say `old`.
- **⌘↵ saves, and it is an interceptor rather than a key handler.** Interceptors
  run before the input's own Return, so stopping it there is what keeps the
  keystroke from also inserting the newline a plain Return writes. Enter on its own
  still writes one, as Legacy's textarea did, and `esc` still cancels.

## The preferences panel

`native/src/settings.rs` is the panel, and the shape it draws is
`core::settings::AppSettings` — the same schema Legacy generated its dialog from,
so a preference added there is a row and a closure rather than a branch in a
template.

Three things about it are decisions rather than arrangement:

- **It is a draft, and nothing reaches the file until Apply.** That is what makes
  Cancel possible, and it is why a size somebody is still typing is never a size
  anything is drawn at. The draft is built whole on every open.
- **The four sizes live in their fields, not in the model.** A toggle and a select
  report their change; a number field is a person mid-thought. The range is applied
  when the draft is written, and the clamped number is written back into the field,
  so the panel closes on what is on file rather than on what was typed.
- **A broken file opens the panel rather than stopping it.** Whoever typed it is who
  can fix it, so the fields show the defaults with the failure beside them — and
  Apply refuses, because that file is the only copy of somebody's preferences.

**Apply writes the file and the review location, and it does not repaint a live
terminal.** Nothing in this app can change a running grid's font, cursor or
scrollback yet. A panel that implied otherwise would be a lie about what a
preference does, so it says nothing about it instead.

The Settings action in the title bar opens this panel, and the review storage,
**Clear cached review exports…** and **Forget delivered reviews…** moved into its
Review notes section: they are one surface now, which is what `AGENTS.md`'s
**Settings → Forget delivered reviews…** had been claiming.

## The tooltip

`native/src/tooltip.rs` is the window's tooltips, and every trigger in the app
goes through its `RestingTooltip` rather than the toolkit's managed one. The
managed tooltip switches: a trigger entered while the last tooltip is still up
takes its place at once, so the wait is paid once per session rather than once
per row, and browsing the Files tree paints the path of every row the pointer
crosses. Here each trigger pays the wait — `Tooltips::rest` clears the overlay
before it asks for the next one, so nothing is inherited — and the wait itself
is `TooltipDefaults::show_delay`.

A trigger is a canvas laid over the control and a window listener it registers
as it is painted, not a hover: `Button::on_hover` is inert on a disabled
button, and a disabled button still has to say why it is disabled. The trigger
is named by its bounds, which is what makes a move from one row to the next a
new trigger rather than a switch. The window host dismisses what was up on a
press or when the pointer leaves the window, as the managed one did.
`tooltip_tests` pins the wait: `every_trigger_waits_its_own_time` on the
sidebar, `a_file_row_names_its_path_after_the_pointer_rests_on_it` on the Files
tree, and `a_disabled_row_still_names_itself` on a button that cannot be
hovered.

## The vendored parser

`native/vendor/alacritty_terminal-0.26.0` is a security backport, not vendored
noise. crates.io 0.26.0 is the latest published release and panics on a remotely
triggerable input, and upstream has published no corrected release to move to.
`native/vendor/alacritty_terminal-0.26.0/PATCHES.md` records the defect and the
one-line fix; `scripts/patched-alacritty.mjs` hash-checks the whole tree, so an
edit there breaks the audit gate.

The `[patch.crates-io]` table lives in the **root** `Cargo.toml`, not in
`native/Cargo.toml`: only the workspace root's patch table is read.

## Security

`scripts/audit-rust.mjs` reads the locked graph of `core/Cargo.toml` and
`native/Cargo.toml`, refuses any path package it cannot hash-pin, and queries
OSV. `node --test scripts/audit-rust.test.mjs` covers it. A new path dependency
that is not a workspace member will fail the audit closed, which is the intent.

## Adding to the port

- A module that no window needs belongs in `core`, even if it came out of
  `legacy/src-tauri/src/services/`. That tree is `pub(crate)`-shaped and its
  types move as one dependency, not as copies.
- A module that draws, takes a key event, or owns a widget belongs in `native`.
  The VT parser and the grid are here on purpose: nothing else parses terminal
  output, so a crate for them would have one caller.
- Reading a checkout's files and writing one are the **same** boundary, in
  `core::files`, for the reason above: a second path to a file on disk is a second
  path policy. Writes of existing checkout files go through `write_preview`.
  Review Markdown export is the deliberate exception: it creates a new file under
  `.muster/reviews` through `export_review_markdown`, which validates the registered
  checkout, rejects symlinks, and creates the export without overwriting. Clearing
  reviews only removes marked app exports from the default cache and refuses workdir
  mode. Workspace state in `~/.muster` is separate and
  `native/src/state_writer.rs` owns it.
- `legacy/` is reference material. Reading it is expected; editing it is not, and
  nothing verifies it any more.

## Versions are v0._._ — all of them

Every version this project ships is `v0.<minor>.<patch>`. The major is `0` and
stays `0` until someone says otherwise in a commit that also does the migration
work described below. A `v1.0.0` is not a version bump; it is the release where
the schema rule below has to already be in place, so it is its own deliberate
act, not a consequence of a `--bump`.

Consequences while the version is `v0.*.*`:

- A breaking change to a stored shape, an IPC contract, a preference key or a file
  format is made in place, and the version moves a minor. Nothing waits for a
  deprecation window, because nothing has to read what an older build wrote.
- There is no "deprecated but still supported" surface, no feature flag guarding a
  rewrite, and no compatibility layer held "one release" for anyone. The old path
  goes in the same commit as the new one.

## Database schema: no migrations before 1.0

`legacy/src-tauri/src/persistence/mod.rs` carries one schema and no ladder.
`create_schema` writes the whole schema into a file that has none and refuses a
file stamped with any other `SCHEMA_VERSION`; a user upgrades by deleting the
database and starting an empty workspace. That is deliberate, and while the
version is `v0.*.*` it stays deliberate.

While the version is `v0.*.*`:

- **Never write a migration.** No `ALTER TABLE`, no version ladder, no back-compat
  shim. Add the column to `create_schema`, bump `SCHEMA_VERSION`, ship it. Every
  existing database is refused by name and starts empty.
- **Never keep legacy code.** No deprecated variant kept readable, no
  `#[serde(default)]` on a field nothing writes yet, no branch for a shape the
  current build cannot produce. Delete the old path with the change.
- A field that exists only to read what an older build wrote is a migration by
  another name. Leave it out and fix the one place that wrote it.

The rule expires at 1.0. The first `v1.0.0` is where the ladder has to start: from
that version someone's work lives in that database and cannot be refused.
