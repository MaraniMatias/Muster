# Terminal-first native Rust implementation specification

**Parent report:** [Migration feasibility and strategy](README.md).  
**Status:** historical specification; a native executable now exists at the workspace root.
Terminal/session correctness and workspace navigation have automated coverage.
Live preferences, safe link/file activation and terminal search remain deferred;
real OS input, GPU rendering and Linux TUI acceptance are not established by
headless tests. See [current status and measurements](ghostty-reference.md#11-chrome-pass-theme-state-writer-refusals-and-the-read-cadence).
**Primary outcome:** a native GPUI Kit window with one real, safe, usable terminal.  
**Baseline:** Muster v0.21.1 at `4412f63bd385bdb3b9eb12b829858578a2009f81`.

## 1. Separate the first proof from the usable terminal

The first implementation slice is the terminal MVP, delivered in small runnable increments with two checkpoints. This prevents a polished-looking prompt from being confused with a functioning terminal. The user performs real application tests and supplies feedback; the implementer owns automated checks, fixes and an explicit pending/pass/fail record. M0 is an early feedback checkpoint, not permission to begin the workspace UI rewrite.

### M0 — architecture smoke test

Required:

- One native Kit window with correctly rendered text.
- One explicitly selected local shell and working directory.
- The existing Muster PTY backend, not a second PTY implementation.
- A native VT grid that can display ANSI colors and cursor-addressed output.
- Keyboard round trip, Ctrl+C, window resize and explicit backend shutdown.
- No access to Muster's production SQLite database.
- No Vue, webview, Tauri host or JavaScript terminal renderer in the native executable.

M0 may use the system monospace face and a deliberately small terminal view. It is not a release and is not the complete terminal acceptance gate. Do not run valuable unattended work in an experimental shell.

### M1 — useful terminal-only application

The approved MVP is a general-purpose terminal on the user's Apple M1 Pro/macOS 27.0.1 arm64, not a demo limited to a specific agent. It must run zsh, sh, Git, Neovim and arbitrary installed terminal commands through the real shell/PTY, without a command allowlist. Existing Muster terminal functionality is the minimum acceptance baseline. Additional features are optional, not justification to delay parity. M0 remains a deliberately incomplete feedback checkpoint.

Add:

- Correct alternate-screen/TUI behavior, scrollback and selection.
- Native text input and IME without duplicate characters.
- A serialized bounded write queue and responsive resize/status operations.
- Clipboard copy/paste, bracketed paste, OSC 52 store policy.
- Correct modified-key negotiation, especially agent Shift+Enter.
- Foreground-process close confirmation, fresh state reads and safe cancel.
- Current shell integration, command exit status and sanitized title reporting.
- Bundled native fonts and predictable cell sizing.
- Parser malicious-input regression checks and source-audit resolution.
- Actual validation with the intended shell, Neovim and an installed coding-agent TUI.

The window can expose a minimal terminal name/status/error surface. Complete standalone terminal parity includes prompt click-to-move, URL/file recognition, safe explicit link opening and live terminal settings, in addition to the checks below. Expose only the controls needed to exercise existing terminal preferences; the full settings dialog is not required. Checkout movement, session navigation and opening documents inside Muster depend on the later workspace UI; record those as deferred integrations rather than pretending they exist in the MVP. The complete sidebar, file browser and review system are not required to judge this milestone.

## 2. Existing terminal execution path

### 2.1 Production spawn today

```text
TerminalSession.vue
  → lib/ipc.ts createTerminal
  → commands/terminal.rs terminal_create
      validate_dimensions
      create OutputGate + output-flow listener
      read config path and shell-integration preference
      construct OutputSink sending Channel<Response>
  → services/terminal.rs create_with_settings
      resolve registered checkout path
      validate optional prompt
      choose user's login shell
      optionally generate shell-integration bootstrap
      create stable session ID
  → terminal/mod.rs TerminalBackend::spawn
      open PTY, set TERM environment, spawn child
      start dedicated reader thread
  → database.add_terminal_session
      on failure: close spawned PTY/child
  → return session + updated workspace
```

This ordering matters. Output can arrive before the frontend receives the session ID. A failure to register the session after spawning must not leave an invisible shell running. The existing service attempts rollback but discards the result of `backend.close` (`services/terminal.rs:147`); this requirement is not fully guaranteed today. Native service integration must surface cleanup failure, retain the exact failed session for bounded retry and test the failed-registration/failed-close path.

### 2.2 Output today

```text
PTY reader: raw 64 KiB maximum chunks
  → OutputSink holds OutputGate
  → delivered byte total incremented before channel send
  → Tauri binary channel
  → frontend keyboard-protocol observer
  → coalescing xterm output writer
  → xterm parsing completion callback
  → cumulative parsed count
  → Tauri terminal-output-flow event
  → OutputGate acknowledgement
```

The control values are:

| Value | Existing behavior |
| --- | --- |
| Reader buffer | 64 KiB. |
| Gate high mark | 2 MiB. |
| Gate low mark | 512 KiB. |
| Frontend report step | 256 KiB. |
| Gate acknowledgement timeout | 5 seconds, then fail open. |
| Backend maximum input | 1 MiB per write. |
| Frontend queued input | 1 MiB total encoded bytes and 128 requests; native queue retains both bounds. |
| Backend nonblocking-write deadline | 1 second. |
| Frontend close drain deadline | 1 second over input then resize queues. |

The output gate is **not a strict permanent memory cap**: source explicitly fails open if a consumer never acknowledges. This is important when designing a new native queue. Reusing the gate plus an unbounded queue does not prove bounded memory.

### 2.3 Input and close today

`TerminalSession.vue` serializes input through a promise queue. It accounts for UTF-8 bytes and request count, rejects over-limit input, snapshots checkout/session/generation, and discards subsequent queued writes after failure instead of retrying uncertain bytes.

`TerminalBackend::write` also validates the payload, avoids indefinitely waiting on a busy writer and uses nonblocking Unix writes with a deadline. The backend's refusal to accept concurrent writes means the native UI must still serialize them.

Close reads current status rather than trusting the periodic poll. If that read fails, close fails safely; it does not assume no process is running. A busy foreground process leads to a confirmation; cancel leaves the PTY untouched. Accepted close stops new work, drains within a deadline, and calls the service/backend close.

### 2.4 Status today

The backend reports shell/process exit state, foreground process/application and shell working directory from the OS. The frontend additionally records title and OSC 133 last-command status.

Do not merge these concepts:

- A shell exiting with code 1.
- A command returning 1 while its shell remains alive.
- A foreground TUI currently running.
- An unverified/ambiguous agent session title.

The terminal view's state must preserve those distinctions for the later sidebar port.

## 3. Reuse boundary and implementation organization

### 3.1 Keep the backend

Preserve the algorithms in:

- `src-tauri/src/terminal/mod.rs`.
- `src-tauri/src/terminal/process.rs`.
- `src-tauri/src/services/terminal.rs`.

Do not replace them with `alacritty_terminal`'s PTY/event-loop machinery or tty7's daemon merely because those projects can also launch shells.

Alacritty is selected for parsing/grid behavior. `portable-pty` remains selected for process execution and lifetime.

### 3.2 M0 should not force a database extraction

For M0 call `TerminalBackend::spawn` with explicit `SpawnOptions`; the backend accepts program, args, cwd, cols, rows and environment, independent of persistence. Use a small source-module reuse boundary or a narrowly extracted existing module. Avoid copying the terminal code into a second implementation.

Because current modules are rooted in the Tauri binary, wiring them into a native executable requires a real module boundary. That is a future code change, not an existing importable `muster_core` API.

M0 must not casually call private shell-integration helpers through imagined public APIs. Raw spawn with an explicit login-shell command is enough for the smoke test. M1 should reuse the real service path to avoid reproducing shell-startup behavior incorrectly.

### 3.3 M1 service integration

When adopting `create_with_settings`, use an isolated database and registered checkout. Existing mechanisms include `Database::register_plain_repo` and `Repo::plain` for a plain-folder test fixture, or `services::workspace::register_folder` for the actual folder-open workflow. The latter also traverses Git/service dependencies; extract its real graph rather than pretending it is a standalone helper.

Reuse config loading and set an isolated shell-integration script directory in tests. Production adoption of `~/.muster/config.yml` comes only after the prototype has a deliberate startup/data policy.

No schema change is needed simply to create one terminal for an already-registered checkout. Do not introduce a new “native session” table or persist a GPUI entity ID.

### 3.4 Candidate view-source choices

| Route | Advantages | Concrete cost | Recommendation |
| --- | --- | --- | --- |
| Embed the whole tty7 app | Complete native terminal behavior exists | Daemon ownership, remote stack, forks, incompatible UX/data model | Reject for this migration. |
| Depend unchanged on gpui-terminal 0.1.0 | Small API, generic streams | Wrong GPUI family; selection/scroll TODOs; second reader owner | Not a complete solution. |
| Adapt compact terminal source for M0 | Small compile/render example | Framework alias/API adaptation; sink-driven feed change | Reasonable smoke-test option. |
| Extract tty7 terminal view wholesale | Mature behaviors | Many application/daemon/config/action dependencies | Too broad without a measured extraction spike. |
| Native Muster view using selected renderer/input source | Fits current lifetime/UX; no daemon | Own integration, adaptation and tests | Recommended target. |

Start by compiling the smallest useful rendering/input seam against Kit. Measure that seam before deciding which renderer source to adapt. Use tty7's mature behavior as the main reference for M1, not as an excuse to import its entire application.

### 3.5 Dependency-selection gate

Before drawing a terminal:

1. Pick a published Kit release and its matching default `gpui-pre-*` set.
2. Use the same GPUI re-exports for all application and terminal code.
3. Avoid direct registry `gpui 0.2.2` alongside Kit's snapshot family.
4. Inspect the exact parser source for the keyboard-stack bug.
5. Prefer a corrected registry parser if available; otherwise use a reviewed explicit patch/revision with an audit plan.
6. Commit a lockfile during implementation and build using Muster's pinned compiler.
7. Check platform features, including macOS text rendering.

Do not enable `gpui-fast` in the first spike. It is an extra engine-selection dimension, not a requirement for full Rust. Compare it only if a measured problem later justifies that experiment.

## 4. Recommended output concurrency model

### 4.1 Worker-owned parsing

Recommended topology:

```text
Muster PTY reader thread
  owns: ordered byte reads + VT Processor + incremental protocol observation
  calls: OutputSink
     ↓ short shared-state mutation
Arc<lock<Term/grid + compact terminal metadata>>
     ↓ coalesced dirty/wakeup signal
GPUI foreground receiver / retained terminal entity
     ↓ snapshot visible cells, selection and cursor
native TerminalElement painting
```

No separate raw-byte output queue is necessary if the sink parses synchronously on the existing reader thread. A slow parser naturally stops that reader from draining the PTY, so the kernel applies backpressure to the child.

This is smaller than duplicating the current webview pipeline in Rust. It can also preserve input responsiveness because parsing is not performed in the GPUI UI loop.

### 4.2 Locking rules

- Only one parser mutates terminal VT state for a session.
- Protect shared grid/modes with a lock appropriate to the selected parser.
- Split large batches into measured bounded parsing sections if holding the lock for a whole 64 KiB read delays layout/selection.
- UI takes a snapshot of **visible** cells and necessary cursor/selection state, not a clone of all scrollback on every frame.
- Release the grid lock before text shaping, drawing, clipboard calls, callbacks, PTY writes or blocking operations.
- Input reads current terminal modes in a brief lock section, then releases it before enqueueing bytes.
- Resize and selection mutations use the same synchronization rules.

Avoid locking the session registry, PTY master and terminal grid simultaneously. Preserve a simple lock order and test close while output/write/resize are active.

### 4.3 UI invalidation

Parse every byte, but repaint only when needed:

- Set one dirty flag/generation after state changes.
- Wake a foreground task at most while a wake is outstanding.
- Drain/coalesce repeated repaint notifications.
- Update the terminal entity on the UI thread.
- Keep parsing hidden terminals, but avoid dirtying the window for every hidden-output chunk.
- On becoming visible, notify and render current state without replaying the output history.

A queue containing one “dirty” signal is sufficient for repaint invalidation. Titles, exit/error events and clipboard requests need explicit semantics rather than being accidentally overwritten by that flag.

### 4.4 Parser events

Alacritty events may request:

- PTY protocol responses.
- Title changes.
- Bell.
- Clipboard store/load.
- Color/size answers.
- Wakeup/invalidation.

The parser's event listener must not synchronously write to the PTY or invoke GPUI while holding the grid lock. Enqueue typed events or update bounded/coalesced metadata; a foreground/controller path performs the UI work and the serialized writer performs the byte response.

Bound or coalesce event classes appropriately. Repaint/title updates may be coalesced; a protocol response cannot silently be lost because a queue is full. On sustained malicious request floods, terminate/report the offending session or apply an explicit bounded policy instead of unbounded allocation. Do not log clipboard contents, shell output or protocol payloads as diagnostics.

### 4.5 Failure and reader lifetime

The current backend drops the reader's `JoinHandle` and `read_output` returns without an explicit EOF/error notification. Source reuse alone therefore does not provide the required stream/failure observation. Add a small reader-lifecycle reporting mechanism when binding the native sink: distinguish normal EOF, read failure and unexpected worker termination, and deliver a terminal completion/failure signal independently of child status. Detect panic/early termination through an owned completion mechanism; do not conceal it as ordinary EOF. Keep the existing safe process-termination algorithms and test reader failure with a still-running child.

Muster's reader deliberately continues draining after sink errors. A native sink error therefore needs a visible terminal error/close policy; merely returning an error and leaving a apparently-live frozen grid is not a usable result.

A parser worker panic can terminate delivery. Prevent known input-triggered panics through corrected dependencies and regression tests, and detect worker/session failure. Do not use `catch_unwind` as a substitute for correcting a known parser corruption defect.

At accepted close, prevent new input, protocol replies and clipboard/UI side effects; disconnect/cancel queued consumers so delivery cannot stall shutdown. Continue parsing tail bytes while the closing controller remains alive, but do not wait indefinitely for reader EOF. Observe stream completion separately from backend close; after completion or an explicit bounded tail-drain deadline, detach the view and allow any late reader delivery to discard bytes without retaining a GPUI entity. Natural child exit keeps received tail output readable. Record a timed-out tail drain rather than claim all output was preserved. No worker may block forever sending to an entity that has disappeared.

### 4.6 Alternative: UI-side parser with OutputGate

If the chosen view fundamentally requires parsing on the UI thread, use a bounded delivery path and the existing gate:

1. One gate per session.
2. `hold()` before handoff.
3. Count `delivered(bytes.len())` before the bytes become visible to the consumer.
4. Parse chunks in order on the owned consumer.
5. Acknowledge cumulative parsed bytes after parser completion, not after receipt or frame paint.
6. Release the gate and disconnect the receiver on consumer teardown or failed delivery.
7. Preserve monotonic/clamped acknowledgements.

The five-second fail-open means the gate alone cannot bound an unbounded transport. Add a cancellation-aware bounded transport if this architecture is used. A synchronous blocking sender must be woken when the receiver is destroyed; a UI task must not be awaited while close depends on that same task running.

Do not keep an acknowledgement system in the preferred worker-parsing architecture merely to preserve old IPC mechanics. Preserve the invariant, not redundant machinery. Remove obsolete gate code from the final native-only backend only once the old async delivery path is gone and replacement coverage exists.

## 5. Native terminal state and ownership

### 5.1 Minimum retained state

A terminal session/controller needs, conceptually:

- Stable backend session ID and current owning checkout ID.
- Terminal parser/grid state and parsed-output generation.
- Input queue state, byte/request accounting and cancellation generation.
- Current size and latest pending resize.
- Focus handle and native input-method handler.
- Selection, scrollback display offset and visible-snapshot state.
- Sanitized terminal title, shell marker state and last command exit.
- Current OS process/cwd status and refresh generation.
- Owned UI tasks/subscriptions and error/close state.

These are conceptual responsibilities, not a proposal to introduce a public trait or separate struct for each bullet. Begin with one retained session entity/controller and split only where ownership or concurrency requires it.

### 5.2 Lifecycle states

| State | Allowed operations | Transition |
| --- | --- | --- |
| Starting | Bind sink, measure size, show loading | Running after successful spawn/registration; failed otherwise. |
| Running | Input, resize, parse, selection, status | Exit, close-request or error. |
| Asking to close | Selection/output may continue; destructive action paused | Cancel → running; confirm → closing. |
| Closing | No new input/resize; bounded drain and shutdown | Closed or surfaced close failure. |
| Exited | Read/copy scrollback; no process input | Close/remove; optional new terminal action later. |
| Failed | Visible error, controlled cleanup; cleanup failure retains exact session ownership for retry | Closed only after successful cleanup, or explicit retry with a new session generation after cleanup. |
| Closed | No callbacks or queues retain it | Registry removal. |

Child exit and output EOF are not always simultaneous. Keep trailing output visible; do not discard queued tail bytes just because a status poll first saw exit. In the worker-parsed path, record parser/stream completion separately from child status.

### 5.3 Spawn races

Create parser/grid, event delivery and lifetime ownership before calling backend spawn. The reader can emit output immediately.

If the view is closed before spawn finishes:

- Mark its generation cancelled.
- If spawn later succeeds, close that exact returned backend session.
- Do not insert it into a newer view.
- Roll back any registered session record through the service path only after successful backend cleanup. Surface close/removal failures and retain enough ownership to retry; do not discard cleanup errors.

A session ID assigned before spawn can be used internally for ownership, but do not announce success before process and registration steps actually succeed.

### 5.4 Hide/move is not close

Later phases will hide a terminal behind a document or move it to another checkout. Neither should drop the grid or kill/respawn its shell.

Keep the session entity indexed by backend ID. The visible layout references it. Moving updates current checkout ownership and link/status context, not PID, reader, parser or input stream.

GPUI entity lifetime must not be driven solely by whether the terminal element is included in the current visible render tree.

## 6. Input, keyboard and IME

### 6.1 One ordered input path

Every byte sent to a session should pass through the same serialized controller path:

- Native printable text.
- Native functional/control/meta keys.
- IME committed text.
- Clipboard paste.
- Terminal-generated replies.
- Prompt click-to-move arrow sequences.
- A guarded manual `cd` after a session move.

Preserve backend payload limits, and account for queue size in **bytes**, not Rust characters or JavaScript string length. Retain the 1 MiB queued-byte total and 128-request cap, including protocol replies. Capture checkout/session/input generation on enqueue. Reject writes after closing or reparenting that would target stale ownership. For registered sessions, preserve the current database-backed `validate_session_owner` and dimension checks in the Tauri-free service boundary before removing command adapters; UI generation checks complement rather than replace that authority. Update/invalidate pending operations when a move commits.

Backend write timeouts can mean a prefix was written. Do not retry the entire payload automatically: that can repeat commands. Surface a partial/uncertain-delivery error and cancel subsequent queued input, as the current frontend does.

### 6.2 Keyboard-mode authority

Use the parser's current mode flags where it implements the negotiated protocol. Do not keep a second independent keyboard-mode parser unless the chosen parser genuinely cannot supply those modes.

Alacritty's grid exposes kitty-related flags and protocol responses, but the application still needs to encode native GPUI key events. Parser support is not full keyboard support.

Required modes/behaviors include:

- Normal/application cursor keys.
- Functional keys used by readline, Neovim and agents.
- Control combinations and Meta/Option behavior.
- Negotiated kitty/CSI-u flags, push/pop/query behavior.
- Alternate screen mode transitions.
- Bracketed paste and focus reporting where requested.

### 6.3 Shift+Enter compatibility

Current Muster behavior is concrete:

- Plain Enter follows the terminal's ordinary Return behavior.
- Shift+Enter without Ctrl/Alt/Meta sends `ESC [ 13 ; 2 u` when the child negotiated disambiguation.
- Without that negotiation, it sends LF (`\n`), not a literal CSI-u escape and not a second carriage return.
- Querying support is not the same as agreeing to use that mode.
- Output announcements can cross read-chunk boundaries.
- Native keydown and text/IME paths must not send the same press twice.

Keep this behavior before importing a broad upstream keymap. tty7 has a useful native encoder and tests, but its composer/newline actions are not automatically Muster's action policy.

### 6.4 Platform shortcut ownership

- On macOS, Cmd is the app modifier; Ctrl belongs to the terminal unless an explicit terminal command claims it.
- Ctrl+C must reach the PTY when appropriate, not become a global copy handler.
- On Linux, global Ctrl shortcuts must not steal established terminal/TUI controls beyond current product bindings.
- Menus and focused text widgets must receive native edit actions correctly.
- Modal dialogs temporarily own their own keyboard focus; closing restores the previous meaningful target.

Follow `src/domain/shortcuts.ts` and current handlers. Do not adopt tty7's entire shortcut set simply because its encoder is reused.

### 6.5 IME and composed text

Provide a native input handler with marked/preedit text, commit, cancellation and caret bounds. Do not model international text as key-to-ASCII mappings.

Required fixtures:

- Spanish accents, ñ and dead-key combinations.
- Emoji and combining marks.
- Chinese/Japanese/Korean composition in the shell and intended TUI.
- Composition cancelled by Escape.
- Focus moved while preedit is active.
- Option-as-Meta on macOS with both Latin and CJK input sources.

tty7's GPUI fork changes `prefers_ime_for_printable_keys` to inspect the keystroke for Option-as-Meta. Check whether the selected Kit snapshot already supports the needed behavior. Do not blindly port a private fork method that the current framework does not expose.

## 7. Clipboard, selection and mouse

### 7.1 Ordinary copy/paste

Required for M1:

- Drag selection, double-click word and triple-click line selection.
- Copy selection without terminal color/control sequences.
- Copy/paste using native menu actions and appropriate platform shortcuts.
- `selectionCopy` preference: changing it stops automatic copy, not selection itself.
- Unicode selected text and wrapped rows.
- Explicit behavior for selection after output reflow or scrollback eviction.

Selecting text is a grid operation, not an editing operation on the child's program. Do not place a text editor over the terminal and simulate output.

### 7.2 Mouse reporting vs forced selection

If a TUI enabled mouse tracking, encode native pointer events in the requested protocol and send them to the PTY. Do not also create a local selection for the same gesture.

Preserve forced selection on macOS Option-drag when application mouse tracking is active. At the ordinary shell, modified selection behavior must not unexpectedly change.

Test:

- TUI click/drag.
- Selection in ordinary shell output.
- Forced copy from Neovim/agent TUI.
- Pointer release outside the view.
- Selection after resizing and scrolling.

### 7.3 Paste safety

Read clipboard text only for explicit user paste, subject to size limits. If bracketed paste is enabled by the child, wrap pasted text in the corresponding start/end sequences; do not send markers to a child that did not request them.

Reject oversized paste visibly before enqueueing. Do not truncate a pasted command silently. Test multiline text, CR/LF normalization and embedded control sequences against intended current behavior.

### 7.4 OSC 52

Muster currently loads xterm's ClipboardAddon so TUIs can copy using OSC 52. Carry that supported store behavior forward.

For native integration:

- Decode and validate the payload with explicit size/resource limits.
- Execute clipboard calls on the appropriate host thread.
- Do not interpret arbitrary OSC as instructions to run a program.
- Do not enable program-initiated clipboard **reads** automatically merely because the parser can emit `ClipboardLoad`.
- If read/query support is considered necessary, define and verify its permission policy separately.

A write-only initial policy avoids letting any terminal program exfiltrate clipboard contents. Confirm current behavior before expanding permissions.

## 8. Shell markers, titles and prompt clicks

### 8.1 Keep shell bootstrap behavior

`services/terminal.rs` chooses `$SHELL` or a Unix fallback and opens a login shell. zsh/bash integration is deliberately careful:

- zsh forwards startup files through a private `ZDOTDIR` setup.
- bash preserves login startup/banner/error behavior using a controlled bootstrap.
- Unsupported/POSIX-sensitive cases can remain uninstrumented.
- Script writes are idempotent and private (0600).
- Integration failure must not prevent a usable ordinary shell.

Do not install permanent changes into the user's `.zshrc`/`.bashrc`. Do not use tty7's broad shell integrations to replace the current semantics without a separate need.

### 8.2 OSC 133 semantics

Current emitted/consumed subset:

| Marker | Meaning | Native state update |
| --- | --- | --- |
| `B` | Prompt has ended; editable shell input begins | Record parser-position prompt anchor. |
| `C` | Command starts | Clear last failure and invalidate editable prompt. |
| `D;<decimal exit>` | Command finished | Record last command exit without confusing shell exit. |

The shell does not emit the full A/B lifecycle. Do not require an `A` marker that the current scripts never produce.

Use a proper incremental escape parser/tokenizer, not a regex over independently decoded UTF-8 chunks. Preserve BEL and ST termination semantics and chunk splitting.

### 8.3 Parser-position observation is the difficult part

A status-only OSC observer can run beside the grid parser. Prompt click positioning requires more: the location at the exact instant `B` is interpreted.

Do not scan a whole chunk for markers and then read the grid cursor after parsing the whole chunk. Later bytes can have moved the cursor, producing a wrong editable region.

Implementation options to verify during M0/M1:

1. A parser extension/handler hook that observes OSC 133 at the exact VT position and delegates all other behavior.
2. A small incremental tokenizer that splits delivery at OSC 133 boundaries, advances the real parser to each boundary, and records the grid cursor there.
3. A reviewed upstream hook if the selected parser already provides it.

Prefer an existing parser capability; do not duplicate the entire ANSI Handler implementation without first inspecting its extension points. The exact hook API is not established by this research.

### 8.4 Prompt click-to-move parity

This feature can follow basic shell usability in an incremental delivery, but it is required before declaring the M1 terminal MVP complete:

- Unmodified plain click within the current editable prompt moves the shell's caret by sending arrow-key bytes.
- No action for output, prompt prefix, alternate screen, active application mouse tracking or unknown prompt position.
- Double click remains word selection.
- A drag that returns to its start remains a drag.
- Modified click keeps link/selection semantics.
- Prompt anchors follow scrolling/reflow, or are invalidated safely when that cannot be proven.
- `C` immediately invalidates the anchor.

Do not replace the shell's editor with tty7's command composer to obtain this behavior. Muster currently edits through the shell; preserving that distinction avoids changing readline/zle semantics.

### 8.5 Titles

Current title handling removes control characters, trims, caps the displayed title to 80 units in the existing frontend, and uses null for an empty result. Native code must preserve bounded, harmless text and test Unicode truncation; choose the documented equivalent rather than splitting a UTF-8 code unit.

An OSC title is useful display metadata, not authoritative proof of process identity or agent ownership. Later sidebar agent matching requires current OS foreground detection plus exactly one title match.

## 9. Rendering, fonts and sizing

### 9.1 Minimum rendering contract

- ANSI 16 colors, indexed 256 colors and true color.
- Per-cell foreground/background, inverse, dim, bold, italic, underline and strikeout as supported by the baseline.
- Cursor shape and blink; unfocused cursor outline.
- Correct blank/erased cell backgrounds.
- Alternate grid and primary grid transitions.
- Wide characters, continuation cells and combining marks.
- Box drawing, powerline/Nerd Symbols and whitespace.
- Selection and scrollback offsets.

Use the parser's grid representation rather than reconstructing a text buffer from output strings.

### 9.2 Cell geometry

Let measured native cell width be `cw`, line height `lh`, and usable content size after padding be `w × h`:

```text
cols = floor(w / cw)
rows = floor(h / lh)
```

Clamp to the existing backend's accepted nonzero dimensions. Never send zero columns/rows from an invisible or minimized view. Width/height calculations must use consistent GPUI logical units; OS pixel scale is a separate quantity.

Current terminal settings are font size 16, line-height multiplier 1.2 and 10,000 scrollback lines. Start with these behavior targets, not an unrelated tty7 preset.

### 9.3 Zoom and monitor scale

The web implementation cancels CSS zoom for its canvas and applies scaled font size. That DOM workaround does not transfer literally to GPUI.

Native design:

- Treat app zoom and OS scale separately.
- Derive terminal font size from user preference and app zoom.
- Measure native cells after fonts are available.
- Recompute rows/cols on zoom, font or display-scale change.
- Do not stretch a previously rendered terminal bitmap to simulate a font change.

Test Retina/non-Retina transitions and Linux fractional-scale sessions. Changing zoom or font must not respawn the PTY or clear scrollback.

### 9.4 Bundled font plan

- M0 may use a known installed monospace face.
- M1 bundles licensed native-loadable Fira Code regular/bold and Nerd Symbols equivalents.
- Register faces before first measurement.
- Use internal family names and verified fallback, not the CSS family string literally.
- Keep font license files and update artifact hashes/notices.
- Verify CJK/emoji fallback independently; the icon font is not a complete Unicode fallback.

The current WOFF2 assets are a browser delivery format. Native TTF/OTF equivalents or a verified conversion are an implementation task, not already-proven raw byte reuse.

### 9.5 Ligatures and Unicode width

Ligatures must preserve fixed cell budgets and caret/hit-test coordinates. Do not enable normal proportional text shaping over an entire row and assume it remains terminal-correct.

tty7's rendering source documents disabling `calt`, `liga` and `clig` when appropriate. Study this behavior; preserve Muster's live ligature preference without turning the terminal into a variable-width text surface.

The baseline xterm uses Unicode 11 widths; a newer Alacritty width table may differ. Correct TUI alignment matters more than copying a version label. Include neutral-width emoji with variation selectors, CJK, ZWJ sequences and combining marks in fixtures. tty7's parser fork includes an emoji-width fix that must be evaluated separately from its keyboard fix.

## 10. Resize and status scheduling

### 10.1 Resize

- Coalesce to the newest measured size.
- Avoid duplicate backend calls for unchanged dimensions.
- Perform PTY resize off the UI thread.
- Update grid and PTY consistently; if one side fails, report/reconcile instead of leaving silent permanent disagreement.
- Never resize after close begins or from a stale session generation.
- Do not queue an entire history of pointer-move sizes.

Interactive shells normally redraw after resize. A failure must not reset the session or drop output. Test resizing while output, paste and a TUI are active.

### 10.2 Status

Reuse OS foreground/cwd introspection. Source today adjusts polling between visible/active and background conditions; avoid one fast timer per hidden session in the final app.

M1 can poll one terminal with an owned task. Later:

- Use a fast visible cadence and slower hidden cadence matching the existing policy.
- Do not overlap status requests for a session.
- Ignore results for closed/moved generations.
- Before destructive close or guarded `cd`, perform a fresh read.
- On status-read failure, keep the distinction between unavailable and idle.

## 11. Safe close, quit and crash behavior

### 11.1 Session close sequence

1. Mark close in progress and reject new input/resize.
2. Read fresh backend status.
3. If read fails, display the failure and leave the session alive.
4. If foreground work exists, show a destructive confirmation naming the program/session.
5. Cancel restores normal interaction and does not signal anything.
6. Confirm drains input/resize only within the documented bound.
7. Invalidate pending input generations when drain times out.
8. Release output consumers/notifications so reader delivery cannot stall cleanup.
9. Call the existing service/backend close on a worker.
10. Remove the registry entry and persistence row only according to actual operation success.

The backend already implements safe group termination and reaping. Do not replace it with “drop the master handle and hope” or `kill(-pid)`.

### 11.2 Application exit sequence

Native window close is cancellable. Application Quit must use the same coordinated controller or a verified equivalent:

- Confirm foreground activity before stopping it.
- Flush relevant state and bounded terminal work.
- Explicitly call backend shutdown.
- Retain a cleanup backstop for exit paths that bypass the ordinary UI action.
- Keep `Drop` as a backstop, not the primary guarantee.

Kit documents native should-close and completed-close hooks. tty7 source also uses app-quit lifecycle facilities, but exact hook availability/awaiting behavior must be checked against the selected Kit family. Do not assert that an unverified `on_quit` method will await a worker indefinitely or run after `process::exit`.

### 11.3 Backend invariants to preserve

Existing source/tests cover:

- Foreground groups whose leader exited while children remain.
- PID/group reuse and foreign-group refusal.
- EPERM/liveness ambiguity on macOS.
- An inflight nonblocking write during close.
- Reader output flooding during close.
- Failed reaper-thread spawn with child retained for retry.
- All started shells shut down during explicit exit.

These tests are more valuable than the specific frontend transport being replaced.

A hard process crash or force-kill cannot guarantee normal orderly cleanup. Document actual OS behavior and residual process handling; do not advertise crash-persistent sessions or daemon recovery that the app does not implement.

## 12. Terminal links and later integration

Not required for the first M0 proof. Recognition, current-cwd resolution and safe explicit external opening are required for M1; checkout reparenting and internal document opening are later workspace integrations:

- URL and file-path recognition in actual terminal cells.
- Current shell working directory as the base for relative paths.
- Registered checkout ownership and path validation via existing file services.
- Platform-modifier click to open a file; unmodified prompt click remains separate.
- Refresh cwd when resolving a link, not only at the last periodic poll.
- No arbitrary command or URL scheme execution from escape sequences.
- Rebind link context when a session moves to another checkout.

Reuse the behavior pinned in `terminal-file-links.test.ts`, `terminal-paths.test.ts`, and `terminal-click-cursor.test.ts`. Native cell coordinates differ from xterm, but path containment and current-directory authority do not.

The first terminal-only window may use the OS external opener for allowed web URLs. Opening a document inside Muster waits for the document pane; show an honest missing-feature state rather than inventing a temporary browser panel.

## 13. Runnable verification plan

No checks below are claimed to have run during documentation research. These are the implementation gates to leave runnable.

### 13.1 Preserve existing backend checks

Use focused filters against the original crate while extracting, then against the native core once it exists:

```sh
cargo test --locked --manifest-path src-tauri/Cargo.toml terminal::
cargo test --locked --manifest-path src-tauri/Cargo.toml services::terminal::
cargo test --locked --manifest-path src-tauri/Cargo.toml sec_06
```

High-value exact existing test names include:

- `preserves_binary_output_and_reports_exit_code`.
- `shell_receives_the_requested_working_directory_and_terminal_environment`.
- `sessions_have_independent_input_and_dimensions_and_close_running_processes`.
- `nonreading_process_times_out_input_and_other_sessions_remain_responsive`.
- `close_does_not_wait_for_an_inflight_nonblocking_write`.
- `process_group_signals_reject_gone_or_reused_foreign_groups`.
- `close_kills_foreground_group_when_its_leader_has_exited`.
- `close_kills_the_foreground_process_group`.
- `shutdown_ends_every_shell_the_app_started`.
- `a_slow_window_stops_the_reader_without_losing_a_byte`.
- `a_session_publishing_intensely_closes_without_waiting_for_the_reader`.

Gate tests remain applicable if the alternative async output architecture is retained. Do not claim old gate tests cover a newly designed native queue.

### 13.2 New pure Rust/model checks

| Check | Failure it must detect |
| --- | --- |
| UTF-8 and OSC/CSI split at every boundary | Corruption caused by chunk-local decoding/parsing. |
| Known keyboard-stack overflow stream | Reader/parser panic and title-stack corruption. |
| Normal/application cursor and kitty key fixtures | Wrong key bytes despite correct parser modes. |
| Shift+Enter negotiated/unnegotiated | Prompt sent accidentally or literal escape inserted. |
| Bracketed paste on/off and queue limit | Wrong markers, partial truncation or unbounded paste. |
| Input failure after partial delivery | Automatic duplicate command retry. |
| Spawn completes after view closes | Invisible leaked shell. |
| Exited child with trailing output | Lost final output. |
| Stale resize/status after close/move | Operation targeting wrong session/owner. |
| Reflow/wide/combining cells and selection | Broken coordinates or copied text. |
| Exact-position OSC 133 B/C/D | Incorrect prompt-click region or command status. |
| Title/clipboard/OSC payload bounds | Unbounded allocation or dangerous control text. |
| Dirty-signal coalescing | Output flood creates one queued UI task per chunk. |
| Worker failure propagation | A frozen grid still shown as healthy. |

### 13.3 Native UI/headless checks

With Kit `test-support`, verify:

- A terminal entity survives unrelated redraws.
- Focus routes input to the terminal, and a dialog steals/restores it correctly.
- Selection and ordinary copy actions update expected state.
- Cancel close does not call backend close.
- Accepted close cannot accept a later key.
- Hidden/moved terminal entity identity and grid state remain unchanged.
- Loading, error, exited and unavailable statuses are rendered accessibly.

Headless tests cannot replace a real GPU/font/IME/OS clipboard run.

### 13.4 Manual M0/M1 acceptance matrix

| ID | Scenario | Required result | Gate |
| --- | --- | --- | --- |
| T01 | Launch from terminal and Finder/native launcher | Window, text and prompt appear; login shell environment/cwd are correct | M0/M1. |
| T02 | Type an echo command with accents | Input/output is correct and not duplicated | M0/M1. |
| T03 | ANSI cursor-address/color fixture | Real grid updates, not raw escape text | M0. |
| T04 | Resize window while shell/TUI is active | `$COLUMNS`/rows and rendering agree after stabilization | M0/M1. |
| T05 | Run a bounded long command; Ctrl+C | Interrupt reaches child; window remains responsive | M0. |
| T06 | Neovim alternate screen and mouse | Enter/exit restores normal screen; mouse reports and forced selection work | M1. |
| T07 | Intended coding-agent TUI | Full display and ordinary interactions work; no wrapping drift | M1. |
| T08 | Agent Shift+Enter vs Enter | Newline and submit remain distinct | M1. |
| T09 | Scroll >10,000 lines | History bound holds; scrolling/copy work; new output does not steal deliberate scroll position | M1. |
| T10 | Multiline clipboard paste | Negotiated bracketed paste and size rejection work | M1. |
| T11 | TUI OSC 52 copy | Clipboard store works without enabling unsolicited read | M1. |
| T12 | Busy process close → cancel → confirm | Cancel does nothing destructive; confirm kills only owned groups | M1. |
| T13 | Quit while output/paste/resize active | Bounded cleanup; no app-owned shells remain | M1. |
| T14 | Child exits normally/nonzero | Correct exit state and final output remain readable | M1. |
| T15 | Continuous output from controlled fixture | Memory plateaus after warmup and history saturation; input/close stay usable | M1. |
| T16 | Nonreading child + oversized input | Input fails within bound; window remains responsive | M1. |
| T17 | Shell `false`, then successful command | Last-command status changes independently of shell lifetime | M1. |
| T18 | IME/dead keys + Option/meta | Correct commit/cancel; shortcuts and composition do not duplicate input | M1. |
| T19 | Nerd symbols, CJK and variation-selector emoji | Cell alignment, hit testing and selection are correct | M1. |
| T20 | Second integrated instance | It does not open/clear shared DB before exclusion | Integration/M2. |
| T21 | Prompt click with drag/double/modifier variants | Only safe editable-shell clicks move the caret | M1. |
| T22 | Relative file link after `cd`/session move | Resolves against current permitted directory and owner | M1 for cwd/external open; M2 for session move/internal open. |
| T23 | Run zsh/sh commands, Git and Neovim | General shell/PTY behavior, exit/control handling and TUI operation match current Muster; no command allowlist | M1. |
| T24 | Change current terminal preferences | Font/ligatures/cursor/scrollbar/selection-copy updates preserve PTY and scrollback; shell integration applies to new shells | M1 standalone settings; follow-worktree integration in M2. |

The user runs real tests on the confirmed Apple M1 Pro/macOS 27.0.1 arm64, beginning with a short checklist for each runnable increment. Record shell/tool/agent versions and relevant input methods as each scenario is exercised; no single chosen agent restricts the terminal's scope. For each scenario record pending/pass/fail, environment, user feedback and any reproduction. The implementer supplies launch/stop instructions and controlled fixtures, runs automated checks and fixes failures before handing back the affected scenarios. Never mark an unreported manual test as passed.

Repeat relevant cases on supported macOS and Linux X11/Wayland targets before platform-complete claims. Lack of a second test machine may remain an explicit blocker for platform completion, not for receiving early terminal feedback. Test Windows only if added explicitly to release scope.

### 13.5 Performance measurements

Record, do not assume:

- Cold launch to first prompt, with fonts warm/cold.
- Input-to-visible-echo latency at idle and under bounded output load.
- RSS baseline, after scrollback saturation and after terminal close.
- Output throughput using identical finite fixture/grid/window settings.
- Frame-time/input stalls during resize, selection and background output.
- CPU usage while idle, hidden and cursor blinking.

Compare against current Muster under the same conditions. tty7's published benchmarks are upstream-specific and do not predict this port's performance.

M1 acceptance is “stable usable behavior with bounded resources,” not an invented FPS or memory target. Set numeric regression budgets from measured baseline runs during implementation.

## 14. Suggested implementation order within the terminal milestone

1. **Dependency smoke:** Kit hello window and visible text under the pinned toolchain; record actual source correspondence and audit coverage before manual handoff.
2. **Parser safety:** exact source choice, keyboard fix verification, malicious-stream test.
3. **Execution seam and native grid:** verify owned background execution, parser-owned worker state and a visible-cell rendering element; keep services independent of GPUI.
4. **Existing backend and lifecycle:** bind OutputSink, explicit shell/cwd, reader EOF/failure observation, spawn-cancellation cleanup and explicit shutdown. Test no leaked shell before handoff.
5. **M0 input/size:** ordered bounded writes, Ctrl+C, coalesced resize, modes and protocol replies. Hand off the small smoke-test checklist to the user; resolve feedback before expanding it.
6. **Service integration:** isolated DB/config, ownership/dimension validators and real create/rollback/close services, including surfaced cleanup failures. Do this before adopting service-dependent shell integration.
7. **Status and close policy:** fresh guarded status, confirmations, input timeout warnings, bounded tail drain and quit backstop.
8. **Scroll/selection/clipboard:** native interactions, resource limits and negotiated Shift+Enter; hand off targeted agent/TUI checks.
9. **Shell integration and terminal links:** current bootstrap, exact-position OSC markers, safe prompt click-to-move, title policy, current-cwd link resolution and explicit external opening; defer workspace-only integrations.
10. **Fonts/IME/settings:** native assets, live terminal preferences without respawn, Unicode fixtures and user-run platform composition checks.
11. **M1 acceptance:** real TUI, finite-load stress, existing/new tests, measurements and documented gaps; deliver fixes in small increments until the user confirms terminal usability.
12. **Next-phase decision:** only after terminal feedback, resolve the product decisions listed in the parent report before starting workspace panels.

This sequence is not permission to merge an unsafe parser while waiting for a later phase. The parser fix and shutdown boundary precede real user testing.

## 15. Completion evidence required

A terminal-first completion report should include:

- Native app/core source paths and dependency revisions actually selected.
- Locked build command and toolchain used.
- Parser patch/fix and source-audit evidence.
- Passing focused backend/model/native UI tests.
- Manual matrix results with OS, shell, agent version and graphics session.
- Process-cleanup verification.
- Performance/memory observations and unresolved gaps.
- Confirmation that no production database was touched by the spike.

A screenshot of a prompt, a passing hello-world build, or a claim that Alacritty supports ANSI is insufficient evidence on its own.
