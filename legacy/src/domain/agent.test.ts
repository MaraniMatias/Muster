import { describe, expect, it } from "vitest";
import {
  agentAttention,
  agentColor,
  agentLabel,
  defaultAgentSession,
  headlineSession,
  isTurnEvent,
  matchAgentSessionTitle,
  sortAgentSessions,
} from "./agent";
import type { AgentSession } from "./agent";

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

/** The id of the one session a title named, or the kind of answer when it named none. */
function matched(sessions: AgentSession[], title: string | null): string | "none" | "ambiguous" {
  const match = matchAgentSessionTitle(sessions, title);
  return match.kind === "one" ? match.session.id : match.kind;
}

describe("agent session helpers", () => {
  it("puts the most recently touched session first and uses it as the default target", () => {
    const sessions = [session({ id: "ses_old", updatedAt: 1 }), session({ id: "ses_new", updatedAt: 9 })];
    expect(sortAgentSessions(sessions).map((item) => item.id)).toEqual(["ses_new", "ses_old"]);
    expect(defaultAgentSession(sessions)?.id).toBe("ses_new");
    expect(defaultAgentSession([])).toBeNull();
  });

  it("breaks a tie deterministically so the default target never flickers", () => {
    const sessions = [session({ id: "ses_b", updatedAt: 5 }), session({ id: "ses_a", updatedAt: 5 })];
    expect(defaultAgentSession(sessions)?.id).toBe("ses_a");
  });

  it("reports a blocked session ahead of a busy one, and neither when absent", () => {
    expect(agentAttention(undefined)).toBe("none");
    expect(agentAttention(session())).toBe("none");
    expect(agentAttention(session({ running: true }))).toBe("busy");
    expect(agentAttention(session({ running: true, awaitingReply: true }))).toBe("blocked");
    expect(agentAttention(session({ awaitingReply: true }))).toBe("blocked");
  });

  it("reads a turn the server ended badly as the last word, not as quiet", () => {
    expect(agentAttention(session({ outcome: "failed" }))).toBe("failed");
    expect(agentAttention(session({ outcome: "interrupted" }))).toBe("none");
    expect(agentAttention(session({ outcome: "succeeded" }))).toBe("none");
    // Working right now outranks a failure that already happened.
    expect(agentAttention(session({ running: true, outcome: "failed" }))).toBe("busy");
  });

  it("takes the agent's color only when it is a hex a stylesheet can use", () => {
    const agents = [
      { id: "plan", name: "Plan", mode: "primary", color: "#FF966C", hidden: false },
      { id: "coder", name: "coder", mode: "all", color: "#4ed6bf", hidden: false },
      { id: "build", name: "Build", mode: "primary", color: null, hidden: false },
      // The value is the server's and is about to become a `background`, so it is checked.
      { id: "hostile", name: "Hostile", mode: "primary", color: "red; background: url(x)", hidden: false },
    ];
    expect(agentColor(agents, "plan")).toBe("#FF966C");
    expect(agentColor(agents, "coder")).toBe("#4ed6bf");
    expect(agentColor(agents, "build")).toBeNull();
    expect(agentColor(agents, "hostile")).toBeNull();
    expect(agentColor(agents, "absent")).toBeNull();
    expect(agentColor(agents, null)).toBeNull();
  });

  it("names an agent by its display name, falling back to its id", () => {
    const agents = [
      { id: "plan", name: "Plan", mode: "primary", color: null, hidden: false },
      { id: "bare", name: "", mode: "primary", color: null, hidden: false },
    ];
    expect(agentLabel(agents, "plan")).toBe("Plan");
    expect(agentLabel(agents, "bare")).toBe("bare");
    // An agent the catalog has not caught up with still answers to its id, which is its name.
    expect(agentLabel(agents, "absent")).toBe("absent");
    // No agent at all is the one case with nothing to say.
    expect(agentLabel(agents, null)).toBeNull();
  });

  it("names the loudest session for a row, and the round's target when nothing is loud", () => {
    const quiet = [session({ id: "ses_quiet", updatedAt: 9, agent: "plan" })];
    const busy = session({ id: "ses_busy", updatedAt: 1, running: true, agent: "coder" });

    // Attention outranks recency: the row's news is the busy turn, not the newest idle one.
    expect(headlineSession([...quiet, busy], null)?.id).toBe("ses_busy");
    // With nothing to report it agrees with where a review would go.
    expect(headlineSession(quiet, "ses_quiet")?.id).toBe("ses_quiet");
    expect(headlineSession(quiet, "ses_gone")?.id).toBe("ses_quiet");
    expect(headlineSession([], "ses_quiet")).toBeNull();
  });

  it("skips a session that never ran, which has no agent to name", () => {
    // A fresh session is the most recently touched and has no agent yet, so it would blank a
    // row that has a real agent to show.
    const neverRan = session({ id: "ses_fresh", updatedAt: 99, agent: null });
    const real = session({ id: "ses_real", updatedAt: 1, agent: "coder" });

    expect(headlineSession([neverRan, real], "ses_fresh")?.id).toBe("ses_real");
    expect(headlineSession([neverRan, real], "ses_real")?.id).toBe("ses_real");
    // With nothing to name there is no row to draw, rather than a nameless one.
    expect(headlineSession([neverRan], "ses_fresh")).toBeNull();
  });

  it("treats only turn transitions as turn events", () => {
    expect(isTurnEvent("turnStarted")).toBe(true);
    expect(isTurnEvent("turnFinished")).toBe(true);
    expect(isTurnEvent("turnFailed")).toBe(true);
    expect(isTurnEvent("toolCalled")).toBe(false);
    expect(isTurnEvent("permissionAsked")).toBe(false);
    expect(isTurnEvent("unknown")).toBe(false);
  });

  it("matches a terminal to the one session its own title names", () => {
    const copy = session({ id: "ses_copy", title: "Copy ids into lists", agent: "coder" });
    const review = session({ id: "ses_review", title: "Review duplicates", agent: "plan" });

    // The TUI writes the title of the session it is showing into the terminal title, so the title is a
    // statement about that terminal. It is inference and not a mapping the service confirms: two
    // terminals showing one session would both report this title, and nothing here can tell that
    // from two terminals each showing their own session under one title. What can be refused is
    // being wrong in a way that shows, and only `one` is allowed to draw.
    expect(matched([copy, review], "Copy ids into lists")).toBe("ses_copy");
    expect(matched([copy, review], "Review duplicates")).toBe("ses_review");

    // No title, no session: this is every terminal whose title was never captured, and it is the
    // ordinary answer rather than a failure.
    expect(matchAgentSessionTitle([copy, review], null).kind).toBe("none");
    expect(matchAgentSessionTitle([copy, review], "").kind).toBe("none");
    // A title the service no longer lists is not matched to whatever is left.
    expect(matchAgentSessionTitle([copy, review], "Add a tinted chip").kind).toBe("none");
    expect(matchAgentSessionTitle([], "Copy ids into lists").kind).toBe("none");
  });

  it("reads a cut title as a prefix of whatever length the TUI cut it to", () => {
    // The ellipsis is the signal that the title was cut. How many characters survive is the TUI's
    // business and it moves between releases, so it is NOT checked: a fixed length refused titles
    // that were plainly cut, and the refusal was invisible, because an unidentified row draws no
    // state and only says "sin sesión". What is checked is the consequence — exactly one session
    // starts with what is left.
    const long = "Plan de implementación para la sidebar y sus estados";
    for (const width of [37, 34, 12]) {
      const cutTitle = `${long.slice(0, width)}…`;
      const cut = session({ id: "ses_cut", title: long, agent: "plan" });
      expect(matched([cut], cutTitle)).toBe("ses_cut");
    }
    // An exact title is still resolved before any prefix reading of it, so a session *called*
    // `Review…` is that session and not the longer one it happens to be a prefix of.
    const literal = session({ id: "ses_literal", title: "Review…", agent: "plan" });
    const longer = session({ id: "ses_longer", title: "Review more of the panel", agent: "plan" });
    expect(matched([literal, longer], "Review…")).toBe("ses_literal");
    // And the refusal is still a refusal: a stem landing on two sessions claims neither, and says so
    // rather than passing for a session that is not there.
    expect(
      matchAgentSessionTitle(
        [session({ id: "ses_cut", title: long }), session({ id: "ses_other", title: `${long} for the inspector` })],
        `${long.slice(0, 37)}…`,
      ).kind,
    ).toBe("ambiguous");
    // A stem that lands on nothing is nothing.
    expect(matchAgentSessionTitle([longer], "Nothing like this…").kind).toBe("none");
    // The residual cost, stated rather than hidden: with no session literally called `Review…`, a
    // terminal whose title reads `Review…` is read as the session it prefixes. That is either the TUI
    // cutting a longer title, or a session renamed away from the title the terminal still carries, and
    // nothing here can tell the two apart — the same "inference, not a mapping" the function's own
    // documentation is about. Refusing instead would mean refusing every real cut title too, which is
    // what cost this row its state in the first place.
  });

  it("refuses a duplicate exact title instead of widening it into a prefix", () => {
    // Two sessions carrying one title. The exact match found both, and the honest answer is that the
    // terminal's title cannot say which one it is on — so widening the same string into a prefix pass
    // would turn a known duplicate into a guess that happens to sound careful. It is also why the
    // answer is `ambiguous` and not `none`: the list holds the session, the title just cannot say
    // which, and a row that said "no session" would be claiming something false.
    const one = session({ id: "ses_one", title: "Review duplicates", agent: "coder" });
    const two = session({ id: "ses_two", title: "Review duplicates", agent: "plan" });
    expect(matchAgentSessionTitle([one, two], "Review duplicates").kind).toBe("ambiguous");

    // The same shape after truncation, which is where a duplicate and a prefix collide: both cut
    // titles begin the same way, so neither may claim the other.
    const long = "Plan de implementación para la sidebar y sus estados";
    const cutTitle = `${long.slice(0, 37)}…`;
    expect(
      matchAgentSessionTitle([session({ id: "a", title: long }), session({ id: "b", title: `${long} too` })], cutTitle)
        .kind,
    ).toBe("ambiguous");
    // And an exact match is never passed over for a prefix: a session called `Copy ids` is not the
    // session called `Copy ids into lists` just because one starts with the other.
    const exact = matchAgentSessionTitle(
      [session({ title: "Copy ids" }), session({ title: "Copy ids into lists" })],
      "Copy ids",
    );
    expect(exact.kind === "one" ? exact.session.title : exact.kind).toBe("Copy ids");
  });
});
