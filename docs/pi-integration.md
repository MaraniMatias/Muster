# Pi integration inventory

Implementation: this branch. The host contract is verified against **`earendil-works/pi`
`v1.1.0`** — the extension event names, handler signatures and session-manager methods below
were read from that tag's
`packages/coding-agent/docs/extensions.md` and
`packages/coding-agent/src/core/extensions/types.ts`. This inventory describes what is implemented
and what is deliberately not.

**Not parity with OpenCode, and not an alternative transport to it.** OpenCode runs a server that
Muster connects to and asks. Pi has no such server: the thing running in a terminal is a TUI a
person is typing into. What Pi has is an extension API, and an extension inside that process can
see its own session and say so. Muster's half of this is a loopback service that an extension
reports to, and nothing more — see [Architecture](#architecture) for why it is one-way.

The two programs are separate answers to a similar question, so a session carries which program it
came from (`AgentProvider` in Rust, `AgentProvider` in TypeScript). Session ids are only ever
meaningful inside their own provider, which is exactly why they cannot be pooled without this
travelling with every row.

**The bridge reads; a launch is not the bridge.** Nothing below ever writes to a Pi somebody is
typing into. What sends a review is a Pi that has only just been started, with the review already in
front of it — see [Review delivery](#review-delivery). What confirms it is that Pi reporting the
marker back; see [Confirming a Pi read the review](#confirming-a-pi-read-the-review).

## Architecture

```
  Pi (TUI, interactive)                    Muster
  ┌───────────────────────────┐            ┌──────────────────────────────┐
  │ muster-bridge.ts          │            │ services/pi.rs               │
  │  ExtensionAPI             │            │  TcpListener on 127.0.0.1:0  │
  │   session_start ──────────┼── TCP ────▶│   per-terminal credential    │
  │   agent_start / settled   │  newline-  │   registry, keyed by terminal │
  │   ui_prompt_start / end   │  delimited │   normalized agent events    │
  │   session_shutdown        │  JSON      │                              │
  └───────────────────────────┘            └──────────────────────────────┘
       ▲ inherits                                    ▲ mints at spawn
       │ MUSTER_PI_BRIDGE                           │ terminal_create
   the shell in a Muster terminal ───────────────────┘
```

### Identity comes from the terminal, not the process name

A terminal's foreground program is what the OS reports, and Pi is a Node program, so that name is
`node` for every Pi and for every other Node process on the machine. It cannot say which terminal a
session is in, and no amount of reading it will change that.

What can is the terminal itself. Muster mints a per-terminal credential when it spawns a shell and
hands it down through the environment; everything the shell starts inherits it. So the value a Pi
session is matched by is one this app issued, not a string the agent wrote about itself. When a live
terminal moves between checkouts, the bridge rehomes its current identity without changing the
credential or the checkout claim already in the process environment.

| Variable                      | Meaning                                                  |
| ----------------------------- | -------------------------------------------------------- |
| `MUSTER_PI_BRIDGE`            | `127.0.0.1:<port>:<credential>` — absent outside Muster |
| `MUSTER_TERMINAL_SESSION_ID`  | The terminal session's own id                            |
| `MUSTER_TERMINAL_CHECKOUT_ID` | The checkout that terminal belongs to                    |

This is why a Pi row is matched by **identity** (`matchAgentSessionTerminal`) where an OpenCode row
is matched by **inference** (`matchAgentSessionTitle`). Those are kept as two separate functions on
purpose: the first can only answer `one` or `none`, because a terminal is one terminal, while the
second guesses from a title and has to refuse whenever the guess is not unique.

### The credential, and what it is worth

One random 192-bit token per terminal session, initially bound to that terminal and checkout. The
bridge is bound to `127.0.0.1` on an ephemeral port, so nothing is reachable from off the machine
and two copies of Muster cannot collide.

A connection is only bound to a terminal once a `hello` resolves its token and the frame's claims
for `terminalId` and the original `checkoutId` agree with what the token was issued for. After a
terminal move, Muster's registry separately records its current checkout; the original claim remains
the credential check, and review rounds remain owned by the checkout they were launched from. The
terminal is resolved **from the token**, so a frame's claim is a check and not the answer.

This is deliberately not a defence against a hostile process running as the same user: such a
process can generally read a terminal's environment anyway. It is a defence against a process
speaking for a terminal it is not in, and the worst a successful impersonation achieves is a row
in the sidebar that says something untrue. **Nothing in this path can make Muster run a command,
write a file, or reach a Pi.** The bridge reads and nothing else.

### The wire protocol

Newline-delimited JSON, one object per line, every frame carrying an explicit `v`. A frame is
refused — ending that connection and nothing else — when the version is not the one this build
speaks, when the line exceeds `MAX_FRAME_BYTES` (16 KiB), or when it is not an object.

| Frame   | Fields                                                                                      |
| ------- | ------------------------------------------------------------------------------------------- |
| `hello` | `v: 2`, `type`, `token`, `terminalId`, `checkoutId`, `sessionId`, plus a snapshot with work |
| `state` | `v: 2`, `type`, `running`, `awaiting`, `outcome`, `title?`, `model?`, `receipts?`, `work`   |
| `bye`   | `v: 2`, `type`, `reason`                                                                    |

Every report is a **whole snapshot**, not a delta. That is what makes the reconnect case — the one
that matters — restore current state with no replay and no merge logic.

`receipts` is a list of `{ marker, entry }` and nothing else: see
[Confirming a Pi read the review](#confirming-a-pi-read-the-review). It is **bounded at 16** on both
sides, because it rides inside the whole snapshot and a session with a long transcript must not be
able to push the rest of the report past `MAX_FRAME_BYTES`. It is **left off entirely** when there
are none, and a frame that omits it is read as saying nothing rather than as an empty list — reading
omission as "none" would let a reconnect un-confirm a delivery by saying nothing about it.

Protocol v2 also requires `work: { generation, sequence, targets, ambiguous }` on `hello` and every
`state`. `targets` carries only bounded, unique absolute paths of successful edits/writes from the
current turn; oversized or ambiguous activity clears the list and sets `ambiguous`. The field is
required so an older reporter cannot silently reset relocation state.

An unknown `type` is read past rather than refused, so one added field does not end a session.

### Turn boundaries: `agent_settled`, not `agent_end` or `turn_end`

Pi's own documentation is explicit that a run proceeds "to `agent_end`", that retries, recovery,
compaction and queued work can continue afterwards, and that `agent_settled` is the final,
notification-only boundary meaning "Pi will not continue automatically". A row closed on `agent_end`
would be closed before a retry that may change the answer; one closed on `turn_end` would be closed
while Pi is still working on it.

| Observed                                      | Reported as                                                    |
| --------------------------------------------- | -------------------------------------------------------------- |
| `agent_start`                                 | running; a `turnStarted` event                                 |
| `agent_before_settle` with `outcome: "error"` | the outcome is `failed`                                        |
| `agent_settled` with `aborted: true`          | the outcome is `interrupted` — stopped, not failed             |
| `agent_settled` otherwise                     | the outcome is `succeeded`                                     |
| `ui_prompt_start` / `ui_prompt_end`           | blocked / not blocked; a `questionAsked` event, then a re-read |

Only the `agent_settled` transition also says, to the rounds, that a turn finished — see
[Confirming a Pi read the review](#confirming-a-pi-read-the-review).

The clock is Muster's. `idleAt` is stamped when a turn is observed to go from running to not, from
this app's clock, because it is a duration a row is drawn with and a reporter's idea of the time is
not something this app may measure.

### Disconnect is unknown, never idle

A connection that drops mid-turn marks the row **not answerable** — `awaitingReply` becomes `null`,
which the sidebar already draws as unavailable — and stamps nothing. No `idleAt`, no outcome, no
`turnFinished`.

That distinction is the whole reason a dropped connection is not treated as a finished one: a row
that reports a turn as settled is a row claiming to know something nobody observed.

A `/reload` ends the runtime, not the session, so a `bye` with `reason: "reload"` keeps the row and
waits for the runtime that replaces it (up to `RECONNECT_GRACE`, 30 s, after which the row is
dropped at the next read rather than by a timer). Every other reason really did end the session. A
runtime that has already been replaced cannot end or disconnect the session that replaced it.

### What cannot break Pi

The factory opens nothing — Pi loads extensions in invocations that never start a session, so a
socket opened there would outlive the runtime it belonged to. The socket is opened from
`session_start` and closed from an idempotent `session_shutdown`. No handler throws, no handler
awaits the network, no retry loop runs in the background, and every socket error is swallowed: an
extension error in Pi's event dispatch would surface in a TUI somebody is using.

With no `MUSTER_PI_BRIDGE` in the environment, the factory registers **nothing at all**. That is
what makes the extension safe to install for good rather than per-session.

## Installation

Muster never writes this into your Pi configuration. An extension runs with your account's
permissions, and that is not a thing an app should add to your machine without you deciding it.

**A Pi Muster starts for you does not need this.** Every review send passes `--extension` with the
copy this build shipped, written into `~/.muster/pi/muster-bridge.ts` immediately before the spawn,
so the bridge and the app always speak the same protocol version. See
[Review delivery](#review-delivery) for how that launch is put together.

**A Pi you start yourself does need it.** Install it for one session:

```bash
pi --extension /path/to/muster/src/pi/muster-bridge.ts
```

or for good, by copying it into your user extensions directory (`<agent-dir>/extensions/`, which is
`~/.pi/agent/extensions/` by default). Pi loads TypeScript directly through jiti, so there is
nothing to compile:

```bash
mkdir -p ~/.pi/agent/extensions
cp /path/to/muster/src/pi/muster-bridge.ts ~/.pi/agent/extensions/muster-bridge.ts
```

A project-local copy goes in `.pi/extensions/` inside the checkout, but project extensions are only
loaded once project trust is granted, and `docs/security.md` in Pi's repository is clear that trust
is a resource-loading switch rather than a sandbox.

Pi loads extensions by canonical path, so a globally installed copy and the shipped one are two
files and both load. The bridge keys its rows by terminal, so a terminal still gets one session
row; it is simply reported twice over one connection pair.

Muster has to be running and the terminal has to be one Muster opened. A Pi started outside a
Muster terminal finds no credential and does nothing.

## Review delivery

Pi has no server to send a message to, so the only route to it is to start one. Every send to Pi
opens a **new terminal** in the checkout the review was made in — never a session picker, because
there is no session to pick yet, and never a queue, because a Pi that has just been started has
nothing to wait for.

### How a launch is put together

```
  Muster                                          the new terminal
  ┌──────────────────────────────┐                ┌──────────────────────────────┐
  │ 1. validate review and find  │                │                              │
  │    `pi` on PATH              │                │   pi --extension <bridge>    │
  │ 2. write the bridge out      │                │   -- @/…/round_….md      │
  │ 3. record the round          │                │                              │
  │ 4. write its prompt file     │                │   reads @file at startup      │
  │ 5. bind its terminal id      │                │                              │
  │ 6. spawn, argv ──────────────┼───────────────▶│   interactive TUI            │
  └──────────────────────────────┘                └──────────────────────────────┘
```

Review validation and steps 1–2 run before anything is recorded, so an empty or oversized review
and a machine with no Pi installed produce no claim. After the round is recorded, its prompt file is
written; then the new terminal id is bound to the round **before the process is spawned**. If that
binding fails, no Pi can receive the review and the round is discarded. The review travels as an
`@file`, which Pi documents as "include a text file or image in the first prompt" and resolves
with `path.resolve` — so an absolute path is taken as written. It is written to
`~/.muster/pi/prompts/`, private, under the name derived from that round's marker, and nothing a
note or a person wrote reaches a path or an argument.

The name is the round's marker rather than a fresh counter because the round has to be able to find
this file again: a delivery the Pi confirmed removes it at once, and a launch that failed on its way
to a terminal removes it with the round. What is left for the age sweep is only a launch that was
written and never read — a Pi that failed to start, or one whose bridge never reported it. Sweeping
by age rather than by "everything else in the folder" is deliberate: a second send can be starting
while this one is, and only age tells the two apart. The 24-hour age sweep is best-effort and
runs when a new Pi review prompt is written; there is no timer, so an expired unconfirmed prompt
can remain on disk until another Pi review send or manual removal. Confirmed prompts and launches
known to have failed are deleted immediately.

### Stored schema and upgrade impact

Pi provider and terminal identity are persisted in schema version 17 (up from 15). Under this
project's pre-1.0 schema policy, existing version-15 databases are deliberately refused; no
migration is written, and the workspace must be recreated empty. The shipped version is not
changed manually here.

### Confirming a Pi read the review

There is no server to ask, so delivery is settled from the other end: the session names the rounds
it was shown. This is the whole of it, and it is worth stating exactly what that claim is.

**A receipt is a marker and the id of the message that carried it.** The extension reads Pi's own
session entries (`sessionManager.getEntries()`), takes the user messages, and reports
`{ marker, entry }` for each bracketed `muster-review:` token it finds. Two identities and no text:
the marker names the round and the entry names the exact transcript message, which together pin a
delivery to one message rather than to anything that merely contains those characters.

**It is read from entries, never from the file.** A session file is bytes to be searched; an entry is
a fact, and only the second can tell a delivered message from the same characters echoed back in an
assistant reply, quoted in a tool result, or left behind on a branch somebody abandoned. Only
**user messages** count, and only the bracketed token `build_round_prompt` writes: a marker-shaped
string a person typed is not one of ours, and `is_review_round_marker` refuses it.

**The transcript is what keeps a receipt, not the reporter's memory.** Pi's entries are append-only
and keep the branches that were navigated away from, so a review on an abandoned branch is still
recorded and is still found. This is the branch semantics, and it is a decision:

- A receipt means **a user message carrying this marker was recorded in this session**. It does not
  mean the message is on the branch currently in front of the person.
- Reading the active branch instead would withdraw the confirmation of a review the agent has
  already acted on the moment somebody navigated back — and the only remedy left for a round that
  looks undelivered is to send it again, which is the one thing that must never happen here.
- So the confirmation is **pinned when it is observed** and is never re-derived from the active
  branch. `dispatching → dispatched` is one-way, and a round past it is reported as delivered
  whatever the session is navigated to afterwards.

**Four things have to be the same round, or nothing is confirmed.** The marker, the checkout the
round was launched from, the terminal the launch opened, and the session that reported. A receipt
from another session, terminal or original checkout matches no round and moves none. The terminal
comes from the credential rather than from the frame, while its current checkout follows a terminal
move — the round's original checkout binding does not.

**Completion is `agent_settled` and nothing else.** A round is only closed from the settlement Pi
announced, for the session bound to it. `turn_end` and `agent_end` are boundaries Pi can keep working
past; neither closes a round, and neither sets the flag this reads. Delivered is not finished, and
finished is not resolved: what a turn did to each line is judged separately, from the diff, and is
drawn as its own thing.

**Nothing here can send anything.** There is no queue, no flush and no retry for this destination:
`review_rounds_requeue`, `review_round_reconcile` and `review_round_flush` all filter Pi out, and
`review_round_settle_pi` only reads. A round left `dispatching` is left there and drawn as _waiting_,
which is what it is. A launch that never got as far as a terminal is undone instead — the round is
discarded and its notes go back to being drafts — because notes marked `sent` for a Pi that was never
started would be a claim about a delivery that cannot have happened. Its reason is kept on the diff
until a launch works, because a Pi send fails before anything is recorded and so leaves no round for
the failure to be drawn on.

| Pi's report      | What Muster settles                                              |
| ---------------- | ---------------------------------------------------------------- |
| session appears  | which session received the round, and delivery from its receipts |
| turn settles     | completion — the round goes `dispatched → acked`                 |
| connection drops | nothing; the round stays where it is                             |
| `ui_prompt_*`    | nothing                                                          |

### Why it is interactive and not a batch run

Verified against `v1.1.0` of `earendil-works/pi`: `resolveAppMode` in `src/main.ts` returns `print`
whenever `--print` is set, `--mode` names another interface, **or either stdin or stdout is not a
TTY**, and `readPipedStdin` also downgrades an interactive run to `print` if stdin has anything
piped into it. So the TUI is reached only by giving Pi the PTY on both ends and nothing on stdin —
which is what a spawn into a terminal does. `--print`, `--mode` and piping are absent by
construction rather than by a flag that could be forgotten.

There is also no initial input written into the PTY. Pi reads the `@file` itself during startup and
hands the result to the interactive mode as its first prompt, so nothing is lost to a terminal that
was not listening yet.

## Terminals and sessions

1. Detect Pi as an agent program in a terminal (`agentProviderForApp`).
2. Report the terminal a Pi session is running in, by identity rather than by title inference.
3. Read the session id, title and model from the running session.
4. Report a turn as started and as settled, and distinguish succeeded, failed and interrupted.
5. Report that Pi is blocked on a prompt the person has to answer.
6. Report a session appearing and leaving.
7. Fold Pi rows into the same `agent_sessions` and `agent_candidate_sessions` answers as OpenCode's.
8. Stop reporting when the terminal closes, when the Pi process exits, or when the extension is
   unloaded.

A checkout with a Pi session and no OpenCode service reports those sessions instead of an error: a
provider that is absent is not a failure when another provider answered.

Pi has **no agent catalog and no colors**, so a Pi row has no agent name and is named by its own
title. It has **no subagent parentage**, so `parentId` is always absent.

### How a Pi row is drawn

A sidebar row asks two different questions depending on the provider, and neither answer is allowed
to stand in for the other:

- **Identity, not the process name.** The row asks which session the bridge says is running in
  _this_ terminal (`matchAgentSessionTerminal`). It cannot ask the runtime instead: Pi is a Node
  program, so `foregroundApp` is `node` for every Pi on the machine, and a row gated on that name
  would draw nothing for a real Pi terminal.
- **Title inference stays OpenCode's.** `matchAgentSessionTitle` only considers OpenCode sessions,
  so a Pi session sharing a title with one cannot claim an OpenCode terminal or make it ambiguous.

From that one answer the row draws the session's own title, the provider beside it, the working,
waiting, failed and idle states read from the session rather than from an agent it never has, and
the elapsed time since its last update. A Pi session that lost its bridge draws as unavailable —
never as confirmed idle, and never with another terminal's session borrowed into the gap.

Every checkout's rows are kept current this way, not only the selected one's: an event re-reads the
worktree it happened in rather than waiting for the slow poll, which matters for a session that just
appeared or left because the fast poll only asks about checkouts already known to be busy.

### Worktree relocation and removal

Only successful Pi `edit` and `write` tool calls contribute paths. Relative paths are resolved against
Pi's execution `ctx.cwd`; reads, failed calls and shell commands do not count. The extension accumulates
a bounded set for the turn without sending tool contents. Muster reuses the shared checkout resolver
and moves the terminal row only when those paths identify one unambiguous, registered worktree.
Multiple destinations, unregistered nested worktrees and duplicate roots do not move it; nested
registered roots resolve to the deepest checkout. The Pi process, terminal scrollback, shell cwd and
Pi cwd stay where they are; manual moves also leave Pi's cwd unchanged.

Removing a worktree is refused while a Pi session is active or its status is unknown after disconnect.
It remains a blocker until Pi reports exit or its terminal is closed. Terminal creation, direct Pi
review launch, terminal close and removal are serialized per checkout, so a new Pi cannot launch in
the gap between the removal check and the filesystem operation.

Sources:
[bridge service](../src-tauri/src/services/pi.rs),
[launch service](../src-tauri/src/services/pi_launch.rs),
[extension](../src/pi/muster-bridge.ts),
[session aggregation](../src-tauri/src/commands/agent.rs),
[review rounds](../src-tauri/src/services/review_round.rs),
[session presentation](../src/presentation/agent-sessions.ts),
[worktree relocation](../src-tauri/src/services/agent/relocation.rs),
[agent domain](../src/domain/agent.ts),
[sidebar](../src/components/Sidebar.vue).

## Not currently integrated

Deliberately out of scope for this stage:

- **Sending to a Pi that is already running.** Every send opens a terminal. A Pi somebody started
  by hand in a Muster terminal is visible in the sidebar and is not a destination.
- **Retrying a send that was never confirmed.** A Pi round that never reports a receipt stays
  `dispatching` and is drawn as _waiting_. Nothing re-sends it, because the only route to a Pi is
  opening one and that is the user's send; and nothing can tell "not read yet" from "read by a Pi
  whose bridge has since gone", so re-sending would risk a duplicate.
- **Answering prompts or approving permissions from Muster.** A blocked Pi row says so; there is no
  reply form.
- **Cancelling a turn.**
- **Sending review or tool contents over the bridge.** `getSessionFile` is unused; receipts are read
  locally from Pi's own entries. Only bounded successful edit/write paths cross the wire for
  relocation. No file contents or transcript text are sent.
- **Surviving a Muster restart.** Every terminal is closed when Muster exits, so a Pi reporting to a
  bridge that no longer exists is a Pi whose terminal is gone. There is no reconnect loop. What does
  survive is the round: a confirmed delivery is read back out of the database by the next process
  rather than re-derived.

## Verification scope

The bridge and the extension are covered by tests on both sides of the wire: protocol v2 work
validation, size limits, credential scoping, reconnect, disconnect-is-not-idle and removal-blocker
tracking are tested in `services/pi.rs`; first-poll Pi relocation and removal reservations in
`services/agent.rs`; and removal refusing active and disconnected Pi sessions in
`services/worktree.rs`. The extension's bounded edit/write activity frames, environment parsing,
outcome mapping and lifecycle wiring are tested in `src/pi/muster-bridge.test.ts`, including a
connection to a real socket. Manual Pi movement without a cwd change is tested in
`src/presentation/terminal-movement.test.ts`.

The launch is covered on the sides that can be: that a review reaches Pi as a file reference and
nothing else, that shell operators and newlines in a note stay inside that file and never reach a
path, that two launches never share a file, that a confirmed round removes its own file and leaves
every other, that what is written is private, that a `pi` that is not installed is reported rather
than guessed, and that a terminal hands its program its arguments as arguments are tested in
`services/pi_launch.rs` and `services/terminal.rs`. What a Pi round may claim at each moment —
provider, then terminal, then session, and never delivery on its own — is tested in
`services/review_round.rs`, along with the reconciliation machinery leaving a Pi round alone.

The confirmation itself is tested on both sides of the wire:

- `domain::review::tests` pins the marker grammar and that a receipt is the bracketed token rather
  than the marker's characters appearing anywhere.
- `services::pi::tests` covers receipts arriving on a `hello` and on a `state`, what is refused as
  not naming a round and an entry, the fixed bound, an omitted field not taking back a receipt
  already reported, only a reported settlement saying a turn finished (a dropped connection and a
  reload neither do), and a session of another checkout proving nothing about this one.
- `services::review_round::tests` drives the real bridge over a real socket: a reported marker
  confirming delivery and only then a settlement closing the round; a receipt matching nothing when
  it comes from the wrong session, terminal, checkout or marker; a settlement closing only the round
  of the session that settled, with two Pi terminals in one checkout; a confirmation still on record
  after the process that recorded it is gone, with nothing requeueing or flushing it; a launch that
  never reached a terminal leaving no round and no `sent` notes, and one that did never being
  discarded; and an unconfirmed round staying `dispatching` with nothing sending it again.
- `src/pi/muster-bridge.test.ts` covers reading receipts out of Pi's own entries, every shape that
  is not one, the bound, surviving the session being navigated away from them, and what a frame does
  and does not carry.

The sidebar presentation is covered on the side that draws it: identity matching, a shared title
across the two providers, the states a Pi row draws without a catalog, a disconnected session drawn
as unavailable, and a worktree nobody has selected re-reading on an event are tested in
`src/components/Sidebar.test.ts`, `src/domain/agent.test.ts` and
`src/presentation/agent-sessions.test.ts`. The destination itself is covered in
`src/components/FileDiff.test.ts` (no session and no busy choice for Pi however many sessions are
running, and the three Pi counts and a failed launch reported without either of those) and
`src/App.test.ts` (the send opens a terminal in the same checkout; a launched round is kept pending;
only the two Pi reports that can settle one cause it to be asked about, each of them once; and a
send that never started leaves its reason on the panel it came from until one does).

The event names, handler signatures and session-manager methods were read from `v1.1.0` of
`earendil-works/pi`. The interactive/print decision and the `@file` handling were read from that
tag's `src/main.ts`, `src/cli/args.ts`, `src/cli/file-processor.ts` and `src/cli/initial-message.ts`.
No live Pi desktop launch was verified here. Manual acceptance remains: with Pi v1.1.0 installed,
send selected notes and confirm Muster opens a fresh interactive terminal in the same checkout,
the notes appear as Pi's initial prompt, sidebar state follows that terminal's bridge identity,
and quitting Pi removes its active session. Protocol v2 and mocked-PTY launch tests do not replace
that live check.
