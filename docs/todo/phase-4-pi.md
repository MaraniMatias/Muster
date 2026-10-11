# Phase 4 — Pi as a review destination (superseded research)

**Superseded.** This is historical research for Pi 0.87.1's RPC mode, which Muster did not
implement. The current integration uses the Pi v1.1.0 extension API and protocol v2; see
[the current Pi integration inventory](../pi-integration.md) for implemented behavior, tests,
and limitations. Keep this research for provenance, not as an implementation plan.

Status: superseded by the implementation documented in `docs/pi-integration.md`.

**Not parity with OpenCode.** As in every non-OpenCode phase: Muster can name the program in front
of one of its terminals (`src-tauri/src/terminal/process.rs:21-34` macOS, `:55` Linux; surfaced as
`foregroundApp`, `src-tauri/src/domain/workspace.rs:83-89`), but the name `pi` does not carry that
process's conversation id, and there is no authorization to write into a session the user started.

pi is the friendliest of the three for an unattended host: strict line framing, a documented
command set, a caller-chosen session id that creates if absent, and a cancel that is a protocol
record. It also has the hardest startup blockers, all of which are **eager** and all of which kill
the process before a turn can start. Both halves of that sentence matter and neither cancels the
other.

Findings below are against a **local 0.87.1 install** of `@earendil-works/pi-coding-agent`
(`earendil-works/pi`), plus that install's bundled `docs/`. Citations are to
`docs/rpc.md`, `docs/rpc-commands.md`, `docs/json.md`, `docs/message-types.md`,
`docs/cli.md`, `docs/session-format.md`, `dist/main.ts` (via `main.js.map` `sourcesContent`),
`dist/core/session-manager.js:290`, and the session file on disk.

## 1. Transport: `--mode rpc`

```
pi --mode rpc
```

with the child spawned **in the checkout directory**. That is the minimum argv and it is sufficient
[source: `docs/cli.md`].

**`--mode rpc` is strict JSONL, not JSON-RPC.** One object per line, split only on `\n`; an
optional preceding `\r` is stripped to accept CRLF input [source: `docs/rpc.md`]. pi's own docs
warn explicitly that Node `readline` also splits on `U+2028` and `U+2029`, which are **valid
inside JSON strings** — so a generic line reader silently corrupts any message containing them. In
Rust this is a `BufReader` over `read_until(b'\n')`, which is right by construction. The warning
is recorded here because the obvious implementation (a line-reader crate) is the wrong one and
nothing about it will fail loudly.

Shapes:

```json
{"id":"req-1","type":"get_state"}
{"id":"req-1","type":"response","command":"get_state","success":true,"data":{ … }}
```

Failure is `success:false` with an `error` on the same record
[source: `docs/rpc.md`, `docs/rpc-commands.md`]. A malformed command produces a parse response
with **no** id: `{"type":"response","command":"parse","success":false,"error":"Failed to parse
command: …"}` [source: `docs/rpc.md`].

**Events carry no id** — session events describe activity, not a request. The one exception is
`bash_execution_update`, which repeats the id of the `bash` command that produced it
[source: `docs/rpc.md`]. So the reader has two independent things to key on: `id` for responses,
`type` for events.

**Shutdown is closing stdin. There is no shutdown command** [source: `docs/rpc.md`]. pi disposes
the active runtime and exits; the client should still handle signals and unexpected exits.

## 2. What is and is not needed in the argv

| Flag                  | Use it?           | Why                                                                                                                             |
| --------------------- | ----------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `--mode rpc`          | **yes**           | the transport                                                                                                                   |
| cwd = checkout        | **yes**           | the child's working directory; there is no flag for it                                                                          |
| `-a` / `--approve`    | **conditionally** | see below                                                                                                                       |
| `--session-id <uuid>` | **yes**           | see §5                                                                                                                          |
| `--session-dir`       | optional          | only if the default location is not wanted                                                                                      |
| `--no-session`        | **no**            | it is the opposite of what is wanted: an in-memory session that is not persisted, which makes the round unrecoverable           |
| `@file` args          | **never**         | RPC mode **rejects** `@file` prompt arguments; send prompts through the `prompt` command [source: `docs/rpc.md`, `docs/cli.md`] |

**No `--approve` is needed to avoid blocking.** In RPC mode `hasUI` is false, and project trust
resolves to _untrusted_ without prompting rather than hanging [source: `dist/main.ts`]. The cost of
that silence is real though: **project-local config is silently ignored** — settings, extensions,
skills and prompt templates supplied by the working folder are not loaded [source:
`docs/security.md`, `docs/cli.md`]. So pass `-a` if that project-local config matters to the user,
and do not pass it if loading a folder's extensions automatically is a risk you would rather not
take. **Which way is unconfirmed as a product decision**, and it is not a detail: `-a` changes
which code runs at startup in a directory Muster did not write.

Note what project trust does _not_ do, from pi's own docs: it is not a startup boundary, it does
not limit what tool calls can reach, and the project `sessionDir` setting is read _before_ trust
resolves [source: `docs/security.md`]. Trust here is a resource-loading switch, not a sandbox, and
the doc should not be written as though it were.

## 3. Turn boundaries — `agent_settled`, not `turn_end`

The stream [source: `docs/json.md`, `docs/message-types.md`]:

```
agent_start → turn_start → message_start / message_update / message_end → turn_end → agent_end → agent_settled
```

| Event                                              | What it means                                                                               |
| -------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| `agent_start`                                      | a low-level agent run started                                                               |
| `turn_start`                                       | one assistant turn started                                                                  |
| `message_start` / `message_update` / `message_end` | streaming content for one message                                                           |
| `turn_end`                                         | one assistant response and its tool calls finished                                          |
| `agent_end`                                        | **that low-level run** ended; `willRetry` says whether a retry is coming                    |
| `agent_settled`                                    | **pi will not continue automatically** — no retries, no compaction recovery, no queued work |

**Use `agent_settled` as the turn-finished signal, not `turn_end` and not `agent_end`.**
`agent_end` closes one low-level run, and automatic retry, overflow recovery, compaction retry,
steering or follow-up work can still continue after it [source: `docs/json.md`]. A round acked on
`turn_end` would be closed while pi is still working on it, and a round acked on `agent_end` would
be closed before a retry that may change the answer.

**Failure is a `turn_end` whose `message.stopReason === "error"`** [source:
`docs/message-types.md`, `docs/json.md`]. There is no separate failure event, and a
`prompt` response means _accepted_, not done — provider failures appear **only** in the event
stream, never in the command response [source: `docs/rpc.md`]. A client that treats a successful
`prompt` response as completion will report every provider failure as a success.

So the rules, in one place:

- subscribe to events **before** sending the prompt, or a fast completion is missed
  [source: `docs/rpc.md`];
- turn started: `turn_start`;
- turn finished: `agent_settled`;
- turn failed: a `turn_end` with `message.stopReason === "error"` seen before `agent_settled`;
- never: a `prompt` response.

## 4. Cancel

`{"type":"abort"}` — and the semantic detail that matters: **it waits for the session to become
idle before responding** [source: `docs/rpc-commands.md`]. So a cancel has a bounded but
unbounded-in-advance cost; a caller with no timeout will wait for a stuck tool.

For Esc-like behaviour, **send `clear_queue` first**: it removes queued steering and follow-up
messages and returns their text, and `abort` on its own _continues_ queued messages that remain in
the session [source: `docs/rpc-commands.md`]. Also available: `abort_bash` (one bash command) and
`abort_retry` (a pending automatic retry).

This is the best cancel story of the three non-OpenCode harnesses: it is a protocol record, it is
documented, and it does not need a signal convention.

## 5. Sessions

`--session-id <id>` **opens the session if it exists and creates it if it does not** [source:
`docs/cli.md`]. That is the clean path and it removes the mint-then-resume dance phases 2 and 3
have to negotiate: there is no separate creation step to get wrong.

- Ids match `^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$` [source:
  `dist/core/session-manager.js:15-19`], so a **UUIDv7 passes** — its hyphens are allowed and it
  starts and ends alphanumeric.
- `--session-id` **conflicts with `--session`, `--continue` and `--resume`**, and combining them is
  a hard exit 1 [source: `docs/cli.md`, `dist/main.ts` (`validateSessionIdFlags`)].
- `--session-id` conflicts _not_ with `--fork`, which is how you choose an id for a fork.

**There is no `list_sessions` command.** `new_session` returns **no id** in its response — only
`{"cancelled": …}` — so a second `get_state` is needed for `data.sessionId`
[source: `docs/rpc-commands.md`]. Since `--session-id` creates if absent, `new_session` is not
needed at all; the flow is spawn with the id, then `get_state` to confirm the id pi actually took
and to read `sessionId`, `sessionFile` and `messageCount` [source: `docs/rpc-commands.md`].

Session files are
`~/.pi/agent/sessions/--<cwd with / \ : → -->  /<ISO8601>_<uuid>.jsonl`, where the directory
encoding is `getDefaultSessionDirPath` [source: `dist/core/session-manager.js:286-295`,
`docs/session-format.md`]. The encoding is a **derived** detail: if Muster needs it, it should
call pi for the path rather than reimplementing the rule, because the rule is a private function
that can change. A real local session file confirms the shape — a header line
`{"type":"session","version":3,"id":"…","timestamp":"…","cwd":"…"}` followed by `model_change`,
`message` and other entries [source: local file inspection, 0.87.1].

## 6. Reading the marker back

**Verified on a real local file: the user's exact text is stored verbatim.** So a literal substring
search for `muster-review:<round id>` over the session file works, and the marker
(`src-tauri/src/domain/review.rs:67-69`) is pure ASCII with no JSON-escapable characters — the same
constraint as Codex, for the same reason: a marker containing a quote or a newline would not
survive a naive search.

Read-back commands, in the order a round would use them
[source: `docs/rpc-commands.md`]:

| Command                   | Use                                                                                                                                                                                                                                                                                                                                               |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `get_messages`            | messages on the active branch, for display                                                                                                                                                                                                                                                                                                        |
| `get_entries {since?}`    | **the one to use for reconciliation.** Entries are an append-only tree with stable ids, so an entry id works as a durable cursor **across client restarts**; `since` returns only entries strictly after it, and includes pre-compaction history and abandoned branches. If `since` matches nothing, the response is `success:false` — handle it. |
| `get_tree`                | the whole session as a tree; the result is an array because navigation can create multiple roots                                                                                                                                                                                                                                                  |
| `get_last_assistant_text` | the last assistant text, `null` if none                                                                                                                                                                                                                                                                                                           |

`get_entries` plus a stored entry id is strictly better than a file grep for the _incremental_ case
— it is the protocol's own cursor and it survives a restart. The file grep is the fallback for
when a round predates any entry id we stored.

## 7. The hard blockers

These are the reason phase 4 is not the shortest of the three. All are **eager**: they happen
before a turn can start.

1. **A model must already be selected, or pi exits 1 at startup when not interactive.** The check
   is `appMode !== "interactive" && !session.model` → print `No models available…` (or
   `No model selected…`) and `process.exit(1)` [source: `dist/main.ts`]. Credentials are therefore
   required **eagerly, not lazily**: there is no RPC handshake Muster can complete first. A user
   with pi installed but unauthenticated gets a process that dies before the protocol starts, and
   the error is on stderr, not in the event stream. That must be surfaced as a real message and
   not as "the agent produced no output".
2. **A missing cwd for a foreign session hard-exits 1** [source: `dist/main.ts`,
   `MissingSessionCwdError`]. Since the cwd is the checkout and the session id is Muster's, the
   pair must always be consistent; a session id from another checkout is an error, not a fallback.
3. **A model-catalog refresh fires in the background at RPC startup with a 15 s timeout**
   [source: `dist/main.ts`: `modelRuntime.refresh({signal: AbortSignal.timeout(15e3)})`, guarded by
   `!offlineMode && appMode === "rpc"`]. It is backgrounded and its failure is swallowed
   (`.catch(() => {})`), so it does not block — but it does mean network traffic at startup.
   **`--offline` or `PI_OFFLINE=1` disables it** [source: `docs/cli.md`, `dist/main.ts`]. For a
   review send, `--offline` is probably the right default (nothing in the review needs a catalog
   refresh, and a review must not hang on the network) — **unconfirmed as a product decision**, and
   it interacts with credential resolution, so settle it before implementing.
4. **Node ≥ 22.19** [source: `package.json` `engines.node`]. Muster does not care, but the
   installed `node` does, and a user on an older Node gets a pi that will not start.
5. **`~/.local/bin/pi` is a POSIX shell launcher into a Node bundle, not a native binary**
   [source: local inspection: a `#!/bin/sh` script resolving a version file under
   `~/.pi/agent/install/` and `exec`ing `node_modules/.bin/pi`]. Two consequences: the launcher
   needs a shell and a writable install root, and process management that assumes a single binary
   (signal delivery, a process tree) is talking to a wrapper. `services/executable.rs` already
   handles the "not on PATH" case by probing `~/.local/bin` (`:21-27`), so the symlink resolves —
   but `Command::new` on it runs `/bin/sh`, which changes what a signal reaches.

**No built-in permission/approval event was confirmed.** Approvals appear extension-mediated
[source: absence of any approval record in `docs/rpc.md` / `docs/rpc-commands.md`]. The security
implication is the one from `docs/security.md`: pi "does not ask for approval before every tool
call", so a review sent to it can read, change and execute files with the permissions of the
account, and project trust does not bound that. Phase 4 must not be described as safer than it
is.

## 8. Round lifecycle

Identical to the other phases, and for the same reasons
(`src-tauri/src/services/review_round.rs`):

1. `begin_round` records the round as `dispatching` with its marker and marks the notes `sent`,
   before the agent is called (`:54-68`, `:50-53`).
2. `build_round_prompt` prepends `AGENT_PROMPT_PREFIX` and `[marker]` (`:36-40`).
3. `check_prompt_size` refuses empty or >512 KiB (`MAX_ROUND_PROMPT_BYTES`,
   `src-tauri/src/domain/review.rs:7`; check at `:288-299`).
4. Spawn `pi --mode rpc` in the checkout, send `prompt`, watch for `turn_start` / `agent_settled`,
   read failure from `turn_end` + `stopReason`.
5. Success → `confirm_round` → `dispatched`. Failure → leave `dispatching` (`:236-239`).
6. On reconnect, `reconcile_round` asks whether the marker is in the session (`:161-191`).

The session id is known before the process starts, so it can be written to the round up front —
the one thing that makes a crash before the first event fully recoverable. `get_state` right after
startup confirms the id pi took and gives `sessionFile` for the read-back path.

Because the process is a long-lived RPC child and not a one-shot, phase 4's shutdown is **closing
stdin** (§1), not a signal — and the child must be reaped. `services/terminal/mod.rs` already has
the reader-thread-and-reap pattern for PTY children (`stop_with_timeout`, and the `stop_all` sweep
at `services/agent.rs:1631-1638`); a review child is not a terminal, so it belongs in a new
service with the same discipline rather than in the terminal backend.

## 9. Finding the `pi` binary

`services/executable.rs::find_executable` searches `PATH`, `/opt/homebrew/bin`,
`/usr/local/bin`, `$HOME/.opencode/bin`, `$HOME/.local/bin` (`:8-29`). A standard pi install
symlinks into `~/.local/bin`, so it is found. `~/.pi/agent/bin` is **not** in the list — add it or
do not, **unconfirmed**, and note the `~/.local/bin` entry is a symlink to a shell script, so
`is_executable` (`:31-47`) is what actually decides it. The `OPENCODE_UNAVAILABLE` message
(`services/agent.rs:892-893`) is the model for the pi equivalent; pi's startup failures are
different in kind (see §7) and want their own messages.

## Unverified / risks

- **`--offline` (or `PI_OFFLINE=1`) as the default for a review send is unconfirmed** and interacts
  with eager credential resolution (§7.3).
- **Whether `-a` / `--approve` should be passed is unconfirmed** and is a security decision, not a
  default (§2).
- **No built-in permission/approval event was confirmed**; approvals appear extension-mediated
  (§7). An unattended review therefore runs with the user's full account permissions and nothing in
  the protocol asking first. Document that in the changelog the way
  `docs/phase-3-claude-code.md` §7 documents its permission decision.
- **pi 0.87.1 is the verified version.** The event and command names are sourced to that install's
  bundled `docs/` and `dist/`. A newer release may add or rename records; a parser must fail loudly
  on an unknown `type` rather than ignore it, because "pi settled and we missed it" and "pi is
  still working" look identical from outside.
- **The `~/.pi/agent/sessions/--<encoded cwd>--` directory name is derived from a private
  function** (`getDefaultSessionDirPath`, `dist/core/session-manager.js:290`). Reimplementing that
  encoding in Rust is a compatibility liability; prefer `get_state`'s `sessionFile`.
- **`new_session` returns no id**, so any flow that reaches for it needs a second `get_state`
  (§5). `--session-id` avoids it; if `new_session` is ever used anyway, do not assume the response
  carries one.
- **The launcher is a shell script** (§7.5). Signal delivery and child reaping were not verified
  against it, and a shell wrapper changes the process tree a stop has to reach.
- Whether a **background model-catalog refresh can print to stdout** and corrupt the JSONL stream.
  `docs/rpc.md` says stdout is reserved for protocol records and diagnostics go to stderr, and the
  refresh failure is swallowed — but a _successful_ refresh's logging was not checked.
  **Unconfirmed.**
- `get_entries {since}` returning `success:false` for an unknown `since` is documented; whether a
  compaction rewrite can invalidate a previously valid entry id is **unconfirmed**.

## What a fake-process test proves

A fake `pi` — a script that reads JSONL commands on stdin and writes canned records on stdout,
then exits when stdin closes — proves our side, and here it proves a lot, because the protocol is
line-oriented and a fake is trivially faithful to it: that responses are correlated by `id` and
events by `type`; that the id-less parse response is handled without corrupting the reader; that
`agent_settled` (not `turn_end`, not `agent_end`) ends the round; that a `turn_end` with
`stopReason: "error"` marks it failed; that a successful `prompt` response is **not** treated as
completion; that the event listener is installed before the prompt is written; that
`clear_queue` precedes `abort` when Esc-like behaviour is wanted; that a `get_state` right after
startup captures the id; and that closing stdin shuts the child down, because there is no shutdown
command to send.

It does **not** prove an uninstalled CLI accepts the real argv, and here the gap is larger than in
the other phases, because pi's failure modes are eager (§7). A fake that ignores its arguments
passes every one of those tests and still ships a phase that dies at startup on a user's machine
with:

- no model selected (exit 1, message on stderr, no protocol at all);
- a missing or foreign cwd for the session (exit 1);
- Node older than 22.19;
- a `~/.local/bin/pi` whose launcher cannot `exec` its bundle.

A fake also cannot tell us whether the background catalog refresh writes anything to stdout and
corrupts the stream, or whether `--session-id` combined with `--offline` still resolves
credentials. The first of those is the kind of bug that only appears under a real network.

The live test follows `docs/agent-live-tests.md`: `#[ignore]`d, opt-in with `--ignored`, dedicated
temporary directories, and a missing required variable **fails the test by name** rather than
passing without running. It needs real credentials, and unlike the Codex and Claude Code live tests
it does not spend tokens on the happy path if it uses `get_state` and `abort` only — which is
another argument for having the marker reconciliation test hit the transcript rather than a
model call.
