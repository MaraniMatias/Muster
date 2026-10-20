# Phase 2 — Codex as a review destination

Status: specified, not implemented. Phase 1 is the prerequisite: it adds
`ReviewTarget` and the `preferences` key, and `"codex"` is added to that union here.

**This is not parity with OpenCode.** The OpenCode integration is a bridge: Muster runs
`opencode serve` per checkout, creates sessions through an HTTP API, streams events, and can stop a
turn (the old agent service; the client that replaced it is `core/src/opencode.rs`). Codex has no
such surface. Phase 2 sends a headless,
one-shot process per review round, which opens a **Muster-managed review conversation**. It does
not write into the session the user has open in a terminal.

That is not a shortcut. Muster _can_ read the foreground process name of its own terminals —
`core/src/pty/process.rs` on macOS via `proc_pidpath` and on Linux via `/proc/<pid>/comm`,
surfaced as `foreground_app` (`core/src/workspace.rs`). What
that name cannot give is the conversation id of a `codex` the user started themselves. There is no
way to address their session, and no authorization to take it over. So the conversation is ours,
and the user gets told which one it is.

## 1. Shape of the integration

|         | OpenCode today                                                      | Codex in phase 2                                                        |
| ------- | ------------------------------------------------------------------- | ----------------------------------------------------------------------- |
| Process | one `opencode serve` per checkout, long-lived (`services/agent.rs`) | one `codex exec` per round, exits when the turn is done                 |
| Session | created through an API, listed, pickable                            | Muster mints an id, one conversation per checkout, reused across rounds |
| Events  | HTTP + SSE from the server                                          | JSONL on stdout, parsed line by line                                    |
| Cancel  | `stopAgent`, awaited by the UI                                      | `turn/interrupt` (app-server) or signal (exec) — see §4                 |
| Picker  | full session list with search                                       | none: there is one conversation, and its name is shown                  |

The session picker in `FileDiff.vue` (`:497-548`) shows only for `opencode`. For `codex` the
control shows the fixed conversation name and nothing else.

## 2. One-shot: `codex exec`

```
codex exec --json --cd <checkout> --skip-git-repo-check -o - <prompt>
```

| Flag                             | Why                                                                                                                                                                                                                                           |
| -------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `exec`                           | the non-interactive subcommand. `--json` (alias `--experimental-json`) makes it emit JSONL on stdout [source: `codex-rs/exec/src/cli.rs`]                                                                                                     |
| `--json`                         | the event stream is the only way to tell turn-started from turn-finished from turn-failed                                                                                                                                                     |
| `--cd <checkout>`                | working directory. The checkout's canonical path, the same one the OpenCode bridge passes to its child (`core/src/opencode.rs`, `Client::directory()`).                                                                                                  |
| `--skip-git-repo-check`          | the checkout is registered and already verified to be a git worktree (`core/src/navigation/discovery.rs`, `RepoKind::Git`); a second check is redundant and would fail on a plain-folder checkout, where notes are already not collected. |
| `-o -` / `--output-last-message` | the final assistant message on stdout, so the diff can show what came back                                                                                                                                                                    |

The prompt goes on **stdin**, not as an argv element. A review is up to 512 KiB
(`MAX_ROUND_PROMPT_BYTES`, `core/src/review.rs`); argv is length-limited by the OS
and shows up in `ps`. `codex exec` accepts the prompt positionally or via `-`/stdin
[source: `codex-rs/exec/src/cli.rs`; learn.chatgpt.com/docs/non-interactive-mode].

**No `--ask-for-approval`.** That flag exists on the TUI only. `exec` forces
`approval_policy=never` and it can be overridden with `-c approval_policy="on-request"`
[source: `codex-rs/exec/src/cli.rs`]. Two consequences, and they are not the same decision:

- `never` is what makes an unattended turn terminate instead of blocking on a prompt nobody can
  answer. It is also the setting under which a misreading of a review comment leads to a write
  without asking.
- `on-request` makes the turn hang forever, because there is no terminal to answer it. The only
  path out is a kill.

Phase 2 ships `never`, and says so in the changelog and in the settings UI. This is a security
decision about an unattended agent, not an argv detail, and it is recorded rather than inherited.

Other flags that exist and are not used: `--ephemeral` (we want the rollout on disk, §5),
`-m/--model`, `-s/--sandbox`, `--add-dir`, `--ignore-user-config`. Whether to pin a model is
**unconfirmed as a product decision**; leaving it unset means the user's own config chooses, which
is the same rule the rest of the app follows.

A nonzero exit follows a non-retryable `error` event, or a `failed`/`interrupted` turn
[source: `codex-rs/exec/src/lib.rs`].

## 3. The events that mean what

`--json` emits one JSON object per line. The ones phase 2 acts on:

| Event            | Means                                                                                                                                        |
| ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `thread.started` | the thread id, a server-generated UUIDv7. The only place the id appears — record it here or not at all. [source: `codex-rs/exec/src/cli.rs`] |
| `turn.started`   | the turn is running. From here, a kill would be a real cancel.                                                                               |
| `turn.completed` | the turn finished. Check the turn's own status before calling it a success.                                                                  |
| `turn.failed`    | the turn failed.                                                                                                                             |
| `item.*`         | per-item progress: file reads, patches, command output. Surfaced, not parsed for control flow.                                               |
| `error`          | a non-retryable error. The process will exit nonzero.                                                                                        |

Failure is not only `turn.failed`: a `turn.completed` can carry a failed status. **Unconfirmed
which** — the exec event schema and the app-server `turn.status` enum are not the same thing, and
phase 2 should treat "a `turn.completed` that is not a clean success" as failed until a real
`codex` says otherwise.

**There is no `turn/failed` method on the app-server.** Failure there is `turn/completed` with
`turn.status = "failed"`, where status is one of `completed | interrupted | failed`
[source: `codex-rs/app-server-protocol/src/protocol/v2/turn.rs`]. A parser written against
`turn/failed` will hang forever waiting for a notification that cannot arrive.

## 4. Cancel

`codex exec` has no protocol-level cancel; the honest answer is a signal to the child, which is
what Muster already does for terminals (`core/src/pty/mod.rs`, `shutdown`).
A SIGTERM to `exec` is **unconfirmed** as producing a `turn.completed` at all — it may simply exit.
If the turn's fate matters (it does: it decides whether a round is `acked` or left `dispatching`),
the app-server route in §6 has a real `turn/interrupt {threadId, turnId}`
[source: `codex-rs/app-server-protocol/src/protocol/v2/turn.rs`].

Phase 2's UI does not offer a cancel button. The user has OpenCode for a live conversation; the
Codex path is a batch send. If a cancel is added, it comes with the app-server, not before it.

## 5. Reading the marker back

A review round must be recognizable after a crash, or an interrupted send is repeated. Phase 1
already established the mechanism: the marker is embedded in the message _before_ the send
(`core/src/opencode/rounds.rs`, `review_round_marker` at `core/src/review.rs`), and reconciliation
asks the transcript whether it is there (`core/src/opencode/rounds.rs`).

Codex persists every thread as
`~/.codex/sessions/rollout-<timestamp>-<conversation_id>.jsonl`
[source: codex-rs session persistence]. Prefer **`thread/searchOccurrences {threadId, searchTerm}`**,
a supported literal-substring search over visible user and final assistant messages that is immune
to JSON escaping [source: `codex-rs/app-server-protocol/src/protocol/v2/thread.rs`]. That method
requires the app-server.

If it is used directly on the rollout file, the search is a naive substring over raw JSONL and it
only works when the marker contains **no character JSON escapes** — newline, tab, CR, `"`, `\`, or
anything non-ASCII. A marker like `muster-review:round:1731...:4122:3` is pure ASCII with no
escapes and is safe. Any change to the marker format has to keep it that way, and that is a real
constraint on `review_round_marker`, not on Codex.

## 6. Sessions, and the one thing that is unverified

**`codex exec resume` — the subcommand's exact name and argv are UNVERIFIED.** This is the single
largest open risk in phase 2, and it is the risk to settle before writing any code, not after.

Without it, the alternatives:

**(a) `codex app-server`** — JSON-RPC 2.0 over stdio, where the `"jsonrpc"` header is omitted on
the wire [source: `codex-rs/app-server-protocol`]. It requires `initialize` then `initialized`;
`thread/start` takes `cwd` and creates a thread **with no turn**; ids are server-generated UUIDv7
with no client-supplied id; `thread/resume {threadId, cwd?}`, `thread/list`,
`thread/read {threadId, includeTurns}`, `turn/start`, `turn/interrupt`, `turn/steer` all exist, and
approvals arrive as server-initiated requests
[source: `codex-rs/app-server-protocol/src/protocol/v2/thread.rs`, `v2/turn.rs`, `common.rs`].
`--cd` **parses on `app-server` but is not read by the dispatch arm — treat it as a no-op**; pass
`cwd` through `thread/start` [source: `codex-rs/cli/src/main.rs`].

The cost of (a) is a long-lived process to manage, and the cost of that cost is the risk below.

**(b) `codex exec` every time, no resume.** Every round is a fresh thread. Simple, stateless,
honest — and it loses the conversation the user would want to see the review in. `thread/list` and
`thread/read` (app-server) can still find previous threads for display, if app-server is run just
for reading.

**The risk that decides it.** The docs describe app-server as "primarily for development and
debugging and may change without notice" [source: learn.chatgpt.com app-server docs]. Its README
hedges that an unused thread is not guaranteed to survive a restart. So: **whether a zero-turn
`thread/start` thread survives a cold process start is unconfirmed.** If it does not, the
"create a session without a model call" requirement has no answer on that surface, and
conversation-per-checkout is not achievable on app-server either.

Auth: `codex login`, `--with-api-key`, `--with-access-token`, `--device-auth`, `codex login
status`; credentials live in `~/.codex/auth.json` or the OS credential store; `CODEX_HOME` and
`CODEX_API_KEY` override [source: codex-rs auth docs]. Auth is required at `turn/start`, not at
startup, so a process can boot without credentials and fail on the first turn. `codex proto` and
`codex mcp-server` were **removed in `rust-v0.43.0`** — do not build against them.

**Recommended sequence**, in this order, each step cheap enough to abandon:

1. Install a real `codex` and run `codex exec resume --help`. Record the exact subcommand, argv,
   and whether it accepts a thread id. Everything else in this document depends on the answer.
2. With a real `codex`, run `codex app-server`, `initialize` / `initialized`, `thread/start` with a
   `cwd`, exit the process, start a new one, and `thread/resume` that id. If the thread is gone,
   stop and re-plan — do not paper over it with a `thread/list` scan that guesses by timestamp.
3. Only then write the service.

## 7. The round lifecycle

The ordering Muster already uses, unchanged:

1. `begin_round` records the round as `dispatching` with its marker and links the notes
   (`core/src/opencode/rounds.rs`). Notes go to `sent` **here**, before the agent is called, so a
   crash cannot leave notes looking delivered when they were not.
2. `build_round_prompt` prepends `AGENT_PROMPT_PREFIX` and `[marker]`
   (`core/src/review.rs`).
3. `check_round_prompt` refuses an empty or >512 KiB prompt (`core/src/review.rs`).
4. Spawn `codex exec`, feed the prompt on stdin, read the event stream.
5. On success, `confirm_round` → `dispatched`. On failure, leave it `dispatching` — the marker is
   what lets reconciliation learn whether it landed (`core/src/opencode/rounds.rs`).
6. On reconnect, `reconcile_round` asks whether the marker is in the transcript and either confirms
   or requeues (`core/src/opencode/rounds.rs`).

The thread id from `thread.started` must be stored on the round. `ReviewRound.session_id` is
already `Option<String>` and already the field reconciliation reads
(`src/domain/review.ts:39`), so this is a value, not a schema change. The one thing to decide:
where it is written before the process exits, because a crash between `thread.started` and the
`confirm_round` leaves a `dispatching` round whose thread id was never stored. **Unconfirmed** how
to handle; storing it the moment the event arrives is the obvious answer and means a partial write
path, not just a success path.

## 8. Finding the `codex` binary

`services/executable.rs::find_executable` searches `PATH`, then
`/opt/homebrew/bin`, `/usr/local/bin`, then `$HOME/.opencode/bin` and `$HOME/.local/bin`
(`:8-29`). Codex is usually on `PATH`; if it is not, `$HOME/.local/bin` is already covered and
`~/.codex/bin` is **not** in the list. Add it or do not — unconfirmed which. The
`OPENCODE_UNAVAILABLE` message (`services/agent.rs:892-893`) is the model for the Codex equivalent,
and the OpenCode one must survive untouched: the two integrations are independent and one being
broken is not evidence about the other.

## Unverified / risks

- **`codex exec resume` does not exist in this document's knowledge.** Exact subcommand name and
  argv unverified. Everything about "reopen that same session" is downstream of it.
- **A zero-turn `thread/start` thread surviving a cold restart is unconfirmed**, and app-server is
  documented as unstable. If it does not survive, conversation-per-checkout is not available.
- Whether `turn.completed` in `exec --json` can carry a failed status, and under what field name.
  **Unconfirmed.** Treat anything but a clean completion as failure until a real binary says
  otherwise.
- Whether SIGTERM to `codex exec` yields a `turn.completed`. **Unconfirmed.** If it does not, a
  killed turn is indistinguishable from a crashed one and the round stays `dispatching` forever
  until a user acts.
- Codex's own version drift: the event names above come from a moving source tree. Pin nothing, but
  treat a parse failure on a known event as a hard error with the raw line in the log, never as a
  silently ignored line.
- `-c approval_policy=...` is documented for `exec`; whether a `-c` string survives being passed
  through a shell-free `Command` is a detail, but the flag name itself is only sourced to
  `codex-rs/exec/src/cli.rs` and has not been run.
- The rollout filename format `rollout-<timestamp>-<conversation_id>.jsonl` is sourced to Codex's
  persistence layout, not to a documented contract. If phase 2 ever greps it directly, that grep
  is a compatibility liability. `thread/searchOccurrences` exists precisely to avoid it.

## What a fake-process test proves

A fake `codex` — a shell script on `PATH` that prints canned JSONL and exits — proves **our**
side and nothing about Codex's: that the parser classifies `thread.started` / `turn.started` /
`turn.completed` / `turn.failed` / `error` correctly, that a partial line at EOF is not mistaken
for a complete record, that a nonzero exit with no error event is still a failure, that stdin is
closed after the prompt so the process can finish, that the thread id is stored before the round is
confirmed, and that a crash between those two leaves a `dispatching` round reconciliation can
resolve.

It does **not** prove that an uninstalled CLI accepts the real argv. A fake that ignores its
arguments passes every test in the list and still ships a phase that fails on the first real
`codex exec`, because a flag was renamed, a subcommand never existed, or `--json` now means
something else. Nothing short of a real binary catches that.

The live test follows `docs/agent-live-tests.md`: `#[ignore]`d, opt-in via `--ignored`, with
`MUSTER_AGENT_BRIDGE_DIR`-style dedicated temporary directories, and a test that **fails with the
name of the missing variable** rather than passing without running. Codex's live test additionally
needs credentials and spends real tokens per run, so it should be one test, not six.
