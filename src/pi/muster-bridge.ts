/**
 * Muster's Pi extension: reports what this Pi is doing to the Muster app running in the terminal
 * it was started from.
 *
 * Install it yourself, for one session or for good — see `docs/pi-integration.md`. Muster never
 * writes it into your Pi configuration, because an extension runs with your account's permissions
 * and that is not a thing an app should add to your machine without you deciding it.
 *
 * ## What it does, and what it deliberately does not
 *
 * It only reports. It never sends anything to Pi, never registers a tool, never renders anything
 * and never changes how a session behaves: the TUI you are typing into is untouched, and a Pi with
 * this loaded and no Muster anywhere in sight does nothing at all.
 *
 * ## The two facts that shape the code
 *
 * **It is a no-op unless Muster started the terminal.** `MUSTER_PI_BRIDGE` is present only when
 * this process was started inside a terminal Muster spawned, and it carries a credential minted
 * for that one terminal. Without it there is nothing to report to and the factory returns without
 * registering anything, so installing this globally is safe for the Pis you run elsewhere.
 *
 * **Nothing here may slow Pi down or break it.** The socket is opened from `session_start` and
 * never from the factory, because Pi loads extensions in invocations that never start a session.
 * Every failure — a refused connection, a write error, a bridge that speaks another version — ends
 * this reporter and nothing else. No handler throws, no handler awaits the network, and no retry
 * loop runs behind Pi's back.
 */

import { randomUUID } from "node:crypto";
import { isAbsolute, resolve } from "node:path";
import { connect, type Socket } from "node:net";

/** Kept in step with `PROTOCOL_VERSION` in `src-tauri/src/services/pi.rs`. */
const PROTOCOL_VERSION = 2;

/** Kept in step with `MAX_FRAME_BYTES` in `src-tauri/src/services/pi.rs`. */
const MAX_FRAME_BYTES = 16 * 1024;

/** `host:port:credential`, present only inside a terminal Muster spawned. */
const BRIDGE_ENV = "MUSTER_PI_BRIDGE";
/** This app's own id for that terminal, and the checkout it belongs to. */
const TERMINAL_ENV = "MUSTER_TERMINAL_SESSION_ID";
const CHECKOUT_ENV = "MUSTER_TERMINAL_CHECKOUT_ID";

/**
 * Kept in step with `ROUND_MARKER_PREFIX` and `is_review_round_marker` in `src-tauri/src/domain/review.rs`.
 *
 * The token a review is recognized by, in the one shape this app writes: `muster-review:` and a
 * round id of `round:` plus three numbers. Anything else — a marker-shaped string somebody typed
 * into a Pi — is not a round and must not confirm one.
 */
const ROUND_MARKER = /\[(muster-review:round:\d+:\d+:\d+)\]/g;

/**
 * How many receipts a reporter keeps.
 *
 * Bounded, and bounded for the same reason `MAX_FRAME_BYTES` is: receipts ride inside the whole
 * snapshot, so a session with a long transcript must not be able to push the rest of the report
 * off the wire. Sixteen is far more reviews than one Pi is ever sent, because a send opens a Pi
 * rather than messaging a running one, and the app keeps the older of the two the same way.
 */
export const MAX_RECEIPTS = 16;

const MAX_WORK_TARGETS = 16;
const MAX_WORK_PATH_BYTES = 512;

/**
 * The parts of Pi's extension API this extension uses.
 *
 * Declared here rather than imported: Pi is not a dependency of this app, and an extension that
 * pinned its host's types would fail to load against any release where the API moved. Pi runs
 * TypeScript directly through jiti, so these names are the only thing that has to be right.
 */
export interface PiSessionEntry {
  type: string;
  id: string;
  parentId: string | null;
  message?: { role?: string; content?: unknown };
}

export interface PiSessionManager {
  getSessionId(): string;
  getSessionFile(): string | undefined;
  getSessionName(): string | undefined;
  /**
   * Every entry recorded in this session, abandoned branches included.
   *
   * This is what a receipt is read from, and the choice is the whole of the branch semantics:
   * the message carrying a review is a fact about the session, not about whichever branch
   * happens to be in front of the person now. Reading the active branch instead would withdraw
   * the confirmation of a review the agent already acted on the moment somebody navigated back,
   * and the fix for that — re-sending — would duplicate a delivery that had already happened.
   */
  getEntries(): PiSessionEntry[];
}

export interface PiContext {
  mode: string;
  cwd: string;
  model?: { provider?: string; id?: string };
  sessionManager: PiSessionManager;
}

/**
 * One review this session was shown, as identities and nothing else.
 *
 * `marker` names the round and `entry` names the exact transcript entry that carried it, which
 * together pin a delivery to one message. No message text is ever read out of an entry, and
 * nothing else about the transcript is sent: a person typing into a Pi has no idea this is
 * running, and none of their words go over the wire.
 */
export interface Receipt {
  marker: string;
  entry: string;
}

export interface PiEventApi {
  on(event: string, handler: (event: never, ctx: PiContext) => void): () => void;
}

/** How a turn ended, in the three words the bridge has a reading for. */
export type Outcome = "succeeded" | "failed" | "interrupted";

/**
 * Everything the bridge is told about one session.
 *
 * Whole snapshot rather than deltas: a reconnect then restores current state with no replay and no
 * ordering to get right, which is the case that matters after a `/reload`.
 */
export interface WorkSnapshot {
  generation: string;
  sequence: number;
  targets: string[];
  ambiguous: boolean;
}

export interface Snapshot {
  title?: string;
  model?: string;
  running: boolean;
  awaiting: boolean;
  outcome?: Outcome;
  work: WorkSnapshot;
  /**
   * The reviews read out of this session, newest first.
   *
   * A whole list rather than a delta, and left off entirely when there is none: the other end
   * reads an absent list as "this frame said nothing about receipts" rather than as an empty
   * session, so a frame that simply omits the field cannot un-confirm a delivery.
   */
  receipts?: Receipt[];
}

/** Where to report to, read from the environment a Muster terminal hands down. */
export interface BridgeAddress {
  host: string;
  port: number;
  token: string;
}

/**
 * The address and credential of the terminal this process was started in, or `null` when there is
 * nothing to report to.
 *
 * Split from the right because the credential is hex and the address always contains a colon, so
 * only splitting on the last one survives a port. Anything malformed is `null` rather than a
 * half-read address: a value this app did not write is a value to refuse.
 */
export function bridgeFromEnvironment(env: Record<string, string | undefined>): BridgeAddress | null {
  const raw = env[BRIDGE_ENV];
  if (!raw) return null;
  const separator = raw.lastIndexOf(":");
  if (separator < 0) return null;
  const token = raw.slice(separator + 1);
  const [host, port] = raw.slice(0, separator).split(":");
  const parsed = Number(port);
  if (!host || !token || !Number.isInteger(parsed) || parsed <= 0 || parsed > 65535) return null;
  return { host, port: parsed, token };
}

/** The label a row shows for the model, in the `provider/id` form the app documents. */
export function modelLabel(model: PiContext["model"]): string | undefined {
  const provider = model?.provider;
  const id = model?.id;
  return provider && id ? `${provider}/${id}` : undefined;
}

/**
 * How a settled run is reported.
 *
 * `agent_settled` is the boundary that means "Pi will not continue automatically", so it is the
 * only completion signal this uses. `agent_end` and `turn_end` are not: a run can still be
 * retried, compacted or resumed after either one, and closing a row on them would report a
 * finished turn while Pi is still working on the answer.
 *
 * `aborted` on the settled event is Pi's own word for the person stopping it with Escape, which
 * is not a failure. The failure case arrives earlier, on `agent_before_settle`, where the outcome
 * is one of completed / aborted / error.
 */
export function outcomeFor(settledAborted: boolean | undefined, before: string | undefined): Outcome {
  if (before === "error") return "failed";
  return settledAborted ? "interrupted" : "succeeded";
}

/** Reads something off Pi's API without letting a shape difference become an extension failure. */
function safeRead<T>(read: () => T): T | undefined {
  try {
    return read();
  } catch {
    return undefined;
  }
}

/**
 * The reviews recorded in this session, newest first, as `(marker, entry id)` pairs.
 *
 * Read from Pi's own entries rather than from the session file: a file is bytes to be searched
 * and an entry is a fact, and only the second can tell a delivered message from the same
 * characters echoed back in an assistant reply, quoted in a tool result, or left behind on a
 * branch somebody abandoned.
 *
 * Only user messages count. A marker inside Pi's own system prompt — which is where a pasted
 * review can end up — is something Pi was told, not something it received.
 */
export function readReceipts(manager: PiSessionManager, limit = MAX_RECEIPTS): Receipt[] {
  const entries = safeRead(() => manager.getEntries());
  if (!Array.isArray(entries)) return [];
  const found: Receipt[] = [];
  const seen = new Set<string>();
  for (let index = entries.length - 1; index >= 0 && found.length < limit; index -= 1) {
    const entry = entries[index];
    if (!entry || entry.type !== "message") continue;
    const message = entry.message;
    if (!message || message.role !== "user") continue;
    const text = userText(message.content);
    if (!text) continue;
    ROUND_MARKER.lastIndex = 0;
    let match = ROUND_MARKER.exec(text);
    while (match) {
      const key = `${match[1]} ${entry.id}`;
      if (!seen.has(key)) {
        seen.add(key);
        found.push({ marker: match[1], entry: entry.id });
      }
      match = ROUND_MARKER.exec(text);
    }
  }
  return found;
}

/**
 * The text of a user message, and nothing else.
 *
 * `string` is what Pi records for a typed prompt; `text` inside the content blocks is what it
 * records for everything else. Images carry neither and are skipped rather than turned into
 * `[object Object]`.
 */
function userText(content: unknown): string | null {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return null;
  let text = "";
  for (const part of content) {
    const block = part as { type?: string; text?: unknown } | null;
    if (block?.type === "text" && typeof block.text === "string") text += block.text;
  }
  return text;
}

/** Only Pi's successful edit/write tools establish a work location. */
export function workTarget(name: unknown, input: unknown, cwd: string): string | null {
  if ((name !== "edit" && name !== "write") || !isAbsolute(cwd)) return null;
  const target = (input as { path?: unknown } | null)?.path;
  return typeof target === "string" && target.trim() ? resolve(cwd, target) : null;
}

/**
 * The connection to the bridge, and the snapshot it carries.
 *
 * Every method is safe to call on a reporter that failed to connect: a missing socket means the
 * state is still kept and simply not sent, which is what a Pi in a terminal with no Muster looks
 * like from the inside.
 */
export class Reporter {
  private socket: Socket | null = null;
  private readonly snapshot: Snapshot = {
    running: false,
    awaiting: false,
    work: { generation: randomUUID(), sequence: 0, targets: [], ambiguous: false },
  };
  /**
   * Every review this session has ever been shown, newest first.
   *
   * A history rather than a single reading, because a receipt is the one thing that must not be
   * taken back: navigating the session onto another branch, compacting it or reloading the
   * extension all leave the message in the transcript but stop it being the newest thing read,
   * and a delivery that came and went would otherwise be confirmed and then un-confirmed.
   */
  private receipts: Receipt[] = [];
  private sessionId = "";
  private disposed = false;

  constructor(
    private readonly address: BridgeAddress,
    private readonly terminalId: string,
    private readonly checkoutId: string,
  ) {}

  /**
   * Opens the connection and introduces this session.
   *
   * Called on every `session_start`, because a session can be replaced without a reload (`new`,
   * `resume`, `fork`) and the session id the bridge holds has to be the one that is actually open.
   */
  connect(ctx: PiContext): void {
    if (this.disposed) return;
    const sessionId = safeRead(() => ctx.sessionManager.getSessionId()) ?? "";
    if (sessionId && sessionId !== this.sessionId) {
      this.snapshot.work = { generation: randomUUID(), sequence: 0, targets: [], ambiguous: false };
      this.snapshot.running = false;
      this.snapshot.awaiting = false;
      delete this.snapshot.title;
      delete this.snapshot.model;
      delete this.snapshot.outcome;
      this.receipts = [];
    }
    this.sessionId = sessionId;
    this.refresh(ctx);
    if (!this.sessionId) return;
    // Node buffers writes made before the socket is up, so the introduction does not have to wait
    // for the connection and the first report cannot be lost to a race it did not know about.
    this.open();
    this.send(this.hello());
  }

  /** Re-reads the session's own facts and sends the whole snapshot. */
  publish(ctx: PiContext): void {
    if (this.disposed) return;
    this.refresh(ctx);
    // Carried on every state frame as well as on the `hello`, and omitted when there are none:
    // a receipt is part of what this session currently is, so a report that left it out would be
    // a report that is not the whole snapshot.
    this.send({
      v: PROTOCOL_VERSION,
      type: "state",
      ...this.snapshot,
      ...(this.receipts.length ? { receipts: this.receipts } : {}),
    });
  }

  /** Begins a new turn's bounded set of completed write targets. */
  beginWork(): void {
    this.snapshot.work = {
      ...this.snapshot.work,
      sequence: this.snapshot.work.sequence + 1,
      targets: [],
      ambiguous: false,
    };
    delete this.snapshot.outcome;
  }

  /** Records a successful edit/write target without sending tool contents. */
  recordWork(name: unknown, input: unknown, cwd: string): boolean {
    const target = workTarget(name, input, cwd);
    if (!target) return false;
    const work = this.snapshot.work;
    work.sequence += 1;
    if (work.ambiguous || Buffer.byteLength(target) > MAX_WORK_PATH_BYTES) {
      work.targets = [];
      work.ambiguous = true;
      return true;
    }
    if (!work.targets.includes(target)) work.targets.push(target);
    if (work.targets.length > MAX_WORK_TARGETS) {
      work.targets = [];
      work.ambiguous = true;
    }
    return true;
  }

  /** Records that a turn started. */
  setRunning(running: boolean): void {
    this.snapshot.running = running;
  }

  /** Records that Pi is blocked on something the person has to answer. */
  setAwaiting(awaiting: boolean): void {
    this.snapshot.awaiting = awaiting;
  }

  /** Records how the last turn ended. */
  setOutcome(outcome: Outcome): void {
    this.snapshot.outcome = outcome;
  }

  /** What this reporter currently holds, for a caller that has to assert on it. */
  current(): Readonly<Snapshot> {
    return this.snapshot;
  }

  /** The reviews this session has been shown, for a caller that has to assert on them. */
  received(): readonly Receipt[] {
    return this.receipts;
  }

  /**
   * Closes the connection, once, whatever brought this here.
   *
   * Idempotent because cancellation, reload, session replacement and process exit all converge here,
   * and none of them may throw because the socket was already gone.
   */
  dispose(reason: string): void {
    if (this.disposed) return;
    this.disposed = true;
    this.send({ v: PROTOCOL_VERSION, type: "bye", reason });
    // `end` flushes the queued `bye`; destroying immediately can turn a real Pi exit into an
    // indistinguishable disconnect and leave its active-session blocker behind.
    this.socket?.end();
  }

  private refresh(ctx: PiContext): void {
    const title = safeRead(() => ctx.sessionManager.getSessionName());
    const model = modelLabel(ctx.model);
    if (title) this.snapshot.title = title;
    else delete this.snapshot.title;
    if (model) this.snapshot.model = model;
    else delete this.snapshot.model;
    // Read on every refresh rather than once at startup: the first prompt is recorded before
    // the session is introduced, but a review pasted in afterwards is recorded later, and both
    // have to be in the list the app asks about.
    //
    // The transcript is what keeps a receipt, not this list: Pi's entries are append-only and
    // keep the branches somebody abandoned, so a review they navigated away from is still
    // recorded and is still found here. What this holds is a reading of that, redone every time.
    this.receipts = readReceipts(ctx.sessionManager);
  }

  private hello(): Record<string, unknown> {
    return {
      v: PROTOCOL_VERSION,
      type: "hello",
      token: this.address.token,
      terminalId: this.terminalId,
      checkoutId: this.checkoutId,
      sessionId: this.sessionId,
      ...this.snapshot,
      // Absent rather than empty when there are none: the other end reads a frame that says
      // nothing about receipts as a frame that says nothing, so a later frame that leaves the
      // field out cannot un-confirm a delivery.
      ...(this.receipts.length ? { receipts: this.receipts } : {}),
    };
  }

  private send(frame: Record<string, unknown>): void {
    if (!this.socket || this.socket.destroyed) return;
    const line = `${JSON.stringify(frame)}\n`;
    // The cap is the other end's rule, checked here so an oversized frame is dropped as one
    // report rather than refused mid-stream, which would cost the connection.
    if (Buffer.byteLength(line) > MAX_FRAME_BYTES) return;
    try {
      this.socket.write(line);
    } catch {
      // A socket that has already gone is not worth propagating into Pi's event dispatch.
    }
  }

  private open(): Socket | null {
    if (this.socket) return this.socket;
    let socket: Socket;
    try {
      socket = connect({ host: this.address.host, port: this.address.port });
    } catch {
      // A refused or unresolvable address is this reporter giving up, and nothing else.
      return null;
    }
    this.socket = socket;
    socket.setNoDelay(true);
    socket.on("error", () => {
      // Swallowed deliberately: an error here says something about Muster, and Pi has no way to
      // act on it. Letting it out would surface as an extension failure in a TUI in use.
    });
    socket.on("close", () => {
      this.socket = null;
    });
    return socket;
  }
}

/**
 * The extension factory. Pi calls this once per extension runtime and awaits it before startup
 * continues, so it does as little as possible: read the environment, and either register handlers
 * or return having registered nothing.
 */
export default function musterBridge(pi: PiEventApi): void {
  const address = bridgeFromEnvironment(process.env);
  const terminalId = process.env[TERMINAL_ENV];
  const checkoutId = process.env[CHECKOUT_ENV];
  if (!address || !terminalId || !checkoutId) return;

  const reporter = new Reporter(address, terminalId, checkoutId);
  // The outcome of the run in flight. `agent_before_settle` reports it and `agent_settled`
  // consumes it, and clearing it here is what stops a failure from being attributed to the next
  // turn when the second event never arrives.
  let before: string | undefined;

  pi.on("session_start", (_event, ctx) => {
    before = undefined;
    reporter.connect(ctx);
  });

  pi.on("session_info_changed", (_event, ctx) => {
    reporter.publish(ctx);
  });

  pi.on("agent_start", (_event, ctx) => {
    before = undefined;
    reporter.beginWork();
    reporter.setRunning(true);
    reporter.publish(ctx);
  });

  pi.on("tool_execution_end", (event, ctx) => {
    const completed = event as { toolName?: unknown; input?: unknown; isError?: unknown };
    if (completed.isError === false && reporter.recordWork(completed.toolName, completed.input, ctx.cwd)) {
      reporter.publish(ctx);
    }
  });

  pi.on("agent_before_settle", (event) => {
    before = safeRead(() => (event as { outcome?: string }).outcome);
  });

  pi.on("agent_settled", (event, ctx) => {
    const aborted = safeRead(() => (event as { aborted?: boolean }).aborted);
    reporter.setRunning(false);
    reporter.setOutcome(outcomeFor(aborted, before));
    before = undefined;
    reporter.publish(ctx);
  });

  pi.on("ui_prompt_start", (_event, ctx) => {
    reporter.setAwaiting(true);
    reporter.publish(ctx);
  });

  pi.on("ui_prompt_end", (_event, ctx) => {
    reporter.setAwaiting(false);
    reporter.publish(ctx);
  });

  // `reload` keeps the row: it ends the runtime, not the session, and the runtime that replaces
  // this one introduces itself a moment later. Every other reason really did end the session.
  pi.on("session_shutdown", (event) => {
    reporter.dispose(safeRead(() => (event as { reason?: string }).reason) ?? "quit");
  });
}
