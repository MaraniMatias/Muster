import { createServer, type AddressInfo, type Socket } from "node:net";
import { describe, expect, it, vi } from "vitest";

import musterBridge, {
  bridgeFromEnvironment,
  MAX_RECEIPTS,
  modelLabel,
  outcomeFor,
  readReceipts,
  Reporter,
  type PiContext,
  type PiSessionEntry,
  type Snapshot,
  workTarget,
} from "./muster-bridge";

/**
 * A Pi the test drives from the outside.
 *
 * The extension only ever talks to `pi.on`, and every handler Pi dispatches is
 * `(event, ctx)`, so this records the registrations and replays them with a context the test
 * chooses. Nothing here is a mock of the extension's own logic — the frames that come out are the
 * ones the shipped file writes.
 */
function harness(env: Record<string, string | undefined> = {}) {
  const handlers = new Map<string, (event: never, ctx: PiContext) => void>();
  const api = {
    on(event: string, handler: (event: never, ctx: PiContext) => void) {
      handlers.set(event, handler);
      return () => handlers.delete(event);
    },
  };
  const previous = { ...process.env };
  for (const [key, value] of Object.entries({
    MUSTER_PI_BRIDGE: "127.0.0.1:41234:abc123",
    MUSTER_TERMINAL_SESSION_ID: "terminal:one",
    MUSTER_TERMINAL_CHECKOUT_ID: "checkout:one",
    ...env,
  })) {
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
  musterBridge(api as never);
  return {
    handlers,
    restore: () => {
      process.env = previous;
    },
    emit(event: string, payload: unknown = {}, ctx: PiContext = context()) {
      handlers.get(event)?.(payload as never, ctx);
    },
  };
}

function context(overrides: Partial<PiContext> = {}, entries: PiSessionEntry[] = []): PiContext {
  return {
    mode: "tui",
    cwd: "/work",
    model: { provider: "anthropic", id: "claude-opus-5" },
    sessionManager: {
      getSessionId: () => "pi-session",
      getSessionFile: () => "/home/x/.pi/agent/sessions/one.jsonl",
      getSessionName: () => "Review the panel",
      getEntries: () => entries,
    },
    ...overrides,
  };
}

/** A user message entry carrying `text`, the shape Pi records a typed prompt as. */
function userEntry(id: string, text: string, parentId: string | null = null): PiSessionEntry {
  return { type: "message", id, parentId, message: { role: "user", content: text } };
}

/** The same, with the content-block shape Pi records anything structured as. */
function userEntryBlocks(id: string, text: string): PiSessionEntry {
  return { type: "message", id, parentId: null, message: { role: "user", content: [{ type: "text", text }] } };
}

/** Collects the frames a real socket carried, by standing in for the bridge at the other end. */
function captureFrames() {
  const lines: string[] = [];
  const sockets = new Set<Socket>();
  const server = createServer((socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    let buffer = "";
    socket.on("data", (chunk) => {
      buffer += String(chunk);
      const parts = buffer.split("\n");
      buffer = parts.pop() ?? "";
      for (const part of parts) if (part.trim()) lines.push(part);
    });
    socket.on("error", () => {});
  });
  return {
    server,
    lines,
    port: () => (server.address() as AddressInfo).port,
    /** Waits for `count` frames, or fails the test rather than hanging on a missing one. */
    async waitFor(count: number): Promise<void> {
      for (let attempt = 0; attempt < 200 && lines.length < count; attempt++) {
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      expect(lines.length).toBeGreaterThanOrEqual(count);
    },
    frames: () => lines.map((line) => JSON.parse(line)),
    close: () => {
      for (const socket of sockets) socket.destroy();
      return new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}

async function withServer<T>(run: (frames: ReturnType<typeof captureFrames>) => Promise<T>): Promise<T> {
  const frames = captureFrames();
  await new Promise<void>((resolve) => frames.server.listen(0, "127.0.0.1", resolve));
  try {
    return await run(frames);
  } finally {
    await frames.close();
  }
}

describe("the bridge address a terminal hands down", () => {
  it("is split from the right, because only the address carries a colon", () => {
    expect(bridgeFromEnvironment({ MUSTER_PI_BRIDGE: "127.0.0.1:41234:deadbeef" })).toEqual({
      host: "127.0.0.1",
      port: 41234,
      token: "deadbeef",
    });
  });

  it("is refused rather than half-read when it is not the shape this app writes", () => {
    // A value this app did not write is a value to refuse, and every one of these would otherwise
    // become a connection to something that is not Muster.
    for (const raw of ["", "nonsense", "127.0.0.1:0:abc", "127.0.0.1:70000:abc", "127.0.0.1:abc", "127.0.0.1:41234:"]) {
      expect(bridgeFromEnvironment({ MUSTER_PI_BRIDGE: raw })).toBeNull();
    }
    expect(bridgeFromEnvironment({})).toBeNull();
  });
});

describe("Pi tool work locations", () => {
  it("resolves only edit/write targets against Pi's execution cwd", () => {
    expect(workTarget("write", { path: "src/new.ts" }, "/actual/worktree")).toBe("/actual/worktree/src/new.ts");
    expect(workTarget("edit", { path: "../shared.ts" }, "/actual/worktree/sub")).toBe("/actual/worktree/shared.ts");
    expect(workTarget("read", { path: "src/new.ts" }, "/actual/worktree")).toBeNull();
    expect(workTarget("bash", { command: "cd /other && touch file" }, "/actual/worktree")).toBeNull();
  });
});

describe("how a settled turn is reported", () => {
  it("separates a failure from a person stopping it", () => {
    // The three words the bridge has a reading for, and they are three different things: a turn
    // that failed, one the person interrupted, and one that completed.
    expect(outcomeFor(false, "error")).toBe("failed");
    expect(outcomeFor(true, undefined)).toBe("interrupted");
    expect(outcomeFor(false, "completed")).toBe("succeeded");
    // An abort that also errored is still a failure: the person stopped a run that was already
    // not going to succeed, and the error is the reason worth showing.
    expect(outcomeFor(true, "error")).toBe("failed");
  });

  it("names the model as provider/id, and says nothing when either half is missing", () => {
    expect(modelLabel({ provider: "anthropic", id: "claude-opus-5" })).toBe("anthropic/claude-opus-5");
    expect(modelLabel({ provider: "anthropic" })).toBeUndefined();
    expect(modelLabel(undefined)).toBeUndefined();
  });
});

describe("a Pi that Muster did not start", () => {
  it("registers nothing at all, so the Pis it is installed for are left alone", () => {
    // The reason this extension is safe to install globally: with no credential in the
    // environment there is nothing to report to, so the factory does nothing at all.
    const registered = vi.fn();
    const previous = { ...process.env };
    delete process.env.MUSTER_PI_BRIDGE;
    try {
      musterBridge({ on: registered } as never);
    } finally {
      process.env = previous;
    }
    expect(registered).not.toHaveBeenCalled();
  });
});

describe("the events the bridge reports", () => {
  it("are the ones Pi documents, and not the turn boundaries it explicitly warns against", () => {
    const { handlers, restore } = harness();
    try {
      expect([...handlers.keys()].sort()).toEqual([
        "agent_before_settle",
        "agent_settled",
        "agent_start",
        "session_info_changed",
        "session_shutdown",
        "session_start",
        "tool_execution_end",
        "ui_prompt_end",
        "ui_prompt_start",
      ]);
      // `agent_end` and `turn_end` are absent on purpose: a run can still be retried, compacted or
      // resumed after either, so a row closed on one of them would report a finished turn while Pi
      // is still working on the answer.
      expect(handlers.has("agent_end")).toBe(false);
      expect(handlers.has("turn_end")).toBe(false);
    } finally {
      restore();
    }
  });
});

describe("completed Pi write tools", () => {
  it("reports successful write targets only, accumulating parallel destinations without tool contents", async () => {
    await withServer(async (frames) => {
      const { emit, restore } = harness({ MUSTER_PI_BRIDGE: `127.0.0.1:${frames.port()}:abc123` });
      const ctx = context({ cwd: "/actual/worktree" });
      try {
        emit("session_start", {}, ctx);
        emit("agent_start", {}, ctx);
        emit(
          "tool_execution_end",
          { toolName: "write", input: { path: "src/one.ts", content: "private text" }, isError: false },
          ctx,
        );
        emit(
          "tool_execution_end",
          { toolName: "edit", input: { path: "/other/file.ts", oldText: "secret" }, isError: false },
          ctx,
        );
        emit("tool_execution_end", { toolName: "write", input: { path: "failed.ts" }, isError: true }, ctx);
        emit("tool_execution_end", { toolName: "read", input: { path: "read-only.ts" }, isError: false }, ctx);
        emit("tool_execution_end", { toolName: "bash", input: { command: "cd /other" }, isError: false }, ctx);
        await frames.waitFor(4);

        const last = frames
          .frames()
          .filter((frame) => frame.type === "state")
          .at(-1);
        expect(last).toMatchObject({
          work: { targets: ["/actual/worktree/src/one.ts", "/other/file.ts"], ambiguous: false },
        });
        expect(JSON.stringify(frames.frames())).not.toContain("private text");
        expect(JSON.stringify(frames.frames())).not.toContain("secret");
      } finally {
        emit("session_shutdown", { reason: "quit" }, ctx);
        restore();
      }
    });
  });
});

describe("the snapshot a session carries", () => {
  it("reports identity from the environment and state from the context", async () => {
    await withServer(async (frames) => {
      const { restore } = harness();
      try {
        const reporter = new Reporter(
          { host: "127.0.0.1", port: frames.port(), token: "abc123" },
          "terminal:one",
          "checkout:one",
        );

        // Driven the way Pi drives it: a session starts, then Pi's own events, then the session
        // ends. Each step waits for what it wrote, because a reporter that is disposed while its
        // first write is still buffered has not reported anything — which is this test's ordering
        // rule, not a behaviour worth asserting either way.
        reporter.connect(context());
        reporter.setRunning(true);
        reporter.publish(context());
        reporter.setAwaiting(true);
        reporter.publish(context());
        await frames.waitFor(3);
        reporter.setRunning(false);
        reporter.setOutcome("succeeded");
        reporter.publish(context());
        await frames.waitFor(4);
        reporter.dispose("quit");
        await frames.waitFor(5);

        const written = frames.frames();
        expect(written[0]).toMatchObject({
          v: 2,
          type: "hello",
          token: "abc123",
          terminalId: "terminal:one",
          checkoutId: "checkout:one",
          sessionId: "pi-session",
          title: "Review the panel",
          model: "anthropic/claude-opus-5",
        });
        expect(written.at(-1)).toMatchObject({ v: 2, type: "bye", reason: "quit" });
        // Every report is a whole snapshot, so a reconnect restores current state with no replay
        // and nothing has to be merged on the other side. The only field that arrives late is
        // the outcome, because there is no outcome to report before a turn has ended.
        const states = written.filter((frame) => frame.type === "state");
        expect(states).toHaveLength(3);
        for (const frame of states) {
          for (const key of Object.keys(frame)) {
            expect(["awaiting", "model", "outcome", "running", "title", "type", "v", "work"]).toContain(key);
          }
          expect(frame).toMatchObject({
            v: 2,
            running: expect.any(Boolean),
            awaiting: expect.any(Boolean),
            work: expect.any(Object),
          });
        }
        expect(states[0]).toMatchObject({ running: true, awaiting: false });
        expect(states[1]).toMatchObject({ running: true, awaiting: true });
        expect("outcome" in states[1]).toBe(false);
        expect(states[2]).toMatchObject({ running: false, awaiting: true, outcome: "succeeded" });
        expect(reporter.current()).toMatchObject<Pick<Snapshot, "running" | "outcome">>({
          running: false,
          outcome: "succeeded",
        });
      } finally {
        restore();
      }
    });
  });

  it("refreshes an idle session's model and clears it when Pi has none", async () => {
    await withServer(async (frames) => {
      const { emit, restore } = harness({ MUSTER_PI_BRIDGE: `127.0.0.1:${frames.port()}:abc123` });
      try {
        emit("session_start");
        await frames.waitFor(1);
        emit("session_info_changed", {}, context({ model: { provider: "openai", id: "o4-mini" } }));
        await frames.waitFor(2);
        expect(frames.frames()[1]).toMatchObject({ type: "state", running: false, model: "openai/o4-mini" });

        emit("session_info_changed", {}, context({ model: undefined }));
        await frames.waitFor(3);
        expect("model" in frames.frames()[2]).toBe(false);
        emit("session_shutdown", { reason: "quit" });
      } finally {
        restore();
      }
    });
  });

  it("starts a replacement session without inheriting the previous session's state", async () => {
    await withServer(async (frames) => {
      const { emit, restore } = harness({ MUSTER_PI_BRIDGE: `127.0.0.1:${frames.port()}:abc123` });
      try {
        const first = context({
          sessionManager: {
            ...context().sessionManager,
            getEntries: () => [userEntry("entry:old", "[muster-review:round:1762000000:4242:7]")],
          },
        });
        emit("session_start", {}, first);
        emit("agent_start", {}, first);
        emit("agent_before_settle", { outcome: "error" }, first);
        emit("agent_settled", { aborted: false }, first);
        await frames.waitFor(3);

        const next = context({
          model: undefined,
          sessionManager: {
            ...context().sessionManager,
            getSessionId: () => "pi-session-next",
            getSessionName: () => undefined,
            getEntries: () => [],
          },
        });
        emit("session_start", {}, next);
        await frames.waitFor(4);
        const hello = frames
          .frames()
          .filter((frame) => frame.type === "hello")
          .at(-1);
        expect(hello).toMatchObject({ sessionId: "pi-session-next", running: false, awaiting: false });
        for (const field of ["title", "model", "outcome", "receipts"]) {
          expect(field in hello!).toBe(false);
        }

        emit("agent_start", {}, next);
        emit("agent_settled", { aborted: false }, next);
        await frames.waitFor(6);
        expect(frames.frames().at(-1)).toMatchObject({ type: "state", outcome: "succeeded" });
        emit("session_shutdown", { reason: "quit" }, next);
      } finally {
        restore();
      }
    });
  });

  it("loses only the reporting, and never Pi, when the bridge refuses the connection", async () => {
    // A refused socket has to cost nothing beyond the reporting itself: Pi is the thing being
    // used here, and a lifecycle handler that threw would be an extension error in the middle of
    // somebody's session.
    const { restore } = harness();
    try {
      const reporter = new Reporter(
        // Port 1 is reserved and nothing listens on it.
        { host: "127.0.0.1", port: 1, token: "abc123" },
        "terminal:one",
        "checkout:one",
      );
      expect(() => {
        reporter.connect(context());
        reporter.setRunning(true);
        reporter.publish(context());
        reporter.dispose("quit");
      }).not.toThrow();
      expect(reporter.current()).toMatchObject<Pick<Snapshot, "running">>({ running: true });
    } finally {
      restore();
    }
  });

  it("flushes the final bye before closing the socket", async () => {
    await withServer(async (frames) => {
      const reporter = new Reporter(
        { host: "127.0.0.1", port: frames.port(), token: "abc123" },
        "terminal:one",
        "checkout:one",
      );
      reporter.connect(context());
      reporter.dispose("quit");
      await frames.waitFor(2);
      expect(frames.frames().at(-1)).toMatchObject({ v: 2, type: "bye", reason: "quit" });
    });
  });

  it("closes once, whatever brought it there", () => {
    // Cancellation, reload, session replacement and process exit all converge here, and none
    // of them may throw because the socket was already gone.
    const reporter = new Reporter({ host: "127.0.0.1", port: 1, token: "abc123" }, "terminal:one", "checkout:one");
    reporter.dispose("quit");
    expect(() => reporter.dispose("reload")).not.toThrow();
  });
});

/**
 * A review this app sent, in the shape `build_round_prompt` writes it: the instructions, then
 * the bracketed marker, then the review.
 */
const MARKER = "muster-review:round:1762000000:4242:7";
const OTHER_MARKER = "muster-review:round:1762000100:4242:8";
const DELIVERED = `You are receiving a code review.\n[${MARKER}]\n\n## src/foo.js\n\n> needs quotes: '`;

describe("the reviews a session is found to have received", () => {
  const manager = (entries: PiSessionEntry[]) => context({}, entries).sessionManager;

  it("are read from Pi's own entries, as a marker and the id of the message carrying it", () => {
    expect(readReceipts(manager([userEntry("entry:one", DELIVERED)]))).toEqual([
      { marker: MARKER, entry: "entry:one" },
    ]);
    // The content-block shape counts too, or a review Pi recorded any other way would be invisible.
    expect(readReceipts(manager([userEntryBlocks("entry:blocks", DELIVERED)]))).toEqual([
      { marker: MARKER, entry: "entry:blocks" },
    ]);
    // Newest first, so the receipt a round is waiting on is the one kept when the list is bounded.
    expect(
      readReceipts(
        manager([userEntry("entry:one", DELIVERED), userEntry("entry:two", `later\n[${OTHER_MARKER}]`, "entry:one")]),
      ),
    ).toEqual([
      { marker: OTHER_MARKER, entry: "entry:two" },
      { marker: MARKER, entry: "entry:one" },
    ]);
  });

  /**
   * What is not a delivery: the same characters somewhere else, or in something Pi was told.
   *
   * Every case here is a way a marker could reach a transcript without the review being sent —
   * the agent echoing it back, a tool result quoting it, a person pasting it, or a marker-shaped
   * string this app never wrote. A receipt is evidence only because this app put the token in a
   * user message, and anything else is a coincidence.
   */
  it("are not read out of anything but a user message this app wrote", () => {
    const notReceipts = [
      [{ type: "message", id: "entry:assistant", message: { role: "assistant", content: DELIVERED } }],
      [{ type: "message", id: "entry:tool", message: { role: "toolResult", content: DELIVERED } }],
      [{ type: "custom", id: "entry:custom", customType: "paste" }],
      [userEntry("entry:custom", "Pi was told to remember muster-review:round:1762000000:4242:7")],
      [userEntry("entry:quoted", `the review said ${MARKER}`)],
      [userEntry("entry:loose", "muster-review:round:1:2 is not a marker this build writes")],
      [userEntry("entry:empty", "")],
    ];
    for (const entries of notReceipts) {
      expect(readReceipts(manager(entries as PiSessionEntry[]))).toEqual([]);
    }
  });

  /**
   * A review somebody navigated away from is still in the transcript and is still found.
   *
   * The branch semantics in one test: the entry is not on the branch the session is currently
   * showing — it is an alternative history — and treating that as "not delivered" would withdraw
   * the confirmation of a review the agent has already acted on, with a resend as the only thing
   * left to offer. What this reads is that a user message carrying the marker was recorded in
   * this session, which stays true however the session is navigated afterwards. The confirmation
   * is pinned when it is observed and is never re-derived from the active branch.
   */
  it("survive the session being navigated away from them", () => {
    const entries = [
      userEntry("entry:root", "an earlier turn"),
      userEntry("entry:review", DELIVERED, "entry:root"),
      // What navigating back leaves behind: a later entry on another branch, and the review's
      // entry still recorded, just no longer the newest thing in the file.
      userEntry("entry:after", "never mind", "entry:root"),
    ];
    const first = readReceipts(manager(entries));
    expect(first).toEqual([{ marker: MARKER, entry: "entry:review" }]);
    // Read again after the navigation: same entry, same marker. The pin is to the message, not
    // to where the person currently is in the session.
    expect(readReceipts(manager([...entries]))).toEqual(first);
    expect(readReceipts(manager([entries[0], entries[2], entries[1]]))).toEqual(first);
  });

  it("are bounded to a fixed number so a long session cannot overflow the frame", () => {
    const entries = Array.from({ length: MAX_RECEIPTS * 3 }, (_, index) =>
      userEntry(`entry:${index}`, `round\n[muster-review:round:1762000000:4242:${index}]`),
    );
    const found = readReceipts(manager(entries));
    expect(found).toHaveLength(MAX_RECEIPTS);
    // The newest, which is the one a round is waiting on.
    expect(found[0].marker).toBe(`muster-review:round:1762000000:4242:${MAX_RECEIPTS * 3 - 1}`);
  });

  it("are nothing at all when Pi's API is not the shape this build knows", () => {
    const broken = {
      ...context().sessionManager,
      getEntries: () => {
        throw new Error("the session is gone");
      },
    };
    expect(readReceipts(broken)).toEqual([]);
    expect(readReceipts({ ...context().sessionManager, getEntries: () => null as never })).toEqual([]);
  });
});

describe("what a report carries about them", () => {
  /** A reporter against a real socket. Disposed by each test, or the server cannot close. */
  async function reporting(frames: ReturnType<typeof captureFrames>, entries: PiSessionEntry[]) {
    const reporter = new Reporter(
      { host: "127.0.0.1", port: frames.port(), token: "[REDACTED:API key param]" },
      "terminal:one",
      "checkout:one",
    );
    reporter.connect(context({}, entries));
    return reporter;
  }

  it("is the receipt and nothing of the message it was read from", async () => {
    await withServer(async (frames) => {
      const reporter = await reporting(frames, [userEntry("entry:one", DELIVERED)]);
      await frames.waitFor(1);

      const written = frames.frames()[0];
      expect(written.receipts).toEqual([{ marker: MARKER, entry: "entry:one" }]);
      // The review itself is a person's text and never leaves the process: what travels is two
      // identifiers and nothing that could be read back.
      expect(JSON.stringify(written)).not.toContain("needs quotes");
      expect(JSON.stringify(written)).not.toContain("src/foo.js");
      reporter.dispose("quit");
    });
  });

  it("is left out entirely when there is nothing to say, rather than sent as empty", async () => {
    await withServer(async (frames) => {
      const reporter = await reporting(frames, [userEntry("entry:one", "just a conversation")]);
      reporter.publish(context({}, [userEntry("entry:one", "just a conversation")]));
      await frames.waitFor(2);

      // A frame that says nothing about receipts is a frame that says nothing, and the other end
      // has to be able to tell that from a session that was sent no review.
      for (const frame of frames.frames()) expect("receipts" in frame).toBe(false);
      expect(reporter.received()).toEqual([]);
      reporter.dispose("quit");
    });
  });

  it("is re-read on every report, so a review pasted in later is still found", async () => {
    await withServer(async (frames) => {
      const reporter = await reporting(frames, []);
      await frames.waitFor(1);
      expect(reporter.received()).toEqual([]);

      reporter.publish(context({}, [userEntry("entry:late", `later\n[${OTHER_MARKER}]`)]));
      await frames.waitFor(2);
      expect(reporter.received()).toEqual([{ marker: OTHER_MARKER, entry: "entry:late" }]);
      expect(frames.frames()[1].receipts).toEqual([{ marker: OTHER_MARKER, entry: "entry:late" }]);
      reporter.dispose("quit");
    });
  });
});
