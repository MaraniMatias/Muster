import { reactive, watch } from "vue";
import type { ComputedRef, InjectionKey } from "vue";
import { isIpcError } from "../domain/ipc";
import type { AgentSession } from "../domain/agent";
import type { ReviewAnchorCheck, ReviewNote, ReviewRound, ReviewSide, ReviewTarget } from "../domain/review";
import type { Checkout, Repo } from "../domain/workspace";
import { createReviewNote, deleteReviewNote, listReviewNotes, updateReviewNote } from "../lib/ipc";
import {
  ackReviewRound,
  clearReviewNoteOutdated,
  dispatchReviewRound,
  listReviewRounds,
  reconcileReviewRound,
  flushReviewRounds,
  requeueReviewRounds,
  resolveReviewNote,
  verifyReviewNoteAnchors,
} from "../lib/ipc";

export interface NewReviewNoteInput {
  path: string;
  side: ReviewSide;
  lineStart: number;
  lineEnd?: number;
  content: string;
  code: string;
}

export interface ActiveReviewNotes {
  checkoutId: string | null;
  notes: ReviewNote[];
  /** Rounds of this checkout, newest first. */
  rounds: ReviewRound[];
  state: "loading" | "ready" | "error";
  error: string;
  addNote(input: NewReviewNoteInput): Promise<boolean>;
  updateNote(id: string, content: string): Promise<boolean>;
  deleteNote(id: string): Promise<boolean>;
  /** Reports the anchor text the diff currently renders, so the backend can stamp drift. */
  verifyAnchors(path: string, checks: ReviewAnchorCheck[]): Promise<boolean>;
  clearOutdated(id: string): Promise<boolean>;
  resolveNote(id: string): Promise<boolean>;
  /**
   * Delivers the chosen notes to one session as a single message, recording the round
   * first. The caller must have picked a session; nothing is sent without a target.
   */
  dispatchRound(sessionId: string, ids: string[], markdown: string, queue?: boolean): Promise<ReviewRound | null>;
  /**
   * Sends the rounds that were held back while the agent was busy.
   *
   * Called when a turn ends, which is what those rounds were waiting for.
   */
  flushQueuedRounds(): Promise<number>;
  /**
   * Settles rounds left unconfirmed by an interrupted send.
   *
   * A round whose marker is in the session's transcript is confirmed; one whose marker is
   * absent never arrived, so retrying it cannot duplicate a delivered review.
   */
  reconcileRounds(): Promise<number>;
  /**
   * Closes the newest confirmed round whose turn has finished.
   *
   * "Acked" means the agent's turn is over, not that the notes are resolved: what the turn
   * did to each line is judged separately, from the diff.
   */
  ackFinishedTurn(): Promise<ReviewRound | null>;
}

/**
 * How the diff view hands its notes to an agent.
 *
 * The round is recorded by the store and the target comes from the session list, and the app
 * shell is what owns both, so the send is the shell's work: the diff decides *which* notes and
 * *whether* to queue, and nothing else. It travels by injection because the diff is reached
 * through `MainPane`, which the shell does not otherwise need to know about.
 *
 * The three reads are getters, so a consumer tracks them by reading them the way it tracks any
 * other reactive source.
 */
export interface ReviewSender {
  /** The checkout's delivery destination. No saved choice means Markdown. */
  readonly target: ReviewTarget;
  /** The checkout's sessions, for the destination picker. */
  readonly sessions: AgentSession[];
  /** The session a review goes to, or null while the checkout has none. */
  readonly targetId: string | null;
  selectTarget(sessionId: string | null): void;
  selectReviewTarget(target: ReviewTarget): void;
  /** Rounds whose turn has not finished, which is what the count under the button reports. */
  readonly unfinishedRounds: number;
  /** Records one round with the chosen notes and delivers it to the target. */
  send(ids: string[], queue: boolean): Promise<void>;
}

export const REVIEW_SENDER: InjectionKey<ReviewSender> = Symbol("muster:review-sender");

function errorText(cause: unknown): string {
  return isIpcError(cause) ? cause.message : cause instanceof Error ? cause.message : String(cause);
}

function sortNotes(notes: ReviewNote[]): ReviewNote[] {
  return [...notes].sort(
    (first, second) =>
      first.path.localeCompare(second.path) || first.lineStart - second.lineStart || first.id.localeCompare(second.id),
  );
}

export function useReviewNotes(
  checkout: ComputedRef<Checkout | null>,
  repo: ComputedRef<Repo | null>,
): ActiveReviewNotes {
  let generation = 0;
  const state = reactive<
    Omit<
      ActiveReviewNotes,
      | "addNote"
      | "updateNote"
      | "deleteNote"
      | "verifyAnchors"
      | "clearOutdated"
      | "resolveNote"
      | "dispatchRound"
      | "flushQueuedRounds"
      | "reconcileRounds"
      | "ackFinishedTurn"
    >
  >({
    checkoutId: null,
    notes: [],
    rounds: [],
    state: "ready",
    error: "",
  });

  function fail(cause: unknown): boolean {
    state.error = errorText(cause);
    return false;
  }

  async function addNote(input: NewReviewNoteInput) {
    const checkoutId = state.checkoutId;
    if (!checkoutId) return false;
    try {
      const created = await createReviewNote({ checkoutId, ...input });
      if (state.checkoutId !== checkoutId) return false;
      state.notes = sortNotes([...state.notes.filter((note) => note.id !== created.id), created]);
      state.error = "";
      return true;
    } catch (cause) {
      return state.checkoutId === checkoutId ? fail(cause) : false;
    }
  }

  async function updateNote(id: string, content: string) {
    const checkoutId = state.checkoutId;
    if (!checkoutId) return false;
    try {
      const updated = await updateReviewNote(checkoutId, id, content);
      if (state.checkoutId !== checkoutId) return false;
      state.notes = sortNotes(state.notes.map((note) => (note.id === id ? updated : note)));
      state.error = "";
      return true;
    } catch (cause) {
      return state.checkoutId === checkoutId ? fail(cause) : false;
    }
  }

  async function deleteNote(id: string) {
    const checkoutId = state.checkoutId;
    if (!checkoutId) return false;
    try {
      await deleteReviewNote(checkoutId, id);
      if (state.checkoutId !== checkoutId) return false;
      state.notes = state.notes.filter((note) => note.id !== id);
      state.error = "";
      return true;
    } catch (cause) {
      return state.checkoutId === checkoutId ? fail(cause) : false;
    }
  }

  async function resolveNote(id: string) {
    const checkoutId = state.checkoutId;
    if (!checkoutId) return false;
    try {
      const updated = await resolveReviewNote(checkoutId, id);
      if (state.checkoutId !== checkoutId) return false;
      state.notes = state.notes.map((note) => (note.id === id ? updated : note));
      state.error = "";
      return true;
    } catch (cause) {
      return state.checkoutId === checkoutId ? fail(cause) : false;
    }
  }

  async function dispatchRound(sessionId: string, ids: string[], markdown: string, queue = false) {
    const checkoutId = state.checkoutId;
    if (!checkoutId || ids.length === 0 || !sessionId) return null;
    try {
      const round = await dispatchReviewRound({ checkoutId, sessionId, ids, markdown, queue });
      if (state.checkoutId !== checkoutId) return null;
      state.rounds = [round, ...state.rounds.filter((item) => item.id !== round.id)];
      // The backend owns the notes now: re-read instead of assuming what it decided.
      await refreshNotes(checkoutId);
      state.error = "";
      return round;
    } catch (cause) {
      if (state.checkoutId === checkoutId) fail(cause);
      return null;
    }
  }

  async function flushQueuedRounds() {
    const checkoutId = state.checkoutId;
    if (!checkoutId) return 0;
    try {
      const sent = await flushReviewRounds(checkoutId);
      if (state.checkoutId !== checkoutId) return 0;
      if (sent > 0) state.rounds = await listReviewRounds(checkoutId);
      state.error = "";
      return sent;
    } catch (cause) {
      if (state.checkoutId === checkoutId) fail(cause);
      return 0;
    }
  }

  async function reconcileRounds() {
    const checkoutId = state.checkoutId;
    if (!checkoutId) return 0;
    try {
      const requeued = await requeueReviewRounds(checkoutId);
      const pending = state.rounds.filter((round) => round.status === "queued" || round.status === "dispatching");
      let settled = 0;
      for (const round of pending) {
        try {
          const reconciled = await reconcileReviewRound(checkoutId, round.id);
          if (reconciled.status === "dispatched" || reconciled.status === "acked") settled += 1;
          state.rounds = state.rounds.map((item) => (item.id === reconciled.id ? reconciled : item));
        } catch {
          // A round we cannot even inspect is left as it was; the next attempt retries it.
        }
      }
      if (state.checkoutId !== checkoutId) return 0;
      if (settled > 0) await refreshNotes(checkoutId);
      return Math.max(settled, requeued);
    } catch (cause) {
      if (state.checkoutId === checkoutId) fail(cause);
      return 0;
    }
  }

  async function ackFinishedTurn() {
    const checkoutId = state.checkoutId;
    if (!checkoutId) return null;
    const round = state.rounds.find((item) => item.status === "dispatched");
    if (!round) return null;
    try {
      const acked = await ackReviewRound(checkoutId, round.id);
      if (state.checkoutId !== checkoutId) return null;
      state.rounds = state.rounds.map((item) => (item.id === acked.id ? acked : item));
      state.error = "";
      return acked;
    } catch (cause) {
      if (state.checkoutId === checkoutId) fail(cause);
      return null;
    }
  }

  async function refreshNotes(checkoutId: string) {
    try {
      const notes = await listReviewNotes(checkoutId);
      if (state.checkoutId !== checkoutId) return;
      state.notes = sortNotes(notes);
    } catch (cause) {
      if (state.checkoutId === checkoutId) fail(cause);
    }
  }

  async function verifyAnchors(path: string, checks: ReviewAnchorCheck[]) {
    const checkoutId = state.checkoutId;
    if (!checkoutId || checks.length === 0) return false;
    try {
      const notes = await verifyReviewNoteAnchors(checkoutId, path, checks);
      if (state.checkoutId !== checkoutId) return false;
      state.notes = sortNotes(notes);
      state.error = "";
      return true;
    } catch (cause) {
      return state.checkoutId === checkoutId ? fail(cause) : false;
    }
  }

  async function clearOutdated(id: string) {
    const checkoutId = state.checkoutId;
    if (!checkoutId) return false;
    try {
      const updated = await clearReviewNoteOutdated(checkoutId, id);
      if (state.checkoutId !== checkoutId) return false;
      state.notes = state.notes.map((note) => (note.id === id ? updated : note));
      state.error = "";
      return true;
    } catch (cause) {
      return state.checkoutId === checkoutId ? fail(cause) : false;
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
      const isCurrent = () => current && requestGeneration === generation;

      state.checkoutId = checkoutId ?? null;
      state.notes = [];
      state.rounds = [];
      state.error = "";
      state.state = "ready";
      if (!checkoutId || isMissing || repoKind !== "git") return;

      state.state = "loading";
      try {
        const [notes, rounds] = await Promise.all([listReviewNotes(checkoutId), listReviewRounds(checkoutId)]);
        if (!isCurrent()) return;
        state.notes = sortNotes(notes);
        state.rounds = rounds;
        state.state = "ready";
      } catch (cause) {
        if (!isCurrent()) return;
        state.notes = [];
        state.rounds = [];
        state.state = "error";
        state.error = errorText(cause);
      }
    },
    { immediate: true },
  );

  return Object.assign(state, {
    addNote,
    updateNote,
    deleteNote,
    verifyAnchors,
    clearOutdated,
    resolveNote,
    dispatchRound,
    flushQueuedRounds,
    reconcileRounds,
    ackFinishedTurn,
  });
}
