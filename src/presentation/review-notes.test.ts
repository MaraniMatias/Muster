// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { computed, ref } from "vue";
import type { ReviewNote, ReviewRound } from "../domain/review";
import type { Checkout, Repo } from "../domain/workspace";

const mocks = vi.hoisted(() => ({
  listReviewNotes: vi.fn(),
  createReviewNote: vi.fn(),
  updateReviewNote: vi.fn(),
  deleteReviewNote: vi.fn(),
  verifyReviewNoteAnchors: vi.fn(),
  clearReviewNoteOutdated: vi.fn(),
  listReviewRounds: vi.fn(),
  resolveReviewNote: vi.fn(),
  ackReviewRound: vi.fn(),
  dispatchReviewRound: vi.fn(),
  flushReviewRounds: vi.fn(),
  requeueReviewRounds: vi.fn(),
  reconcileReviewRound: vi.fn(),
  settlePiReviewRound: vi.fn(),
}));

vi.mock("../lib/ipc", () => ({
  listReviewNotes: mocks.listReviewNotes,
  createReviewNote: mocks.createReviewNote,
  updateReviewNote: mocks.updateReviewNote,
  deleteReviewNote: mocks.deleteReviewNote,
  verifyReviewNoteAnchors: mocks.verifyReviewNoteAnchors,
  clearReviewNoteOutdated: mocks.clearReviewNoteOutdated,
  listReviewRounds: mocks.listReviewRounds,
  resolveReviewNote: mocks.resolveReviewNote,
  ackReviewRound: mocks.ackReviewRound,
  dispatchReviewRound: mocks.dispatchReviewRound,
  flushReviewRounds: mocks.flushReviewRounds,
  requeueReviewRounds: mocks.requeueReviewRounds,
  reconcileReviewRound: mocks.reconcileReviewRound,
  settlePiReviewRound: mocks.settlePiReviewRound,
}));

import { useReviewNotes } from "./review-notes";

function note(overrides: Partial<ReviewNote> = {}): ReviewNote {
  return {
    id: "note:1",
    checkoutId: "checkout:first",
    path: "src/foo.js",
    side: "new",
    lineStart: 10,
    lineEnd: null,
    content: "revisit this calculation",
    code: "const result = a + b;",
    codeHash: "0000000000000001",
    outdated: false,
    roundId: null,
    status: "draft",
    createdAt: "1",
    updatedAt: "1",
    ...overrides,
  };
}

/** A Pi round: it names a terminal and a session, and it is never dispatched by the UI. */
function piRound(overrides: Partial<ReviewRound> = {}): ReviewRound {
  return {
    id: "round:pi",
    checkoutId: "checkout:first",
    provider: "pi",
    terminalId: "session:terminal:one",
    sessionId: "ses_pi",
    status: "dispatching",
    marker: "muster-review:round:1762000000:4242:7",
    noteIds: ["note:1"],
    createdAt: "1",
    updatedAt: "1",
    ...overrides,
  };
}

const first: Checkout = {
  id: "checkout:first",
  repoId: "repo:test",
  path: "/first",
  canonicalPath: "/first",
  isPrimary: true,
  changedFiles: 1,
  isMissing: false,
  sessions: [],
};
const second: Checkout = { ...first, id: "checkout:second", path: "/second", canonicalPath: "/second" };
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

function harness(checkoutRef = ref<Checkout | null>(first), repoRef = ref<Repo | null>(gitRepo)) {
  return {
    checkout: computed(() => checkoutRef.value),
    repo: computed(() => repoRef.value),
    checkoutRef,
    repoRef,
  };
}

async function settle() {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

describe("useReviewNotes", () => {
  beforeEach(() => {
    mocks.listReviewRounds.mockResolvedValue([]);
  });

  it("loads notes for the active git checkout and adds, edits and deletes them", async () => {
    mocks.listReviewNotes.mockResolvedValue([note()]);
    mocks.createReviewNote.mockResolvedValue(note({ id: "note:2", lineStart: 20, content: "second" }));
    mocks.updateReviewNote.mockResolvedValue(note({ id: "note:1", content: "edited" }));
    mocks.deleteReviewNote.mockResolvedValue(undefined);
    const state = useReviewNotes(harness().checkout, harness().repo);
    await settle();

    expect(state.state).toBe("ready");
    expect(state.notes).toHaveLength(1);

    await state.addNote({ path: "src/foo.js", side: "new", lineStart: 20, content: "second", code: "const x = 1;" });
    expect(mocks.createReviewNote).toHaveBeenCalledWith({
      checkoutId: "checkout:first",
      path: "src/foo.js",
      side: "new",
      lineStart: 20,
      content: "second",
      code: "const x = 1;",
    });
    expect(state.notes.map((item) => item.lineStart)).toEqual([10, 20]);

    await state.updateNote("note:1", "edited");
    expect(mocks.updateReviewNote).toHaveBeenCalledWith("checkout:first", "note:1", "edited");
    expect(state.notes[0].content).toBe("edited");

    await state.deleteNote("note:2");
    expect(mocks.deleteReviewNote).toHaveBeenCalledWith("checkout:first", "note:2");
    expect(state.notes.map((item) => item.id)).toEqual(["note:1"]);
  });

  it("reports drifted anchors and lets the user accept one", async () => {
    mocks.listReviewNotes.mockResolvedValue([note()]);
    mocks.verifyReviewNoteAnchors.mockResolvedValue([note({ outdated: true })]);
    mocks.clearReviewNoteOutdated.mockResolvedValue(note());
    const state = useReviewNotes(harness().checkout, harness().repo);
    await settle();

    expect(await state.verifyAnchors("src/foo.js", [{ id: "note:1", currentCode: "const result = a - b;" }])).toBe(
      true,
    );
    expect(mocks.verifyReviewNoteAnchors).toHaveBeenCalledWith("checkout:first", "src/foo.js", [
      { id: "note:1", currentCode: "const result = a - b;" },
    ]);
    expect(state.notes[0].outdated).toBe(true);

    // Nothing to check is not an error, and it must not reach the backend.
    expect(await state.verifyAnchors("src/foo.js", [])).toBe(false);
    expect(mocks.verifyReviewNoteAnchors).toHaveBeenCalledTimes(1);

    expect(await state.clearOutdated("note:1")).toBe(true);
    expect(mocks.clearReviewNoteOutdated).toHaveBeenCalledWith("checkout:first", "note:1");
    expect(state.notes[0].outdated).toBe(false);
  });

  it("dispatches a round to one session and re-reads the notes the backend decided", async () => {
    mocks.listReviewNotes.mockResolvedValue([note()]);
    mocks.dispatchReviewRound.mockResolvedValue({
      id: "round:1",
      checkoutId: "checkout:first",
      sessionId: "ses_one",
      status: "dispatched",
      marker: "muster-review:round:1",
      noteIds: ["note:1"],
      createdAt: "1",
      updatedAt: "2",
    });
    // The backend owns the note's new state; the composable must pick that up, not guess.
    mocks.listReviewNotes
      .mockResolvedValueOnce([note()])
      .mockResolvedValueOnce([note({ status: "sent", roundId: "round:1" })]);
    const state = useReviewNotes(harness().checkout, harness().repo);
    await settle();

    const round = await state.dispatchRound("ses_one", ["note:1"], "# Code Review\n");
    expect(round?.status).toBe("dispatched");
    expect(mocks.dispatchReviewRound).toHaveBeenCalledWith({
      checkoutId: "checkout:first",
      sessionId: "ses_one",
      ids: ["note:1"],
      markdown: "# Code Review\n",
      queue: false,
    });
    expect(state.rounds.map((item) => item.id)).toEqual(["round:1"]);
    expect(state.notes[0].status).toBe("sent");
    expect(state.notes[0].roundId).toBe("round:1");

    // No target or nothing to send must not reach the agent.
    expect(await state.dispatchRound("", ["note:1"], "# Code Review")).toBeNull();
    expect(await state.dispatchRound("ses_one", [], "# Code Review")).toBeNull();
    expect(mocks.dispatchReviewRound).toHaveBeenCalledTimes(1);
  });

  it("holds a round back when the user asks to wait for the agent", async () => {
    mocks.listReviewNotes.mockResolvedValueOnce([note()]).mockResolvedValueOnce([note({ status: "sent" })]);
    mocks.dispatchReviewRound.mockResolvedValue({
      id: "round:queued",
      checkoutId: "checkout:first",
      sessionId: "ses_one",
      status: "queued",
      marker: "muster-review:round:queued",
      noteIds: ["note:1"],
      createdAt: "1",
      updatedAt: "1",
    });
    mocks.listReviewRounds.mockResolvedValue([
      {
        id: "round:queued",
        checkoutId: "checkout:first",
        sessionId: "ses_one",
        status: "queued",
        marker: "muster-review:round:queued",
        noteIds: ["note:1"],
        createdAt: "1",
        updatedAt: "1",
      },
    ]);
    const state = useReviewNotes(harness().checkout, harness().repo);
    await settle();

    // The choice is handed to the backend whole: the round carries its own message, so the
    // send that happens later does not depend on anything the UI still remembers.
    const round = await state.dispatchRound("ses_one", ["note:1"], "# Code Review\n", true);
    expect(round?.status).toBe("queued");
    expect(mocks.dispatchReviewRound).toHaveBeenCalledWith(expect.objectContaining({ queue: true }));
    expect(state.rounds[0].status).toBe("queued");
  });

  it("flushes held-back rounds and re-reads the list only when something went out", async () => {
    mocks.listReviewNotes.mockResolvedValue([note({ status: "sent" })]);
    mocks.listReviewRounds.mockResolvedValue([
      {
        id: "round:queued",
        checkoutId: "checkout:first",
        sessionId: "ses_one",
        status: "queued",
        marker: "muster-review:round:queued",
        noteIds: ["note:1"],
        createdAt: "1",
        updatedAt: "1",
      },
    ]);
    mocks.flushReviewRounds.mockResolvedValue(1);
    const state = useReviewNotes(harness().checkout, harness().repo);
    await settle();
    expect(state.rounds[0].status).toBe("queued");

    mocks.flushReviewRounds.mockClear();
    mocks.listReviewRounds.mockClear();

    expect(await state.flushQueuedRounds()).toBe(1);
    expect(mocks.flushReviewRounds).toHaveBeenCalledWith("checkout:first");
    // Something went out, so the local copy is stale and has to be re-read.
    expect(mocks.listReviewRounds).toHaveBeenCalledTimes(1);

    // Nothing waiting: no reason to ask the backend for a fresh list.
    mocks.flushReviewRounds.mockResolvedValue(0);
    expect(await state.flushQueuedRounds()).toBe(0);
    expect(mocks.listReviewRounds).toHaveBeenCalledTimes(1);
  });

  it("confirms a round whose message landed and retries one that never arrived", async () => {
    mocks.listReviewNotes.mockResolvedValue([note()]);
    mocks.listReviewRounds.mockResolvedValue([
      {
        id: "round:landed",
        checkoutId: "checkout:first",
        sessionId: "ses_one",
        status: "dispatching",
        marker: "muster-review:round:landed",
        noteIds: ["note:1"],
        createdAt: "1",
        updatedAt: "1",
      },
      {
        id: "round:lost",
        checkoutId: "checkout:first",
        sessionId: "ses_one",
        status: "dispatching",
        marker: "muster-review:round:lost",
        noteIds: ["note:1"],
        createdAt: "1",
        updatedAt: "1",
      },
    ]);
    mocks.requeueReviewRounds.mockResolvedValue(2);
    mocks.reconcileReviewRound.mockImplementation(async (_checkoutId: string, roundId: string) => ({
      id: roundId,
      checkoutId: "checkout:first",
      sessionId: "ses_one",
      status: roundId === "round:landed" ? "dispatched" : "queued",
      marker: `muster-review:${roundId}`,
      noteIds: ["note:1"],
      createdAt: "1",
      updatedAt: "2",
    }));
    const state = useReviewNotes(harness().checkout, harness().repo);
    await settle();

    expect(await state.reconcileRounds()).toBeGreaterThan(0);
    const byId = Object.fromEntries(state.rounds.map((item) => [item.id, item.status]));
    // Landed: confirmed, so it will not be sent again. Lost: back to queued, so it will.
    expect(byId["round:landed"]).toBe("dispatched");
    expect(byId["round:lost"]).toBe("queued");
    // The marker, not a guess, is what decided each one.
    expect(mocks.reconcileReviewRound).toHaveBeenCalledWith("checkout:first", "round:landed");
    expect(mocks.reconcileReviewRound).toHaveBeenCalledWith("checkout:first", "round:lost");
  });

  it("resolves an outstanding note", async () => {
    mocks.listReviewNotes.mockResolvedValue([note({ status: "sent" })]);
    mocks.resolveReviewNote.mockResolvedValue(note({ status: "resolved" }));
    const state = useReviewNotes(harness().checkout, harness().repo);
    await settle();

    expect(await state.resolveNote("note:1")).toBe(true);
    expect(mocks.resolveReviewNote).toHaveBeenCalledWith("checkout:first", "note:1");
    expect(state.notes[0].status).toBe("resolved");
  });

  it("closes the newest confirmed OpenCode round when the agent's turn finished", async () => {
    mocks.listReviewNotes.mockResolvedValue([note({ status: "sent" })]);
    mocks.listReviewRounds.mockResolvedValue([
      {
        id: "round:live",
        checkoutId: "checkout:first",
        provider: "opencode",
        sessionId: "ses_one",
        status: "dispatched",
        marker: "muster-review:round:live",
        noteIds: ["note:1"],
        createdAt: "1",
        updatedAt: "1",
      },
      {
        id: "round:other-session",
        checkoutId: "checkout:first",
        provider: "opencode",
        sessionId: "ses_still_running",
        status: "dispatched",
        marker: "muster-review:round:other",
        noteIds: ["note:1"],
        createdAt: "1",
        updatedAt: "1",
      },
      {
        id: "round:done",
        checkoutId: "checkout:first",
        provider: "opencode",
        sessionId: "ses_one",
        status: "acked",
        marker: "muster-review:round:done",
        noteIds: ["note:1"],
        createdAt: "1",
        updatedAt: "1",
      },
      {
        id: "round:pi",
        checkoutId: "checkout:first",
        provider: "pi",
        terminalId: "session:terminal:one",
        sessionId: "ses_pi",
        status: "dispatched",
        marker: "muster-review:round:pi",
        noteIds: ["note:1"],
        createdAt: "1",
        updatedAt: "1",
      },
    ]);
    mocks.ackReviewRound.mockResolvedValue({
      id: "round:live",
      checkoutId: "checkout:first",
      provider: "opencode",
      sessionId: "ses_one",
      status: "acked",
      marker: "muster-review:round:live",
      noteIds: ["note:1"],
      createdAt: "1",
      updatedAt: "2",
    });
    const state = useReviewNotes(harness().checkout, harness().repo);
    await settle();

    const acked = await state.ackFinishedTurn("ses_one");
    // Only the live one closes; an already acked round is not touched again, and a Pi round is
    // left alone whatever its state — it is closed by the settlement the Pi itself announced,
    // and closing it on a turn observed to end is the inference that bridge refuses to make.
    expect(acked?.id).toBe("round:live");
    expect(mocks.ackReviewRound).toHaveBeenCalledTimes(1);
    expect(mocks.ackReviewRound).toHaveBeenCalledWith("checkout:first", "round:live");
    expect(state.rounds.find((item) => item.id === "round:pi")?.status).toBe("dispatched");
    expect(state.rounds.find((item) => item.id === "round:other-session")?.status).toBe("dispatched");
  });

  /**
   * A Pi round is settled from the Pi's own reports and from nowhere else, and asking again
   * changes nothing: every transition the backend makes is refused once it has been taken, so a
   * reconnect or a second settlement is not a second delivery.
   */
  it("settles a Pi round from what the Pi reported, and only that", async () => {
    mocks.listReviewNotes.mockResolvedValue([note({ status: "sent" })]);
    // The list is re-read rather than patched, so the backend is what decides: what it reports
    // after the settlement is what the store holds.
    let stored = piRound({ status: "dispatching" });
    mocks.listReviewRounds.mockImplementation(async () => [stored]);
    mocks.settlePiReviewRound.mockImplementation(async () => {
      stored = piRound({ status: "dispatched" });
      return [stored];
    });
    const state = useReviewNotes(harness().checkout, harness().repo);
    await settle();

    const settled = await state.settlePiRound("ses_pi");
    expect(mocks.settlePiReviewRound).toHaveBeenCalledWith("checkout:first", "ses_pi");
    expect(settled.map((item) => item.status)).toEqual(["dispatched"]);
    expect(state.rounds[0].status).toBe("dispatched");

    // Nothing was sent: this is not a route that could retransmit a review. Counted rather than
    // asserted absent, because this file's mocks carry across the tests in it.
    const sendsBefore = mocks.dispatchReviewRound.mock.calls.length;
    const flushesBefore = mocks.flushReviewRounds.mock.calls.length;
    await state.settlePiRound("ses_pi");
    expect(mocks.dispatchReviewRound).toHaveBeenCalledTimes(sendsBefore);
    expect(mocks.flushReviewRounds).toHaveBeenCalledTimes(flushesBefore);

    // A failure leaves the round where it was rather than guessing at it, and asking again
    // when nothing moved is not a second delivery.
    mocks.settlePiReviewRound.mockRejectedValue({ code: "operation_failed", message: "the bridge is gone" });
    expect(await state.settlePiRound("ses_pi")).toEqual([]);
    expect(state.rounds[0].status).toBe("dispatched");
    expect(state.error).toBe("the bridge is gone");
  });

  it("settles a moved Pi session against its current checkout and refreshes the original round owner", async () => {
    mocks.listReviewNotes.mockResolvedValue([note({ status: "sent" })]);
    let stored = piRound({ status: "dispatching" });
    mocks.listReviewRounds.mockImplementation(async (checkoutId: string) =>
      checkoutId === "checkout:first" ? [stored] : [],
    );
    mocks.settlePiReviewRound.mockImplementation(async () => {
      stored = piRound({ status: "dispatched" });
      return [stored];
    });
    const state = useReviewNotes(harness().checkout, harness().repo);
    await settle();

    const settled = await state.settlePiRound("ses_pi", "checkout:moved");
    expect(mocks.settlePiReviewRound).toHaveBeenCalledWith("checkout:moved", "ses_pi");
    expect(settled[0]?.checkoutId).toBe("checkout:first");
    expect(mocks.listReviewRounds).toHaveBeenLastCalledWith("checkout:first");
    expect(state.rounds[0]?.status).toBe("dispatched");
  });

  it("surfaces failures and keeps the notes readable", async () => {
    mocks.listReviewNotes.mockResolvedValue([note()]);
    mocks.createReviewNote.mockRejectedValue({ code: "operation_failed", message: "review note rejected" });
    mocks.verifyReviewNoteAnchors.mockRejectedValue({
      code: "checkout_ownership_mismatch",
      message: "review note does not belong to the requested checkout",
    });
    const state = useReviewNotes(harness().checkout, harness().repo);
    await settle();

    expect(await state.addNote({ path: "src/foo.js", side: "new", lineStart: 11, content: "x", code: "" })).toBe(false);
    expect(state.error).toBe("review note rejected");
    expect(state.notes).toHaveLength(1);

    expect(
      await state.verifyAnchors("src/foo.js", [{ id: "note:elsewhere", currentCode: "const result = a - b;" }]),
    ).toBe(false);
    expect(state.error).toBe("review note does not belong to the requested checkout");
    expect(state.notes).toHaveLength(1);
  });

  it("settles unconfirmed rounds when the checkout comes back", async () => {
    mocks.listReviewNotes.mockResolvedValue([note({ status: "sent" })]);
    mocks.listReviewRounds.mockResolvedValue([
      {
        id: "round:1",
        checkoutId: "checkout:first",
        sessionId: "ses_one",
        status: "dispatching",
        marker: "muster-review:round:1",
        noteIds: ["note:1"],
        createdAt: "1",
        updatedAt: "1",
      },
    ]);
    mocks.requeueReviewRounds.mockResolvedValue(1);
    mocks.reconcileReviewRound.mockResolvedValue({
      id: "round:1",
      checkoutId: "checkout:first",
      sessionId: "ses_one",
      status: "dispatched",
      marker: "muster-review:round:1",
      noteIds: ["note:1"],
      createdAt: "1",
      updatedAt: "2",
    });
    const { checkout, repo } = harness();
    const state = useReviewNotes(checkout, repo);
    await settle();

    expect(await state.reconcileRounds()).toBe(1);
    expect(state.rounds[0].status).toBe("dispatched");
  });

  it("reloads when the checkout changes and skips plain folders", async () => {
    mocks.listReviewNotes.mockResolvedValue([note()]);
    const { checkout, repo, checkoutRef } = harness();
    const state = useReviewNotes(checkout, repo);
    await settle();
    expect(mocks.listReviewNotes).toHaveBeenCalledWith("checkout:first");

    mocks.listReviewNotes.mockResolvedValue([note({ id: "note:9", checkoutId: "checkout:second" })]);
    checkoutRef.value = second;
    await settle();
    expect(state.checkoutId).toBe("checkout:second");
    expect(state.notes.map((item) => item.id)).toEqual(["note:9"]);

    const plainRepo = ref<Repo | null>({ ...gitRepo, kind: "plain" } as Repo);
    const plainState = useReviewNotes(
      computed(() => first),
      computed(() => plainRepo.value),
    );
    await settle();
    expect(plainState.state).toBe("ready");
    expect(plainState.notes).toEqual([]);
    expect(state.notes).toHaveLength(1);
  });
});
