# Muster: native Rust migration feasibility and implementation strategy

**Status:** original feasibility research, followed by an implemented native terminal and workspace shell. The historical plan below is not a claim of full parity.

**Historical implementation snapshot (2026-10-14):** the root Cargo workspace builds `core`
and `native`; Tauri/Vue is parked in `legacy/`. Native Git discovery, worktree
mutations, sidebar/titlebar navigation, terminal ownership, dark-theme roles,
single-instance locking and coalesced state writes are implemented. Review notes,
agent delivery, live preferences, file/link activation and terminal search remain
unimplemented. See [current verification and measurement](ghostty-reference.md#11-chrome-pass-theme-state-writer-refusals-and-the-read-cadence).
Real OS IME, GPU rendering and Linux runtime acceptance remain manual gates.
**Current audit-patch summary (AP-01–AP-11, over `69ae8ba`):** file activation,
read-only source/Markdown readings and web-link opening are implemented. Links
use direct weak-window dispatch and failures use the window's dismissible banner.
Local Markdown images are prepared by bounded descriptor reads on workers;
focus follows the visible reading. Checkout rows use change presence, not line
totals; Files scrolls horizontally while Changes fits panel-width rows and
shortens long names. See [current behavior and regression evidence](opening-reading-and-scrolling.md#current-contract-after-ap-01ap-10)
and [AP-11's current suite record](audit-action-plan.md#ap-11).
Historical research and pass records below are not current acceptance evidence.

## Native notes pass (v0.1.0)

The Code footer includes explicit formatting with a user-installed `prettier` on
`PATH` outside the checkout. Formatting uses an isolated temporary input, ignores
project configuration, and leaves the result as an unsaved draft. Prettier is not
bundled; the supported parsers match Legacy's formatter, not every editor grammar.

Diff notes can be exported as Markdown or explicitly sent to an app-created session
of an already-running OpenCode API-major-2 service. Muster reads its local service
registration; it does not install or start that service or send through a shell.
Round prompts and note snapshots are persisted before dispatch. An uncertain send
is reconciled by its marker; absence blocks automatic retry rather than claiming
exactly-once delivery. Protocol tests use a mock server, not a live provider.
Unsent note drafts remain window-local. Settings exposes only supported review
options, not Legacy's unrelated terminal/theme preferences.

Native workspace schema 2 and review-round schema 2 reject incompatible files
without rewriting them. There is no migration: reset an incompatible state file
explicitly, retaining a backup if needed. The native crates move to `0.1.0` for this
stored-contract change; the frozen Legacy application remains `0.21.1`.

## Local build and packaging

`scripts/release.sh check` runs the Rust formatting, clippy and workspace test
checks plus the RustSec audit. `build` adds the optimized native build; `release`
packages that build as `dist/Muster.app` on macOS. The entry point requires
Rust/rustup 1.97.1 and Node.js. It is local-only: it does not create tags, publish,
sign with a distribution identity or notarize. See `scripts/release.sh --help`.
AP-12 integration/platform acceptance remains pending; real GPU/IME, image pixels,
Linux runtime and release measurements are not established by headless tests.

**Application baseline:** Muster `v0.21.1`, commit `4412f63bd385bdb3b9eb12b829858578a2009f81`.  
**Research snapshots:** retrieved on 2026-10-11 UTC; exact upstream revisions are recorded below.  
**Worktree:** `.worktrees/docs-native-rust-gpui`, branch `docs/native-rust-gpui`.  
**Scope:** replace Tauri 2 + Vue with a native Rust desktop application using GPUI Kit; preserve the general interface and workflows. The first implementation milestone is one native window containing a real terminal, not the whole application.

## Reading order

1. This report: feasibility, dependency decisions, architecture, delivery sequence, risks and evidence.
2. [Terminal-first implementation specification](terminal-first.md): PTY reuse, parser/rendering decisions, concurrency, input, lifecycle, and acceptance checks for the first milestone.
3. [Interface and workflow parity specification](interface-parity.md): the current layout and behavior that a complete port must preserve, with component mappings and verification scenarios.
4. [Ghostty reference and terminal gap analysis](ghostty-reference.md): what the native terminal already provides, what it is missing against Legacy parity, and the ranked behaviors worth taking from Ghostty.

These documents distinguish **observed facts**, **proposed decisions**, and **unverified questions**. A source-level assessment is not a successful build, and a terminal library's feature list is not proof that Muster's actual agent TUI works with it.

## 1. Executive decision

**Source inspection supports pursuing the port, but native build/runtime feasibility remains unproven until M0 → M1. The expected work is primarily a frontend rewrite and an application-host replacement, not a rewrite of the Git, database, file, worktree, or PTY engines.**

The recommended stack is:

- **GPUI Kit** for the application window, layout, menus, inputs, dialogs, trees, editor and diff surfaces.
- **Muster's existing `portable-pty` backend** for local shells and process lifetime.
- **An Alacritty terminal parser/grid**, with a reviewed correction for the keyboard-stack defect described in section 5, for VT state.
- **A native terminal view** scoped to Muster. Use `tty7` and `gpui-terminal` as source-level references and selectively reusable implementations; neither is currently a drop-in replacement compatible with the examined Kit snapshot.
- **The existing Rust services and SQLite schema**, kept wherever their behavior remains unchanged.

Do **not** start by forking the whole `tty7` application. Do **not** start by rebuilding the entire sidebar. Do **not** treat PTY bytes as plain text in a text widget. Do **not** wrap the existing web UI in a GPUI webview: that does not meet the full-native objective.

The earliest useful result is:

> Launch a GPUI Kit window, start a shell through Muster's PTY backend in an explicitly selected working directory, render ANSI/VT output in a native grid, type into it, resize it, run a fullscreen TUI, and close it without leaking or incorrectly killing processes.

A first successful window and shell validates the direction. It does not yet justify removing the existing production application.

### What this recommendation preserves

- The repository → checkout/worktree → terminal hierarchy.
- A left navigation sidebar, central work surface, and right files/changes inspector.
- Focus and split layouts, including a terminal that stays alive behind a document.
- The positions and purposes of the important actions, rather than a new generic IDE layout.
- Actual shell/agent execution in a PTY, rather than a custom substitute for the agent's own interface.
- Destructive-operation confirmations and persistent-data protections.

### What may legitimately change

- Default control styling, dialog chrome, borders, corner radii and small spacing differences.
- The rendering mechanism, text shaping and native focus implementation.
- Internal state ownership and the removal of webview IPC.

Changing a button's appearance is acceptable. Removing that button's action, changing its location in the workflow, or changing which process it stops is not.

## 2. Names and scope clarification

| Name in the request | Investigated interpretation | Meaning in this report |
| --- | --- | --- |
| GUIKit / gpuikit / gpui-kit | `longbridge/gpui-kit` | The native Rust UI framework and component family. |
| PTY / pti | A pseudo-terminal; concretely Muster's `portable-pty = "0.9"` | The OS-facing process and byte-stream layer, not a visual component. |
| tty7 / tttp7 | `l0ng-ai/tty7` | The existing Rust/GPUI terminal-workbench application. |

Repository search found `l0ng-ai/tty7` with exactly the relevant Rust/GPUI/Alacritty architecture. Searches for the literal misspellings did not identify a better-matching project. This interpretation is explicit, not a claim that every spoken spelling names an independent library.

### “Full Rust” boundary

The proposed desktop UI and application host contain no Vue, JavaScript runtime, HTML/CSS DOM, embedded browser, xterm.js, or Tauri runtime. Rust may call native OS APIs and existing system tools. Git and the user's external coding agents remain external programs as they are today.

Avoid enabling Kit's optional JavaScript extension host (`gpui-shell`), webview component, or browser target. Their existence in the upstream repository does not mean a desktop Kit application must use them.

**Approved formatting boundary:** external Node/Prettier execution is allowed; no JavaScript runtime is embedded in the native UI/host. Prefer a suitable native Rust formatter if it meets the required language/config behavior. If Node/Prettier is used, record a TODO/technical-debt item to evaluate native replacement, plus its installation/version/support policy. Formatting is deferred until the editor phase and does not block the terminal MVP.

## 3. Evidence baseline: what Muster already contains

### 3.1 Current frontend

`package.json` and the inspected Vue files show:

- Vue 3, TypeScript, Vite, Tailwind and Reka UI.
- xterm.js 6 with fit, Unicode 11, WebGL and clipboard addons.
- CodeMirror 6 language/editor packages.
- `@git-diff-view/vue` plus an application-specific large-diff path.
- Markdown-it, DOMPurify, Shiki and Prettier.
- Tauri API, dialog and clipboard plugins.

The UI is not just declarative markup. Several important product behaviors live in TypeScript: session movement, shell markers, keyboard negotiation, navigation revisions, stale-result rejection, drafts, diff annotations, selection semantics and shutdown ordering. These behaviors need a Rust implementation even though the business services are already Rust.

### 3.2 Current backend

The Rust package is presently a binary rooted in `src-tauri/src/main.rs`; it is **not already an independently importable core library**. `main.rs` declares `commands`, `config`, `domain`, `git`, `persistence`, `services`, and `terminal` modules.

The useful separation is already visible:

| Area | Existing path | Reuse assessment |
| --- | --- | --- |
| PTY and process lifetime | `src-tauri/src/terminal/mod.rs` | Strong reuse candidate; no Tauri imports in this module. |
| Process name/cwd introspection | `src-tauri/src/terminal/process.rs` | Reuse on current macOS/Linux targets. |
| Shell creation and integration | `src-tauri/src/services/terminal.rs` | Reuse; database-backed creation is separate from raw backend spawn. |
| Database and UI persistence | `src-tauri/src/persistence/mod.rs` | Reuse unless stored shapes deliberately change. |
| Rust domain types | `src-tauri/src/domain/` | Reuse; terminal layout already has a native model. |
| Preferences | `src-tauri/src/config.rs` | Reuse path, keys, normalization and current defaults. |
| Files, reviews, workspaces, worktrees | `src-tauri/src/services/` | Reuse logic; do not move filesystem/process work into view rendering. |
| Agent service | `src-tauri/src/services/agent.rs` | Already has an injected `EventSink`; reconnect native observers to it. |
| Git subprocess execution | `src-tauri/src/git/mod.rs:33` | Replace the Tauri blocking-executor call. |
| Git watcher notifications | `src-tauri/src/services/git.rs` | Replace `AppHandle`/`Emitter` delivery, retain watcher behavior. |
| Command adapters | `src-tauri/src/commands/` | Replace the transport, **not** the validation and operation ordering. |
| Host/window/plugins/menus | `src-tauri/src/main.rs`, `menus.rs` | Rebuild against GPUI and native platform facilities. |

“No Tauri imports” does not mean a module can be copied into any crate without its domain, dependencies or sibling modules. Extraction must follow the actual module graph. Conversely, the fact that a file sits under `src-tauri/` does not make its algorithms Tauri-specific.

### 3.3 Especially valuable existing work

1. PTY teardown verifies process/session ownership before group signalling.
2. Writes have size limits, contention handling and deadlines.
3. Closing does not naively wait forever for a busy PTY writer or reader.
4. Shell startup preserves real login-shell behavior while optionally installing OSC 133 hooks.
5. Failed database registration after spawning attempts to close the shell. The current service discards the close error (`services/terminal.rs:147`); native integration must surface cleanup failure and retain ownership for retry, not claim rollback is guaranteed.
6. File services enforce path containment and compare expected content before replacing files.
7. Review dispatch persists a round before contacting the agent.
8. Async frontend controllers reject stale results and preserve newer navigation.

These are not implementation details to discard for a shorter GPUI example. They are the safety and behavior baseline.

## 4. GPUI Kit assessment

### 4.1 What was actually inspected

The downloaded Kit snapshot is:

`longbridge/gpui-kit@1ccf011620da24d633f7cd6d3c11ca2e97798dcf`

Its root and Kit manifests declare:

- `gpui-kit`, `gpui-base`, `gpui-component` and assets version **0.7.1**.
- Rust edition 2024.
- Exact default GPUI snapshot packages **`gpui-pre-* = 0.3.8`**.
- Default Kit features `component` and `assets`.
- Optional `gpui-fast`, `test-support`, profiling, inspector and language grammar features.

The registry page was also read through the agent-reach web route and documents `gpui-kit = "0.7"`. Therefore **Kit does not inherently require a Git dependency**. The manifest's exact version set and the eventual application's lockfile should be validated together; this report does not claim a successful resolution of a new Muster manifest.

Use a pinned published release and its aligned GPUI family for an implementation spike. Do not combine old examples importing upstream `gpui` with newer Kit examples without checking their source and version.

### 4.2 Entry points and lifetime model

The examined getting-started recipe uses:

- `gpui_kit::application()` for the native platform host.
- `gpui_kit::init(cx)` before constructing component facilities.
- `gpui_kit::open_window(...)` to mount a content entity within the Kit root.
- Retained `Entity<T>` values for inputs, models and views.
- Owned `Task` and `Subscription` handles for ongoing work.

Do not recreate a terminal, editor state, diff state, subscription or background task inside every `render()` call. Rendering should describe the view, not repeat spawning or filesystem side effects.

The window documentation distinguishes a cancellable native close request (`window.on_window_should_close`) from a completed close observer (`cx.on_window_closed`). Direct `remove_window()` requires the application to perform its own confirmation first. This distinction is essential for terminal shutdown and dirty files.

### 4.3 Components relevant to Muster

| Need | Examined Kit facility | Remaining application work |
| --- | --- | --- |
| Top chrome | Title bar, menus, buttons, icons | Breadcrumb semantics, drag-only empty region, platform controls. |
| Left navigation | Sidebar, nested menu items, suffix slots, virtual lists | Repository/worktree/session row model, state labels, drag/drop and rename. |
| Panel sizing | Resizable primitives | Current constraints, focus/split mode, drawer behavior and persistence. |
| Terminal session/layout state | Tabs/resizable primitives; Dock exists | Preserve session identity and stored layout model. Current visible split mode is terminal plus preview, not a general terminal-pane splitter. |
| File navigation | Tree/virtual-list primitives | Lazy filesystem reads, watched revisions, selection and sticky ancestry. |
| File editing | `Editor` + `EditorState`, grammar features | Safe saves, retained drafts, current toolbar and formatting decision. |
| Diffs | `Diff`, `DiffState`, annotations and source coordinates | Large-diff strategy, review state, send/queue workflows and backend transport. |
| Markdown | TextView/Markdown facilities | Link routing, supported syntax, review-origin containment and position restoration. |
| Preferences | Settings/form/input/select/switch controls | Existing keys, validation, apply/cancel semantics and runtime updates. |
| Confirmations | Dialog/AlertDialog and root overlays | Destructive semantics, failures, focus return and layered questions. |
| Feedback | Notifications/toasts/spinners | Current error placement, message ownership and no duplicate failures. |

This is strong evidence that the **general interface can be preserved without implementing an entire widget toolkit**.

### 4.4 What Kit does not solve

- It is not a terminal emulator; the snapshot's terminal-named files found by filename are icon assets, not a terminal component.
- A source editor is not a terminal grid.
- A serializable Dock layout is not automatically compatible with Muster's terminal layout JSON.
- A native diff viewer does not decide which review notes are sent, queued, outdated or relocated.
- Accessibility built into standard controls does not automatically make a custom terminal element accessible.
- A native image widget does not prove parity with the current video previews or full image-format coverage.
- GPU rendering and upstream FPS claims do not establish Muster's latency or memory usage.

### 4.5 Toolchain and platform implications

The examined installation guide documents Rust **1.92+** for its current dependency graph and macOS **15+**. Muster pins Rust **1.97.1**. The documented compiler floor is therefore not, by itself, a reason to change Muster's toolchain; a locked build is still required. The locally available `rustc` observed during research is a different Homebrew compiler, not proof that the pin was used.

The upstream macOS 15+ statement above is a reported framework requirement, not Muster's approved minimum. The user chose the current system, without legacy compatibility work. The initial test machine is an Apple M1 Pro running macOS 27.0.1, build 26A434, arm64 (verified locally with `sw_vers` and `uname -m`). Use that as the initial implementation/runtime baseline; do not add older-macOS support to this migration.

For Linux the Kit guide lists fontconfig, X11/XKB, Wayland, Vulkan and other development packages; its setup script also includes WebKit. That list is an **upstream development recipe**, not proof that a minimal no-webview Muster binary requires every library on it. Derive production dependencies from the selected feature graph and runtime linkage. Do not replace the current Linux installer with a guessed “GTK4 required” list.

GPUI Kit supports Windows, but the current release pipeline targets macOS and Linux. Windows is not automatically added to this task; Muster's shell discovery and process introspection would need separate platform work.

## 5. Terminal ecosystem assessment

### 5.1 PTY, VT parser and renderer are different layers

```text
shell / agent / nvim
        ↕ operating-system pseudo-terminal
Muster portable-pty backend
        ↕ ordered bytes
VT parser + terminal grid + modes
        ↕ snapshots and terminal input encodings
GPUI terminal element
        ↕ focus, keyboard, IME, pointer, clipboard
native application window
```

A PTY starts and connects to processes. It does not interpret cursor addressing, alternate screen, colors, scroll regions or input modes. An Alacritty grid parses and stores VT state. It does not, by itself, draw a GPUI terminal or bind native keyboard and IME events.

### 5.2 `tty7`: useful source, wrong whole-application foundation

Examined snapshot:

`l0ng-ai/tty7@f1847b70b29a804c4b27b6049d6bda4907a73ff4`

Observed architecture:

- A `tty7-app` GUI binary, `tty7-core`, server/CLI crates, and additional gateway machinery.
- A background server owns shells; quitting the GUI intentionally leaves sessions running.
- The daemon is principally a byte pipe; the client maintains an Alacritty mirror for rendering.
- The terminal view uses native selection, search, scrollbars, keyboard modes, IME and rendering elements.
- The terminal files also depend on application configuration, actions, localization, daemon routing, remote context, agent state, editor-grade command input and other tty7-specific modules.

Its dependency family is materially different from Kit 0.7.1:

- `gpui` / platform from a fixed Zed revision, with patches to the `l0ng-ai/zed` `tty7` branch.
- `gpui-component` from `l0ng-ai/gpui-component`, branch `tty7`, declaring 0.5.x.
- `alacritty_terminal` from `l0ng-ai/alacritty`, exact revision `1276f128fbaa8832cbc66210675cbe8aeb570499`.
- Additional SSH, keychain, mobile, update and daemon dependencies that Muster does not need for a local terminal.

The package is `publish = false`; the examined repository does not expose a small ready-to-use terminal widget crate independent of that application.

**Critical product mismatch:** Muster explicitly kills its shells on shutdown and clears session rows on reopening. tty7 intentionally keeps shells alive after its window closes. Adopting the tty7 daemon would silently change lifetime, persistence, close confirmations, ownership and security. That is a different feature project, not a UI migration.

Recommended reuse:

- Study `src/terminal/element.rs` for cell drawing, styling, cursor and hit testing.
- Study `input.rs` for native key encoding and IME ownership.
- Study `view.rs`, `scrollbar.rs`, `search.rs` for selection and scrollback coordination.
- Study `remote.rs` for worker-owned parsing and synchronization concepts, **not** as a backend to import wholesale.
- Preserve Apache-2.0 attribution and notices for copied/adapted code; inspect the relevant files and patches individually.

Do not include tty7's daemon, remote stack, persistence, composer, agent detection or entire app shell merely to obtain its renderer.

### 5.3 `zortax/gpui-terminal`: a compact spike candidate, not full parity

Examined snapshot:

`zortax/gpui-terminal@51f0292938876c8da3de03f0139088591e3be518`

Observed manifest:

- `gpui-terminal = 0.1.0` source package.
- `gpui = 0.2.2`.
- `alacritty_terminal = 0.25.1`.
- `portable-pty = 0.9`, plus channel/locking/clipboard dependencies.

Its API accepts generic `Read`/`Write` streams with resize, title, clipboard and exit callbacks. That is a useful separation from a particular PTY implementation. However:

1. Its `gpui` types are not Kit's `gpui-pre 0.3.8` types. Cargo aliases alone do not make two independently sourced frameworks interchangeable.
2. The README explicitly lists missing mouse text selection and missing scrollback navigation; source confirms placeholder mouse/scroll handlers.
3. It owns a background reader thread and byte-delivery task. Muster already owns its reader and exposes an `OutputSink`, not raw reader/writer handles. Straight insertion requires an adapter or a targeted constructor change and would risk duplicate lifetime ownership.
4. The source must be checked for queue bounds and write blocking before adopting its I/O path.

**Use it as a small code reference or an explicitly temporary smoke-test path after dependency alignment, not as evidence that the complete terminal requirement is already implemented.**

### 5.4 Verified Alacritty keyboard-stack defect

This is a concrete blocker, not a hypothetical concern.

The inspected Alacritty main snapshot is `29dc55375da817d53540f4caefbe708703c57b07`. The `v0.17.0` tag's terminal manifest reports `alacritty_terminal 0.26.0`. In both inspected sources, `push_keyboard_mode` checks `keyboard_mode_stack.len()` but removes an entry from **`title_stack`** when the keyboard stack is full.

If the title stack is empty, that removal can panic. Terminal output is untrusted, and a program can send many keyboard-mode pushes. tty7's manifest documents exactly this defect and its corrected fork, plus an emoji-width correction.

Required policy before enabling kitty keyboard negotiation in a shipping native build:

- Select a parser revision that contains the fix, or apply a reviewed, reproducible source patch.
- Add a regression test for more than the stack limit's worth of `CSI > 1 u` pushes with an empty title stack.
- Check title-stack integrity and subsequent input/painting after that test stream.
- Review the exact parser source, not only a newer package version string.
- Do not solve this by disabling all modified-key behavior and silently breaking the agent's Shift+Enter workflow.

Two acceptable implementation routes:

1. Prefer a corrected **published** parser release if verified at implementation time.
2. Otherwise use an audited source patch or exact-revision fork, carrying attribution and updating source-audit policy deliberately. tty7's pinned parser fork is a candidate, not automatically an audited dependency.

Do not introduce a blanket allowance for all Git dependencies to get past CI.

### 5.5 Dependency compatibility rule

Choose one GPUI family per native application. Terminal code must compile against the same `Entity`, `Window`, `Context`, input event and text system types as Kit.

Three different names in Cargo can still represent incompatible type universes:

- Registry `gpui 0.2.2` in the compact terminal.
- Zed/forked Git GPUI in tty7.
- Registry `gpui-pre 0.3.8` re-exported by Kit 0.7.1.

Recommended approach: keep the Kit family and port a small amount of terminal source to its API. Reject a full dependency-family downgrade solely to make an old terminal example compile. A compile spike must measure how much source adaptation is actually needed before committing to a large extraction.

## 6. Recommended target architecture

### 6.1 Runtime boundaries

```text
GPUI application host
  ├─ startup paths, single-instance ownership, logging, native menus
  ├─ Workspace view/model
  │   ├─ sidebar + header + main view + inspector
  │   ├─ terminal session entities keyed by stable session ID
  │   ├─ document/diff/review entities
  │   └─ actions, request generations, subscriptions, task ownership
  └─ existing Rust services
      ├─ database/config/domain
      ├─ Git/watchers/worktree/files/reviews
      ├─ agent service with injected event delivery
      └─ portable-pty terminal backend
```

No webview IPC is needed between the native views and the Rust services. Direct typed calls remove serialization overhead, but **do not** make blocking operations safe on the UI thread.

### 6.2 Smallest initial organization

Do not first reorganize every backend file into a multi-crate framework.

For a nonshipping terminal smoke test, a small separate native executable can reuse the existing terminal module and its workspace types through source-module references. That proves the host/parser/view path without copying the backend or pulling all Tauri plugins into the native executable. These references are temporary research scaffolding, not the final public API.

When the native app needs database-backed services, establish one Tauri-free module/library boundary. A small core library can be justified then to run existing tests independently and share actual service code during development. It is also valid to keep a single final application package with separate modules if that yields the smaller dependency graph. There is no present need for a plugin architecture, a generalized UI backend trait, or one crate per panel.

The final shipping app must not preserve a dual Vue/GPUI compatibility layer. Development in a separate worktree and an unpublished spike is not a feature-flag strategy for supported old versions.

### 6.3 PTY output path

Preferred initial native path:

- Keep the existing dedicated reader thread.
- Own the VT parser in its output sink.
- Parse bytes into shared terminal state on that worker, with short bounded lock sections.
- Mark state dirty and coalesce UI wakeups.
- Let the UI snapshot visible cells and paint them, without reprocessing the entire byte stream.

This avoids an unbounded raw-output queue on the UI thread. It does not require keeping Tauri's cumulative output acknowledgements when no asynchronous byte queue exists. The alternative using the existing `OutputGate` is specified in the terminal document for renderers that require UI-side parsing.

A crucial distinction: **all bytes must be parsed in order, but not every intermediate frame must be painted**. Coalescing frames is safe; dropping terminal bytes is not.

### 6.4 Input and operations

Retain a per-session serialized, bounded input path. Native key events, IME commit, paste, protocol replies and guarded `cd` operations use it. Do not replace the existing one-second blocking-write bound with synchronous writes from a key handler.

For services:

- Snapshot the relevant checkout/session/file identity before starting work.
- Run blocking Git, filesystem, database and HTTP calls off the UI thread.
- Re-enter the appropriate entity through a weak reference.
- Reject results belonging to an older navigation, file version or session ownership generation.
- Report errors in the same place and with the same destructive-operation consequences as today.

Before database-backed integration, establish a concrete execution seam replacing Tauri's runtime: prefer the selected GPUI family's background execution facilities where verified suitable for blocking work; otherwise use a small owned worker path. Keep the reusable services independent of GPUI. Document result delivery, cancellation, failure reporting and shutdown ownership, and test a stalled operation while input/close remain responsive. Do not presume cancelling an await cancels a filesystem operation or Git write. A free `async fn` such as `git::run_git` needs an explicit replacement execution boundary, not a guessed one-line API substitution.

Move the existing command-level session-ownership and dimension validations into the Tauri-free terminal service boundary when integrating services; call that same boundary from the native controller. The controller also rejects stale queued generations. Database ownership remains authoritative for registered sessions; do not silently substitute a stale UI cache. M0's raw session has explicit local ownership and no database. Preserve the checks and tests before deleting the command adapters.

### 6.5 Events

Reuse the existing `AgentService::EventSink` model to decouple Git watchers from Tauri. The application decides which native entity observes an event.

Keep event payloads typed and narrowly scoped. Coalesce invalidation signals, not persistent review actions or terminal bytes. Retain subscription handles on their owner so callbacks neither cancel immediately nor outlive a closed session.

## 7. Persistence, startup and shutdown

### 7.1 Database continuity

The examined schema is **15**. Changing the UI framework alone does not require a database schema change. Prefer reading the same current structures rather than replacing application layouts with a Kit Dock serialization blob.

The existing database open path:

1. Enables foreign keys.
2. Calls the single current schema constructor.
3. Deletes stored sessions and terminal layouts.
4. Clears the active session preference.

This matches Muster's current nonpersistent-PTY lifetime and conflicts with tty7's persistent daemon model.

### 7.2 Never test the spike against the live workspace database

`main.rs` obtains the platform app-data directory and opens `muster.sqlite3`. Because opening clears session records, an experimental native process must not casually open the production database while the Tauri app is running.

Use no database for the first raw-PTY smoke test. For subsequent integration tests use a temporary/isolated data directory and copies of fixtures. Do not change the user's production configuration or source shell startup files to make a demonstration work.

### 7.3 Single instance must precede database open

Replace `tauri-plugin-single-instance` before using the real database. On Unix, an OS-held lock is a reasonable local implementation candidate; use an existing dependency's safe locking primitive where available and verify target coverage. A file that merely exists is not a robust lock after crashes.

If the native and Tauri versions ever run side by side during development, separate their data directories. They do not automatically share single-instance exclusion because one uses a Tauri plugin and the other may use a lock. A shared production namespace requires one coordinated exclusion scheme, not two unrelated locks.

Refusing a second instance safely can be the initial integration behavior. Bringing the existing window to the foreground is a separate parity item and may require a small local activation transport. Do not add a session daemon just for activation.

### 7.4 Pre-1.0 repository rules remain in force

- Versions remain `v0.<minor>.<patch>`.
- No schema migration ladder or `ALTER TABLE` migration.
- If a stored shape changes, update `create_schema`, increment `SCHEMA_VERSION`, and clearly report the reset policy.
- Refuse a database with another schema version; do not automatically delete it.
- Do not introduce legacy readers or fields that only accommodate an older format.
- Keep current config defaults because they are current behavior; do not add new back-compat shims.
- Use the existing release script for version writes; do not bump a release during research.

### 7.5 Close and quit are a workflow

The final native host needs one coordinated close policy:

1. Stop accepting new destructive/terminal work for the closing scope.
2. Read fresh process state; on failure, do not assume no process is running.
3. Ask the same confirmation if closing would stop a process.
4. Flush bounded input/resize and persistent-state work with explicit deadlines.
5. Stop owned agent activity according to existing service semantics.
6. Invoke `TerminalBackend::shutdown()` explicitly, not only through `Drop`.
7. Remove the window and terminate the host only after cleanup has completed or a bounded fallback has been recorded.

A native framework does not erase unsaved drafts, process races or nonreturning application loops. Verify both platform close buttons and application Quit shortcuts. Window-close hooks and app-quit hooks may not be identical.

## 8. Platform, packaging, security and licensing

### 8.1 Tauri currently supplies more than a webview

The host replacement must explicitly cover:

- Platform data/home/log paths and startup error reporting.
- Single-instance prevention and activation.
- Window geometry/maximized restoration and disconnected-monitor validation.
- Native menus and macOS edit actions.
- Folder dialogs, clipboard and external URL/file opening.
- Rotating logging and diagnostic policy.
- App metadata, bundle identity, icons and OS registration.
- Packaging and signing.

Keep the current services for opening files/URLs where they already enforce policy. Validate native dialog availability in the selected GPUI platform API before adding a second dialog library.

### 8.2 Release pipeline

The current release contract includes five bundle combinations:

- macOS arm64 DMG.
- Linux x86_64 DEB and AppImage.
- Linux arm64 DEB and AppImage.

The scripts expect Tauri build commands and artifact locations. A GPUI executable alone does not reproduce those bundles.

For the first terminal milestone: a local runnable executable is sufficient; do not claim release readiness. For the final port:

1. Inspect and preserve release safety/recovery rules before changing scripts.
2. Evaluate a small bundler or deterministic platform scripts; Kit is not a Tauri bundler replacement by itself.
3. Preserve bundle identity, icons, metadata, license files and artifact naming/validation.
4. Preserve hardened runtime/ad-hoc signing policy where currently used, then verify actual distribution behavior.
5. Repoint build/test/audit scripts and pinned-toolchain checks together.
6. Preserve clean-main/tag immutability and retained-artifact recovery policy.

`tty7`'s own release scripts can be a reference for native bundling, but copying its update system, CLI/server packages or names would add unrelated scope.

### 8.3 Dependency audit

`scripts/audit-rust.mjs` currently rejects external Git sources and unknown non-workspace path dependencies. It only grants special treatment to the hash-checked glib backport.

- Published Kit's registry family can fit this policy without a generic Git-source exception.
- Copying tty7's manifests or adopting its parser fork will not fit unchanged.
- A vendored parser patch needs explicit source identity, hashes, scope and test coverage; path dependencies must not be silently trusted.
- A package published under a new snapshot name may not receive advisories under the original name. This is verified for `gpui-pre-*`; the concrete remediation is specified below.
- Keep `Cargo.lock` and locked CI builds. A branch name plus a historical lock is less explicit than an exact source revision for a security-sensitive parser.

#### 8.3.1 Renamed-crate advisory blindness (verified, required remediation)

This is not a hypothetical audit concern. It was verified against the live registries.

**Observed.** GPUI Kit renames GPUI through Cargo package aliases. The inspected workspace manifest resolves:

| Cargo dependency | Real crates.io package | Verified latest |
| --- | --- | --- |
| `gpui` | `gpui-pre` | 0.3.9 |
| `gpui_platform` | `gpui-pre-platform` | — |
| `gpui_web` | `gpui-pre-web` | — |
| `gpui_macros` | `gpui-pre-macros` | — |
| `reqwest_client` | `gpui-pre-reqwest-client` | — |
| `sum-tree` | `gpui-pre-sum-tree` | — |
| `gpui-fast*` | `gpui-fast*` | 0.1.3 |

The original `gpui` crate is a separate published package currently at 0.2.2.

**Mechanism.** `scripts/audit-rust.mjs` builds its OSV batch as `package: { name, ecosystem: "crates.io" }` from `cargo metadata`, which reports the **real** package name (`gpui-pre`). OSV matches ecosystem package names exactly and has no alias knowledge. An advisory published against `gpui` is therefore invisible to a query for `gpui-pre`, and vice versa.

**Research snapshot.** The recorded queries returned no advisories for either `gpui` or `gpui-pre`. This is not proof that a fork has no vulnerabilities, and must be rechecked for the actual implementation lockfile. Future advisories against the fork's own name can be detected; advisories under the upstream name may be missed.

**Required remediation — name and source provenance, not a name-only exemption.**

1. **Fork-aware review record.** For each renamed package actually resolved, record the locked package/version, upstream package/repository, corresponding upstream source revision, fork changes and evidence establishing that correspondence. Do not assume the fork's semver equals upstream semver. An unknown source correspondence is an unresolved audit gate, not a clean result.
2. **Advisory discovery and applicability.** Keep normal version-aware OSV queries for the real package. Discover upstream advisories by upstream package name without incorrectly passing the fork's version as an upstream version; review affected versions/commits and fixes against the recorded source. Apply the existing severity policy to applicable advisories, and fail verification when applicability cannot be established. Deduplicate findings without hiding their affected package or provenance.
3. **Coverage tests.** Extend `scripts/audit-rust.test.mjs` to reject resolved renamed packages without reviewed provenance and to invalidate review when locked versions/source change. Include an upstream-only advisory fixture with different fork/upstream versions, an applicable advisory, a verified fix and an unresolved correspondence. A mapping or exemption reason alone does not establish coverage.

**Placement.** Establish this policy and source correspondence in M0 before claiming dependency-audit clearance or delivering the spike for manual testing. It need not precede the first local compile. No blanket Git-source allowance or advisory suppression is permitted.

**Carry-over risk.** Any other renamed or re-namespaced dependency introduced later inherits the same problem. Treat a new alias in the resolved graph as a required review item, not a detail.

The glib patch may become unnecessary if the new **actual** Linux dependency graph excludes that glib release. Do not delete it until the Tauri dependency chain is removed and the selected graph proves it unused. Do not assume a different graphics toolkit is either immune to the existing advisory or dependent on the same patch.

### 8.4 Threat model after removing the webview

CSP, DOMPurify and Tauri capabilities cease to be the main runtime boundary. Their old configuration can be deleted with the web stack, but the underlying requirements remain:

- Terminal output, OSC payloads, titles and links are untrusted.
- Local file paths still require containment and symlink-aware validation.
- Clipboard reads initiated by a program are more sensitive than copying text out.
- Opening URLs must use a bounded allowlist policy, not dispatch arbitrary schemes.
- Shell integration scripts must remain private, predictable and idempotent.
- Agent service endpoints and review targets require current ownership checks.
- No release build may acquire an unrestricted development automation bridge.

A GPU-rendered view is not a security boundary. Resource limits and malformed-input tests are part of the terminal component, not optional hardening.

### 8.5 Fonts and attribution

Muster currently bundles regular/bold Fira Code and Nerd Symbols in **WOFF2** assets, with artifact hashes. GPUI's documented native font registration examples use raw **TTF** files and internal family names. Do not assume those WOFF2 bytes can be handed unchanged to every native text backend.

A native build should package licensed TTF/OTF equivalents or a verified conversion, register them before first measurement, preserve icon fallback, and regenerate notices/hashes deliberately. Native text metrics may differ without changing the user's functional interface.

Kit and tty7 source are Apache-2.0; the compact terminal declares MIT OR Apache-2.0. Keep required source attribution and NOTICE material for any extracted code and transitive forks. Retain all font redistribution licenses. Muster's existing MIT license is not a reason to omit third-party Apache obligations.

## 9. Full-port gaps that must not be hidden

### 9.1 Native editor and formatting

Kit provides a real source editor with search, language rules, decorations and grammar features. It does not make CodeMirror and Prettier semantics identical.

Port editing first while preserving safe saves, undo, drafts, language selection and indentation. Treat formatting as a separate compatibility decision:

- Calling installed project Prettier from Rust still executes JavaScript externally and adds an installation dependency. Muster currently bundles Prettier and selected plugins; project availability, versions, language coverage and configuration trust may differ. This route requires an explicit availability/support policy, not a parity assumption.
- A native formatter such as a language-specific tool can keep the app implementation Rust but differs in language/config coverage.
- Reimplementing all Prettier behavior in Rust is not a sensible scope assumption.

The user permits external Node/Prettier, with a preference for a suitable native Rust implementation. TODO / technical debt if Node is selected: evaluate native replacement without silently losing language/config coverage. Resolve installation, version and missing-tool behavior during editor implementation. The full port cannot call the Format button done until its intended functionality is verified; no formatter work is needed for the terminal MVP.

### 9.2 Diff/review transport

Kit's examined Diff component is unusually relevant: virtualized multi-file patches, original/modified source coordinates, selection, inline annotations, custom headers and annotation-start callbacks. Use it before writing a whole diff renderer.

However, Muster's backend also exposes a paged large-diff path. Loading a complete huge patch solely because Kit virtualizes drawing still consumes parse and source memory. Prototype the actual large-file limit and choose whether to retain page-driven rows for that path. Do not mistake virtualization for bounded input loading.

Review persistence, anchor verification, queued sends and relocation remain application/service logic. Native annotations are presentation, not an alternative review database.

### 9.3 Markdown and media

A native Markdown renderer can replace browser markup for common Markdown, links, code blocks and review documents. Confirm tables, tasks, nested formatting, syntax colors, copy and internal links against existing tests.

Raster/SVG previews have plausible native facilities. The inspected routing recognizes png, jpg/jpeg, gif, webp, avif, ico, bmp, SVG, and mp4/webm/mov/ogv video; it does not establish audio playback as current scope. Verify native image decoding and Markdown syntax coverage. The user explicitly accepts deferring inline video: show a preview-unavailable state instead of embedding a player or automatically launching one. Preserve the existing explicit modifier-click external-open action from the file panel so the OS-selected player can be used. Video is later work, not a terminal-MVP or cutover blocker under this accepted scope. No webview fallback.

### 9.4 Accessibility

Use Kit's AccessKit-backed controls for standard interactions. The custom terminal still needs focus, an accessible name, state and a strategy for reading terminal text without flooding assistive technology on every output chunk. Headless role tests are useful but do not prove VoiceOver/Orca behavior. Accessibility is not traded away to obtain a terminal screenshot sooner.

## 10. Delivery sequence and decision gates

| Phase | Deliverable | What proves completion | What is intentionally absent |
| --- | --- | --- | --- |
| M0: dependency/graphics smoke | Native Kit window, text and tiny terminal path | Locked compile; visible text; real PTY round trip; resize; clean quit | Production DB, sidebar, release packaging. |
| M1: usable terminal-only app | One safe native shell/TUI with selection, scrollback, input, clipboard and close guard | Terminal acceptance matrix in `terminal-first.md` | Full workspace panels. |
| M2: native workspace shell | Existing header/sidebar/checkout/session hierarchy and terminal layouts | Native parity scenarios for navigation, rename, move and focus/split preview; no PTY respawn on reparent | Full document/review feature completion. |
| M3: files/editor/inspector | Native tree, changes list, safe editor and Markdown | Safe save/draft/stale-result tests and inspector behavior | Formatting/media gaps until explicitly resolved. |
| M4: diffs/reviews/agents | Native diff annotations and unchanged review/agent services | Complete send/queue/anchor/relocation scenarios | No new persistent daemon, remote feature set or agent proxy. |
| M5: production cutover | Native-only release pipeline and supported-platform parity | All required checks, audits, bundles and manual target runs | Old Vue/Tauri compatibility layer. |

### M0 is an experiment, not permission to cut safety corners

No production data. No long-running real task that can be accidentally abandoned. A minimal proof may defer fancy selection and search, but must still close processes correctly. M1 is the first usable terminal deliverable and the appropriate point for the user to judge the migration direction.

### Iterative delivery and user-owned manual testing

Implement and deliver the terminal MVP first: M0 is its small architecture checkpoint; M1 is the usable terminal checkpoint. Do not start rebuilding workspace panels before the user has tested the terminal and confirmed that direction. Deliver small runnable increments, with exact launch instructions, automated-check results, known limitations and a short subset of terminal scenarios to try. The user performs real shell/TUI, GPU, IME, clipboard and platform tests and supplies feedback; the implementer fixes failures and maintains the evidence record. A pending or unreported manual check is not a pass. Automated regression tests for process lifetime, concurrency, malformed input and data safety remain the implementer's responsibility.

Start on the user's available development platform. Track other supported targets as pending rather than blocking every early feedback increment on a complete platform matrix. M1 can be accepted for that development target, but platform-complete claims and production cutover require the supported macOS/Linux checks. Record automated build evidence per target separately from manual runtime evidence. Measure terminal adaptation during M0/M1, editor/diff adaptation before committing their implementation, and packaging separately; no full-port effort estimate is established yet.

### Confirmed scope and remaining implementation decisions

| Item | Decision / timing |
| --- | --- |
| Initial user test target | Apple M1 Pro, macOS 27.0.1 arm64; user performs real manual tests and supplies feedback. |
| Terminal MVP | General-purpose PTY terminal: zsh, sh, Git, Neovim and arbitrary installed terminal commands. Current terminal functionality is the minimum, not a single-agent demo. M0 is an incomplete checkpoint; M1 completes standalone terminal behavior. No unrelated new features are required by “or more.” |
| Platform policy | Current macOS baseline, no legacy support work. Existing Linux release targets remain in the final plan; Linux details/runtime evidence are later work, not blockers for the first Mac MVP. |
| External JavaScript tools | Node/Prettier allowed externally; prefer a suitable native Rust formatter. Record TODO/technical debt if Node is used. No JS UI/embedded runtime. |
| Formatter delivery details | Installation/version/language/config and missing-tool policy to be resolved in the editor phase, not before the terminal MVP. |
| Video | Inline playback deferred by user approval. Later file pane shows preview unavailable and keeps explicit external-open via the existing modifier-click gesture; no automatic player launch or embedded player. |

Shell/tool versions and input-method details can be recorded per manual run; they do not narrow the terminal to a predefined command allowlist. Internal document opening and checkout/session navigation remain workspace-phase integration; their absence in the standalone MVP must be reported explicitly.

Technical choices such as executor APIs, parser revision, reader observation and source adaptation are implementation responsibilities. Report a blocker if inspection disproves the approach; do not require the user to choose Rust APIs.

### Stop/reconsider conditions

- A selected terminal implementation cannot share Kit's GPUI type family without a large framework fork.
- The selected framework cannot build/run on the approved current macOS baseline or the later required Linux targets.
- Native rendering cannot handle the actual agent TUI/IME/modified-key workflows reliably.
- Fixing the parser defect requires a source policy the project cannot audit safely.
- Neither a suitable native formatter nor the permitted external Node/Prettier route can preserve the agreed formatting scope.

When one occurs, report the verified fact and the simplest alternative. Do not respond by inventing a permanent web/native hybrid or silently weakening the requirement.

## 11. Risk register

| ID | Risk | Consequence | Control / exit criterion |
| --- | --- | --- | --- |
| R1 | Multiple GPUI package families | Unusable shared types or duplicated framework | One family in the resolved graph; terminal compile spike. |
| R2 | Keyboard-stack parser panic | Output can break a live terminal | Verified source fix plus adversarial-stream regression. |
| R3 | Whole tty7 daemon adoption | Changed lifetime and storage semantics | Keep Muster PTY ownership; no daemon in migration scope. |
| R4 | PTY writes on UI thread | Frozen input/window | Worker queue, current bounds/deadlines and realistic nonreading-child test. |
| R5 | Unbounded output/event queues | Memory growth and delayed input | Worker parsing, bounded/coalesced delivery; sustained-output test. |
| R6 | Wrong shutdown group | Lost unrelated user processes | Retain and re-run ownership-verification tests. |
| R7 | Native spike opens live DB | Session/layout records deleted | No DB in M0; isolated data in integration; lock-before-open. |
| R8 | Recreated entity on move/hide | Shell/output loss | Registry keyed by session ID; view reparenting not spawn. |
| R9 | Sidebar “simplification” | Product workflow changes | Behavior matrix and baseline scenarios, not generic template UI. |
| R10 | Stale async results | Wrong files, selection or review target | Identity/revision guards ported from TS. |
| R11 | Unsafe native save | Overwrite user's concurrent edit | Reuse expected-content file service and draft tests. |
| R12 | Formatter/media handwaving | Incomplete full-native port | Explicit product/technical decisions before completion claim. |
| R13 | WOFF2/font fallback assumptions | Grid width/Unicode/icon failures | Native font registration and mixed-script fixtures per platform. |
| R14 | New OS minimum/graphics needs | Compatibility regression | Baseline deployment target and Linux driver matrix. |
| R15 | Source audit relaxed globally | Supply-chain regression | Narrow reviewed patches; unchanged refusal for unknown sources. |
| R16 | Lost native menu/IME integration | Copy/paste or composition fails | Native platform tests, especially macOS Edit and Option handling. |
| R17 | UI accessibility inferred from Kit | Terminal inaccessible | Semantic tests and assistive-technology review. |
| R18 | Tauri packaging assumed reusable | No valid release bundles | Bundle-specific CI validation and release-safety review. |
| R19 | Renamed crates blind the OSV audit | Upstream advisories may be missed or misclassified using fork versions | Source-provenance records, upstream advisory applicability review and coverage tests (§8.3.1), as an M0 gate. |

## 12. Verification strategy

### Existing tests to retain or translate

- Rust terminal/process/shell-integration tests: preserve them through backend reuse.
- Rust persistence/config/worktree/files/review/agent tests: reuse their service behavior.
- Tauri command tests: extract the ownership, dimension and lifecycle checks they currently pin before deleting adapters.
- TypeScript domain/controller tests: translate behavior into Rust model tests, not necessarily Vue test mechanics.
- xterm-specific WebGL/DOM tests: remove only with the old renderer; replace the underlying renderer-failure requirement with native checks.
- Component tests: use Kit's `test-support` for action, focus, overlay and retained-entity behavior.
- User-owned manual native target tests: real GPU, OS menus, IME, fonts, dialogs, clipboard and process cleanup. The implementer supplies runnable increments and checklists, records feedback and leaves pending checks explicitly unverified.

A pure documentation change does not need to run the entire current application test suite. This research did **not** compile GPUI/tty7, run a native terminal, benchmark performance or execute release commands. Such checks belong to the approved implementation phase and remain explicit gates, not alleged completed work.

### Initial future checks

Existing command shapes, to be used only during implementation:

```sh
cargo test --locked --manifest-path src-tauri/Cargo.toml terminal::
cargo test --locked --manifest-path src-tauri/Cargo.toml services::terminal::
cargo test --locked --manifest-path src-tauri/Cargo.toml sec_
pnpm exec vitest run src/components/TerminalSession.test.ts
pnpm exec vitest run src/presentation/terminal-movement.test.ts
```

The native crate path and test commands must be fixed once that crate exists; this report does not invent an already-present `native/Cargo.toml` or claim those commands were run.

## 13. Evidence and primary-source index

### Local sources at the application baseline

All paths below are relative to the worktree/repository root. Line references identify inspected areas; future code moves will change them.

| ID | Source | Supports |
| --- | --- | --- |
| L1 | `package.json`; `src-tauri/Cargo.toml`; `rust-toolchain.toml` | Current stack, dependencies, version, toolchain and scripts. |
| L2 | `src-tauri/src/main.rs:134–275`, `:357–383` | Single-instance ordering, app-data path, DB open, host state, agent sink and explicit shutdown. |
| L3 | `src-tauri/src/terminal/mod.rs:19–365`, `:384–890`, `:920–1123` | PTY interface, write/lifetime safety, reader and flow control. |
| L4 | `src-tauri/src/terminal/process.rs` | Current macOS/Linux process/cwd introspection. |
| L5 | `src-tauri/src/services/terminal.rs:1–203`, `:211–499` | Database-backed spawn/rollback, shell selection, OSC 133 hooks and private scripts. |
| L6 | `src-tauri/src/commands/terminal.rs:88–224`, `:392–426` | Tauri output channel/ack adapter, ownership and dimensions. |
| L7 | `src-tauri/src/persistence/mod.rs:18–135`, `:192–218`, `:2389–2501` | Schema 15, layout shapes, startup cleanup and schema refusal. |
| L8 | `src-tauri/src/config.rs:77–267` | Actual preferences/defaults/normalization. |
| L9 | `src/App.vue:1970–2317`; `src/components/MainPane.vue` | General layout, header, inspector drawers and retained terminal. |
| L10 | `src/components/Sidebar.vue:1127–1352`, template thereafter | Order, row state, agent inference, selection and local collapses. |
| L11 | `src/components/TerminalSession.vue:534–903` | Bounded input, close guard, `cd`, output setup, Shift+Enter and titles. |
| L12 | `src/lib/muster-terminal.ts:16–278`; `terminal-renderer.ts`; `terminal-keys.ts`; `terminal-shell-integration.ts` | Font/render settings, parsing acknowledgements, keyboard and shell markers. |
| L13 | `src/domain/ui-state.ts`; `src-tauri/src/domain/terminal_layout.rs`; `src/presentation/terminal-movement-controller.ts` | Geometry, terminal layout and movement race rules. |
| L14 | `src/components/DocumentPane.vue:506–630`; `FileDiff.vue`; `src/presentation/review-notes.ts` | Safe draft handling, formatting and review presentation. |
| L15 | `scripts/audit-rust.mjs:15–66`; `scripts/patched-glib.mjs`; `scripts/artifacts.test.mjs` | Source-audit policy, glib integrity and bundled assets. |
| L16 | `scripts/build-app.mjs`; `scripts/release-artifacts.mjs`; `.github/workflows/release.yml`; `src-tauri/tauri.conf.json` | Existing packaging contract and host metadata. |
| L17 | `AGENTS.md` | Worktree placement, v0 policy, no migrations, release and dev-bridge rules. |

### Upstream sources: immutable snapshot links

External acquisition used the agent-reach GitHub CLI route and its Jina web route. Repositories were downloaded to `/tmp` for read-only inspection, not added to the application worktree. Search results and README claims were checked against manifests and relevant implementation where material.

- **U1 — Kit README and manifests:** [README](https://github.com/longbridge/gpui-kit/blob/1ccf011620da24d633f7cd6d3c11ca2e97798dcf/README.md), [workspace manifest](https://github.com/longbridge/gpui-kit/blob/1ccf011620da24d633f7cd6d3c11ca2e97798dcf/Cargo.toml), [Kit manifest](https://github.com/longbridge/gpui-kit/blob/1ccf011620da24d633f7cd6d3c11ca2e97798dcf/crates/kit/Cargo.toml).
- **U2 — Kit host/platform/lifetime:** [getting started](https://github.com/longbridge/gpui-kit/blob/1ccf011620da24d633f7cd6d3c11ca2e97798dcf/website/docs/getting-started.md), [installation](https://github.com/longbridge/gpui-kit/blob/1ccf011620da24d633f7cd6d3c11ca2e97798dcf/website/docs/installation.md), [multi-window lifecycle](https://github.com/longbridge/gpui-kit/blob/1ccf011620da24d633f7cd6d3c11ca2e97798dcf/website/docs/multi-window.md), [tasks](https://github.com/longbridge/gpui-kit/blob/1ccf011620da24d633f7cd6d3c11ca2e97798dcf/website/docs/task.md).
- **U3 — Kit components:** [Sidebar](https://github.com/longbridge/gpui-kit/blob/1ccf011620da24d633f7cd6d3c11ca2e97798dcf/website/component/sidebar.md), [Editor](https://github.com/longbridge/gpui-kit/blob/1ccf011620da24d633f7cd6d3c11ca2e97798dcf/website/component/editor.md), [Diff and annotations](https://github.com/longbridge/gpui-kit/blob/1ccf011620da24d633f7cd6d3c11ca2e97798dcf/website/component/diff.md), [fonts](https://github.com/longbridge/gpui-kit/blob/1ccf011620da24d633f7cd6d3c11ca2e97798dcf/website/docs/fonts.md).
- **U4 — tty7 architecture/dependencies:** [README](https://github.com/l0ng-ai/tty7/blob/f1847b70b29a804c4b27b6049d6bda4907a73ff4/README.md), [manifest](https://github.com/l0ng-ai/tty7/blob/f1847b70b29a804c4b27b6049d6bda4907a73ff4/Cargo.toml).
- **U5 — tty7 terminal source:** [renderer](https://github.com/l0ng-ai/tty7/blob/f1847b70b29a804c4b27b6049d6bda4907a73ff4/src/terminal/element.rs), [input](https://github.com/l0ng-ai/tty7/blob/f1847b70b29a804c4b27b6049d6bda4907a73ff4/src/terminal/input.rs), [view](https://github.com/l0ng-ai/tty7/blob/f1847b70b29a804c4b27b6049d6bda4907a73ff4/src/terminal/view.rs), [client/parser bridge](https://github.com/l0ng-ai/tty7/blob/f1847b70b29a804c4b27b6049d6bda4907a73ff4/src/terminal/remote.rs).
- **U6 — compact terminal:** [README](https://github.com/zortax/gpui-terminal/blob/51f0292938876c8da3de03f0139088591e3be518/README.md), [manifest](https://github.com/zortax/gpui-terminal/blob/51f0292938876c8da3de03f0139088591e3be518/Cargo.toml), [view](https://github.com/zortax/gpui-terminal/blob/51f0292938876c8da3de03f0139088591e3be518/src/view.rs).
- **U7 — Alacritty defect:** [inspected main terminal state](https://github.com/alacritty/alacritty/blob/29dc55375da817d53540f4caefbe708703c57b07/alacritty_terminal/src/term/mod.rs#L1288-L1305), [release-tag terminal state](https://github.com/alacritty/alacritty/blob/v0.17.0/alacritty_terminal/src/term/mod.rs), [release-tag manifest](https://github.com/alacritty/alacritty/blob/v0.17.0/alacritty_terminal/Cargo.toml). The tag source was read through GitHub API; the immutable main snapshot supplies the fixed reference.
- **U8 — registry existence:** [gpui-kit on crates.io](https://crates.io/crates/gpui-kit). Dynamic page, not a lockfile or immutable compatibility guarantee.
- **U9 — native input bug context:** [tty7 issue 177: Option-as-Meta](https://github.com/l0ng-ai/tty7/issues/177), [tty7 issue 203: emoji display width](https://github.com/l0ng-ai/tty7/issues/203). Useful regression leads, not proof that Muster needs every upstream fork.

### Confidence ledger

**Verified by inspected source:** existing PTY reuse boundary; schema cleanup; Tauri coupling points; Kit registry family/version pins; tty7 daemon/fork architecture; compact terminal selection/scroll gaps; Alacritty stack bug; current interface/persistence/formatting behaviors.

**Supported but not executed:** Kit's documented platform and editor/diff capabilities; feasibility of adapting terminal source to Kit; native font conversion/supply; future release bundling.

**Not verified:** successful native build, exact terminal source adaptation size, actual macOS/Linux runtime parity, IME behavior, performance, parser fork audit clearance, complete formatting/media replacement and final release artifacts.

## 14. Final recommendation

Approve **M0 → M1 only** as the first implementation slice: a terminal-only native app with an isolated lifecycle and data boundary. Preserve the PTY backend; pick one GPUI family; resolve the parser defect before enabling the modified-key path; prove the actual shell and agent TUI work before spending time on the rest of the panels.

After that gate, rebuild the existing workspace presentation with Kit components while translating the product-specific controllers. The intended destination is **the same Muster workflow implemented natively**, not “tty7 with a different name” and not a redesigned generic IDE.
