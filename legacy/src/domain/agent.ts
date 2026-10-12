export type AgentEventKind =
  "unknown" | "turnStarted" | "turnFinished" | "turnFailed" | "toolCalled" | "permissionAsked" | "questionAsked";

/** One OpenCode session in a checkout. `id` is only valid inside that checkout's server. */
export interface AgentSession {
  id: string;
  checkoutId: string;
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
  /** The session a subagent was spawned from. */
  parentId: string | null;
  /** How the last turn ended: `succeeded`, `failed` or `interrupted`. */
  outcome: string | null;
  createdAt: number;
  updatedAt: number;
}

/** One agent a checkout's server offers, as OpenCode describes it. */
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
 * The one program in front of a shell that is also an agent, and so owns the row's agent line.
 *
 * A terminal's `foregroundApp` is its process name as the OS reports it, so this is that name
 * and nothing else: it is what makes a row an agent row, and what makes a terminal the one an
 * OpenCode session moved along with it.
 */
export const AGENT_APP = "opencode";

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

/** The session a review round should go to: the most recent live one, else the newest. */
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
  const nameable = sessions.filter((session) => session.agent);
  const loud = nameable.filter((session) => agentAttention(session) !== "none");
  if (loud.length) return sortAgentSessions(loud)[0] ?? null;
  return nameable.find((session) => session.id === targetId) ?? defaultAgentSession(nameable);
}

export function isTurnEvent(kind: AgentEventKind): boolean {
  return turnEvents.includes(kind);
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
 */
export function matchAgentSessionTitle<Session extends { title: string }>(
  sessions: Session[],
  title: string | null,
): AgentTitleMatch<Session> {
  const nothing: AgentTitleMatch<Session> = { kind: "none" };
  if (!title) return nothing;
  const exact = sessions.filter((session) => session.title === title);
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
  const matches = sessions.filter((session) => session.title.startsWith(stem));
  const only = matches[0];
  if (matches.length === 1 && only) return { kind: "one", session: only };
  return matches.length > 1 ? { kind: "ambiguous" } : nothing;
}
