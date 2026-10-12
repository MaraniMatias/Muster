import { listen } from "@tauri-apps/api/event";
import { onScopeDispose, reactive, watch } from "vue";
import type { ComputedRef, Ref } from "vue";
import type { AgentAttention, AgentAgent, AgentEvent, AgentRelocation, AgentSession } from "../domain/agent";
import {
  agentAttention,
  agentColor,
  agentLabel,
  defaultAgentSession,
  headlineSession,
  isTurnEvent,
} from "../domain/agent";
import { isIpcError } from "../domain/ipc";
import type { Checkout, Repo } from "../domain/workspace";
import {
  createAgentSession,
  listAgentAgents,
  listAgentCandidateSessions,
  listAgentRelocations,
  listAgentSessions,
  stopAgent,
} from "../lib/ipc";

/** Name of the Tauri event the bridge emits normalized agent events on. */
export const AGENT_EVENT = "muster://agent-event";

/** What a row says about the agent: which one, in OpenCode's color, and how loudly. */
export interface AgentHeadline {
  label: string;
  color: string | null;
  attention: AgentAttention;
}

export interface ActiveAgentSessions {
  checkoutId: string | null;
  sessions: AgentSession[];
  /** Every agent the checkout's server offers, for the colors and names above. */
  agents: AgentAgent[];
  /** The one session a row would speak for, already resolved to a name and a color. */
  headline: AgentHeadline | null;
  /** The session a review round goes to unless the user picks another. */
  targetId: string | null;
  state: "loading" | "ready" | "error";
  error: string;
  /** Live events for the active checkout, newest last. */
  events: AgentEvent[];
  /**
   * Increments once per turn that was seen to finish. The service announces no completion, so
   * consumers that need to re-check a finished turn watch this.
   */
  turnsCompleted: number;
  reload(): Promise<boolean>;
  createSession(title: string): Promise<AgentSession | null>;
  selectTarget(sessionId: string | null): void;
  stop(): Promise<void>;
}

const MAX_EVENTS = 200;
/**
 * How often a working session is re-read while the service reports a turn running.
 *
 * The service announces no turn-completed event: a turn ends in silence and only
 * `/api/session/active` stops reporting it. So an ending has to be observed, and this is the cost.
 */
const BUSY_POLL_MS = 2000;
/**
 * How often the service is asked about again on the slow question.
 *
 * Both hooks that use it are asking something a person decides on their own schedule — whether they
 * have started OpenCode, whether a turn has begun — and neither is answered by an event this app
 * receives. Named for the cadence rather than for one of its two callers.
 */
const SLOW_POLL_MS = 5000;

/**
 * How often to ask where every session is working.
 *
 * The turn that moves a session also ends it, and this app hears a turn end only by reading
 * again, so a poll that stopped while everything was idle would be the poll that was not
 * running when the move happened. This one does not stop: it is one request to a server on the
 * same machine, and it is the only way a change made inside someone's own TUI is ever seen.
 */
const RELOCATION_POLL_MS = 2000;

function errorText(cause: unknown): string {
  return isIpcError(cause) ? cause.message : cause instanceof Error ? cause.message : String(cause);
}

/**
 * Counts settled turns, keeping running or awaiting-reply turns in `wasRunning`.
 *
 * `wasRunning` is updated in place because it is the only record of what the previous read said:
 * A pending reply is not completion, even when the service no longer reports running.
 */
function settledTurns(sessions: AgentSession[], wasRunning: Set<string>): number {
  let settled = 0;
  for (const session of sessions) {
    if (session.running || session.awaitingReply) {
      wasRunning.add(session.id);
    } else if (wasRunning.delete(session.id)) {
      settled += 1;
    }
  }
  // A session the service no longer lists cannot be running, so it cannot still be owed a turn.
  const listed = new Set(sessions.map((session) => session.id));
  for (const sessionId of [...wasRunning]) {
    if (!listed.has(sessionId)) wasRunning.delete(sessionId);
  }
  return settled;
}

/**
 * Records what the service reports without counting anything, for the first read of a checkout.
 *
 * The first read has nothing to compare against, so it only seeds the record. Skipping this would
 * leave a turn that was already running when the checkout was opened looking like it had never
 * run, and its ending would go unnoticed.
 */
function recordRunning(sessions: AgentSession[], wasRunning: Set<string>): void {
  wasRunning.clear();
  for (const session of sessions) {
    if (session.running || session.awaitingReply) wasRunning.add(session.id);
  }
}

/**
 * Applies one event to the session list.
 *
 * The server's own truth is authoritative and is re-read on every turn event, because
 * `/api/session/active` is what decides `running`. An event only moves the row between reads:
 * a turn that started shows as working at once instead of at the next poll, and one that ended
 * is left to the re-read rather than being declared finished here.
 */
export function applyAgentEvent(sessions: AgentSession[], event: AgentEvent): AgentSession[] {
  if (!event.sessionId) return sessions;
  return sessions.map((session) => {
    if (session.id !== event.sessionId) return session;
    switch (event.kind) {
      case "turnStarted":
        return { ...session, running: true };
      case "permissionAsked":
      case "questionAsked":
        // The next read clears this only when every pending request has been answered.
        return { ...session, awaitingReply: true };
      default:
        return session;
    }
  });
}

export function useAgentSessions(
  checkout: ComputedRef<Checkout | null>,
  repo: ComputedRef<Repo | null>,
): ActiveAgentSessions {
  let generation = 0;
  /** Whether a read asked for by a timer or by an event is still on its way. */
  let refreshing = false;
  /** Whether one more read is owed while the one on its way has not answered yet. */
  let refreshAgain = false;
  /** Whether the scope that owns the event subscription is already gone. */
  let disposed = false;
  /** The bridge's own release for the one subscription this hook holds, once it has answered. */
  let unlistenAgentEvents: (() => void) | undefined;
  /**
   * Sessions the service reported as running at the previous read.
   *
   * A turn ending is the transition out of that set, which is how a finished turn is noticed:
   * the service announces no completion event, and this also counts a turn the person ran in
   * their own TUI, which no event on this stream would ever have carried.
   */
  const wasRunning = new Set<string>();
  const state = reactive<Omit<ActiveAgentSessions, "reload" | "createSession" | "selectTarget" | "stop">>({
    checkoutId: null,
    sessions: [],
    agents: [],
    headline: null,
    targetId: null,
    state: "ready",
    error: "",
    events: [],
    turnsCompleted: 0,
  });

  /**
   * Resolves the row's line from the session list and the catalog together.
   *
   * It is a watch rather than a computed because the whole of `state` is reactive already, and
   * the row must not be able to disagree with the list it was drawn from.
   */
  function syncHeadline() {
    const session = headlineSession(state.sessions, state.targetId);
    const label = agentLabel(state.agents, session?.agent);
    state.headline =
      session && label
        ? { label, color: agentColor(state.agents, session.agent), attention: agentAttention(session) }
        : null;
  }

  watch(() => [state.sessions, state.agents, state.targetId], syncHeadline);

  function fail(cause: unknown): false {
    state.error = errorText(cause);
    state.state = "error";
    return false;
  }

  /**
   * Re-reads the catalog when a session names an agent it does not have.
   *
   * The server fills that list in stages: right after boot it holds its own built-in agents and
   * the ones from the project, and the ones from the user's agent directory arrive later. Read
   * once per checkout that would leave a row without its color for the rest of the session, so
   * the gap is what triggers the read. In the steady state this asks for nothing.
   */
  async function refreshAgentsBehind(sessions: AgentSession[]) {
    const checkoutId = state.checkoutId;
    if (!checkoutId) return;
    const known = new Set(state.agents.map((agent) => agent.id));
    if (!sessions.some((session) => session.agent && !known.has(session.agent))) return;
    try {
      const agents = await listAgentAgents(checkoutId);
      // A newer checkout may have taken over while this was in flight.
      if (state.checkoutId === checkoutId) state.agents = agents;
    } catch {
      // A catalog that cannot be read leaves the row with the accent, which it already had.
    }
  }

  async function reload() {
    const checkoutId = state.checkoutId;
    // A scope that is gone has no row left to fill in, so a read asked for after it is not even
    // sent. The timers that ask are released with the scope, but the event handler is not, and a
    // read that outlives the row it was asked for is work nobody is waiting on.
    if (!checkoutId || disposed) return false;
    const request = ++generation;
    try {
      const sessions = await listAgentSessions(checkoutId);
      if (disposed || state.checkoutId !== checkoutId || request !== generation) return false;
      state.turnsCompleted += settledTurns(sessions, wasRunning);
      state.sessions = sessions;
      // Keep an explicit choice only while it still exists; otherwise follow the newest.
      if (!state.targetId || !state.sessions.some((session) => session.id === state.targetId)) {
        state.targetId = defaultAgentSession(state.sessions)?.id ?? null;
      }
      state.state = "ready";
      state.error = "";
      await refreshAgentsBehind(sessions);
      return true;
    } catch (cause) {
      // Checked against the disposal and the generation as the success path checks them. A read
      // that was superseded answering late has nothing left to say: without this, one stale failure
      // wrote its error over whatever the newer read had already put in place.
      return !disposed && state.checkoutId === checkoutId && request === generation ? fail(cause) : false;
    }
  }

  /**
   * Asks for a read, but never two at once.
   *
   * Both timers and the event listener ask on a schedule of their own, so a service slower than
   * `BUSY_POLL_MS` used to chain reads that each waited on the one before it and then threw the
   * answer away: `generation` drops the answer, and this drops the work behind it. One read is on
   * its way and one more is held behind that, which is what three callers firing in the same moment
   * means. `reload()` stays the raw read rather than this wrapper because it answers whether it
   * read anything, and folding one caller's read into another's answer would make that a lie.
   *
   * The read held behind the one on its way is dropped when the scope goes, not chained: the timers
   * that ask are released with the scope, so a chain still running would be the last thing left
   * reading a checkout nobody is drawing.
   */
  function refreshNow() {
    if (refreshing) {
      refreshAgain = true;
      return;
    }
    refreshing = true;
    void reload().finally(() => {
      refreshing = false;
      if (!refreshAgain || disposed) return;
      refreshAgain = false;
      refreshNow();
    });
  }

  async function createSession(title: string) {
    const checkoutId = state.checkoutId;
    if (!checkoutId) return null;
    try {
      const created = await createAgentSession(checkoutId, title);
      if (state.checkoutId !== checkoutId) return null;
      state.sessions = [created, ...state.sessions];
      state.targetId = created.id;
      state.error = "";
      return created;
    } catch (cause) {
      fail(cause);
      return null;
    }
  }

  function selectTarget(sessionId: string | null) {
    state.targetId = sessionId;
  }

  async function stop() {
    const checkoutId = state.checkoutId;
    if (!checkoutId) return;
    try {
      await stopAgent(checkoutId);
      state.sessions = [];
      state.targetId = null;
      state.error = "";
    } catch (cause) {
      fail(cause);
    }
  }

  watch(
    [() => checkout.value?.id, () => checkout.value?.isMissing, () => repo.value?.kind],
    async ([checkoutId, isMissing, repoKind], _previous, onCleanup) => {
      const requestGeneration = ++generation;
      let current = true;
      onCleanup(() => {
        current = false;
      });
      state.checkoutId = checkoutId ?? null;
      state.sessions = [];
      // The catalog is the same for every session in a checkout, so it is read once per
      // checkout and left alone: a name or a color that changed on disk is not worth a poll.
      state.agents = [];
      state.targetId = null;
      state.events = [];
      wasRunning.clear();
      state.error = "";
      state.state = "ready";
      if (!checkoutId || isMissing || repoKind !== "git") return;

      state.state = "loading";
      try {
        // Best effort: an agent with no catalog is a row without a color, not a row in error.
        const [sessions, agents] = await Promise.all([
          listAgentSessions(checkoutId),
          listAgentAgents(checkoutId).catch(() => [] as AgentAgent[]),
        ]);
        if (!current || requestGeneration !== generation) return;
        state.agents = agents;
        state.sessions = sessions;
        // Seeded rather than counted: there is no previous read for this checkout to differ from.
        recordRunning(sessions, wasRunning);
        state.targetId = defaultAgentSession(sessions)?.id ?? null;
        state.state = "ready";
        await refreshAgentsBehind(sessions);
      } catch (cause) {
        if (!current || requestGeneration !== generation) return;
        state.sessions = [];
        state.state = "error";
        state.error = errorText(cause);
      }
    },
    { immediate: true },
  );

  /**
   * Re-reads while a session is running or awaiting a reply.
   *
   * The service announces no completion event, so a turn ending is a transition seen between two
   * reads. Pending turns also poll so replies clear the waiting state; idle turns do not poll. It
   * covers a turn started in the person's own TUI, which is the common case here.
   */
  function pollRunningSessions() {
    if (!state.sessions.some((session) => session.running || session.awaitingReply)) return;
    refreshNow();
  }

  const pollTimer = setInterval(pollRunningSessions, BUSY_POLL_MS);

  /**
   * Asks again while there is no service to talk to.
   *
   * The service belongs to whoever started it, so this app does not start one: it waits here until
   * that person runs OpenCode, and connects on the first read that answers. The interval is slower
   * than the busy poll because a person starts OpenCode on their own schedule, not between two
   * turns, and every one of these is a request that answers nothing.
   */
  const disconnectedTimer = setInterval(() => {
    if (state.state !== "error") return;
    refreshNow();
  }, SLOW_POLL_MS);

  onScopeDispose(() => {
    disposed = true;
    clearInterval(pollTimer);
    clearInterval(disconnectedTimer);
    unlistenAgentEvents?.();
    unlistenAgentEvents = undefined;
  });

  // One subscription for the app's lifetime: events are filtered by checkout, so a single
  // listener is cheaper than one per checkout. It is still released with the scope, because a
  // remount or a hot reload would otherwise stack a subscription behind every one before it.
  void listen<AgentEvent>(AGENT_EVENT, ({ payload }) => {
    const checkoutId = state.checkoutId;
    if (!checkoutId || payload.checkoutId !== checkoutId) return;
    state.events = [...state.events, payload].slice(-MAX_EVENTS);
    if (isTurnEvent(payload.kind)) {
      // `/api/session/active` is the authority on whether a turn is running and it lags the
      // event, so the list is re-read and that read is what settles a finished turn.
      refreshNow();
      return;
    }
    state.sessions = applyAgentEvent(state.sessions, payload);
  })
    .then((dispose) => {
      // The scope can be gone before the bridge answers, in which case there is nothing left to
      // receive events and the subscription would outlive the row that asked for it.
      if (disposed) dispose();
      else unlistenAgentEvents = dispose;
    })
    .catch(() => {
      // No subscription, rather than a hook that cannot be built: the polls keep the row current
      // on their own, so this costs live events and nothing else. The rejection is taken here
      // because an unhandled one takes the effect scope with it.
    });

  return Object.assign(state, { reload, createSession, selectTarget, stop });
}

/** What one session says about itself, for a terminal to be matched against it. */
export interface TerminalAgentSession {
  /**
   * The session's own id, which is what a reported move names.
   *
   * Carried next to the title because identity rests on the pair: the title says which session a
   * terminal is showing and the id says which session moved, and neither answers the other without
   * this side by side.
   */
  readonly id: string;
  /** The session's own title, which is what its TUI writes into the terminal title. */
  title: string;
  /** The agent running it, with the color OpenCode paints it with, absent when the session has none. */
  agent: AgentHeadline | null;
  /** Whether the service reports a turn running in this session right now. */
  running: boolean;
  /** Pending requests outrank running; null means the location could not be read. */
  awaitingReply: boolean | null;
  /**
   * When the service last touched this session, in epoch milliseconds.
   *
   * Carried because it is the only real clock the row has: the elapsed time in a terminal's right
   * slot is how long ago this session was last updated, read from the service and from nowhere
   * else. Nothing else is a duration this app observed, so nothing else may be drawn as one.
   */
  updatedAt: number;
}

/** What one checkout's sessions say, which is everything a terminal in it could be matched to. */
export interface TerminalAgentRow {
  /**
   * Every session this checkout's service is running.
   *
   * A list rather than one headline on purpose: OpenCode 2.0.22 has no route that maps a TUI
   * process to a session, so the only thing that can say which session a terminal has open is the
   * title that terminal's own TUI wrote into it. That is a title to match against, and it needs
   * every candidate to match against. Empty when the service cannot be reached, which is a list
   * that says nothing rather than a session that says the wrong thing.
   */
  readonly sessions: TerminalAgentSession[];
}

export interface TerminalAgentRows {
  /** Keyed by checkout id, which is what a terminal row knows about itself. */
  readonly byCheckout: Record<string, TerminalAgentRow>;
  row(checkoutId: string): TerminalAgentRow;
  reload(): Promise<void>;
}

/**
 * Which checkouts a pass is about.
 *
 * `"all"` asks about every checkout that has a terminal, which is the only right question once
 * nothing is running: a turn can have started in the person's own TUI and nothing on this stream
 * says so. `"busy"` asks only about the checkouts with a turn running in them, which is all that
 * the fast poll can have changed.
 */
type RefreshScope = "all" | "busy";

/**
 * A cheap description of what a row would draw, so an answer that draws the same row is not written.
 *
 * It covers everything a terminal row reads out of its entry — the id a reported move is checked
 * against, the title it matches on, the agent it names, whether it is working or awaiting a reply,
 * and the clock in its trailing slot — and nothing else, because the row is only ever read and a
 * finer comparison would settle nothing.
 */
function rowSignature(entry: TerminalAgentRow): string {
  return entry.sessions
    .map((session) => {
      const agent = session.agent;
      const named = agent ? `${agent.label}/${agent.color}/${agent.attention}` : "";
      return `${session.id}|${session.title}|${named}|${session.running}|${session.awaitingReply}|${session.updatedAt}`;
    })
    .join("\n");
}

/**
 * Reads the agent state of every checkout that has terminals, keyed by checkout.
 *
 * `useAgentSessions` follows the *active* checkout, while a sidebar row is about one terminal in
 * one worktree: two terminals in different worktrees are different rows and must not share one
 * checkout's answer. Each checkout is read on its own and never merged, so a row can only ever be
 * drawn from its own worktree's sessions.
 *
 * It deliberately does not decide which session a given terminal has open. OpenCode 2.0.22 has no
 * route, header or event mapping a TUI process to a session, so that question has no answer to
 * read; see the note at the top of `services/agent.rs`. What *is* per terminal is the directory it
 * runs in, and that is what is scoped here.
 */
export function useTerminalAgentRows(checkoutIds: ComputedRef<string[]>): TerminalAgentRows {
  /** Whether a pass is on its way, so no other pass is started beside it. */
  let refreshing = false;
  /** Whether another pass is owed while the one on its way has not answered yet. */
  let refreshAgain = false;
  /** Whether the queued pass owes a full read, or only the checkouts with a turn running. */
  let againAll = false;
  /** Whether the scope that owns these two polls is already gone. */
  let disposed = false;
  /** The callers whose ask was folded into the queued pass, told once that pass has answered. */
  const waiting: Array<() => void> = [];
  /** What each published row drew, so an answer that would draw the same row is not written. */
  const published = new Map<string, string>();
  const byCheckout = reactive<Record<string, TerminalAgentRow>>({});
  /** Checkouts with a turn running, so the poll knows there is something to wait for. */
  const busy = new Set<string>();

  const row = (checkoutId: string): TerminalAgentRow => byCheckout[checkoutId] ?? { sessions: [] };

  async function read(checkoutId: string): Promise<TerminalAgentRow> {
    // The repository-wide list, not the worktree's own: a terminal's session is not necessarily
    // located in the worktree the terminal is filed under, and a row that cannot name its session
    // draws no state at all. Nothing here is trusted on its own — the row matches a title and refuses
    // an ambiguous one.
    const sessions = await listAgentCandidateSessions(checkoutId);
    const agents = await listAgentAgents(checkoutId).catch(() => [] as AgentAgent[]);
    // Every session is offered, and no headline is picked: the terminal row is about one terminal,
    // and the only thing that can say which session that terminal has open is the title the
    // terminal's own TUI wrote into it. It matches against this list by title
    // (`matchAgentSessionTitle`), and a terminal that names none — or several — draws no state.
    return {
      sessions: sessions.map((session) => {
        const label = agentLabel(agents, session.agent);
        return {
          id: session.id,
          title: session.title,
          agent: label ? { label, color: agentColor(agents, session.agent), attention: agentAttention(session) } : null,
          running: session.running,
          awaitingReply: session.awaitingReply,
          updatedAt: session.updatedAt,
        };
      }),
    };
  }

  /**
   * Writes an answer, unless it draws the row that is already there.
   *
   * `byCheckout` is reactive and every terminal row reads out of it, so an equal answer written as
   * a fresh object re-renders the whole sidebar twice a second for the price of a poll that found
   * nothing. The row is only ever read, so nothing in it can be patched into place and the cheap
   * question is the one worth asking: would this answer draw the same row?
   */
  function publish(checkoutId: string, entry: TerminalAgentRow) {
    const signature = rowSignature(entry);
    if (published.get(checkoutId) === signature) return;
    published.set(checkoutId, signature);
    byCheckout[checkoutId] = entry;
  }

  async function refresh(scope: RefreshScope) {
    // A checkout that went away must stop answering, or a closed worktree keeps its row.
    const wanted = [...new Set(checkoutIds.value)];
    for (const checkoutId of Object.keys(byCheckout)) {
      if (wanted.includes(checkoutId)) continue;
      delete byCheckout[checkoutId];
      busy.delete(checkoutId);
      published.delete(checkoutId);
    }
    const asked = scope === "busy" ? wanted.filter((checkoutId) => busy.has(checkoutId)) : wanted;
    const read_ = await Promise.all(
      asked.map(async (checkoutId) => {
        try {
          return [checkoutId, await read(checkoutId)] as const;
        } catch {
          // A service that is not run leaves the row with no session to match, which is the state
          // to wait in rather than a failure to report.
          return [checkoutId, { sessions: [] as TerminalAgentSession[] }] as const;
        }
      }),
    );
    // A scope that is gone has no row left to draw into, and a terminal that closed while this read
    // was in flight has no row left to draw. Both are answers to a question that no longer applies,
    // and the pass behind this one would only have to take them back out again.
    if (disposed) return;
    const stillWanted = new Set(checkoutIds.value);
    for (const [checkoutId, entry] of read_) {
      if (!stillWanted.has(checkoutId)) {
        busy.delete(checkoutId);
        published.delete(checkoutId);
        continue;
      }
      publish(checkoutId, entry);
      // Only what was asked about moves: a checkout the busy pass did not read is still busy, or
      // the pass would not have left it in the set.
      if (entry.sessions.some((session) => session.running)) busy.add(checkoutId);
      else busy.delete(checkoutId);
    }
  }

  /**
   * Runs a pass, and holds the next ask until the one on its way has answered.
   *
   * The set of checkouts changing, the fast poll and the slow poll all ask at once and none of them
   * waits for the read before it, so a service slower than the interval chained passes whose answers
   * were each superseded by the next one. `refreshing` is what makes a superseded answer impossible
   * here: a second pass cannot start while the first is still reading, so nothing to supersede.
   *
   * A held ask keeps its scope, and a held full read widens the queued pass rather than being
   * dropped by it, which is how a terminal that opened mid-pass still gets a row out of the pass
   * behind it.
   */
  function reload(scope: RefreshScope = "all"): Promise<void> {
    // Nothing is left to ask once the scope is gone. The chain behind a slow read is what would
    // outlive the timers this disposal clears, so it is answered here rather than run.
    if (disposed) return Promise.resolve();
    if (refreshing) {
      refreshAgain = true;
      againAll ||= scope === "all";
      // Answered by the pass behind the one on its way, because that is the pass that will report
      // for this ask: a read that began before it cannot. Held until the queue drains, so a caller
      // that folded two asks into one pass is not told they are done before either of them ran.
      return new Promise<void>((resolve) => waiting.push(resolve));
    }
    refreshing = true;
    againAll = false;
    return refresh(scope).finally(() => {
      refreshing = false;
      if (refreshAgain) {
        refreshAgain = false;
        void reload(againAll ? "all" : "busy");
        againAll = false;
        return;
      }
      // Nothing is owed behind this pass, so nobody is left waiting on a pass that will not happen.
      for (const resolve of waiting.splice(0)) resolve();
    });
  }

  watch(checkoutIds, () => void reload(), { immediate: true });

  /**
   * Re-reads while anything is running.
   *
   * The service announces no turn-completed event, so this is how a row notices a turn ending,
   * and it covers a turn the person started in their own TUI. It stops the moment every row is
   * idle, so an idle app is not polled. It asks about the busy checkouts alone: nothing about an idle
   * one can have changed since it was last read, and asking anyway is what made every open terminal
   * cost a round trip twice a second to keep up with the one agent that was working.
   */
  const pollTimer = setInterval(() => {
    if (busy.size > 0) void reload("busy");
  }, BUSY_POLL_MS);

  /**
   * Asks about every checkout, and slowly, whether or not anything is running.
   *
   * The service announces no turn-started event to this hook, and a turn started in the user's own TUI
   * is the one nobody here can hear about. So a checkout this app has drawn as idle still has to be
   * looked at: otherwise its row says idle while the agent has been working for minutes, and nothing
   * would correct it until some other event happened to reload the list.
   *
   * It runs while other checkouts are busy too, and that is the whole point of asking at all. Gating
   * it behind `busy.size === 0` made one long-running worktree silence the only question that can
   * find an externally started turn: the fast poll cannot hear it, because a turn that began in a
   * TUI leaves the idle worktree out of the set entirely. Every checkout would then keep whatever it
   * was last drawn as for as long as some other checkout stayed busy.
   *
   * The cadence is the slow one and not the fast one because the fast poll covers this already for
   * the checkouts it can see; what the slow pass adds is the width, not the rate.
   */
  const slowTimer = setInterval(() => void reload("all"), SLOW_POLL_MS);

  onScopeDispose(() => {
    disposed = true;
    clearInterval(pollTimer);
    clearInterval(slowTimer);
    // Nobody is left waiting on a pass that a released scope will never run.
    for (const resolve of waiting.splice(0)) resolve();
  });

  return { byCheckout, row, reload };
}

/**
 * Reports the OpenCode sessions that have started working in another worktree.
 *
 * OpenCode records the directory a session works in and changes it when an agent is sent to work
 * somewhere else, which is what its TUI shows at the footer. That directory is the only sign of
 * the move there is, and it can only be read across every location at once, because a session
 * that has left stops answering for the one it left. The backend keeps the previous read, so
 * what arrives here is a change rather than a position. Each enabled period starts with a baseline
 * snapshot that refreshes current state without delivering disabled-time moves.
 *
 * It says nothing about which terminal a session belongs to, and nothing here decides: see the
 * note at the top of `services/agent.rs`. `onRelocated` is handed the two checkouts and is the
 * only place a row is moved, because a caller that has the terminal list is the only one that
 * can tell which terminal a session is behind.
 */
export function useAgentRelocations(
  onRelocated: (relocation: AgentRelocation) => void,
  enabled: Readonly<Ref<boolean>>,
): void {
  let reading = false;
  let disposed = false;
  let generation = 0;
  let baselineGeneration: number | undefined;
  let timer: ReturnType<typeof setInterval> | undefined;

  /**
   * One read on its way at a time; interval ticks while it is in flight are skipped.
   *
   * The backend keeps a move on offer for half a minute because it cannot tell whether the caller
   * kept it, which only helps while somebody is listening: a read slower than the interval used to
   * be dropped as superseded by the next poll's, so a service that answers slower than the interval
   * lost every answer and no move was ever reported. A read that is already in flight is simply
   * skipped, and the next interval picks up whatever the answer was.
   */
  async function reload(requestGeneration: number) {
    if (!enabled.value || disposed || reading || requestGeneration !== generation) return;
    reading = true;
    try {
      const baseline = baselineGeneration === requestGeneration;
      const moved = await listAgentRelocations(baseline);
      // Disabling follow invalidates this read: its answer belongs to a mode that was not listening.
      if (disposed || !enabled.value || requestGeneration !== generation) return;
      if (baseline) baselineGeneration = undefined;
      else for (const relocation of moved) onRelocated(relocation);
    } catch {
      // A missing service or incomplete baseline is retried by the next poll rather than reported.
    } finally {
      reading = false;
      // If enable changed while this read was in flight, honor the new mode's immediate read without
      // overlapping the old request. Ordinary interval ticks while reading remain skipped.
      if (!disposed && enabled.value && requestGeneration !== generation) void reload(generation);
    }
  }

  watch(
    enabled,
    (isEnabled) => {
      generation += 1;
      if (timer !== undefined) clearInterval(timer);
      timer = undefined;
      baselineGeneration = undefined;
      if (!isEnabled) return;

      const requestGeneration = generation;
      baselineGeneration = requestGeneration;
      void reload(requestGeneration);
      timer = setInterval(() => void reload(requestGeneration), RELOCATION_POLL_MS);
    },
    { immediate: true, flush: "sync" },
  );
  onScopeDispose(() => {
    disposed = true;
    generation += 1;
    if (timer !== undefined) clearInterval(timer);
  });
}
