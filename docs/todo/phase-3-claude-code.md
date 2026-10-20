# Phase 3 — Claude Code as a review destination

Status: specified, not implemented. Depends on phase 1 for `ReviewTarget`, and borrows phase 2's
one-shot shape (`docs/todo/phase-2-codex.md`).

**Not parity with OpenCode.** Same reasoning as every non-OpenCode phase: Muster can name the
program in front of one of its terminals (`core/src/pty/process.rs`; surfaced as `foreground_app`,
`core/src/workspace.rs`) but the name
`claude` does not carry that process's conversation id, and there is no authorization to write
into a session the user started. So Claude Code is driven headless, into a **Muster-managed
conversation**, one process per round.

Claude Code's own situation is the worst of the four harnesses for an unattended host, and the
doc says so up front rather than at the end: **the transport is solid, the session lifecycle has a
verified hole, and the permission model has to be answered by a person before any code is
written.**

## 1. Transport: persistent print mode with stream-json

The argv is not a guess. It is taken from Anthropic's own spawner inside
`@anthropic-ai/claude-agent-sdk@0.3.284` (`sdk.mjs`), which pushes exactly:

```
--output-format stream-json  --verbose  --input-format stream-json
```

and **never** passes `--print` / `-p` — zero occurrences in that file [source: `sdk.mjs`,
`0.3.284`]. Using the SDK's own argument list rather than the docs' is deliberate: it is the one
list that is exercised against a real model by Anthropic's own test suite.

| Flag                          | Why                                                                       |
| ----------------------------- | ------------------------------------------------------------------------- |
| `--input-format stream-json`  | we write user messages as JSON records on stdin, not one argv string      |
| `--output-format stream-json` | the only output format with per-turn structure                            |
| `--verbose`                   | required alongside `stream-json`; without it the output is not structured |
| cwd                           | the checkout's canonical path, as a child process cwd, not a flag         |

A user message is one record on stdin:

```json
{
  "type": "user",
  "message": { "role": "user", "content": [{ "type": "text", "text": "<review markdown>" }] },
  "parent_tool_use_id": null
}
```

[source: `sdk.mjs`, `sdk.d.ts`]

**The Agent SDK adds nothing here.** It is a wrapper around the same binary, and it is
JS/TS-only, so it cannot be the transport for a Rust host anyway. Spawn the binary.

## 2. Turn boundaries

| Record                                  | Means                                                     |
| --------------------------------------- | --------------------------------------------------------- |
| `{"type":"system","subtype":"init", …}` | startup. Carries `session_id` and a `capabilities` array. |
| assistant / user message records        | progress                                                  |
| `{"type":"result"}`                     | **the turn is over**                                      |

`result` carries a `subtype` from
`success | error_max_turns | error_during_execution | error_max_budget_usd | error_max_structured_output_retries`,
plus `is_error`, `session_id` and `user_message_uuid` [source: `sdk.d.ts`]. So failure is a
`result` with a non-`success` subtype or `is_error: true` — not a separate event. A parser that
waits for a failure event hangs.

`session_id` on the `result` is a second, independent source of the session id. Prefer it over
scraping the `init` record, and cross-check the two; a mismatch is a version problem worth
surfacing rather than smoothing over.

**A turn can also end without a `result`.** See §3.

## 3. SIGINT and SIGTERM are not the same, and this is the load-bearing detail

| Signal      | Result                                                                                                    |
| ----------- | --------------------------------------------------------------------------------------------------------- |
| **SIGINT**  | ends the turn cleanly. The `result` is written.                                                           |
| **SIGTERM** | **exits 143, leaves the turn unfinished, and records no `result` for it** [source: `sdk.mjs`, `0.3.284`]. |

The consequence for Muster is concrete: a round killed with SIGTERM is a round whose
`result` never arrives, so "did the turn finish?" has no answer from the protocol. A round in
that state has to be left `dispatching`, exactly as a crashed send is
(`core/src/opencode/rounds.rs`) — recognizable, not repeated. That is the
existing mechanism doing its job, but it means **Muster must send SIGINT, not SIGTERM**, and that
is a code-level requirement with a comment on it, because SIGTERM is the reflex and a refactor to
"standardize the shutdown signal" would break the round bookkeeping silently.

There is a second-order effect: **an interrupted turn stays interrupted on resume** unless
`CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1` is set [source: `sdk.mjs`]. So a user who resumes the
Muster conversation by hand after an interrupted turn gets the interruption again. Decide whether
Muster sets that variable on the child it spawns, and write down which way — **unconfirmed as a
product decision.**

## 4. Cancel

Cancel is a control record on stdin, not a signal:

```json
{ "type": "control_request", "request_id": "muster-cancel-1", "request": { "subtype": "interrupt" } }
```

[source: `sdk.mjs`]. This is the only in-protocol cancel, and it is strictly better than §3's
signal for a turn we started ourselves: it ends the turn cleanly and the `result` still arrives.

**Feature-detect it.** The `system/init` record carries a `capabilities` array; look for
`interrupt_receipt_v1` and only send the record when it is there [source: `sdk.d.ts`]. Do not
gate on a Claude Code version number — the version is not the contract, the advertised capability
is.

## 5. Sessions: the mint, the resume, and the hole

The lifecycle, as documented:

- `--session-id <uuid>` is **caller-chosen** and combines with `--input-format stream-json`
  [source: `sdk.d.ts`].
- `--resume` on an **unknown** id is a **hard error**: `No conversation found with session ID:
<id>`, exit 1 [source: `sdk.d.ts`].
- A `-p` run that produced **no model turns** still creates a resumable session — fixed in
  2.1.187 per the changelog [source: Claude Code CHANGELOG]. Before that fix, a zero-turn session
  did not exist to resume, which is why the version floor is not optional.
- `--resume` also accepts an absolute path to the `.jsonl`.
- Sessions created with `-p` are excluded from `claude --continue` and from the interactive
  picker. That is fine and even desirable: Muster's conversation is Muster's.

**The recipe is therefore: mint with `--session-id <uuid>`, later resume with the same id.** The
Muster id is a UUIDv7 generated per checkout, stored where `ReviewRound.session_id` already lives
(`src/domain/review.ts:39`, `Option<String>`), and the name shown to the user is derived from it.

**The exact minting argv is [unverified].** Whether a no-turn session can be created without a
`result` record — that is, whether a process that is started, told a session id, and then closed
produces a resumable conversation — is the open question. The 2.1.187 fix makes the _case_
supported; it does not say a turn-less session is written. Two ways to close the gap, in order of
preference:

1. Start the process, write one user record, read until the first assistant record, then interrupt.
   Costs one model call and is guaranteed to leave a session.
2. Start and close without any turn, then `--resume` the id and check. If the resume fails with the
   hard error above, fall back to (1) permanently.

**Testing for the resume error is mandatory either way.** `--resume` on an unknown id is exit 1
and a specific message, so a wrong guess fails loudly rather than quietly sending a review into a
new conversation. That is the good case, and phase 3 should lean on it.

**There is no CLI command that lists sessions.** The supported API is the SDK's `listSessions` /
`getSessionMessages`, and those are **JS/TS-only — there is no Rust path.** From Rust the options
are reading the transcript directly, or shelling out to a Node helper [source:
`@anthropic-ai/claude-agent-sdk` exports]. Any session picker in the Claude Code phase therefore
needs one of those two, and that is a much larger commitment than the Codex or pi phases, which
both have a listing surface (`thread/list`, `get_state` / `SessionManager.list`). **Phase 3 ships
with no picker.** One conversation per checkout, named, shown, not chosen.

## 6. Reading the marker back

Transcripts are `~/.claude/projects/<project>/<session-id>.jsonl`, where `<project>` is the cwd
with non-alphanumerics replaced by `-` [source: Claude Code docs].

**The docs state this format is internal to Claude Code, changes between releases, and that scripts
parsing it directly can break on any release** [source: code.claude.com/docs/en]. So:

- User message text is stored verbatim, which means a literal substring search for
  `muster-review:<round id>` works. The marker (`core/src/review.rs`) is pure
  ASCII with no JSON-escapable characters, so the same constraint as Codex holds and the same
  reasoning applies.
- The cwd encoding and the file layout are **unsupported**. Reconstructing a path from a cwd by
  hand is the fragile move; prefer `--resume <id>` and let the binary find its own file.

There is no equivalent of Codex's `thread/searchOccurrences`. Marker read-back is a grep over a
file whose format is documented as unstable — record that as a known liability in the code, near
the grep, not only in this file.

## 7. Permissions: the question that has to be answered by a person

`--permission-mode` accepts
`default | acceptEdits | plan | auto | dontAsk | bypassPermissions | manual`, plus `--allowedTools`,
`--disallowedTools`, `--dangerously-skip-permissions`. The SDK pairs `bypassPermissions` with
`--allow-dangerously-skip-permissions` [source: `sdk.d.ts`, `sdk.mjs`].

**Without a mode that never prompts, a turn hangs forever.** There is no terminal attached to answer
it, and no user is watching a process they did not start. That rules out `default` for a
background turn. The remaining options are not equivalent:

| Mode                                                   | Consequence                                                                                     |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------- |
| `dontAsk`                                              | decline anything that would prompt; the turn continues. Narrowest of the never-prompting modes. |
| `acceptEdits`                                          | file edits are accepted without asking.                                                         |
| `bypassPermissions` / `--dangerously-skip-permissions` | nothing is asked about anything.                                                                |

How an unattended agent should handle approvals is a **security decision, not an argv detail**,
and it must be settled explicitly before this phase is implemented — by a person, in the changelog
and in the settings UI, with the default named. Codex's equivalent (`approval_policy=never`) is
recorded in `docs/todo/phase-2-codex.md`; Claude Code's has a `bypassPermissions` option Codex does
not, so "the same decision as Codex" is not a sufficient answer here. The floor is: whatever is
chosen, it is documented, it is visible, and the user can tell what it is from the app without
reading a source file.

`--allowedTools` is worth pairing with whatever mode is chosen, so the set of things the review
can cause is bounded by Muster rather than by the model. The exact set is a product decision and is
**unconfirmed**.

## 8. Round lifecycle

Identical to phase 2 and to what exists today, and for the same reasons:

1. `begin_round` records the round as `dispatching` with its marker and marks the notes `sent`,
   before the agent is called (`core/src/opencode/rounds.rs`).
2. `build_round_prompt` prepends `AGENT_PROMPT_PREFIX` and `[marker]` (`core/src/review.rs`).
3. `check_round_prompt` refuses empty or >512 KiB (`MAX_ROUND_PROMPT_BYTES`,
   `core/src/review.rs`).
4. Spawn with `--session-id`, write the user record, read the stream.
5. Success → `confirm_round` → `dispatched`. Failure → leave `dispatching`
   (`core/src/opencode/rounds.rs`).
6. On reconnect, `reconcile_round` greps the transcript for the marker and either confirms or
   requeues (`core/src/opencode/rounds.rs`).

The Muster-generated `--session-id` must be written to the round **before** the first turn, and it
is known before the process starts, which is the one thing phase 3 has that phase 2 does not: the
session id is never in doubt, so reconciliation never has to search by name or timestamp.

## 9. Finding the `claude` binary

`services/executable.rs::find_executable` searches `PATH`, `/opt/homebrew/bin`,
`/usr/local/bin`, `$HOME/.opencode/bin`, `$HOME/.local/bin` (`:8-29`). Claude Code is typically a
`~/.local/bin/claude` or an npm shim, so it is usually already found. `~/.claude/bin` is **not** in
the list; add it or do not — **unconfirmed**. The `OPENCODE_UNAVAILABLE` message
(`services/agent.rs:892-893`) is the model for the Claude Code equivalent.

## Unverified / risks

- **The exact minting argv for a zero-turn session is [unverified].** The 2.1.187 fix covers the
  _case_ of a `-p` run that made no model turns; it does not document an argv that creates a
  session without a turn at all. Fallback (a one-model-call mint, then interrupt) is described in
  §5 and works regardless.
- **Whether SIGINT is delivered as a clean end when the child is not in a TTY** is **unconfirmed**.
  The docs describe SIGINT as ending the turn cleanly in the SDK's own spawn conditions, which are a
  pipe, but the code that handles the signal has not been read.
- **`CLAUDE_CODE_RESUME_INTERRUPTED_TURN`**: whether to set it on the child is an open product
  decision (§3).
- **`interrupt_receipt_v1`**: the capability name is sourced to `sdk.d.ts`, not to the public docs.
  Whether the array is present on every version, and what a version without it should do (send
  anyway? signal instead?), is **unconfirmed**.
- **The transcript format is explicitly documented as unstable.** Any grep over
  `~/.claude/projects/**` can break on any release. There is no supported alternative from Rust
  except `getSessionMessages` from the JS/TS SDK.
- **No session listing from Rust.** Phase 3 ships without a picker as a direct consequence (§5).
- **The permission mode is undecided** and it is the largest open item in this phase (§7).
  `bypassPermissions` is available here and is not available in Codex, so the decision cannot be
  inherited.
- Whether a resumed conversation that already contains a Muster marker would cause
  `reconcile_round` to confirm a round that was never actually completed. The marker is in the
  _user_ message, so a round that was sent and then killed shows the marker and reconciles as
  landed — which is correct, because it was sent. The subtle case is a round whose _turn_ never
  finished: the marker is present, the review was delivered, and `dispatched` is the right answer.
  Noted because it looks like a bug and is not one.

## What a fake-process test proves

A fake `claude` — a script that reads stdin records, writes canned stream-json, and exits on
SIGINT or SIGTERM as configured — proves our side thoroughly: that the parser reads
`system/init` and takes `session_id`, that a `result` with each `subtype` maps to success or
failure, that `capabilities` is feature-detected rather than version-gated, that the round is
confirmed only after a `result`, and — most valuable of all — that **SIGTERM leaves a round
`dispatching` while SIGINT does not**. That last one is a real behavioural difference with real
bookkeeping consequences and a fake process is the right way to pin it.

It does **not** prove an uninstalled CLI accepts the real argv. A fake that ignores its arguments
passes every one of these tests. Specifically, a fake cannot tell us:

- whether `--session-id` plus `--input-format stream-json` actually creates a resumable session
  (§5, the largest open risk);
- whether `--resume` on an id we minted by the wrong method fails with the documented hard error or
  with something else;
- whether the 2.1.187 no-turn-session fix behaves as the changelog describes on the installed
  version;
- whether the permission mode we chose silently blocks, silently over-permits, or is not even
  accepted by this version.

The live test follows `docs/agent-live-tests.md`: `#[ignore]`d, opt-in with `--ignored`, dedicated
temporary directories, and a missing required variable **fails the test by name** rather than
passing without running. This one additionally needs real credentials and spends real tokens, and
it needs at least two cases: one mint-and-resume round trip (the thing the fake cannot check), and
one interrupt case that verifies the SIGINT/SIGTERM difference against the real binary.
