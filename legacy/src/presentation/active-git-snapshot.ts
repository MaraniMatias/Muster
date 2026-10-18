import { listen } from "@tauri-apps/api/event";
import { reactive, ref, toValue, watch } from "vue";
import type { ComputedRef, MaybeRefOrGetter } from "vue";
import type { GitStatus } from "../domain/git";
import { isIpcError } from "../domain/ipc";
import type { Checkout, Repo } from "../domain/workspace";
import { getGitStatus } from "../lib/ipc";

export interface ActiveGitSnapshot {
  checkoutId: string | null;
  status: GitStatus | null;
  loading: boolean;
  statusState: "loading" | "ready" | "error";
  statusError: string;
  changesStatusError: string;
  changesWatchError: string;
  statusRevision: number;
  statusEventRevision: number;
  statusEventCheckoutId: string | null;
}

function errorText(cause: unknown): string {
  return isIpcError(cause) ? cause.message : cause instanceof Error ? cause.message : String(cause);
}

export function useActiveGitSnapshot(
  checkout: ComputedRef<Checkout | null>,
  repo: ComputedRef<Repo | null>,
  onDefaultBranchUnknown: () => void,
  unwatchedRepos: MaybeRefOrGetter<ReadonlySet<string>>,
): ActiveGitSnapshot {
  let generation = 0;
  let requestedDefaultBranch = false;
  let refreshCurrentStatus: (() => Promise<void>) | undefined;
  let refreshCurrentStatusGeneration = 0;
  /** Why this checkout's change signal never arrived, when that is the listener's doing.
   *  The panel reads it together with the repository watcher below, so the field has one
   *  writer and both ways of losing live updates read the same way. */
  const listenerError = ref("");
  const state = reactive<ActiveGitSnapshot>({
    checkoutId: null,
    status: null,
    loading: false,
    statusState: "ready",
    statusError: "",
    changesStatusError: "",
    changesWatchError: "",
    statusRevision: 0,
    statusEventRevision: 0,
    statusEventCheckoutId: null,
  });

  watch(
    [() => checkout.value?.id, () => checkout.value?.isMissing, () => repo.value?.kind],
    async ([checkoutId, isMissing, repoKind], _previous, onCleanup) => {
      const requestGeneration = ++generation;
      let current = true;
      let unlistenStatus: (() => void) | undefined;
      let statusRequest = 0;
      const isCurrent = () => current && requestGeneration === generation;

      onCleanup(() => {
        current = false;
        generation += 1;
        if (refreshCurrentStatusGeneration === requestGeneration) refreshCurrentStatus = undefined;
        unlistenStatus?.();
      });

      state.checkoutId = checkoutId ?? null;
      state.status = null;
      state.loading = false;
      state.statusState = "ready";
      state.statusError = "";
      state.changesStatusError = "";
      state.statusEventCheckoutId = null;
      listenerError.value = "";
      requestedDefaultBranch = false;
      if (!checkoutId) return;
      if (isMissing) {
        state.statusError = "Directory missing";
        state.changesStatusError = "Directory missing";
        state.statusState = "error";
        return;
      }
      if (repoKind !== "git") return;

      state.loading = true;
      state.statusState = "loading";

      const refreshStatus = async () => {
        const refreshRequest = ++statusRequest;
        state.changesStatusError = "";
        if (!state.status) state.statusState = "loading";
        try {
          const result = await getGitStatus(checkoutId);
          if (!isCurrent() || refreshRequest !== statusRequest) return;
          state.status = result;
          state.statusError = "";
          state.changesStatusError = "";
          state.statusState = "ready";
          state.statusRevision += 1;
        } catch (cause) {
          if (!isCurrent() || refreshRequest !== statusRequest) return;
          state.status = null;
          state.statusError = errorText(cause);
          state.changesStatusError = errorText(cause);
          state.statusState = "error";
          if (isIpcError(cause) && cause.code === "default_branch_unknown" && !requestedDefaultBranch) {
            requestedDefaultBranch = true;
            onDefaultBranchUnknown();
          }
        } finally {
          if (isCurrent() && refreshRequest === statusRequest) state.loading = false;
        }
      };
      refreshCurrentStatus = refreshStatus;
      refreshCurrentStatusGeneration = requestGeneration;

      try {
        // The signal names every checkout a change speaks for: a commit in one worktree moves
        // the merge base its siblings count against, so one save can be about all of them.
        const dispose = await listen<string[]>("git-status-changed", (event) => {
          if (event.payload.includes(checkoutId) && isCurrent()) {
            state.statusEventCheckoutId = checkoutId;
            state.statusEventRevision += 1;
            void refreshStatus();
          }
        });
        if (!isCurrent()) {
          dispose();
          return;
        }
        unlistenStatus = dispose;
      } catch (cause) {
        if (isCurrent()) listenerError.value = errorText(cause);
      }
      if (!isCurrent()) return;
      await refreshStatus();
    },
    { immediate: true },
  );

  // A repository whose watcher could not start never tells this panel that anything changed,
  // so its rows would sit on numbers nothing can refresh. The reason belongs next to them.
  watch(
    [
      () => repo.value?.id,
      () => {
        const repoId = repo.value?.id;
        return repoId ? toValue(unwatchedRepos).has(repoId) : false;
      },
      () => listenerError.value,
    ],
    ([, unwatched, listener]) => {
      state.changesWatchError = unwatched
        ? "This repository is not being watched, so its changes will not refresh on their own."
        : listener || "";
    },
    { immediate: true },
  );

  watch(
    [() => repo.value?.defaultBranch, () => checkout.value?.id],
    ([branch, checkoutId], [previousBranch, previousCheckoutId]) => {
      if (
        checkoutId &&
        checkoutId === previousCheckoutId &&
        branch !== previousBranch &&
        repo.value?.kind === "git" &&
        !checkout.value?.isMissing
      ) {
        requestedDefaultBranch = false;
        void refreshCurrentStatus?.();
      }
    },
  );

  return state;
}
