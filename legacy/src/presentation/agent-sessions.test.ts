// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { computed, effectScope, ref, watch } from "vue";
import type { AgentEvent, AgentSession } from "../domain/agent";
import type { Checkout, Repo } from "../domain/workspace";

const mocks = vi.hoisted(() => ({
  listAgentSessions: vi.fn(),
  listAgentCandidateSessions: vi.fn(),
  listAgentAgents: vi.fn(),
  listAgentRelocations: vi.fn(),
  createAgentSession: vi.fn(),
  stopAgent: vi.fn(),
  listen: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));
vi.mock("../lib/ipc", () => ({
  listAgentSessions: mocks.listAgentSessions,
  listAgentCandidateSessions: mocks.listAgentCandidateSessions,
  listAgentAgents: mocks.listAgentAgents,
  listAgentRelocations: mocks.listAgentRelocations,
  createAgentSession: mocks.createAgentSession,
  stopAgent: mocks.stopAgent,
}));

import { applyAgentEvent, useAgentRelocations, useAgentSessions, useTerminalAgentRows } from "./agent-sessions";

function session(overrides: Partial<AgentSession> = {}): AgentSession {
  return {
    id: "ses_one",
    checkoutId: "checkout:first",
    title: "review",
    running: false,
    idleAt: 1,
    awaitingReply: false,
    agent: null,
    model: null,
    parentId: null,
    outcome: null,
    createdAt: 1,
    updatedAt: 1,
    ...overrides,
  };
}

const checkout: Checkout = {
  id: "checkout:first",
  repoId: "repo:test",
  path: "/first",
  canonicalPath: "/first",
  isPrimary: true,
  changedFiles: 1,
  isMissing: false,
  sessions: [],
};
const gitRepo: Repo = {
  id: "repo:test",
  kind: "git",
  name: "test",
  root: "/first",
  defaultBranch: "trunk",
  checkouts: [],
  createdAt: "now",
  lastOpenedAt: "now",
};

async function settle() {
  // Enough microtask turns for the two sequential reads a row makes (sessions, then the
  // catalog) plus the awaits in between them.
  for (let index = 0; index < 12; index += 1) await Promise.resolve();
}

describe("applyAgentEvent", () => {
  it("shows a started turn as working at once instead of waiting for the next read", () => {
    const started = applyAgentEvent([session()], event({ kind: "turnStarted" }));
    expect(started[0].running).toBe(true);
  });

  it.each(["permissionAsked", "questionAsked"] as const)("reports %s without overwriting running", (kind) => {
    const blocked = applyAgentEvent([session({ running: true })], event({ kind }));
    expect(blocked[0].awaitingReply).toBe(true);
    expect(blocked[0].running).toBe(true);
  });

  it("ignores events for another session and events with no session", () => {
    const sessions = [session()];
    expect(applyAgentEvent(sessions, event({ kind: "turnStarted", sessionId: "ses_other" }))).toEqual(sessions);
    expect(applyAgentEvent(sessions, event({ kind: "turnStarted", sessionId: null }))).toEqual(sessions);
  });
});

function event(overrides: Partial<AgentEvent>): AgentEvent {
  return {
    checkoutId: "checkout:first",
    sessionId: "ses_one",
    kind: "unknown",
    rawType: "session.inbox.enqueued",
    data: {},
    ...overrides,
  };
}

describe("useAgentSessions", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.listAgentSessions.mockResolvedValue([]);
    // An empty catalog is the common case: a row with no color of its own falls back.
    mocks.listAgentAgents.mockResolvedValue([]);
    // The bridge answers a registration with the release for it, which the hook keeps and uses.
    mocks.listen.mockImplementation(async () => vi.fn());
  });

  it("defaults the target to the most recently updated session", async () => {
    mocks.listAgentSessions.mockResolvedValue([
      session({ id: "ses_old", updatedAt: 1 }),
      session({ id: "ses_new", updatedAt: 9 }),
    ]);
    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await settle();

    expect(state.state).toBe("ready");
    expect(state.targetId).toBe("ses_new");
  });

  it("keeps a chosen target across a reload while it still exists", async () => {
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one" }), session({ id: "ses_two", updatedAt: 9 })]);
    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await settle();

    state.selectTarget("ses_one");
    expect(state.targetId).toBe("ses_one");

    expect(await state.reload()).toBe(true);
    expect(mocks.listAgentSessions).toHaveBeenCalledTimes(2);
    expect(state.targetId).toBe("ses_one");

    // Gone from the server: the target follows what is actually there.
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_two", updatedAt: 9 })]);
    expect(await state.reload()).toBe(true);
    expect(state.targetId).toBe("ses_two");
  });

  it("surfaces an agent that cannot start", async () => {
    mocks.listAgentSessions.mockRejectedValue({
      code: "agent_unavailable",
      message: "OpenCode is unavailable",
    });
    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await settle();

    expect(state.state).toBe("error");
    expect(state.error).toBe("OpenCode is unavailable");
    expect(state.sessions).toEqual([]);
  });

  it("does not start a server for a plain folder", async () => {
    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => ({ ...gitRepo, kind: "plain" }) as Repo),
    );
    await settle();

    expect(mocks.listAgentSessions).not.toHaveBeenCalled();
    expect(state.state).toBe("ready");
  });

  it("applies live events for the active checkout only", async () => {
    mocks.listAgentSessions.mockResolvedValue([session()]);
    let handler: ((event: { payload: AgentEvent }) => void) | undefined;
    mocks.listen.mockImplementation(async (_name: string, callback: typeof handler) => {
      handler = callback;
      return vi.fn();
    });
    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await vi.waitFor(() => expect(handler).toBeDefined());
    expect(mocks.listen).toHaveBeenCalledTimes(1);

    // `running` is re-read from the service after a turn event, so the durable record of the
    // event is what this asserts; the working state is covered by the completion tests.
    handler?.({ payload: event({ kind: "turnStarted" }) });
    await settle();
    expect(state.events.at(-1)?.kind).toBe("turnStarted");

    // Asked events show waiting optimistically; subsequent polling is authoritative.
    handler?.({ payload: event({ kind: "permissionAsked", rawType: "permission.asked" }) });
    await settle();
    expect(state.sessions[0].awaitingReply).toBe(true);
    expect(state.sessions[0].running).toBe(false);

    handler?.({ payload: event({ kind: "turnStarted", checkoutId: "checkout:other" }) });
    await settle();
    // Another checkout's events are never applied here.
    expect(state.events.every((item) => item.checkoutId === "checkout:first")).toBe(true);
  });

  it("counts a turn that was seen to finish, and does not invent one that never ran", async () => {
    vi.useFakeTimers();
    // A session that has never run is not reported running, so it must not read as working.
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", idleAt: null })]);
    let handler: ((event: { payload: AgentEvent }) => void) | undefined;
    mocks.listen.mockImplementation(async (_name: string, callback: typeof handler) => {
      handler = callback;
      return vi.fn();
    });
    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await settle();
    expect(state.sessions[0].running).toBe(false);
    expect(state.turnsCompleted).toBe(0);

    // A turn starts, which the service announces and then reports. `running` is what it says,
    // not an inference from an idle time, so the row is working on the service's word. The
    // mocked read is set first, because the event triggers the read.
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", running: true })]);
    handler?.({ payload: event({ kind: "turnStarted" }) });
    await settle();
    expect(state.sessions[0].running).toBe(true);

    // The service stops reporting it: that transition is the completion signal.
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", idleAt: 99 })]);
    await vi.advanceTimersByTimeAsync(2000);
    await settle();
    expect(state.turnsCompleted).toBe(1);
    expect(state.sessions[0].running).toBe(false);

    // Nothing is running any more, so an idle checkout is not polled.
    const callsWhenIdle = mocks.listAgentSessions.mock.calls.length;
    await vi.advanceTimersByTimeAsync(6000);
    expect(mocks.listAgentSessions.mock.calls.length).toBe(callsWhenIdle);
    expect(state.turnsCompleted).toBe(1);
    vi.useRealTimers();
  });

  it("counts a turn the person started in their own TUI, which this client never prompted", async () => {
    vi.useFakeTimers();
    // No event on this stream is needed: the service reports the turn running on its own, which
    // is the whole point of reading `/api/session/active`. Before, a row could only call a turn
    // working if this client had seen it start, so a turn typed in the TUI looked idle.
    mocks.listen.mockImplementation(async () => vi.fn());
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", running: true })]);
    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(state.sessions[0].running).toBe(true);
    expect(state.turnsCompleted).toBe(0);

    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", idleAt: 99 })]);
    await vi.advanceTimersByTimeAsync(2000);
    await settle();
    expect(state.turnsCompleted).toBe(1);
    vi.useRealTimers();
  });

  it.each([false, true])(
    "polls pending turns without counting completion, initial pending: %s",
    async (initialPending) => {
      vi.useFakeTimers();
      mocks.listAgentSessions.mockResolvedValue([session({ running: !initialPending, awaitingReply: initialPending })]);
      const state = useAgentSessions(
        computed(() => checkout),
        computed(() => gitRepo),
      );
      await settle();
      mocks.listAgentSessions.mockResolvedValue([session({ awaitingReply: true })]);
      await vi.advanceTimersByTimeAsync(2000);
      expect(state.turnsCompleted).toBe(0);
      expect(state.sessions[0].awaitingReply).toBe(true);
      await vi.advanceTimersByTimeAsync(4000);
      expect(state.turnsCompleted).toBe(0);

      mocks.listAgentSessions.mockRejectedValue(new Error("pending endpoint failed"));
      await vi.advanceTimersByTimeAsync(2000);
      expect(state.state).toBe("error");
      expect(state.sessions[0].awaitingReply).toBe(true);
      expect(state.turnsCompleted).toBe(0);

      mocks.listAgentSessions.mockResolvedValue([session()]);
      await vi.advanceTimersByTimeAsync(2000);
      expect(state.sessions[0].awaitingReply).toBe(false);
      expect(state.turnsCompleted).toBe(1);
      await state.reload();
      expect(state.turnsCompleted).toBe(1);
      vi.useRealTimers();
    },
  );

  it("keeps counting when a turn fails, without waiting for an idle time", async () => {
    vi.useFakeTimers();
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", running: true })]);
    let handler: ((event: { payload: AgentEvent }) => void) | undefined;
    mocks.listen.mockImplementation(async (_name: string, callback: typeof handler) => {
      handler = callback;
      return vi.fn();
    });
    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await settle();

    // A failed turn reports an error rather than going idle, and still ends.
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", idleAt: 99 })]);
    handler?.({ payload: event({ kind: "turnFailed" }) });
    await settle();
    await vi.advanceTimersByTimeAsync(2000);
    await settle();
    expect(state.turnsCompleted).toBe(1);
    vi.useRealTimers();
  });

  it("clears its sessions when the checkout changes", async () => {
    mocks.listAgentSessions.mockResolvedValue([session()]);
    const checkoutRef = ref<Checkout | null>(checkout);
    const state = useAgentSessions(
      computed(() => checkoutRef.value),
      computed(() => gitRepo),
    );
    await settle();
    expect(state.sessions).toHaveLength(1);

    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_second", checkoutId: "checkout:second" })]);
    checkoutRef.value = { ...checkout, id: "checkout:second" };
    await settle();

    expect(state.checkoutId).toBe("checkout:second");
    expect(state.sessions.map((item) => item.id)).toEqual(["ses_second"]);
  });

  it("re-reads a catalog that is still filling in, and stops asking once it is whole", async () => {
    // What the server really does: it hands over its built-in agents first and the user's own
    // agents a moment later. A catalog read once per checkout would leave that row without its
    // colour for the rest of the session.
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_coder", agent: "coder" })]);
    mocks.listAgentAgents.mockResolvedValue([
      { id: "build", name: "Build", mode: "primary", color: null, hidden: false },
    ]);

    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await settle();

    // The first read is behind, and the row is drawn in the accent it falls back to.
    expect(state.headline).toEqual({ label: "coder", color: null, attention: "none" });

    // The agent turns up in the catalog, so it is read again and the row follows it.
    mocks.listAgentAgents.mockResolvedValue([
      { id: "build", name: "Build", mode: "primary", color: null, hidden: false },
      { id: "coder", name: "coder", mode: "all", color: "#4ed6bf", hidden: false },
    ]);
    mocks.listAgentAgents.mockClear();
    await state.reload();
    expect(mocks.listAgentAgents).toHaveBeenCalledTimes(1);
    expect(state.headline).toEqual({ label: "coder", color: "#4ed6bf", attention: "none" });

    // Whole now: a reload that changes nothing does not ask again.
    await state.reload();
    expect(mocks.listAgentAgents).toHaveBeenCalledTimes(1);
  });

  it("asks again while there is no service, and connects when one appears", async () => {
    vi.useFakeTimers();
    mocks.listAgentSessions.mockRejectedValue(new Error("OpenCode is not running."));

    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(state.state).toBe("error");
    const whileDisconnected = mocks.listAgentSessions.mock.calls.length;
    expect(whileDisconnected).toBeGreaterThan(0);

    // Nothing is started on Muster's side, so the only way to notice a service is to ask again.
    await vi.advanceTimersByTimeAsync(5000);
    expect(mocks.listAgentSessions.mock.calls.length).toBeGreaterThan(whileDisconnected);

    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", title: "later" })]);
    await vi.advanceTimersByTimeAsync(5000);

    expect(state.state).toBe("ready");
    expect(state.sessions.map((entry) => entry.title)).toEqual(["later"]);

    // Connected, it stops asking about a service that is not missing.
    const onceConnected = mocks.listAgentSessions.mock.calls.length;
    await vi.advanceTimersByTimeAsync(10000);
    expect(mocks.listAgentSessions.mock.calls.length).toBe(onceConnected);
  });

  it("keeps a row on the accent when the catalog cannot be read at all", async () => {
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", agent: "plan" })]);
    mocks.listAgentAgents.mockRejectedValue(new Error("no such route"));

    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await settle();
    await state.reload();

    // A catalog that cannot be read is not an error: the session is still listed and named.
    expect(state.state).toBe("ready");
    expect(state.headline).toEqual({ label: "plan", color: null, attention: "none" });
  });

  it("releases the event subscription with the scope that asked for it", async () => {
    const unlisten = vi.fn();
    mocks.listen.mockImplementation(async () => unlisten);
    const scope = effectScope();
    scope.run(() =>
      useAgentSessions(
        computed(() => checkout),
        computed(() => gitRepo),
      ),
    );
    await settle();

    expect(mocks.listen).toHaveBeenCalledTimes(1);
    expect(unlisten).not.toHaveBeenCalled();

    // A remount leaves the scope behind, so the subscription has to go with it: one that is not
    // released is still answering events into a row that no longer exists, and every mount stacks
    // another one behind it.
    scope.stop();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("releases a subscription the bridge only hands over after the scope is gone", async () => {
    const unlisten = vi.fn();
    let answerListen!: (dispose: () => void) => void;
    mocks.listen.mockImplementation(() => new Promise<() => void>((resolve) => (answerListen = resolve)));
    const scope = effectScope();
    scope.run(() =>
      useAgentSessions(
        computed(() => checkout),
        computed(() => gitRepo),
      ),
    );
    await settle();

    scope.stop();
    expect(unlisten).not.toHaveBeenCalled();

    // The registration answers only now, with nobody left to receive what it would deliver.
    answerListen(unlisten);
    await settle();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("still reads a row when the subscription cannot be registered at all", async () => {
    mocks.listen.mockRejectedValue(new Error("event bridge is not up"));
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one" })]);
    let state: ReturnType<typeof useAgentSessions> | undefined;
    const scope = effectScope();
    scope.run(() => {
      state = useAgentSessions(
        computed(() => checkout),
        computed(() => gitRepo),
      );
    });
    await settle();

    // The polls are what keeps the row correct anyway, so a registration that fails costs live
    // events and nothing else. Left unhandled it would take the whole scope down with it.
    expect(state?.state).toBe("ready");
    expect(state?.sessions.map((entry) => entry.id)).toEqual(["ses_one"]);
    scope.stop();
  });

  it("does not start a second read while one is still on its way", async () => {
    vi.useFakeTimers();
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", running: true })]);
    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.listAgentSessions).toHaveBeenCalledTimes(1);

    // A service slower than the interval: the read is still in flight and the poll asks twice more.
    let answerRead!: (sessions: AgentSession[]) => void;
    mocks.listAgentSessions.mockImplementation(() => new Promise<AgentSession[]>((resolve) => (answerRead = resolve)));
    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentSessions).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(2000);
    await vi.advanceTimersByTimeAsync(2000);
    // The generation drops the answer of a read that was overtaken, not the work behind it: two
    // more intervals went by and no third read was spent.
    expect(mocks.listAgentSessions).toHaveBeenCalledTimes(2);

    // The ask is held rather than lost, and it runs as soon as the read in flight answers.
    answerRead([session({ id: "ses_one", idleAt: 99 })]);
    await settle();
    expect(mocks.listAgentSessions).toHaveBeenCalledTimes(3);
    expect(state.turnsCompleted).toBe(1);
    vi.useRealTimers();
  });

  it("does not let a stale failed read write its error over a newer one", async () => {
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one" })]);
    const state = useAgentSessions(
      computed(() => checkout),
      computed(() => gitRepo),
    );
    await settle();
    expect(state.state).toBe("ready");

    // Two reads overlap: the older one is slow, the newer one answers first.
    let failStale!: (cause: unknown) => void;
    mocks.listAgentSessions.mockImplementationOnce(
      () => new Promise<AgentSession[]>((_resolve, reject) => (failStale = reject)),
    );
    const stale = state.reload();
    expect(await state.reload()).toBe(true);
    expect(state.state).toBe("ready");

    // The older read failing afterwards has nothing left to correct: it was superseded while it
    // was still in flight, so its failure says nothing about the state the newer read published.
    failStale({ code: "agent_unavailable", message: "stale failure" });
    expect(await stale).toBe(false);
    expect(state.state).toBe("ready");
    expect(state.error).toBe("");
  });

  it("does not chain a queued read past the scope that asked for it", async () => {
    vi.useFakeTimers();
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", running: true })]);
    const scope = effectScope();
    scope.run(() =>
      useAgentSessions(
        computed(() => checkout),
        computed(() => gitRepo),
      ),
    );
    await settle();

    // A service slower than the fast cadence: the read is on its way and the next poll owes one
    // behind it.
    let answerRead!: (sessions: AgentSession[]) => void;
    mocks.listAgentSessions.mockImplementation(() => new Promise<AgentSession[]>((resolve) => (answerRead = resolve)));
    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentSessions).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentSessions).toHaveBeenCalledTimes(2);

    scope.stop();
    const whenReleased = mocks.listAgentSessions.mock.calls.length;
    mocks.listAgentSessions.mockResolvedValue([session({ id: "ses_one", running: true, updatedAt: 9 })]);

    // The read held behind the one on its way is dropped rather than chained: the timers that ask
    // are released with the scope, so a chain that still ran would be the last thing reading a
    // checkout nobody is drawing.
    answerRead([session({ id: "ses_one", running: true })]);
    await settle();
    expect(mocks.listAgentSessions).toHaveBeenCalledTimes(whenReleased);

    // No interval survived either.
    await vi.advanceTimersByTimeAsync(30000);
    expect(mocks.listAgentSessions).toHaveBeenCalledTimes(whenReleased);
    vi.useRealTimers();
  });
});

describe("useTerminalAgentRows", () => {
  /** Answers each checkout with the candidates its own service knows. */
  function perCheckout(byCheckout: Record<string, Partial<AgentSession>[]>): void {
    mocks.listAgentCandidateSessions.mockImplementation(async (checkoutId: string) =>
      (byCheckout[checkoutId] ?? []).map((overrides) => session({ checkoutId, ...overrides })),
    );
  }

  beforeEach(() => {
    mocks.listen.mockImplementation(async () => vi.fn());
    mocks.listAgentAgents.mockResolvedValue([
      { id: "build", name: "Build", mode: "primary", color: null, hidden: false },
      { id: "coder", name: "Coder", mode: "all", color: "#4ed6bf", hidden: false },
      { id: "plan", name: "Plan", mode: "primary", color: null, hidden: false },
    ]);
  });

  it("gives each terminal its own worktree's agent, independently", async () => {
    // The case this exists for: two Muster terminals, two worktrees, two different agents. A
    // single active-checkout answer would paint both rows with whichever worktree is selected.
    perCheckout({
      "checkout:first": [{ id: "ses_one", agent: "coder", running: true }],
      "checkout:second": [{ id: "ses_two", agent: "plan" }],
    });

    const state = useTerminalAgentRows(computed(() => ["checkout:first", "checkout:second"]));
    await settle();

    // Every session is offered rather than one headline per checkout: a terminal row is about one
    // terminal, and the only thing that can say which session it has open is its own title. A row
    // matches against this list itself, so anything the list drops cannot be named by anybody.
    expect(state.byCheckout["checkout:first"].sessions).toEqual([
      {
        id: "ses_one",
        title: "review",
        agent: { label: "Coder", color: "#4ed6bf", attention: "busy" },
        running: true,
        awaitingReply: false,
        updatedAt: 1,
      },
    ]);
    expect(state.byCheckout["checkout:second"].sessions).toEqual([
      {
        id: "ses_two",
        title: "review",
        agent: { label: "Plan", color: null, attention: "none" },
        running: false,
        awaitingReply: false,
        updatedAt: 1,
      },
    ]);
  });

  it("keeps two terminals in one worktree from being told they are different sessions", async () => {
    // One checkout, one session. Both rows speak for the same directory scope because that is
    // the only scope OpenCode can answer in; what must not happen is either row claiming a
    // session id the service never reported for this terminal.
    perCheckout({ "checkout:first": [{ id: "ses_only", agent: "coder" }] });
    mocks.listAgentCandidateSessions.mockClear();

    const state = useTerminalAgentRows(computed(() => ["checkout:first"]));
    await settle();

    expect(state.byCheckout["checkout:first"].sessions[0]?.agent?.label).toBe("Coder");
    // One read for the one checkout, asked of that checkout alone: the row cannot be drawn from
    // any other worktree's sessions, so it never asks for them.
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledTimes(1);
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledWith("checkout:first");
  });

  it("drops a checkout that no longer has terminals", async () => {
    perCheckout({ "checkout:first": [{ id: "ses_one", agent: "coder" }] });
    const ids = ref(["checkout:first", "checkout:second"]);
    const state = useTerminalAgentRows(computed(() => ids.value));
    await settle();
    expect(state.byCheckout["checkout:first"]).toBeDefined();

    // A closed worktree must stop answering, or its row outlives the terminal it described.
    ids.value = ["checkout:first"];
    await settle();
    expect(Object.keys(state.byCheckout)).toEqual(["checkout:first"]);
  });

  it("reports no agent while the service is not run, without starting one", async () => {
    mocks.listAgentCandidateSessions.mockRejectedValue(new Error("OpenCode is not running."));

    const state = useTerminalAgentRows(computed(() => ["checkout:first"]));
    await settle();

    expect(state.byCheckout["checkout:first"]).toEqual({ sessions: [] });
    expect(state.row("checkout:absent")).toEqual({ sessions: [] });
  });

  it.each([false, true])("offers a session with no agent, awaiting reply: %s", async (awaitingReply) => {
    // A fresh session has no agent behind it yet, which is not the same as not existing: its title
    // is what a terminal with it open writes into its own title, and the row can still say which
    // session it is looking at. It just has no mode to name.
    perCheckout({ "checkout:first": [{ id: "ses_fresh", agent: null, awaitingReply }] });

    const state = useTerminalAgentRows(computed(() => ["checkout:first"]));
    await settle();

    // The clock rides along because it is the only duration a terminal row has: the elapsed time in a
    // row's trailing slot is this session's own last update, read from the service and nowhere else.
    expect(state.byCheckout["checkout:first"].sessions).toEqual([
      { id: "ses_fresh", title: "review", agent: null, running: false, awaitingReply, updatedAt: 1 },
    ]);
  });

  it("publishes pending transitions without an agent or any other session change", async () => {
    perCheckout({ "checkout:first": [{ id: "ses_one", agent: null }] });
    const scope = effectScope();
    const state = scope.run(() => useTerminalAgentRows(computed(() => ["checkout:first"])))!;
    await settle();
    for (const awaitingReply of [false, true, false]) {
      perCheckout({ "checkout:first": [{ id: "ses_one", agent: null, awaitingReply }] });
      await state.reload();
      expect(state.row("checkout:first").sessions).toEqual([
        { id: "ses_one", title: "review", agent: null, running: false, awaitingReply, updatedAt: 1 },
      ]);
    }
    scope.stop();
  });

  it("re-reads fast while a row is running and slowly once every row is idle", async () => {
    vi.useFakeTimers();
    perCheckout({ "checkout:first": [{ id: "ses_one", agent: "coder", running: true }] });

    const state = useTerminalAgentRows(computed(() => ["checkout:first"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(state.byCheckout["checkout:first"].sessions[0]?.running).toBe(true);
    const whileRunning = mocks.listAgentCandidateSessions.mock.calls.length;

    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentCandidateSessions.mock.calls.length).toBeGreaterThan(whileRunning);

    perCheckout({ "checkout:first": [{ id: "ses_one", agent: "coder" }] });
    await vi.advanceTimersByTimeAsync(2000);
    expect(state.byCheckout["checkout:first"].sessions[0]?.running).toBe(false);

    // Idle is not the end of the questions. A turn started in the person's own TUI announces nothing
    // to this hook, so a row that stopped asking would say "idle" while the agent works, and nothing
    // would correct it until some unrelated event reloaded the list.
    const onceIdle = mocks.listAgentCandidateSessions.mock.calls.length;
    await vi.advanceTimersByTimeAsync(5000);
    expect(mocks.listAgentCandidateSessions.mock.calls.length).toBeGreaterThan(onceIdle);
    const slow = mocks.listAgentCandidateSessions.mock.calls.length - onceIdle;

    // And it asks at the slow cadence, not the fast one: a second 5s window costs about one request.
    await vi.advanceTimersByTimeAsync(5000);
    expect(mocks.listAgentCandidateSessions.mock.calls.length - onceIdle).toBeLessThanOrEqual(slow + 1);

    // A turn that began while the panel was idle is seen, which is the whole reason for the above.
    perCheckout({ "checkout:first": [{ id: "ses_one", agent: "coder", running: true }] });
    await vi.advanceTimersByTimeAsync(5000);
    expect(state.byCheckout["checkout:first"].sessions[0]?.running).toBe(true);
    vi.useRealTimers();
  });

  it("asks only about the busy checkouts when the fast poll fires", async () => {
    vi.useFakeTimers();
    perCheckout({
      "checkout:first": [{ id: "ses_one", agent: "coder", running: true }],
      "checkout:second": [{ id: "ses_two", agent: "plan" }],
    });

    const state = useTerminalAgentRows(computed(() => ["checkout:first", "checkout:second"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(state.byCheckout["checkout:second"].sessions[0]?.running).toBe(false);
    mocks.listAgentCandidateSessions.mockClear();

    await vi.advanceTimersByTimeAsync(2000);
    // One agent is working and the other row is idle, so the idle worktree is not asked about at
    // all: the fast poll exists to notice a turn ending, and nothing about an idle one has changed.
    expect(mocks.listAgentCandidateSessions.mock.calls.map(([id]) => id)).toEqual(["checkout:first"]);
    vi.useRealTimers();
  });

  it("asks about every checkout when the slow poll fires", async () => {
    vi.useFakeTimers();
    perCheckout({
      "checkout:first": [{ id: "ses_one", agent: "coder" }],
      "checkout:second": [{ id: "ses_two", agent: "plan" }],
    });

    useTerminalAgentRows(computed(() => ["checkout:first", "checkout:second"]));
    await vi.advanceTimersByTimeAsync(0);
    mocks.listAgentCandidateSessions.mockClear();

    // Nothing is running, so this is the slow poll's question, and it is the only one that can find
    // a turn somebody started in their own TUI: both worktrees are asked, the idle one included.
    await vi.advanceTimersByTimeAsync(5000);
    expect(mocks.listAgentCandidateSessions.mock.calls.map(([id]) => id).sort()).toEqual([
      "checkout:first",
      "checkout:second",
    ]);
    vi.useRealTimers();
  });

  it("does not start a second pass while one is still on its way", async () => {
    vi.useFakeTimers();
    perCheckout({ "checkout:first": [{ id: "ses_one", agent: "coder", running: true }] });
    mocks.listAgentCandidateSessions.mockClear();

    useTerminalAgentRows(computed(() => ["checkout:first"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledTimes(1);

    // A service slower than the interval: the pass is still reading and the poll asks twice more.
    let answerRead!: (sessions: AgentSession[]) => void;
    mocks.listAgentCandidateSessions.mockImplementation(
      () => new Promise<AgentSession[]>((resolve) => (answerRead = resolve)),
    );
    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(2000);
    await vi.advanceTimersByTimeAsync(2000);
    // Without the in-flight guard each of those intervals starts a pass of its own, and every
    // answer but the last one is dropped on arrival for having been overtaken.
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledTimes(2);

    answerRead([session({ id: "ses_one", agent: "coder", running: true })]);
    await settle();
    // The ask was held rather than lost: the pass behind the one on its way runs it.
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledTimes(3);
    vi.useRealTimers();
  });

  it("leaves the store alone when an answer draws the row that is already there", async () => {
    vi.useFakeTimers();
    perCheckout({ "checkout:first": [{ id: "ses_one", agent: "coder" }] });

    const state = useTerminalAgentRows(computed(() => ["checkout:first"]));
    await vi.advanceTimersByTimeAsync(0);
    const drawn = state.byCheckout["checkout:first"];

    // `byCheckout` is reactive and every terminal row reads out of it, so a poll that found nothing
    // must not hand the sidebar a new object to redraw itself from.
    await vi.advanceTimersByTimeAsync(5000);
    expect(state.byCheckout["checkout:first"]).toBe(drawn);

    // A row that would draw something else is published, so the shortcut is not a store that
    // stops updating.
    perCheckout({
      "checkout:first": [{ id: "ses_one", agent: "coder", running: true, updatedAt: 9 }],
    });
    await vi.advanceTimersByTimeAsync(5000);
    expect(state.byCheckout["checkout:first"]).not.toBe(drawn);
    expect(state.byCheckout["checkout:first"].sessions[0]?.running).toBe(true);
    vi.useRealTimers();
  });

  it("still asks about an idle checkout while another one stays busy", async () => {
    vi.useFakeTimers();
    // The regression this covers. One long turn in the first worktree used to gate the full pass off
    // entirely, and the fast poll cannot hear a turn somebody started in their own TUI: that worktree
    // is not in the busy set, so nothing would ever ask it and its row sat on "idle" for as long as
    // the first worktree kept working.
    perCheckout({
      "checkout:first": [{ id: "ses_long", agent: "coder", running: true }],
      "checkout:second": [{ id: "ses_quiet", agent: "plan" }],
    });

    const state = useTerminalAgentRows(computed(() => ["checkout:first", "checkout:second"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(state.byCheckout["checkout:second"].sessions[0]?.running).toBe(false);

    // Somebody starts a turn in the quiet worktree's own TUI. Nothing on this stream announces it,
    // and the first worktree goes on working through every slow window below.
    perCheckout({
      "checkout:first": [{ id: "ses_long", agent: "coder", running: true }],
      "checkout:second": [{ id: "ses_quiet", agent: "plan", running: true }],
    });

    await vi.advanceTimersByTimeAsync(20000);
    expect(state.byCheckout["checkout:first"].sessions[0]?.running).toBe(true);
    expect(state.byCheckout["checkout:second"].sessions[0]?.running).toBe(true);
    vi.useRealTimers();
  });

  it("keeps the fast pass narrow to the busy checkouts and leaves the width to the slow one", async () => {
    vi.useFakeTimers();
    perCheckout({
      "checkout:first": [{ id: "ses_one", agent: "coder", running: true }],
      "checkout:second": [{ id: "ses_two", agent: "plan" }],
      "checkout:third": [{ id: "ses_three", agent: "plan" }],
    });

    useTerminalAgentRows(computed(() => ["checkout:first", "checkout:second", "checkout:third"]));
    await vi.advanceTimersByTimeAsync(0);
    mocks.listAgentCandidateSessions.mockClear();

    // A fast window asks about the one working checkout alone: that scoping is the load the poll
    // removes, and it must survive the slow pass being widened back to everything.
    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentCandidateSessions.mock.calls.map(([id]) => id)).toEqual(["checkout:first"]);

    // The next fast window is just as narrow, and then the slow one asks about everybody: that is
    // the pass that can find a turn somebody started outside the app.
    await vi.advanceTimersByTimeAsync(3000);
    expect(mocks.listAgentCandidateSessions.mock.calls.map(([id]) => id)).toEqual([
      "checkout:first",
      "checkout:first",
      "checkout:first",
      "checkout:second",
      "checkout:third",
    ]);
    vi.useRealTimers();
  });

  it("widens the queued pass instead of losing a full ask that lands during a busy one", async () => {
    vi.useFakeTimers();
    perCheckout({
      "checkout:first": [{ id: "ses_one", agent: "coder", running: true }],
      "checkout:second": [{ id: "ses_two", agent: "plan" }],
    });

    useTerminalAgentRows(computed(() => ["checkout:first", "checkout:second"]));
    await vi.advanceTimersByTimeAsync(0);
    mocks.listAgentCandidateSessions.mockClear();

    // A service slower than the fast cadence: the busy pass is still reading when the next fast one
    // and the slow one both ask behind it.
    let answerRead!: (sessions: AgentSession[]) => void;
    mocks.listAgentCandidateSessions.mockImplementation(
      () => new Promise<AgentSession[]>((resolve) => (answerRead = resolve)),
    );
    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(3000);
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledTimes(1);

    // The pass that runs behind the one on its way is the full one. The queued "all" widened the
    // queued "busy" rather than being replaced by it, which is the only thing that ever gets the
    // idle worktree asked here.
    mocks.listAgentCandidateSessions.mockClear();
    answerRead([session({ id: "ses_one", checkoutId: "checkout:first", agent: "coder", running: true })]);
    await settle();
    expect(mocks.listAgentCandidateSessions.mock.calls.map(([id]) => id).sort()).toEqual([
      "checkout:first",
      "checkout:second",
    ]);
    vi.useRealTimers();
  });

  it("does not put back a checkout whose terminal closed while the read was on its way", async () => {
    vi.useFakeTimers();
    perCheckout({
      "checkout:first": [{ id: "ses_one", agent: "coder" }],
      "checkout:second": [{ id: "ses_two", agent: "plan" }],
    });
    const ids = ref(["checkout:first", "checkout:second"]);

    const state = useTerminalAgentRows(computed(() => ids.value));
    await settle();
    expect(Object.keys(state.byCheckout).sort()).toEqual(["checkout:first", "checkout:second"]);

    // Every write to the store is recorded whole, because a row that comes back and is dropped again in
    // the same tick is not something the end state can show: what matters is that it was never drawn.
    const drawn: string[] = [];
    watch(
      () => JSON.stringify(state.byCheckout),
      (store) => drawn.push(store),
      { flush: "sync" },
    );

    // The full pass is still reading when the worktree goes away.
    const answers = new Map<string, (sessions: AgentSession[]) => void>();
    mocks.listAgentCandidateSessions.mockImplementation(
      (checkoutId: string) => new Promise<AgentSession[]>((resolve) => answers.set(checkoutId, resolve)),
    );
    await vi.advanceTimersByTimeAsync(5000);
    expect([...answers.keys()].sort()).toEqual(["checkout:first", "checkout:second"]);

    // The answer about the closed worktree arrives after the terminal is gone, and it is not the answer
    // that was there before: writing it would give the closed worktree a row back that says it is
    // working, and the pass behind this one would only have to take it away again.
    ids.value = ["checkout:first"];
    const before = drawn.length;
    answers.get("checkout:second")?.([
      session({ id: "ses_two", checkoutId: "checkout:second", agent: "plan", running: true }),
    ]);
    answers.get("checkout:first")?.([session({ id: "ses_one", checkoutId: "checkout:first", agent: "coder" })]);
    await settle();

    expect(drawn.slice(before).some((store) => store.includes("checkout:second"))).toBe(false);
    expect(Object.keys(state.byCheckout)).toEqual(["checkout:first"]);
    expect(state.row("checkout:second")).toEqual({ sessions: [] });
    vi.useRealTimers();
  });

  it("leaves no timer and no queued pass behind when the scope is released mid-pass", async () => {
    vi.useFakeTimers();
    perCheckout({ "checkout:first": [{ id: "ses_one", agent: "coder", running: true }] });
    mocks.listAgentCandidateSessions.mockClear();

    const scope = effectScope();
    const state = scope.run(() => useTerminalAgentRows(computed(() => ["checkout:first"])))!;
    await settle();
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledTimes(1);

    // A service slower than the fast cadence: the pass is on its way and the next interval owes one.
    let answerRead!: (sessions: AgentSession[]) => void;
    mocks.listAgentCandidateSessions.mockImplementation(
      () => new Promise<AgentSession[]>((resolve) => (answerRead = resolve)),
    );
    await vi.advanceTimersByTimeAsync(2000);
    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledTimes(2);

    scope.stop();
    const whenReleased = mocks.listAgentCandidateSessions.mock.calls.length;
    perCheckout({
      "checkout:first": [{ id: "ses_one", agent: "coder", running: true, updatedAt: 9 }],
    });

    // The answer lands after the row is gone. The pass owed behind it must not run: its timers are
    // released, so a chain that still ran would be the only thing left polling this checkout.
    answerRead([session({ id: "ses_one", checkoutId: "checkout:first", agent: "coder", running: true })]);
    await settle();
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledTimes(whenReleased);
    expect(state.byCheckout["checkout:first"].sessions[0]?.updatedAt).toBe(1);

    // And no interval survived the scope: advancing well past both cadences asks for nothing.
    await vi.advanceTimersByTimeAsync(30000);
    expect(mocks.listAgentCandidateSessions).toHaveBeenCalledTimes(whenReleased);
    vi.useRealTimers();
  });
});

describe("useAgentRelocations", () => {
  // Every test runs its hook in a scope it stops: an interval left running keeps asking for the
  // rest of the file, and a read this test never made is indistinguishable from one it did.
  beforeEach(() => {
    mocks.listAgentRelocations.mockClear();
    mocks.listAgentRelocations.mockResolvedValue([]);
  });

  it("reports each move the service named, and keeps asking while everything is idle", async () => {
    // The turn that moves a session also ends it, so a poll that stopped when nothing was
    // running would be the poll that was not running when the move happened.
    vi.useFakeTimers();
    mocks.listAgentRelocations.mockResolvedValue([]);
    const seen: string[] = [];
    const scope = effectScope();
    scope.run(() =>
      useAgentRelocations(
        (relocation) => seen.push(`${relocation.fromCheckoutId}->${relocation.toCheckoutId}`),
        ref(true),
      ),
    );
    await vi.advanceTimersByTimeAsync(0);
    const afterFirstRead = mocks.listAgentRelocations.mock.calls.length;
    await vi.advanceTimersByTimeAsync(6000);
    expect(mocks.listAgentRelocations.mock.calls.length).toBeGreaterThan(afterFirstRead);

    mocks.listAgentRelocations.mockResolvedValue([
      { sessionId: "ses_one", fromCheckoutId: "checkout:first", toCheckoutId: "checkout:second" },
    ]);
    await vi.advanceTimersByTimeAsync(2000);
    expect(seen).toEqual(["checkout:first->checkout:second"]);
    scope.stop();
    vi.useRealTimers();
  });

  it("reports nothing while the service is not run, without failing the caller", async () => {
    mocks.listAgentRelocations.mockRejectedValue(new Error("OpenCode is not running."));
    const onRelocated = vi.fn();
    const scope = effectScope();
    scope.run(() => useAgentRelocations(onRelocated, ref(true)));
    await settle();
    expect(onRelocated).not.toHaveBeenCalled();
    scope.stop();
  });

  it("still reports a move from an answer that arrives after the next poll went out", async () => {
    // A service slower than the interval used to lose every answer: each poll's answer was dropped
    // as superseded by the next one, and a service that is always slower than the interval would
    // never report a move at all. One read at a time is what makes a slow answer worth waiting for.
    vi.useFakeTimers();
    const onRelocated = vi.fn();
    let release: (() => void) | undefined;
    const scope = effectScope();
    scope.run(() => useAgentRelocations(onRelocated, ref(true)));
    await vi.advanceTimersByTimeAsync(0);
    mocks.listAgentRelocations.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          release = () =>
            resolve([{ sessionId: "ses_one", fromCheckoutId: "checkout:first", toCheckoutId: "checkout:second" }]);
        }),
    );
    // Several intervals go by while that read is still out, and none of them starts a second one.
    await vi.advanceTimersByTimeAsync(6000);
    expect(mocks.listAgentRelocations).toHaveBeenCalledTimes(2);

    release?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(onRelocated).toHaveBeenCalledTimes(1);
    // Asking again after the answer landed is what keeps the next move coming.
    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentRelocations).toHaveBeenCalledTimes(3);
    scope.stop();
    vi.useRealTimers();
  });

  it("reports nothing from a read that answers after the scope is gone", async () => {
    // The timer is cleared on dispose, but a read already dispatched is still in flight, and its
    // callback would otherwise run against a window that is closing.
    vi.useFakeTimers();
    const onRelocated = vi.fn();
    let release: (() => void) | undefined;
    mocks.listAgentRelocations.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          release = () =>
            resolve([{ sessionId: "ses_one", fromCheckoutId: "checkout:first", toCheckoutId: "checkout:second" }]);
        }),
    );
    const scope = effectScope();
    scope.run(() => useAgentRelocations(onRelocated, ref(true)));
    await vi.advanceTimersByTimeAsync(0);
    scope.stop();
    release?.();
    await vi.advanceTimersByTimeAsync(0);

    expect(onRelocated).not.toHaveBeenCalled();
    vi.useRealTimers();
  });

  it("does not poll while disabled, starts immediately when enabled, and stops again when disabled", async () => {
    vi.useFakeTimers();
    const enabled = ref(false);
    const onRelocated = vi.fn();
    const scope = effectScope();
    scope.run(() => useAgentRelocations(onRelocated, enabled));

    expect(mocks.listAgentRelocations).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
    await vi.advanceTimersByTimeAsync(6000);
    expect(mocks.listAgentRelocations).not.toHaveBeenCalled();

    enabled.value = true;
    expect(mocks.listAgentRelocations).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(0);
    enabled.value = false;
    expect(vi.getTimerCount()).toBe(0);
    const stoppedAt = mocks.listAgentRelocations.mock.calls.length;
    await vi.advanceTimersByTimeAsync(6000);
    expect(mocks.listAgentRelocations).toHaveBeenCalledTimes(stoppedAt);

    scope.stop();
    vi.useRealTimers();
  });

  it("ignores an in-flight response that completes while disabled", async () => {
    vi.useFakeTimers();
    const enabled = ref(true);
    const onRelocated = vi.fn();
    let release!: () => void;
    mocks.listAgentRelocations.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          release = () =>
            resolve([{ sessionId: "ses_one", fromCheckoutId: "checkout:first", toCheckoutId: "checkout:second" }]);
        }),
    );
    const scope = effectScope();
    scope.run(() => useAgentRelocations(onRelocated, enabled));
    expect(mocks.listAgentRelocations).toHaveBeenCalledTimes(1);

    enabled.value = false;
    expect(vi.getTimerCount()).toBe(0);
    release();
    await vi.advanceTimersByTimeAsync(0);
    expect(onRelocated).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(6000);
    expect(mocks.listAgentRelocations).toHaveBeenCalledTimes(1);

    scope.stop();
    vi.useRealTimers();
  });

  it("does not replay a pending move after re-enable and still reports fresh work", async () => {
    vi.useFakeTimers();
    const enabled = ref(true);
    const onRelocated = vi.fn();
    const stale = {
      sessionId: "ses_one",
      fromCheckoutId: "checkout:first",
      toCheckoutId: "checkout:second",
      observedAt: 10,
    };
    const fresh = { ...stale, toCheckoutId: "checkout:third", observedAt: 20 };
    const scope = effectScope();
    scope.run(() => useAgentRelocations(onRelocated, enabled));
    await vi.advanceTimersByTimeAsync(0);

    let release!: () => void;
    mocks.listAgentRelocations.mockImplementationOnce(
      () => new Promise((resolve) => (release = () => resolve([stale]))),
    );
    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentRelocations).toHaveBeenNthCalledWith(2, false);
    enabled.value = false;
    release();
    await vi.advanceTimersByTimeAsync(0);
    expect(onRelocated).not.toHaveBeenCalled();

    mocks.listAgentRelocations.mockResolvedValueOnce([stale]);
    enabled.value = true;
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.listAgentRelocations).toHaveBeenNthCalledWith(3, true);
    expect(onRelocated).not.toHaveBeenCalled();

    mocks.listAgentRelocations.mockResolvedValueOnce([fresh]);
    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentRelocations).toHaveBeenNthCalledWith(4, false);
    expect(onRelocated).toHaveBeenCalledExactlyOnceWith(fresh);

    scope.stop();
    vi.useRealTimers();
  });

  it("baselines the latest enable epoch after a slow request and reports only later work", async () => {
    vi.useFakeTimers();
    const enabled = ref(true);
    const onRelocated = vi.fn();
    let release!: () => void;
    mocks.listAgentRelocations.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          release = () => resolve([]);
        }),
    );
    const scope = effectScope();
    scope.run(() => useAgentRelocations(onRelocated, enabled));
    expect(mocks.listAgentRelocations).toHaveBeenNthCalledWith(1, true);

    enabled.value = false;
    enabled.value = true;
    enabled.value = false;
    enabled.value = true;
    mocks.listAgentRelocations.mockResolvedValue([
      { sessionId: "ses_one", fromCheckoutId: "checkout:first", toCheckoutId: "checkout:second" },
    ]);
    release();
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.listAgentRelocations).toHaveBeenNthCalledWith(2, true);
    expect(onRelocated).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(2000);
    expect(mocks.listAgentRelocations).toHaveBeenNthCalledWith(3, false);
    expect(onRelocated).toHaveBeenCalledWith({
      sessionId: "ses_one",
      fromCheckoutId: "checkout:first",
      toCheckoutId: "checkout:second",
    });

    scope.stop();
    vi.useRealTimers();
  });
});
