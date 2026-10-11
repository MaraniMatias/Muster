export type AgentEventKind =
  "unknown" | "turnStarted" | "turnFinished" | "turnFailed" | "toolCalled" | "permissionAsked" | "questionAsked";

/**
 * Which agent program a session belongs to.
 *
 * These are two separate programs that happen to answer similar questions, so a session says
 * which one it came from and nothing may read one provider's answer as the other's. Session ids
 * are only ever meaningful inside their own provider, which is exactly why the two cannot be
 * pooled into one list without this travelling with each entry.
 */
export type AgentProvider = "opencode" | "pi";

/** One agent session Muster knows about, scoped to the checkout that owns it. */
export interface AgentSession {
  /** The program's own session id, only ever valid inside its checkout's provider. */
  id: string;
  checkoutId: string;
  /** The program this session belongs to; everything else here is read through it. */
  provider: AgentProvider;
  /**
   * The Muster terminal session this agent session is running in, when the provider says so.
   *
   * Pi's extension reports this, so a Pi row names its session by identity instead of by reading
   * a title off the terminal. OpenCode offers no such route — it cannot say which TUI process has
   * which session open — so it is always absent there and that row falls back to title inference,
   * which is a weaker thing this contract keeps named apart.
   */
  terminalId: string | null;
  title: string;
  /**
   * True while the service is draining a turn for this session.
   *
   * Reported, not derived: `/api/session/active` is the service's own answer, so it covers a
   * turn a person started in their own TUI. A turn only this client saw start cannot say that,
   * which is why the busy state is read rather than inferred from `idleAt`.
   */
  running: boolean;
  /** When the server last saw this session go idle, if ever. */
  idleAt: number | null;
  /** A pending reply; null means a candidate location could not be read. Scoped reads fail instead. */
  awaitingReply: boolean | null;
  /** The agent running it, which is also the mode: OpenCode spells `build`/`plan` as agents. */
  agent: string | null;
  /** The model behind it, as `provider/id#variant`. */
  model: string | null;
  /**
   * The session a subagent was spawned from.
   *
   * OpenCode-only: Pi has no native subagent parentage, so this is always null for it.
   */
  parentId: string | null;
  /** How the last turn ended: `succeeded`, `failed` or `interrupted`. */
  outcome: string | null;
  createdAt: number;
  updatedAt: number;
}

/**
 * One agent a checkout's server offers, as OpenCode describes it.
 *
 * The catalog is OpenCode-only: Pi has no agent list and no colors, so a Pi row draws without
 * one rather than borrowing OpenCode's.
 */
export interface AgentAgent {
  id: string;
  name: string;
  /** `primary` is a mode a person picks, `subagent` is one a parent spawns, `all` is both. */
  mode: string;
  /** The hex OpenCode paints this agent with, absent when it has no color of its own. */
  color: string | null;
  /** Internal agents: real, and not something to offer as a choice. */
  hidden: boolean;
}

/**
 * One session that is working in a worktree other than the one it was in.
 *
 * OpenCode reports the directory every session is in and changes it when an agent is sent
 * somewhere else, which is what its TUI shows at the footer. Both sides are checkouts of this
 * workspace, so this is a relocation rather than a session living in another project.
 */
export interface AgentRelocation {
  sessionId: string;
  fromCheckoutId: string;
  toCheckoutId: string;
  /** Unix milliseconds when first observed; stable across backend replay. */
  observedAt: number;
}

export interface AgentEvent {
  checkoutId: string;
  sessionId: string | null;
  kind: AgentEventKind;
  /** The server's own event type, kept for diagnostics. */
  rawType: string;
  data: unknown;
}

/**
 * The program in front of a shell that is also an agent, and so owns the row's agent line.
 *
 * A terminal's `foregroundApp` is its process name as the OS reports it, so these are those
 * names and nothing else: they are what makes a row an agent row, and what makes a terminal the
 * one a session moved along with it.
 *
 * **OpenCode only.** `AGENT_APP` is kept as the one name because the title inference below is
 * OpenCode's: only its TUI writes `OC | <session title>` into the terminal title. Pi reports the
 * session a terminal is running directly (`AgentSession.terminalId`), so it needs no title
 * convention and gets no title-derived match — a Pi row is identified by id or draws no state.
 */
export const AGENT_APP = "opencode";

/** Every program this panel treats as an agent, for the questions that are about all of them. */
export const AGENT_PROVIDER_APPS: Record<AgentProvider, string> = { opencode: "opencode", pi: "pi" };

/** The provider a foreground program is, or null when it is not one of ours. */
export function agentProviderForApp(app: string | null | undefined): AgentProvider | null {
  if (!app) return null;
  return (
    (Object.keys(AGENT_PROVIDER_APPS) as AgentProvider[]).find((provider) => AGENT_PROVIDER_APPS[provider] === app) ??
    null
  );
}

const turnEvents: AgentEventKind[] = ["turnStarted", "turnFinished", "turnFailed"];

/** How loudly a session wants for attention, worst last. */
export type AgentAttention = "none" | "busy" | "blocked" | "failed";

/** An agent session is busy, blocked, failed, or simply not there. */
export function agentAttention(session: AgentSession | undefined): AgentAttention {
  if (!session) return "none";
  if (session.awaitingReply) return "blocked";
  if (session.running) return "busy";
  // A turn the server ended badly is the last word, so it outranks a quiet session.
  return session.outcome === "failed" ? "failed" : "none";
}

const HEX_COLOR = /^#(?:[0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i;

/**
 * The agent's own color, when it is one a stylesheet can take.
 *
 * The value is the server's, and it is about to become a `background`, so it is checked
 * against the hex OpenCode actually writes rather than trusted. Anything else is no color,
 * and the row falls back to the accent.
 */
export function agentColor(agents: AgentAgent[], agentId: string | null | undefined): string | null {
  if (!agentId) return null;
  const color = agents.find((agent) => agent.id === agentId)?.color;
  return color && HEX_COLOR.test(color) ? color : null;
}

/** The display name of an agent, falling back to its id when the server names it nothing. */
export function agentLabel(agents: AgentAgent[], agentId: string | null | undefined): string | null {
  if (!agentId) return null;
  const agent = agents.find((candidate) => candidate.id === agentId);
  return agent?.name || agentId;
}

/** Sorts so the most recently touched session is first: the default send target. */
export function sortAgentSessions(sessions: AgentSession[]): AgentSession[] {
  return [...sessions].sort((first, second) => second.updatedAt - first.updatedAt || first.id.localeCompare(second.id));
}

/** The newest OpenCode session a review round can target. */
export function defaultOpenCodeSession(sessions: AgentSession[]): AgentSession | null {
  return sortAgentSessions(sessions.filter((session) => session.provider === "opencode"))[0] ?? null;
}

/** The newest session of any provider, for rows that display mixed agent state. */
export function defaultAgentSession(sessions: AgentSession[]): AgentSession | null {
  const sorted = sortAgentSessions(sessions);
  return sorted[0] ?? null;
}

/**
 * The one session a row speaks for.
 *
 * A session that wants attention outranks a quiet one, because that is the row's news; among
 * equals the most recently touched wins, and with nothing to report the round's own target is
 * named so the row agrees with where a review would go.
 *
 * A session that never ran has no agent, and a row cannot name nothing, so those are only
 * considered when there is no session to name: a fresh one is the newest by `updatedAt` and
 * would otherwise blank a row that has a perfectly good agent to show.
 */
export function headlineSession(sessions: AgentSession[], targetId: string | null): AgentSession | null {
  // A Pi session has no catalog entry and therefore never an `agent`, but it is still the one
  // row speaks for: it is named by its own title. OpenCode keeps the old rule, where a session
  // with no agent is a session that never ran and has nothing to name.
  const nameable = sessions.filter((session) => session.agent || session.provider === "pi");
  const loud = nameable.filter((session) => agentAttention(session) !== "none");
  if (loud.length) return sortAgentSessions(loud)[0] ?? null;
  return nameable.find((session) => session.id === targetId) ?? defaultAgentSession(nameable);
}

export function isTurnEvent(kind: AgentEventKind): boolean {
  return turnEvents.includes(kind);
}

/**
 * The raw event types Pi's bridge reports, named rather than folded into the turn kinds.
 *
 * A session appearing, disappearing or finishing a blocking prompt is not a turn boundary, and
 * the three `AgentEventKind` values that would fit it would each be a lie: a row that appeared is
 * not working, and a prompt that ended is not a turn. They travel as their own types so a reader
 * can ask what happened instead of reconstructing it.
 */
export const PI_EVENT_TYPES = ["pi.session_started", "pi.session_ended", "pi.ui_prompt_end"] as const;

/**
 * Whether an event means the session list has to be read again.
 *
 * A turn event already does that on its own, so this is only about the rest: the Pi bridge
 * reports a session appearing and a blocking prompt being answered with no turn either side of
 * them, and nothing re-reads the list in between.
 */
export function isPiLifecycleEvent(rawType: string): boolean {
  return (PI_EVENT_TYPES as readonly string[]).includes(rawType);
}

/**
 * The session a terminal is showing, when the provider says which one directly.
 *
 * **This is identity, not inference.** Pi's extension reports the terminal session a session is
 * running in (`AgentSession.terminalId`), so this is an equality check against a value the
 * provider owns, not a string someone wrote about itself. It therefore resolves to `one` or
 * `none` and never to `ambiguous`: a terminal is one terminal, so two sessions claiming it is a
 * contradiction to report rather than a choice between them.
 *
 * It is deliberately kept apart from [`matchAgentSessionTitle`]. That one guesses from a title
 * and must refuse whenever the guess is not unique; this one is told. Folding them into a single
 * function would mean the title path could return the identity path's certainty, which is exactly
 * the confusion the panel's own documentation refuses to make.
 *
 * Only Pi is asked, because only Pi answers. A terminal's foreground program is not the gate:
 * process names depend on the runtime and platform, and even when one says `pi`, that is weaker
 * evidence than the bridge's direct terminal identity. The bridge already said which terminal each
 * session is in, so that is what is asked, and a session from another provider is not a candidate
 * here even in the day it grows a route of its own.
 */
export type AgentTerminalMatch<Session> = { kind: "one"; session: Session } | { kind: "none" };

export function matchAgentSessionTerminal<Session extends { provider: AgentProvider; terminalId: string | null }>(
  sessions: Session[],
  terminalId: string | null,
): AgentTerminalMatch<Session> {
  if (!terminalId) return { kind: "none" };
  const found = sessions.filter((session) => session.provider === "pi" && session.terminalId === terminalId);
  // More than one is a contradiction, so it draws nothing rather than picking one.
  return found.length === 1 && found[0] ? { kind: "one", session: found[0] } : { kind: "none" };
}

/** The character OpenCode appends when it cuts a session title short for the terminal title. */
const TITLE_ELLIPSIS = "…";

/**
 * What a terminal's own title named.
 *
 * `one` is the only answer a row may draw a state from. `ambiguous` is a finding rather than a
 * failure to find: the service's own list holds several sessions carrying that name, which is
 * something a row can say out loud. `none` is everything else — no title was ever captured, or the
 * title names nothing the service lists.
 */
export type AgentTitleMatch<Session> = { kind: "one"; session: Session } | { kind: "ambiguous" } | { kind: "none" };

/**
 * The session a terminal is showing, when its own terminal title names exactly one.
 *
 * **This is inference, not a mapping, and the panel is written as if it knows that.** The service
 * offers no route, header or event that says which session a given TUI process has open (see
 * `services/agent.rs`), so the only thing available is a string the TUI wrote about itself, matched
 * against the titles of the sessions the service lists. Two readings are therefore possible and
 * nothing here can tell them apart: a correct match, and two terminals showing one session between
 * them. What can be refused is being wrong in a way that shows: anything but `one` draws no state
 * rather than a state borrowed from the nearest session.
 *
 * The candidates are the service's whole list rather than one worktree's, because a terminal's
 * session is not necessarily located in the worktree the terminal is filed under; that is why a
 * duplicate title is a real answer here and not an edge case, and why `ambiguous` is kept apart from
 * `none` instead of collapsing into it.
 *
 * The order is exact first, then prefix, because an exact title is the strongest thing here and
 * must not be passed over for a looser reading of it:
 *
 * - An exact match that is unique wins outright.
 * - An exact match that is not unique — two sessions carrying one title — is `ambiguous`. Falling
 *   through to the prefix pass would turn a duplicate into a prefix, and the duplicate is the fact.
 * - Only a title carrying the TUI's own ellipsis is read as a prefix, and only when exactly one
 *   session starts with what is left of it. The ellipsis is the signal that the title was cut; how
 *   many characters the cut was is the TUI's business, changes between releases, and is not checked.
 *
 * **Only OpenCode's sessions are candidates.** This inference exists because only OpenCode writes
 * `OC | <session title>` into a terminal title; a Pi session is named by the terminal it reported
 * itself rather than by a string it wrote about itself. Leaving Pi titles in the pool would let a Pi
 * session that happened to share a title with an OpenCode session turn an OpenCode terminal into a
 * Pi row — one provider's answer read as the other's, which is the reading this file refuses to make
 * anywhere else.
 */
export function matchAgentSessionTitle<Session extends { provider: AgentProvider; title: string }>(
  sessions: Session[],
  title: string | null,
): AgentTitleMatch<Session> {
  const nothing: AgentTitleMatch<Session> = { kind: "none" };
  if (!title) return nothing;
  const opencode = sessions.filter((session) => session.provider === "opencode");
  const exact = opencode.filter((session) => session.title === title);
  if (exact.length) {
    const only = exact[0];
    return exact.length === 1 && only ? { kind: "one", session: only } : { kind: "ambiguous" };
  }

  if (!title.endsWith(TITLE_ELLIPSIS)) return nothing;
  const stem = title.slice(0, -TITLE_ELLIPSIS.length);
  // How wide the TUI cuts the title is the TUI's business and it has changed between releases — the
  // installed 2.0.24 still slices at 37, but nothing guarantees that is the number arriving here. A
  // fixed length is the wrong shape of check: it refused a title that was plainly a cut one, and the
  // refusal is invisible, because an unidentified row draws no state and only says "sin sesión". The
  // ellipsis IS the signal that the title was cut, so what has to be checked is the CONSEQUENCE — that
  // exactly one session starts with what is left — and not an arithmetic detail about how many
  // characters the cut happened to be.
  const matches = opencode.filter((session) => session.title.startsWith(stem));
  const only = matches[0];
  if (matches.length === 1 && only) return { kind: "one", session: only };
  return matches.length > 1 ? { kind: "ambiguous" } : nothing;
}
