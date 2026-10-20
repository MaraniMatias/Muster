# AGENTS.md

Muster is being rebuilt as a native Rust desktop application. The root is a Cargo
workspace of two crates; the Tauri + Vue app it replaces is parked in `legacy/`.

## Layout

```text
Cargo.toml        workspace: core + native, and the [patch.crates-io] table
core/             muster-core   PTY + workspace shapes + Git discovery. No window, no toolkit.
native/           muster            the app: GPUI Kit, VT grid, keyboard, sidebar, lifecycle
legacy/           the Tauri 2 + Vue 3 app, frozen at v0.22.0. Not built here.
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
  Both files carry no version at all, so a stored shape is the whole contract: it is
  what it is, a change is made in place, and a file this build cannot read is
  refused by name rather than read through or migrated.
- The preferences live in `~/.muster/settings.json`, written by `core::settings`.
  This is the one store in `~/.muster` that reads a file it does not fully know:
  every field
  defaults, a key this build does not have is ignored, and a size outside its
  documented range is clamped rather than refused, because a preference holds no
  work and the file is deliberately a document a person may open and edit. It
  carries no colour, no theme and no zoom -- one appearance is dark and
  `native/src/theme.rs` is its only authority -- and no review storage, which
  `core::navigation` already persists. A file that does not parse is never
  written over.
- Linux builds need the system development packages the native crate links
  against: Vulkan, X11, xcb, XKB (and its X11 half), freetype and fontconfig. See
  the `linux-build` job in `.github/workflows/ci.yml` for the exact list.

## Native UI design system

`native/src/theme.rs` owns the app's dark palette, semantic roles, and common
control styling. UI chrome uses the platform UI font; Fira Code is reserved for
the terminal, source editor, diff text, and other code/text previews. Do not use
Legacy as the authority for native UI appearance. Keep product colors and state
colors in the theme instead of adding raw color values at call sites.

Use the toolkit's semantic button variants consistently: default for ordinary
actions, primary only for the commit action in a decision area, danger for
irreversible/destructive actions, and ghost/text for quiet toolbar or inline
actions. Disabled controls stay visibly disabled without changing a primary or
destructive action into a misleading neutral action. Native controls and
in-app commands use the default arrow cursor; reserve the pointing hand for
actual external links.

Use one compact geometry system across screens: 4px corners on controls,
8px corners on overlays, a small set of sizes for compact rows/toolbars and
regular actions, and consistent input/select heights within a form. Prefer
semantic toolkit sizes and shared helpers over one-off dimensions. Keep hover,
pressed, selected and focus-visible distinct; a selected row must remain
selected while hovered. Selected navigation uses its own surface and text
state, not a one-sided accent bar.

`Roles::row_rest` and `Roles::row_hover` are the one place a row's state-to-
colour combination is decided. GPUI refines hover after the rest style, so a
selected row's hover must preserve its selected treatment.

### Not the terminal's palette

`crate::terminal::Palette` is a separate contract and is not themed. It is what
a VT sequence asked for: ANSI indices, OSC colours, an explicit background. The
application draws it verbatim, and no role here reaches `TerminalHost::resolve_color`.
There is no terminal preference, no theme preference and no switcher: one
appearance, dark, chosen by `main.rs` before the first window opens.

The terminal grid, editor and diff share one body background:
`highlight_theme.editor.background` is `theme.background`. Keep terminal palette
colors separate from the UI theme; no chrome role reaches
`TerminalHost::resolve_color`.

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

## The unfinished work in the sidebar

A file being edited and a diff holding notes become rows of the workdir they
belong to — under its terminals, above **New terminal**. `TerminalWindow::items_in`
is what draws them, and it is a **read of live state, not a list kept in step
with it**: there is no stored collection of open items, and nothing to add,
forget or reconcile.

- A **document** is a row while it is a draft — the document on screen holding
  something the file does not, which is exactly the state the Save button is
  drawn for. Typing makes the row; saving, discarding, or undoing back to what
  the file holds takes it away. A file that is merely open, or merely read, is
  not unfinished work, so clicking through ten files adds nothing.
- A **diff** is a row while it holds a note, one row per file the notes are on,
  from `ReviewState::noted_paths`. A whole change set is not a row of its own:
  its notes belong to the files they were written on, and each of those is a row
  a person can click back into. Outdated notes do not count, by the same rule the
  diff's own count uses, so the panel and the pane cannot disagree.

Both carry what is waiting on them in the row's one label string —
`code.rs (edited)`, `notes.txt (2 notes)` — because the marker is what tells your
typing apart from your review at a glance. The whole row is the item, so it has
no close button: a draft is finished in the editor (Save or Cancel) and a note is
deleted where it was written, because a quiet X that threw away unsaved edits or
a written note would be a destructive action wearing a neutral control's clothes.

Three rules the rows depend on, each pinned by a test in
`native/src/sidebar_tests.rs`:

- **One open thing, one selected row.** The item row wears the selection, and
  the terminal row and the workdir row give it up: the workdir row stands in for
  "this is what the pane is showing" only when nothing inside it is. `shown_by`
  holds that the pane only shows an item while its own checkout is active, so a
  row under any other workdir is never the selected one.
- **Leaving a draft asks about the draft.** `request_open_item` goes through
  `request_document_transition`, so jumping from a file being edited to a review
  raises Save / Discard / Cancel exactly as going to a terminal does.
- **A row is drawn only under a workdir the panel still shows.** Notes outlive
  the worktree row they belong to, so an archived or removed workdir takes its
  rows with it by simply not being drawn, and restoring it brings back the ones
  its review still holds.

## The file tree

`native/src/inspector.rs` owns the Files tree: a scrolling box with a window of
fixed-height rows built either side of the top and the rest of the tree accounted
for as space above and below, so the scrollbar belongs to the tree, not the window.
It is deliberately **not** `gpui::uniform_list`: that does not publish where it is
scrolled to, and the folder stack drawn over the rows it names is exactly that number.
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
`native/src/review.rs` owns the draft. It composes a range label, a `Select` over
the line numbers a range could start on, a `Textarea`, and Save/Cancel actions.
The picker uses the toolkit's `Vec<SharedString>` delegate so each candidate is
only a line label. The toolkit places each annotation in a padded box; the composer
takes back that padding with `m_neg_3` so the card sits flush between diff lines.

Keyboard behavior is part of the composer contract: the platform modifier+Return
saves by intercepting Return before the `Textarea`, plain Return inserts a newline,
and Escape cancels. The shortcut is not currently shown in the card.

Both sides of a change are sides: a note opens on a line the diff **removed** as
readily as on one it added, widens over removed lines, is washed over them, and is
saved and exported against `ReviewSide::Old`. Nothing about the composer treats
the added side as the real one.

**Exported old-side lines are negative numbers**: `review_line_label` writes `-3`
for old line 3 and `3` for new line 3, and the Markdown fence carries that side
(` ```js{-3,-72} `). The composer's picker additionally prefixes a newly added
line with `+` and an old-side line with `-`; unchanged new-side context stays
unprefixed. Those picker marks help distinguish diff rows and do not change the
export format.

Five rules it depends on, each pinned by a test:

- **A range is written the way GitLab writes one.** The `+` fixes where the note
  ends, and a `+` clicked on a line above an open draft says where it starts: that
  click widens the draft and keeps its text, because it is the same comment about
  more of the same change. A click at or below it is a different line and opens a
  new note. A note being edited is never widened this way — its range is what it
  was saved against.
- **Where the range ends was decided by the `+`.** Only the picker moves it, and
  it is offered from the moment the note opens rather than after a range exists.
  Its candidates are every line the diff draws from the top of the note's own hunk
  down to the line it ends on, **on either side**: a change that removed the lines
  above an added one is part of the same range, so a note that ends on a `+` may
  start on a `−`. An untouched line is one line of both sides and is named on the
  side the note is anchored to. Its confirm is routed back through
  `set_review_note_start`, so the picker and the draft are one state rather than
  two.
- **The box does not move, and the lines do.** The card is attached to the
  composer's **anchor** — the line the `+` or `−` was clicked on — so choosing
  another start leaves the box where the person is typing; and the range is driven
  onto `DiffState::set_selected_lines` by one helper, so the lines the note covers
  are washed in the diff's own selection — visible while the note is open,
  re-marked when the diff is rebuilt underneath it, and cleared when it is saved,
  cancelled or deleted.
- **Both sides are sides.** `begin_review_note` opens a draft on a `−` line as
  readily as on a `+` one, the picker offers the removed lines above it, the wash
  is drawn over them, and the saved note and its export say `old`. A note is a
  range on **one** side, so choosing a start on the other side turns the note to
  that side and ends it on the last line of that side the range covers; a note
  that covers both sides is two notes.
- **⌘↵ saves, and it is an interceptor rather than a key handler.** Interceptors
  run before the input's own Return, so stopping it there is what keeps the
  keystroke from also inserting the newline a plain Return writes. Enter on its own
  still writes one, and `esc` still cancels.
- **A saved note is edited and removed from two icons on its header row.** They
  are icons rather than labelled buttons so the note reads as the card's text and
  not as its footer, and each names itself for the tooltip and for a screen
  reader. The pencil opens the composer above with the saved text and range in it,
  because editing a note is writing another one about the same lines; the cross
  asks first, and the answer lives in the window (`review.delete_confirmation`)
  rather than in the card, so the dialog is the thing on screen while it stands.

## The Pi integration

Pi is an agent this app can watch and can send a review to. It is not OpenCode
and it does not work the same way, and the difference is worth naming.

**What Pi says.** Pi reports through two escape sequences, and they are not
the same claim. OSC 7501 is Pi's own: it names a state, and it is the only
thing that can say *which* state. OSC 9;4 is the progress indicator every
terminal understands, and it can only say that something is running. So an
explicit state wins over the progress, and the progress is what is left when Pi
has said nothing more precise. The payload is bounded: a status longer than
4096 characters, a field that is not one of the four, a field named twice or a
malformed message is refused whole, because half a status is not a status.

**The handshake.** Pi asks `OSC 7501;?`, and the answer is the same question
back. Without it Pi keeps its state to itself. The reply is bounded per
session, because a program that keeps asking is not a program asking once.

**What may be believed.** A terminal's output is not Pi's output: another
program in front of the shell can print whatever it likes. What says a report
came from Pi is that the operating system names Pi as the process in front of
the shell, so that is the only thing a report is believed against -- and a
report is forgotten the moment the OS says something else is in front, or the
moment the shell runs something else. The panel's own list of terminals cannot
answer this, which is why the OS is asked.

**What the row says.** The row wears Pi's glyph, a spinner while a turn is
running, and the words `working`, `task complete` or `last task failed`. A
blocked turn says nothing: the panel has no shape for a question this program
asked, so the row stays neutral rather than inventing a state.

**A review to Pi.** The review is written to a private file under the app's own
folder -- `~/.muster/pi/prompts`, owner-only, swept after a day -- and handed
to a new Pi terminal in the checkout as one argument after `--`. A long review
is not a long command line, and nothing in the path is ever read by a shell.
The terminal is opened only once the prompt is on disk: a terminal that opened
with nothing to read would be a terminal that opened wrong. The prompt is
owned from the moment it is written and is removed unless a live Pi takes it:
a terminal that failed to start, an app that refused the session, a window
that went away before the terminal opened -- each of them leaves no prompt
behind, and a prompt removed before Pi reads it is a review Pi never saw.

**A worktree Pi is working in.** A live Pi process whose directory is inside a
worktree keeps that worktree from being archived or deleted, even when the
terminal it runs in belongs to another worktree. The *directory* fails closed:
a process whose directory the operating system will not name protects the
worktree it might be in. The *identity* does not: a process the OS will not
name is not counted as Pi at all, which is what the parked app does too and is
not changed here.

## The OpenCode integration

OpenCode is a service a person starts, and this app is a client of it. It never
starts one and never ends one: `core::opencode::discover` reads the registration
and checks that the service answers there, and a service that is not running leaves
the rows with no state, which is a state to wait in rather than a failure to report.

**What is read.** `core::opencode::observe` is the reading layer: the sessions the
service lists, the agent catalog, which sessions are running, and which are waiting
for a question or a permission. Three things make it safe to draw.

- The session list is read **without a directory**, because a session that moved to
  another worktree is not listed by the directory it left, and the row that would say
  so would draw nothing at all. The catalog and the pending lists answer for one
  location, so each location is asked about on its own.
- A pending list that **cannot be read** leaves its sessions *unknown* rather than
  idle: a question this app could not see is not a question that was not asked.
- A colour the service gave is checked for being a colour before it is painted
  (`theme::service_tint`), and the theme's own ink is what a row wears instead.

**What a row says.** `native/src/agent_watch.rs` turns that reading into what a
terminal row wears: a spinner while a turn runs, a question while it waits, the
error ink after a failed turn, the agent's own name and colour, and how long ago the
service last touched the session. The title the TUI wrote is the only thing that says
which session a terminal has open, and it is matched conservatively: a title that
names several sessions names none of them, and a service that could not be read names
none either.

**Where the reading happens.** The poll is on the background executor and comes back
as a value, so a service that takes its time costs a tick of the panel and nothing
else. Every answer carries the generation of the request it answered, and an answer
from an earlier generation is dropped rather than drawn: a stale answer describes a
world that is no longer on screen. The task belongs to the window, so it stops when
the window goes.

**The review round.** A review to OpenCode is a round in
`~/.muster/opencode-rounds.json`, and the store is what makes a send safe to retry.
A busy session is asked about rather than interrupted: *Send now*, *Queue* or *Not
now*. A queued round is written down **before** anything is offered, with the exact
message it will be sent with, so a round that waits is a round that provably was
never taken. A turn that finishes settles only the rounds that were already delivered
to that session: the round that was just flushed belongs to the turn that follows, and
an acknowledgement is not a note being resolved. A send that may have landed is never
re-offered on this app's own.

A queue is offered only on a **fresh** reading of the service, and only to a session
that is positively free: running, waiting for an answer, and a pending list that could
not be read are all reasons to wait, and a round whose session is not in the checkout
any more waits rather than holding up the rounds behind it. One reading answers the
whole queue, so several waiting reviews do not spend a deadline each. An ordinary send
reads the activity again before it offers anything, and claims nothing until it knows:
a busy session is a question for the person, and an unknown pending state is a
refusal.

**Following the work.** `native/src/follow.rs` moves a terminal's **row** when the
work it is doing moves to another worktree. The process, its scrollback and its
window stay exactly where they are: nothing here writes to a terminal, feeds a
directory to a shell, or tells a program in front of the shell where to go. Two
questions are followed separately — the shell's own directory, and the agent's work —
and the setting says which of them (`off`, `cd`, `agent`, `both`). A move needs a
destination that is a registered checkout: the deepest registered root wins, a nested
Git checkout this app does not know about is never folded into its parent, and two
roots that are one directory are ambiguous. Exactly one terminal may be showing the
session, and a move the person made by hand is not undone by an observation that is
older than it.

The **effective target** is kept, not just the cursor: no new tool work is not a
return to the session's starting directory. A session's own directory is its origin,
the baseline is the moment following was enabled, and work finished before that is
not a relocation. One pass runs at a time, and a result that arrives after the facts
it was computed from have changed is dropped rather than applied.

**Removing a worktree.** The service is asked before anything is archived or
deleted, because a session that was started outside this app has no terminal of this
app's to look at. The read is fresh: a row that said "idle" a minute ago is not what
the service says now, and the panel's own state is only a fallback. Three answers
come out of it — nothing is working there, something is, or **nobody could say** — and
the third one is a refusal, because an answer that could not be read is not an idle
session. A service that is not running at all is not evidence of anything, so it only
refuses when this app had already seen it working; that evidence survives a poll that
could not be read, and only a positively idle session erases it. The removal is
**reserved before** the network is waited on and **revalidated before** anything is
archived or deleted, so a worktree that changed while the service was being asked is
not removed on the strength of a stale answer.
The Pi guard stays: the operating system's answer about Pi's processes is a separate
claim, and neither one replaces the other.

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
**Clear cached exports** and **Forget delivered reviews** moved into its
Review notes section: they are one surface now, which is what `AGENTS.md`'s
**Settings → Forget delivered reviews…** had been claiming.

**The Review notes section is two blocks, and only the first is a preference.**
The storage row carries the folder it currently resolves to as a third line
under its help text, cut to the column and named in full on a rest; the two
maintenance actions are below it on one right-aligned line, because as rows of
their own each button stretched to the panel's width, which is a toolbar drawn
across the dialog and makes a maintenance command look like the panel's main
one. A disabled action says why: both are offered from the title bar, and the
one that is unavailable is unavailable for a reason the panel states on a rest.

**Clearing the cache needs a checkout, not a diff open.** The default location
is one folder, `~/.muster/tmp/reviews`, that every checkout writes to, so which
diff is open is not what the action acts on. It used to be disabled in exactly
the state the panel is opened in — the A08 failure the forget route had — and
the confirmation says so, because clearing is now reachable from anywhere.

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
button, and a disabled button still has to say why it is disabled. The canvas is
`absolute().inset_0()` and nothing else: `size_full()` on an absolute child of
a plain element measures against a zero-height reference and lands it *below*
the control rather than over it, which only shows up on a trigger that is not a
button. The trigger
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
act, not a consequence of a `--bump`. A `v1.0.0-rc.N` tag is that act's
candidate rather than the act: the crate carries the pre-release version the
tag names, GitHub files the release as a pre-release, and `v1.0.0` stays
unclaimed until the final one ships.

Consequences while the version is `v0.*.*`:

- A breaking change to a stored shape, an IPC contract, a preference key or a file
  format is made in place, and the version moves a minor. Nothing waits for a
  deprecation window, because nothing has to read what an older build wrote.
- There is no "deprecated but still supported" surface, no feature flag guarding a
  rewrite, and no compatibility layer held "one release" for anyone. The old path
  goes in the same commit as the new one.

## Stored shapes: one shape, no version, and no migrations

There is no database in this app. What it keeps is two JSON files in `~/.muster` --
the native workspace and the review round store -- and **neither carries a version
number**. `SCHEMA_VERSION` and `schemaVersion` are gone from both, and the file's
own shape is the contract: every field required, `deny_unknown_fields`, so a file
this build cannot read simply fails to parse. This app is going out again and has
no users but one, so there is no file written by an earlier build that anybody has a
reason to keep working, and a number that could disagree with the shape is a second
thing to keep true.

While that is true:

- **There is no ladder, because there is no number.** A stored shape is what it is:
  it is changed in place and the change is not a version bump. A file carrying a
  `schemaVersion` key is not this build's file and is refused like any other one.
- **A file this build cannot read is refused by name**, never read through defaults
  and never written over. Deleting it starts an empty store.
- **Never write a migration.** No `ALTER TABLE`, no version ladder, no back-compat
  shim, no compatibility reader, no `#[serde(default)]` on a field nothing writes.
  Delete the old path with the change, and leave out a field that exists only to read
  what an older build wrote.
- `legacy/src-tauri/src/persistence/mod.rs` still carries `SCHEMA_VERSION = 15` and
  its SQLite schema. That tree is frozen reference material, is not built here, and
  nothing in this workspace reads it; its number is not this app's to move.

This is not a rule that expires at `1.0.0`. It becomes a question worth a ladder
when somebody else's work is in those files, and that is a fact about who is using
the app rather than about a release number.
