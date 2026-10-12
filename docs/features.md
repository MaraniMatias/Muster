# Muster feature catalog

What this build actually does, file by file, with the evidence for each claim. Written by reading
the tree rather than the README, so it does not repeat what the README already says.

**How to read this.** Sections 1–4 are shipped. Section 5 is written down but not built. Section 6
is what must not be claimed out loud. Section 7 is what is known to be broken or unreachable.
Section 8 is the cut that leaves an MVP standing.

**Scale.** `v0.20.1` · 43 tags · 359 commits · 76 IPC commands · 10 SQLite tables · ~23.7k lines of
frontend (`src/`, tests excluded) · ~42.4k lines of Rust (`src-tauri/src/`) · ~1,550 automated tests
(68 frontend files, 28 Rust test modules).

---

## 1. Workspace: the checkout is the unit

The repository root and each of its worktrees are first-class checkouts. Each one owns its files,
its terminals, its diff state and its review notes.

| Capability                                                                                              | Where                                                                    |
| ------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| Register a repository, or a plain folder as a `plain` repo                                              | `src/components/Sidebar.vue:1641`, `src-tauri/src/commands/folder.rs:11` |
| Recent projects in the breadcrumb menu                                                                  | `src/App.vue:440-460`, `commands/workspace.rs:57`                        |
| Create a worktree; the task name slugs into a `feature/<slug>` branch, editable, with start point shown | `src/components/WorktreeDialog.vue:97-122`                               |
| `git worktree add -b <branch> <path> <default-ref>`                                                     | `src-tauri/src/services/worktree.rs:188-194`                             |
| Archive vs Delete: dirty-file list, unmerged commits, active sessions, optional branch deletion         | `WorktreeDialog.vue:124-176`, `services/worktree.rs:491,507`             |
| Archive is refused while any session is live                                                            | `WorktreeDialog.vue:323-325`                                             |
| Worktrees created outside the app are picked up by a sync pass                                          | `src/App.vue:1845`, `services/worktree.rs:606`                           |
| Close a workdir / a missing checkout without touching files                                             | `src/App.vue:1524-1555`, `commands/workspace.rs:98,113`                  |
| Restore every archived worktree of a repo from the repo header, with a count badge                      | `Sidebar.vue:1359-1368`                                                  |
| Appends `/.worktrees/` to `.git/info/exclude`, behind a symlink guard                                   | `services/worktree.rs:825-944`                                           |

## 2. Terminals: a real PTY, not an emulator

`portable-pty` 0.9 with OSC 133 shell integration written to disk, output flow control, and
foreground-process introspection on both supported platforms.

| Capability                                                                                                                                                                         | Where                                                           |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- |
| PTY open + spawn, `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=Muster`                                                                                              | `src-tauri/src/terminal/mod.rs:86-127`                          |
| OSC 133 `C` and `D;$status` only — deliberately not the full prompt lifecycle                                                                                                      | `services/terminal.rs:205-224`                                  |
| Hook scripts written to disk, `0600`, with a `ZDOTDIR` shim forwarding `.zshenv/.zshprofile/.zshrc/.zlogin`                                                                        | `services/terminal.rs:290-450`                                  |
| Output gate: the reader blocks until the window acks parsed bytes, so nothing is dropped                                                                                           | `commands/terminal.rs:76-210`                                   |
| Foreground process via `proc_pidpath` on macOS, `/proc/<pid>/comm` + `cwd` on Linux                                                                                                | `terminal/process.rs:26-178`                                    |
| Termination: SIGTERM → 200 ms grace → SIGKILL, to the foreground and shell groups, each verified against the PTY's own session with `tcgetsid`/`getsid`                            | `terminal/mod.rs:464-530, 642-744`                              |
| **Drag a terminal to another worktree**, process and scrollback intact; optional `cd` into an idle shell; reports "moved but directory not changed"                                | `Sidebar.vue:320-522`, `SessionPane.vue:229-286`                |
| **Follow worktree**: terminals relocate themselves on shell `cd` and/or agent relocation (`off \| cd \| agent \| both`)                                                            | `src/App.vue:193-222`, `src/domain/settings.ts:30-32`           |
| Rename inline on double-click; close only after confirming against the real process; `exit` closes its own pane                                                                    | `Sidebar.vue:1516-1528`, `TerminalSession.vue:584-622, 395-410` |
| xterm 6 with WebGL renderer + recovery watcher, custom overlay scrollbar, Shift+Enter fixed for both CSI-u and plain newline                                                       | `TerminalSession.vue:936-965, 214-362, 813-828`                 |
| **ctrl/cmd+click a path in the output** opens it in the preview, and an `http(s)` address in the browser; a plain click opens nothing                                              | `lib/terminal-file-links.ts`, `lib/terminal-paths.ts`           |
| A bare name is read against the **shell's own directory**, so an `ls` opens at the root of a checkout and below it alike; extension-less names (`LICENSE`) count on a listing line | `services/files.rs:406-468`, `terminal/mod.rs:326-329`          |

## 3. Diffs and review notes: the core loop

| Capability                                                                                                                                                                                                                                                                | Where                                                                  |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| Read-only Git surface, in full: `git_status`, `git_diff_stats`, `git_checkout_diff_stats`, `git_diff`, `git_diff_page`, `git_viewed_files`, `git_mark_viewed`, `git_watch_repo`, `git_unwatch_repo`                                                                       | `src/lib/ipc.ts:112-140`                                               |
| Unified syntax-highlighted diff, collapsible hunks, per-checkout `+N/-N` badges                                                                                                                                                                                           | `src/components/FileDiff.vue:838-920`, `Sidebar.vue:1436-1447`         |
| Large diffs virtualized: 32-line pages, 8-page LRU, 3 concurrent requests, per-page in-place error, "diff changed while loading" detection                                                                                                                                | `src/components/use-large-diff.ts:8-15, 150-196`                       |
| Diff limits are explicit refusals, not partial reads: >100k lines, >10k hunks, >32 MiB, >64 KiB/line                                                                                                                                                                      | `FileDiff.vue:725-728`                                                 |
| Note anchored to a **line or a range**, GitLab style: `+` fixes the end, a second `+` above sets the start; the first line can be re-picked from a dropdown                                                                                                               | `src/components/ReviewComposer.vue:35-43, 88-98`                       |
| `draft → sent → resolved`, plus a separate sticky `outdated` flag                                                                                                                                                                                                         | `src/domain/review.ts:3`, `src-tauri/src/persistence/mod.rs:2460-2475` |
| Anchor drift: the first anchored line is hashed (FNV-1a) and re-checked; `outdated` is sticky until the user clears it                                                                                                                                                    | `src-tauri/src/domain/review.rs:78-86`, `services/review.rs:132-163`   |
| Resolve is only offered when the diff can prove it; `unknown` is a real answer, not a guess                                                                                                                                                                               | `src/domain/review.ts:10-16`, `ReviewNoteList.vue:32-45`               |
| **OpenCode round reconciliation**: the `muster-review:<id>` marker is written into the prompt, and on reconnect the transcript is searched for it to confirm or requeue; Pi rounds use session receipts and are never requeued                                                                 | `domain/review.rs:62-69`, `services/review_round.rs:36-42, 161-218`, `docs/pi-integration.md` |
| Notes are marked `sent` **before** the agent is called, so a crash can neither fake nor hide delivery                                                                                                                                                                     | `services/review_round.rs:50-68`                                       |
| **Pi rounds are confirmed from the other end**: the session reports `{marker, entry}` for each review it was shown, read from its own entries; delivery needs this round's own marker from its own session, and completion is `agent_settled` alone. Nothing re-sends one | `docs/pi-integration.md`, `services/review_round.rs`, `services/pi.rs` |
| Outdated notes are held back unless explicitly included; a busy OpenCode target offers Send now / Queue / Not now; Pi always starts a new terminal and has no queue                                                                                                                                                              | `FileDiff.vue:230-237, 244-263, 668-671`, `docs/pi-integration.md`     |
| Export is CodeReview.nvim-compatible with per-extension language fences                                                                                                                                                                                                   | `src/domain/review.ts:71-172`                                          |
| Export writes `<timestamp>.md` with `create_new(true)` into the folder `reviews.storage` names: `~/.muster/tmp/reviews`, or `<checkout>/.muster/reviews` in the working directory                                                                                         | `services/files.rs:75-105, 810-858`                                    |
| The folder is chosen in Settings, named under the selector with its file count and size, and emptied of its exports from there; `workdir` is refused by the command itself                                                                                                | `services/files.rs:164-232`, `SettingsDialog.vue:288-321`              |

## 4. Agent integration and everything around it

**Two harnesses, reached two different ways.** `ReviewTarget = "markdown" | "opencode" | "pi"`,
`src/domain/review.ts:11`. Muster never launches or ends an OpenCode service; it discovers one and
talks to it. Pi has no server: it reports through a loopback bridge its own extension writes to, and
a review reaches it by starting one — see `docs/pi-integration.md`.

| Capability                                                                                                                                                                      | Where                                                                     |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| Discovery from the service registration file; API major must be `2`; probe with a 3 s timeout                                                                                   | `src-tauri/src/services/opencode.rs:30, 55-79, 107-161`                   |
| **A non-loopback address is refused before the password is sent**; TLS-less schemes are refused rather than downgraded                                                          | `services/opencode.rs:120-141`                                            |
| Hand-written HTTP/1.1 and SSE framing with chunked support; 32 KB header, 1 MB frame, 16k-line caps; reconnect up to 30 s backoff                                               | `services/agent.rs:766-1001, 88`                                          |
| Every request carries `?directory=` and `x-opencode-directory`; a reply naming a different directory is rejected                                                                | `services/agent.rs:105, 244-267`                                          |
| A generation guard: a checkout whose path moved under it refuses rather than writes to the wrong tree                                                                           | `commands/agent.rs:26-46`                                                 |
| Relocation detection reads tool calls for `shell.workdir` and `edit/write/patch.path`                                                                                           | `services/agent/relocation.rs:48-151`                                     |
| Session identity is **inferred** from the TUI's `OC \| <title>` prefix matched against the service's list; unmatched says `sin sesión`, ambiguous says `varias con este nombre` | `src/presentation/workspace.ts:156-196`, `src/domain/agent.ts:186-211`    |
| Live event bus to the UI; sidebar glyph encodes working / awaiting reply / failed turn / idle                                                                                   | `src/presentation/agent-sessions.ts:25, 134-149`, `Sidebar.vue:1137-1202` |

**Files and preview.** CodeMirror 6 with 13 grammars and a searchable override picker; dirty drafts
per file identity; Save / Cancel; compare-and-swap write against on-disk content with a
`file_changed` conflict surfaced; Prettier in a web worker using the nearest config above the file,
producing a draft that aborts if the document moved mid-flight (`DocumentPane.vue:326-350,
503-541, 543-627`). Markdown preview with front matter, local image resolution, sanitized by
DOMPurify, relative `.md` links opening in-pane and `http(s)` links to the browser
(`DocumentPane.vue:953-986, 1161-1179`). SVG, image and video preview from blob URLs with
pixel-differencing so a playing video is not replaced (`src/domain/media.ts:21-48`). Shiki
read-only highlighting in View mode. The Files tree is virtualized at a 64-row window with sticky
ancestor folders and dimmed dotfiles/ignored files (`InspectorPane.vue:114-116, 457-460, 566-592`).

**Chair.** Settings are generated from a schema, so the template has no per-preference branch
(`src/domain/settings.ts:279-421`). Drafts apply on Apply, with Reset to defaults and per-colour
reset only when changed. Writes are queued, applied optimistically, and roll back on rejection
(`App.vue:862-900`). Zoom is 8 steps from 80 % to 150 % applied as CSS `zoom` on the root — not
webview zoom — so xterm re-rasterizes (`src/domain/zoom.ts:14-98`). Layout and per-checkout UI state
live in SQLite, clamped on read, debounced, and flushed before close. Settings live outside the
database, in `~/.muster/config.yml` (`src/domain/settings.ts:1-8`).

**Persistence.** `SCHEMA_VERSION = 17`, one schema, no ladder; a file stamped with any other version
is refused by name (`src-tauri/src/persistence/mod.rs:19, 2570-2585`). Ten tables: `repos`,
`checkouts`, `sessions`, `recent_paths`, `preferences`, `checkout_terminal_layouts`,
`viewed_files`, `checkout_ui_states`, `review_notes`, `review_rounds` (`mod.rs:2589-2677`).

**Security.** Eight capabilities, asserted by value in `main.rs:390-415`; no `shell`, no `fs`, no
`http`. Strict CSP without `unsafe-eval` or `*`. Three layers of path sandboxing: relative paths
reject `..`, absolutes and any `.git` component; resolution re-canonicalizes and must stay under the
checkout root, catching symlinks that escape; the review export root has its own check
(`services/files.rs:1199-1221`, `services/checkout.rs:43-75`, `services/files.rs:1438-1450`).
The folder that root is resolved from is a preference and never a path: `workdir` joins the
checkout's canonical directory with a fixed name, and the clear refuses that mode outright and
otherwise removes only the names an export writes.
Terminal session IDs are validated against SQLite before every operation. `open_url` accepts only
`http(s)` and is spawned without a shell. macOS hardened runtime. Vendored `glib 0.18.5` security
backport, hash-checked tree-wide by `scripts/patched-glib.mjs`.

---

## 5. Written down, not built

`docs/todo/` retains historical integration specs and a security plan. Codex and Claude Code are
not implemented. `phase-4-pi.md` is superseded research for Pi RPC; current Pi support uses the
extension API documented in `docs/pi-integration.md`.

## 6. Do not claim

| Claim                                                   | Reality                                                                                                                                                                                                                     |
| ------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| "Supports Codex / Claude Code / pi"                     | Codex and Claude Code are specs only. OpenCode and Pi have native integrations; Markdown export is also available.                                                                                                         |
| "It's an IDE" / "it does Git"                           | Git is read-only plus worktree lifecycle. No `add`, `commit`, `stash`, `fetch`, `push`, `merge`, `rebase`, `log` or `blame` anywhere in production code. Branch refs are written only by `worktree add -b` and `branch -D`. |
| "Terminals split into panes"                            | Not reachable. `normalizeTerminalLayout` collapses any stored layout to a single session (`src/domain/terminal-layout.ts:112-135`), and `resizeSplit` / `findSessionTab` / `SplitDirection` have no callers.                |
| "You can stop the agent"                                | `agent_stop` is implemented and nothing calls it. `WorktreeDialog.vue:326-332` blocks removal while an agent is live and offers no way out.                                                                                 |
| "It upgrades across versions"                           | The rename Marvis → Muster is breaking with no migration, by the `v0.x` policy. `marvis.sqlite3` is not read; the user starts empty. See `CHANGELOG.md` → Unreleased.                                                       |
| Anything about `docs/screanshot/` or `docs/demo-video/` | The screenshots are untracked and the video tree is gitignored. Neither is in a clone.                                                                                                                                      |
| Windows                                                 | Not a bundle target. Process introspection and menus return `None` there (`terminal/process.rs:181-197`).                                                                                                                   |

## 7. Known gaps

- **Dead split machinery.** `src/domain/terminal-layout.ts` models `kind: "split"` and takes a
  `split` argument, but `SessionPane.vue` never passes one. Roughly 90 lines of unreachable model.
- **`src-tauri/src/services/executable.rs` (76 lines) is entirely unreferenced.** `find_executable`
  and `is_executable` have no callers outside their own test module. It was written to locate the
  OpenCode binary before the service was found through its registration file. Clippy does not warn
  because the items are `pub` in a binary crate.
- **Match on key position, not character.** `isSlashKey` (`src/App.vue:745-763`) accepts
  `⌘⇧7`, so on a US keyboard a chord that types `&` also toggles panels. The comment states the
  trade knowingly and `App.test.ts` pins both halves.
- **Turn completion is polled at 2 s**, not evented, because the service emits no completion event
  (`src/presentation/agent-sessions.ts:58-64`).
- **OpenCode caps its session list at 50.** The app sends `500` and it is ignored; a relocated
  session that then goes quiet past the newest 50 is dropped. Worked around with a 300 s TTL.
- **Round reconciliation serializes the whole transcript** to string-search it for the marker, with
  no cap before serialization (`services/agent.rs:624-632`).
- **Notes outside a loaded large-diff window** report `unknown` and cannot be resolved; a range
  spanning unloaded pages is refused with an error.
- **Raw `window.confirm` in three places** (`App.vue:1528, 1549`) instead of the app's own
  `ConfirmDialog`.
- **Repo and worktree fold state is memory-only** and lost on reload, by design
  (`Sidebar.vue:1219-1247`).
- **`services/files.rs:665` is an `unreachable!()` panic site** inside production code.

## 8. The MVP cut

**Strict MVP — the whole loop, without the expensive part.** Register repo → create worktree → open
terminal → read the diff → leave a note on a line → export Markdown. Delete the OpenCode bridge
(7.9k lines of Rust, 86 tests) and the story still stands complete, because Markdown is already the
documented universal destination.

**Recommended MVP — the same plus delivery to OpenCode.** That bridge is what separates this from a
diff viewer, and the round reconciliation is the argument. It is already built and tested; dropping
it buys nothing that the pitch needs.

**Not MVP:** command palette, terminal splits, a diagnostics panel, or the other three harnesses.
None exist and none add to the story.

---

## Appendix: the full IPC surface

76 commands registered in `src-tauri/src/main.rs:280-355`.

| Group         | Count | Commands                                                                                                                                                                                                                                                               |
| ------------- | ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| app           | 3     | `app_prepare_exit`, `open_url`, `frontend_diagnostic`                                                                                                                                                                                                                  |
| agent         | 7     | `agent_sessions`, `agent_candidate_sessions`, `agent_relocations`, `agent_agents`, `agent_session_create`, `agent_prompt`, `agent_stop`                                                                                                                                |
| workspace     | 12    | `restore_workspace`, `sync_workspace_repo`, `register_folder`, `list_recent_paths`, `locate_missing_checkout`, `close_checkout`, `close_missing_checkout`, `archive_checkout`, `restore_archived_worktrees`, `set_default_branch`, `select_checkout`, `select_session` |
| files         | 10    | `files_list`, `file_read`, `file_read_media`, `file_probe`, `file_write`, `file_read_prettier_config`, `file_read_markdown_image`, `review_export_markdown`, `review_folder`, `review_folder_clear`                                                                    |
| git           | 9     | `git_status`, `git_diff_stats`, `git_checkout_diff_stats`, `git_diff`, `git_diff_page`, `git_viewed_files`, `git_mark_viewed`, `git_watch_repo`, `git_unwatch_repo`                                                                                                    |
| review notes  | 8     | `review_notes`, `review_note_create`, `review_note_update`, `review_note_delete`, `review_notes_mark_sent`, `review_note_anchors_verify`, `review_note_outdated_clear`, `review_note_resolve`                                                                          |
| review rounds | 6     | `review_rounds`, `review_round_dispatch`, `review_rounds_requeue`, `review_round_flush`, `review_round_reconcile`, `review_round_ack`                                                                                                                                  |
| terminal      | 9     | `terminal_create`, `terminal_write`, `terminal_resize`, `terminal_status`, `terminal_close`, `terminal_rename`, `terminal_move`, `terminal_layout_load`, `terminal_layout_save`                                                                                        |
| worktree      | 4     | `worktree_defaults`, `worktree_create`, `worktree_removal_info`, `worktree_remove`                                                                                                                                                                                     |
| ui state      | 8     | `ui_layout_load`, `ui_layout_save`, `settings_load`, `settings_save`, `checkout_ui_state_load`, `checkout_ui_state_save`, `review_target_load`, `review_target_save`                                                                                                   |
| folder        | 1     | `open_folder`                                                                                                                                                                                                                                                          |

Events from Rust to the webview: `muster://agent-event`, `terminal-output-flow`,
`git-status-changed`, `checkout-file-activity`, `git-worktrees-changed`, `git-watch-failed`,
`git-viewed-changed`.

Git is invoked as `std::process::Command` with `OsString` argv arrays — there is no `git2` and no
`gix`. Reads run with `GIT_OPTIONAL_LOCKS=0` and a 30 s deadline; writes deliberately have no
timeout, because a half-finished write is worse than a wait (`src/git/mod.rs:28-79`).
