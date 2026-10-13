# Ghostty evaluation for Muster's native terminal

**Status:** source-backed evaluation and proposed delivery gates, not implementation approval.
**Reviewed:** 2026-10-13 UTC.
**Muster baseline:** `c9684353f2cf2f7c023072dbac3804daa6346327`. The terminal source
is unchanged from `9bb3cd3`; unrelated working-tree application edits were left alone.
**Ghostty snapshot:** [`adbcb3da673407ab79254620536c909b23e32419`](https://github.com/ghostty-org/ghostty/tree/adbcb3da673407ab79254620536c909b23e32419).
This is a development revision, **not a claim about a released version**. References
below are pinned to it rather than to a moving `main` or a remembered feature list.

## 1. Decision

**Keep Muster's PTY backend, GPUI view, workspace ownership, and terminal lifetime.
Improve correctness and parity before adding visual refinements. Do not replace the
VT engine in the same change as mouse, selection, input, and rendering behavior.**

Ghostty is primarily **Zig**, with a SwiftUI macOS application and GTK Linux
application, not a Rust terminal. Its desktop renderers use Metal and OpenGL,
respectively [G1]. GPUI already owns our rendering platform; importing Ghostty's
renderer is not necessary to learn from its terminal behavior.

The previous report was too confident. Basic drag-copy, same-row ASCII prompt
clicks, and block color inversion exist and have tests. That does **not** establish
full Legacy parity, Unicode correctness, native input composition, or TUI mouse
support. Several existing gaps directly affect the three protected behaviors.

### Corrections to the first evaluation

- Ghostty is not a GTK-only application, and it **does** expose `libghostty-vt` for
  embedding. Dismissing all integration as importing an entire application was wrong.
- Our selection already translates viewport rows into signed grid rows by subtracting
  `display_offset`. It can select visible history after scrolling is connected;
  the viewport clamp does not itself prohibit scrollback selection.
- Alacritty's `SelectionType::Semantic` expands boundaries inside `to_range`; no
  separate manual word-search algorithm is needed. `Lines` follows logical wrapped
  lines, so physical-row versus logical-line semantics must be decided and tested.
- Native click movement is **not equivalent to Legacy**: it rejects wrapped rows and
  counts display columns rather than editable characters.
- Ghostty's per-cell cursor inversion is configurable, not an unconditional default.
  Its default clipboard selection target also depends on platform [G2].
- Unsafe paste protection is security work, not merely a multiline confirmation dialog.
  Bracketed paste does not make arbitrary clipboard bytes safe [G5, G6].
- Dirty-row tracking does not prove that the GPU only paints changed pixels; it lets
  rendering state and shaped data be rebuilt incrementally [G4].
- The previous `scroll_lock` and sixel claims were not established. They are withdrawn;
  neither is a prerequisite of the proposal below.

## 2. Architecture alternatives

| Route | What it actually gives | Costs and risks | Recommendation |
| --- | --- | --- | --- |
| Keep Alacritty VT/grid; improve the adapters | Reuses tested parsing, reflow, selections, and current Rust build; direct fixes in existing modules | Muster remains responsible for input, rendering, protocol replies, and UI integration | Preferred for the demonstrated parity and safety gaps |
| Replace only VT/grid with `libghostty-vt` | C/Zig API for terminal state, effects, input encoders, render snapshots, selection and search [G1, G7] | Zig build/toolchain or pinned native binaries, Rust FFI ownership/lifetimes, API changes, cross-platform packaging and auditing | Technically viable; require an isolated feasibility comparison before deciding |
| Embed a complete Ghostty surface/application integration | More of Ghostty's complete terminal stack | A separate rendering/input integration, native surface ownership, focus/clipboard callbacks and process-ownership questions | Not justified without a concrete requirement the other routes cannot meet |

`libghostty-vt` does not require replacing `portable-pty` or the workspace. Its C API
uses synchronous effects callbacks during VT writes and explicitly warns against
reentrancy and expensive/blocking callbacks [G7]. Replacing the engine would therefore
**not automatically solve** our locking problem, clipboard policy, or GPUI IME integration.
Nor would a VT library alone provide the complete Ghostty application UX.

Upstream states that its VT functionality is available but API signatures are still in
flux, and that libghostty is not yet version-tagged at the reviewed snapshot [G1].
It is MIT-licensed [G8]; copying substantial code requires preserving its notice.
A Rust port is not achieved by copying Zig source into the repository.

### Integration gate, if engine replacement is considered

Before a switch, prove all of the following in an isolated comparison:

1. Build and link the pinned revision on the supported macOS and Linux targets;
   no Ghostty GUI or second PTY owner is needed for the VT-only route.
2. Feed the same split byte streams through the existing backend; preserve query
   replies, shell markers, title/exit metadata, alternate screen and selection.
3. Establish Rust ownership for opaque handles and borrowed buffers. No references
   may outlive an update, callback or freed terminal; callbacks cannot unwind across C.
4. Replay correctness and malicious-input fixtures, then compare visible state and
   actual shell/TUI behavior. Performance comparisons use the same font, viewport,
   workload and build mode, not unrelated terminal application benchmarks.
5. Define source/binary hashes, build inputs, notices, packaging, update policy and
   vulnerability coverage for Zig/C artifacts. `scripts/audit-rust.mjs:29-70` audits
   Cargo packages, not arbitrary native libraries linked by a build script; passing
   that audit alone would not cover the new engine.
6. Account for the documented native-Rust build direction and extra compiler burden.
   Select **one** production engine; no permanent duplicate grid, fallback engine, or
   second parser of ordinary output. Do not reinterpret this research as permission
   to change the project's language/build boundary.

**No FFI spike or engine migration was performed during this review.** Its feasibility,
performance advantage, distribution cost and comparative maintenance effort remain
unproven. Current evidence supports fixing the adapter, not rewriting the engine.

## 3. Protected behavior: selection copies without a key

### Present behavior

`native/src/terminal/view.rs:1097-1225` starts a drag, updates a parser-owned selection,
and copies nonempty selected text on release, including release outside the pane.
`copy_selection` writes the system clipboard (`view.rs:610-619`). A real-window test
checks both selection and automatic copy, not just the core string result.

Keep automatic copy **enabled by default**, with no required modifier. Do not replace
it with Linux PRIMARY-only selection: the user's requirement is system-clipboard
copy, including macOS. Ghostty's reviewed `copy-on-select` supports `none`, `primary`,
`clipboard`, and `both`; its default is PRIMARY on Linux and none elsewhere [G2].
That default is not appropriate to copy into Muster.

### Gaps and smallest complete route

- Double-click currently selects the clicked cell and its two neighbors
  (`view.rs:1184-1191`), not a word. Triple-click has no dedicated state and can become
  a fresh caret-moving single click after the second click clears `last_click`.
- Reuse Alacritty's `Semantic` and `Lines` selections
  (`native/vendor/alacritty_terminal-0.26.0/src/selection.rs:271-324`). They already
  expand words, matching brackets, and wrapped logical lines. Test these semantics
  against Legacy rather than assuming that every "word" boundary is identical.
- Track click count, selection kind and the gesture anchor together so a double-click
  drag remains word-granular and a triple-click drag line-granular. Ghostty does this
  explicitly and resets gestures on screen-generation changes [G3].
- `core.rs:674-682` already subtracts the viewport offset. The missing pieces are
  wheel/navigation wiring and drag auto-scroll, not a new history coordinate system.
- The view currently retains a viewport-coordinate drag origin. If the viewport
  scrolls during a drag, reinterpreting that origin against a new offset moves the
  anchor to different content. Keep the original grid anchor; update only the head.
- Shift currently prevents local selection, and mouse-reporting mode disables it
  altogether. Add an explicit selection override when a TUI owns the mouse, without
  sending the same gesture to both the child and the local selection.
- Restore Legacy's selection-copy preference when live terminal settings are wired;
  disabling copy must not disable highlighting. Do not build a generic preference
  framework just to add word selection.

Copy exactly the selected text, including combining characters, without adding shell
escapes, executing it, or silently replacing codepoints. Ghostty's optional clipboard
codepoint mapping is not a suitable default for copying commands or code.

## 4. Protected behavior: click moves the editable shell cursor

### Present behavior and material divergence

`core.rs:493-514` requires an OSC 133 `B` anchor and the cursor and click on the same
physical row. It returns `column.abs_diff(cursor_column)`. The view obtains another
snapshot to choose direction and queues that many arrow presses (`view.rs:634-653`,
`1194-1199`). This works for the tested simple ASCII prompt.

Legacy's `terminal-click-cursor.ts:163-194,214-291` traverses rows, skips wide-character
spacers, rejects ambiguous blank/tab-fill spans and clicks beyond the editable end,
and refuses the alternate screen. The native implementation does not preserve these
rules. Wide characters can cause too many arrow presses; wrapped commands are refused.

Ghostty also requires semantic prompt information. It either synthesizes cursor keys
or sends an explicitly negotiated shell click event [G2, G5]. Its arrow route follows
soft-wrapped continuations, not arbitrary independent hard-newline input [G9]. It does
not magically recover every shell's logical edit buffer from PTY bytes.

### Required approach

- Recover the established Legacy behavior using parser width/spacer and wrap metadata;
  do not derive editable characters from font advance or column difference.
- Keep shell integration as the authority for the editable region, and verify zsh/ZLE
  and bash/Readline behavior. Do not send prompt arrows to a running TUI or alternate
  screen. OSC 133 is UX metadata from child output, not an authentication boundary.
- Distinguish soft wrapping from hard multiline input and ambiguous tabs/graphemes.
  Support proven mappings; refuse ambiguous mappings rather than editing the wrong
  location. Mark true hard-multiline cases separately from wrapped-command parity.
- Compute direction and movement from **one** state read. The current count and
  direction are obtained separately; intervening output can make them disagree.
- Preserve the prompt's identity over display scrolling and handle resize/reflow
  deliberately. `prompt_anchor()` gates on the saved `display_offset`
  (`core.rs:483-491`), and `resize()` clears the anchor (`399-403`). Merely deleting
  the offset guard is not a complete fix: screen movement can also invalidate a
  recorded row. Tests must cover output scroll, resize and subsequent prompt redraw.
- Preserve input order and queue bounds. N separately queued arrows can be partially
  accepted or separated by a concurrent protocol reply. Calculate a bounded ordered
  movement request using current arrow mode; never retry uncertain partial writes.
- Multi-click/drag gestures must not also move the caret. Prevent stale clicks after
  an alternate-screen switch, release outside, resize or session close.

The existing arrow encoder and shell bootstrap remain reusable. Neither a generic
mouse click-to-position implementation nor an engine replacement is sufficient alone.

## 5. Protected behavior: cursor colors follow the character

`view.rs:806-847,901-918` resolves cell foreground/background, applies inverse,
selection and dim, then swaps the resolved colors for an active focused block cursor.
This preserves the principal requested behavior. Ghostty can express this through
`cursor-color = cell-foreground` and `cursor-text = cell-background` [G2, G10]; do not
assume it is Ghostty's universal default.

### Required distinctions

1. **Occupied cell:** swap its resolved foreground/background, including ANSI/RGB
   colors and inverse styling. Keep the glyph readable and aligned to its grid cell.
2. **Empty cell:** use the normal terminal cursor color as requested. Today the block
   also swaps colors on spaces; `CURSOR_COLOR` is used for outlines/bars/underlines,
   not for a focused empty block. Specify the normal fallback from the current theme,
   and test a styled space versus an unoccupied cell before implementing this rule.
3. **Inactive window and active-but-unfocused terminal:** keep a visible non-active
   cursor where appropriate. Today the block can disappear when the sidebar has focus
   but the window is active: no swap and no outline (`input.rs:59-66`, `view.rs:956-960`).
4. **Hidden cursor, scrolled-offscreen cursor and blink-off phase:** paint neither a
   block nor an inverted glyph. Blink must gate the inversion as well as the overlay.
5. **Bar/underline/hollow:** retain their requested shape; do not automatically force
   Ghostty's shell-integration bar cursor, which would remove the requested block UX.
6. **Wide/combining glyph:** preserve the cell span and all codepoints under the cursor;
   do not break a grapheme while breaking a programming ligature's shaping run.

Blink is not just a periodic repaint. `core.rs:449` drops `CursorBlinkingChange`, and
`CursorSnapshot` omits blinking even though the parser's cursor style carries it.
Expose effective style/blink state, honor DECSCUSR and the selected DEC mode policy,
reset the visible phase on input, and stop scheduling when hidden, unfocused or closed.
Ghostty documents a nullable default versus an explicit blink override [G2].

The current shaping loop already breaks runs at the cursor (`view.rs:747-758`), as
Ghostty's `font-shaping-break` can do. Keep it, but add a rendering-path regression:
existing tests of `compatible_run_end` do not establish the cursor split itself.

## 6. Other high-impact findings

| Area | Observed state in Muster | Consequence / recommendation |
| --- | --- | --- |
| Scrollback | Core supports scrolling; snapshot carries offset; view/session have no navigation path | Add wheel/trackpad with fractional accumulation, platform-appropriate shortcuts, a correctly measured thumb and drag auto-scroll; history navigation must not go to the PTY |
| Mouse reporting | `mouse_tracking` only suppresses local selection; no mouse-event encoder/path is installed | A TUI can request clicks, movement and releases, yet receive none. Implement negotiated formats/modes and the selection override before claiming Neovim/agent mouse parity |
| Alternate scroll and focus | Modes exist in the parser but are not exposed in the six-boolean input snapshot (`modes.rs`) | Wheel handling must distinguish history, mouse reporting and alternate-scroll arrows. Send focus-in/out only when requested; do not report sidebar focus as terminal input |
| Wide characters | Snapshot drops width/spacer flags; view guesses width from font advance (`view.rs:720-725`) | Parser grid is authoritative. Carry width/spacer metadata so fallback fonts cannot consume a neighbor or duplicate a wide glyph |
| Combining text / emoji | Snapshot copies `cell.c` but not `Cell::zerowidth()` (`core.rs:717-731`) | Combining marks and multi-codepoint glyphs can disappear even when parsing/copying preserved them. Carry all stored codepoints and shape them at the correct cell span |
| IME | No GPUI InputHandler; only key-event commits (`input.rs:10-22`) | Full native composition is not proven. Integrate composition/candidate positioning and commit ownership without duplicate characters; verify dead keys, accents and CJK on a real macOS window |
| Dynamic colors | Parser stores overrides; view uses static `Palette`; color queries resolve through xterm_256 | OSC palette/default/cursor-color changes are not faithfully rendered or answered. Resolve query and rendering colors from the same effective state |
| Hyperlinks | Parser cells have link data; snapshots omit it | Carry OSC 8 identity; local detection for URLs/files is separate parity work. Require explicit activation, validated schemes, checkout-aware path resolution and no command execution |
| Search | No native terminal search UI | Useful after reliable history; search logical content, navigate matches and keep history/selection separate. Do not scrape painted pixels |
| Settings | Font and ligatures fixed in the view; Legacy cursor/selection controls absent | Restore live terminal controls with proper measurement/resize invalidation; preserve existing defaults. Settings should not respawn the shell |
| Reflow | Alacritty already handles grid resize; prompt anchor is discarded | Reuse and test resize/reflow, do not write a second reflow engine. Protect selection, wide glyphs and prompt mapping during repeated resizes |

### Responsiveness and locking: fix before adding more event paths

`session.rs:904-923` holds the parser mutex across `core.feed()` **and** `handle_events()`.
The latter invokes host callbacks and `push_reply()` under that same lock
(`1167-1243`). A full queue can make each reply wait up to one second while UI snapshot,
selection and resize require that lock. This is a verified blocking path; actual
frame latency under it has not been measured.

Take events under the lock, then dispatch callbacks/queue work after releasing it.
Keep event order and limits, and define visible failure/cancellation if protocol
replies cannot be delivered. Do not drop queries silently, add unbounded queues,
or split parser locks while retaining callbacks inside them. Reader failure and
session close still need to use the current safe lifetime path.

Ghostty's two-phase render-state update [G4] is a useful model: capture minimal state
under exclusive access, finish expensive processing outside it. Our snapshot already
moves into the paint closure; that move does **not** imply a second deep clone.
But `visible_rows()` allocates/copies the visible grid and `snapshot()` also extracts
selected text under the mutex, followed by repeated shaping during paint.

Profile lock duration, snapshot/selection extraction, shaping, paint time and input
latency before choosing dirty-row caching. Cache invalidation must include scrolling,
resize, palette/font changes, selection, old/new cursor positions, blink and focus.
GPUI may still repaint the canvas even if we reuse shaped rows. No speculative FPS
or memory claims justify replacing the renderer.

### Clipboard and paste: preserve user action, validate child requests

Automatic user selection-copy is trusted UI input. OSC 52 is a request from arbitrary
child output, potentially remote; it must not inherit that permission automatically.

The parser exposes clipboard events, but the real host's `store_clipboard` is a no-op,
and the read request is only recorded (`app.rs::host`, `session.rs:1180-1185`). No
host dialog or user-visible permission flow was found. Refusal is safe for privacy,
but it is not a completed permission implementation. Do not promise it works from
core test names alone. Retain limits and explicit read/write policy, with cancellation
on close and no clipboard content in diagnostic logs.

`paste.rs:41-66` caps input and wraps bracketed pastes, but preserves arbitrary control
bytes inside the payload. A clipboard containing `ESC [ 201 ~` can terminate the
bracketed region early; raw control characters can affect the line editor/process.
Ghostty checks for this delimiter, tests paste safety, and sanitizes dangerous input
bytes before encoding [G5, G6]. Its normal policy can trust multiline *bracketed*
paste, but that is conditional, not blanket approval of every clipboard string.

Define a deliberate sanitize/reject/confirm policy for ordinary text paste and
non-bracketed multiline commands. Validate before changing selection, scroll position
or queue contents; cancel sends zero bytes. Snapshot the negotiated paste mode at
submission, preserve the existing size/queue limits, and leave intentional raw
protocol writes separate from the clipboard paste path.

## 7. What to defer, and why

- **Kitty graphics:** verified upstream API [G7], but not a named current requirement.
  Needs bounded decoding/storage, renderer support, resize/deletion behavior and
  malicious-image testing. A parser recognizing images does not display them in GPUI.
- **Sixel:** not verified by this inspection; do not advertise or make it a dependency
  selection criterion without a concrete requirement and separate source verification.
- **Background images/transparency/custom shaders:** optional aesthetics after color,
  focus and contrast correctness; no changes to the window renderer are proposed.
- **Tabs/splits/window layouts:** retain Muster workspace ownership. This defers copying
  Ghostty's layout stack, not existing Muster layout parity requirements.
- **Font fallback/metrics refinements:** parser width and combining marks come first;
  then verify real bundled/fallback fonts, baseline, box-drawing, bold/italic and scale.
  Do not add a large font-option surface or automatic clipboard substitution.
- **Full keyboard-protocol expansion:** preserve already-tested negotiation and agent
  Shift+Enter. Key-release/repeat reporting is not established by a press-only handler;
  add missing negotiated behaviors with byte-level tests rather than claiming the
  complete Kitty keyboard protocol is implemented.

## 8. Delivery sequence and boundaries

The earlier estimate of "about four places" was not credible for the complete change.
This is multiple coordinated behaviors, not one large patch.

1. **Freeze the contract and add focused failing regressions.** Record Legacy behavior
   for selection, wrapped/wide prompt clicks and cursor colors; retain existing tests.
   This document is a proposed contract, not evidence those new cases already pass.
2. **Safety/correctness foundation.** Move event dispatch out of the parser lock;
   validate paste; preserve width/combining/wrap information in snapshots. Keep parser,
   PTY, bounded queue and close behavior unchanged except for necessary local fixes.
3. **Selection and mouse/scroll routing together.** Introduce semantic/line gestures,
   stable grid anchors, copy on release, TUI reporting/selection override and history
   navigation. Test routing before turning on wheel behavior in alternate-screen apps.
4. **Complete prompt-click parity.** Traverse supported wrapped input, compute an
   ordered movement from one state read, and handle anchor lifecycle safely. Do not
   enable guessed hard-multiline mappings.
5. **Cursor/input/settings parity.** Normal empty-cell color, visible unfocused block,
   blink/protocol state, dynamic colors, live preferences and real native composition.
6. **Safe links and search.** Implement existing link/file parity; add search as an
   independent enhancement. No change to checkout/worktree identity or persistence.
7. **Measure optimization.** Compare baseline and changed builds; add dirty-row shaping
   or snapshot reuse only where the profile establishes a benefit.

Each step should be independently reviewable and verifiable. Stages 2–5 are release
gates for the affected behavior, not optional polish. No release is declared complete
merely because terminal text renders or the existing tests still pass.

**Change surface:** primarily `terminal/core.rs`, `session.rs`, `view.rs`, `modes.rs`,
`input.rs`, `paste.rs`, and focused tests; app callbacks/settings only when required.
No Legacy edits, vendored parser edits, new database format or process revival.
Using existing parser APIs does not require changing the security backport hash.

**Rollback:** retain reviewable commits and a known-good build. Do not persist new
terminal runtime state or change the native workspace shape for this work. A running
session must never be hot-swapped between engines; any later engine comparison/switch
starts separate processes in an isolated environment. Binary rollback does not migrate
live PTYs, and no goal here requires doing so.

### Status after the implementation pass

Stages 1–5 are implemented and covered by tests on this branch. They landed as one
commit because the parser snapshot, session API, input routing and view gestures do
not compile apart from each other; the sequence above is the review order, not the
commit order.

| Stage | Implemented as |
| --- | --- |
| 2 | Events are drained under the parser lock and dispatched after it is released; a reply that cannot be delivered becomes a sticky `poll_error`. Paste strips C0/C1 controls and embedded bracketed-paste markers, and a non-bracketed multiline paste needs an explicit warning dialog before `paste_confirmed`. `CellSnapshot` carries the parser's width, spacer and combining codepoints. |
| 3 | `SelectionKind::{Simple,Word,Line}` is expanded by the parser's own selection; the drag anchor is a grid point that survives viewport scrolling; copy happens on release; TUI mouse reporting is encoded per negotiated mode and Shift overrides it locally; wheel, keyboard and scrollbar drive history scrolling with edge-drag auto-scroll. |
| 4 | `prompt_caret_move` returns an ordered arrow count and direction from one authoritative read, follows soft-wrapped rows, refuses tabs, combining clusters, wide spacers, alternate screens, mouse-tracking apps and hard multiline input. |
| 5 | The block cursor uses the program's color or the theme cursor color on an empty cell, stays visible when the sidebar holds focus, follows `DECSCUSR`/blink state, and the grid paints the program's dynamic background. `ElementInputHandler` owns only the uncommitted composition buffer. |

Stages 6 and 7 are untouched. Live preferences, OSC 8 links, file/URL activation and
search remain proposals, not claims.

## 9. Acceptance matrix

| Group | Minimum verification before shipping |
| --- | --- |
| Auto-copy | Forward/reverse drag, double/triple click, release outside, Unicode, empty selection; clipboard changes only when a completed intended selection exists; Ctrl+C still interrupts |
| Selection/history | Scroll before/during/after drag; continued output; reverse direction; soft wrap vs hard newline; selection older than retained history; resize; no viewport-anchor drift or stale hidden range |
| Mouse ownership | Neovim/agent TUI requested modes, press/move/release/wheel coordinates and modifiers; override selects and copies without a duplicate child event; mode/screen switch during drag resets safely |
| Prompt clicks | Real zsh/bash, left/right, middle of text, wrapped continuations, wide glyph/spacer, combining text, tabs, prompt/output clicks, past end, resize/scroll, active command, alternate screen; no partial or reordered movement |
| Cursor | ANSI/RGB/default/inverse/dim/selected cell, empty/styled space, wide/combining glyph; focused/sidebar-focused/inactive/hidden; blink phase and reset; DECSCUSR; ligature split without grapheme split |
| Native text | Dead keys, Option/layout input, accented and CJK composition with candidates, cancel/commit, no duplicate bytes; real OS input as well as unit tests |
| Paste/security | Bracketed end-marker injection, controls, CR/LF, non-bracketed multiline, oversize, confirmation/cancel, full/stopped queue and session close; no payload logging |
| Colors/links | Dynamic color change/query/reset consistency; OSC 8 and detected file/URL activation; invalid schemes, misleading label and checkout-relative paths never execute code |
| Lifecycle | Output flood/full input queue plus queries, resize, selection and close; bounded latency and memory; no blocked UI callback under parser lock, leaked process or lost failure signal |
| Performance | Same hardware/font/build and 80x24 plus 300x100 workloads, steady output, history/selection and idle/hidden terminals; measure p95 input/frame/lock duration and allocations rather than promise an unmeasured speedup |

Run focused regressions per stage, then the project's pinned-toolchain workspace tests,
locked clippy and format checks. Actual macOS input/rendering and Linux TUI behavior
need real-window validation; headless shaping with a mock text system is not enough.

## 10. Verification performed and limits

Executed against the working tree of this implementation pass:

```sh
rustup run 1.97.1 cargo test --locked --workspace
# 70 passed (muster-core), 163 passed (muster-native), 0 failed
rustup run 1.97.1 cargo test --locked -p muster-native terminal:: -- --test-threads=1
# 92 passed, 0 failed
rustup run 1.97.1 cargo clippy --locked --workspace --all-targets -- -D warnings
# clean
rustup run 1.97.1 cargo fmt --all --check
# clean
```

The terminal tests are real processes under a real PTY: the prompt-click cases drive
`/bin/sh` with an OSC 133 marker and read what the child answered, and the
mouse-reporting case asserts the bytes `dd` received. The IME case drives the view's
`EntityInputHandler` directly, and composition offsets are checked against UTF-16
surrogate pairs. Cargo reports a pre-existing future-incompatibility warning for
`block v0.1.6`; it was not changed.

This covers the stages above in headless tests. It does **not** validate the rest of
the acceptance matrix: real macOS keyboard/IME sessions, real GPU rendering, Linux
TUI behavior, and measured lock/paint latency under a real flood remain unverified.
OSC 8 links, file activation and search are still unimplemented rather than untested.

No new engine was compiled, no Ghostty application was launched, and no GPU benchmark
was run.

## 11. Pinned upstream evidence

- **G1 — architecture, embedding and API maturity:** [README](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/README.md#L111-L170).
- **G2 — cursor, click and clipboard configuration:** [Config cursor options](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/config/Config.zig#L846-L924), [paste protection](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/config/Config.zig#L2446-L2502), [copy targets/default](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/config/Config.zig#L2524-L2546).
- **G3 — selection gesture behavior:** [SelectionGesture](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/terminal/SelectionGesture.zig#L21-L49), [default modes](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/terminal/SelectionGesture.zig#L134-L152), [click-count progression](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/terminal/SelectionGesture.zig#L264-L294).
- **G4 — render snapshot, short lock and dirty state:** [render.h](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/include/ghostty/vt/render.h#L22-L74), [generic renderer dirty state](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/renderer/generic.zig#L1550-L1628).
- **G5 — prompt click and paste dispatch:** [Surface prompt click](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/Surface.zig#L4268-L4379), [paste safety](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/Surface.zig#L6241-L6269).
- **G6 — paste sanitization and encoding:** [input/paste.zig](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/input/paste.zig#L40-L129).
- **G7 — embeddable API and build:** [vt.h](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/include/ghostty/vt.h), [effects callbacks](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/include/ghostty/vt/terminal.h#L64-L109), [shared/static builds](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/build.zig#L128-L168), [Kitty graphics header](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/include/ghostty/vt/kitty_graphics.h).
- **G8 — license:** [MIT notice](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/LICENSE).
- **G9 — wrapped prompt movement and limitations:** [Screen.promptClickMove](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/terminal/Screen.zig#L3547-L3679), [soft-wrap regression](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/terminal/Screen.zig#L12103-L12139).
- **G10 — actual cursor-color resolution:** [generic renderer](https://github.com/ghostty-org/ghostty/blob/adbcb3da673407ab79254620536c909b23e32419/src/renderer/generic.zig#L2757-L2852).

Source was retrieved read-only through GitHub's API at the pinned commit. Relevant
implementation regions were read directly; the first report's broad automated
"facts" extraction is not used as evidence for this revision. Unverified capabilities
are explicitly deferred rather than presented as facts.
