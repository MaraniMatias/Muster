import { onScopeDispose, reactive, toValue, watch } from "vue";
import { listen } from "@tauri-apps/api/event";
import type { MaybeRefOrGetter } from "vue";
import type { Repo } from "../domain/workspace";
import { unwatchGitRepo, watchGitRepo } from "../lib/ipc";

type WatchPlanSnapshot = Array<[string, string[]]>;

function checkoutPlanKey(checkoutIds: string[]): string {
  return JSON.stringify([...checkoutIds].sort());
}

function watchPlanSnapshot(repos: Repo[]): string {
  return JSON.stringify(
    repos
      .flatMap((repo) => {
        if (repo.kind !== "git") return [];
        const checkoutIds = repo.checkouts
          .filter((checkout) => !checkout.isMissing)
          .map((checkout) => checkout.id)
          .sort();
        return checkoutIds.length > 0 ? [[repo.id, checkoutIds] as [string, string[]]] : [];
      })
      .sort(([left], [right]) => left.localeCompare(right)),
  );
}

/**
 * Keeps a watcher on every Git repository in the workspace.
 *
 * The sidebar names every checkout at once, so the numbers on the rows nobody is looking at
 * have to be as fresh as the one that is. That only holds if every checkout has a change
 * signal: an unwatched worktree is one the backend cannot tell to re-read, so its row would
 * describe a change set that had already moved on. Watching the repository rather than the
 * checkout is what makes the signal complete, because the worktrees of a repo share the Git
 * directory a commit is published through.
 *
 * Returns the repositories whose watcher could not start or stay active. Nothing can refresh the rows of a
 * checkout whose repository is in it, so the panel that shows those rows is where the reason
 * belongs: a toast expires and leaves the numbers stale with nothing to explain them.
 */
export function useGitWatchers(repos: MaybeRefOrGetter<Repo[]>): ReadonlySet<string> {
  const unwatched = reactive(new Set<string>());
  /** Each watched repository and the worktrees it was watched for, so a worktree joining or
   *  leaving one that is already watched re-registers it rather than going unwatched. */
  const watched = new Map<string, string>();
  const registrations = new Map<string, string>();
  const failedPlans = new Map<string, string>();
  const starting = new Set<string>();
  const pending = new Map<string, string | null>();
  let unlistenFailure: (() => void) | undefined;
  let disposed = false;
  let listenerReady = false;
  let listenerFailed = false;
  let latestWanted = new Map<string, string>();

  function reconcile(wanted: Map<string, string>): void {
    for (const repoId of new Set([...watched.keys(), ...failedPlans.keys()])) {
      if (!wanted.has(repoId)) release(repoId);
    }
    for (const [repoId, worktrees] of wanted) register(repoId, worktrees);
  }

  function markAllUnwatched(wanted: Map<string, string>): void {
    for (const repoId of [...unwatched]) {
      if (!wanted.has(repoId)) unwatched.delete(repoId);
    }
    for (const repoId of wanted.keys()) unwatched.add(repoId);
  }

  /** Registration is one call at a time per repository: a burst of workspace edits would
   *  otherwise ask for several watches of the same repo, and the backend answers each of them
   *  with a watcher, not the one already held. */
  function register(repoId: string, worktrees: string): void {
    if (starting.has(repoId)) {
      pending.set(repoId, worktrees);
      return;
    }
    if (watched.get(repoId) === worktrees || failedPlans.get(repoId) === worktrees) return;
    failedPlans.delete(repoId);
    watched.set(repoId, worktrees);
    const registrationId = crypto.randomUUID();
    registrations.set(repoId, registrationId);
    starting.add(repoId);
    // A worktree joining re-registers on purpose: the plan the backend built is missing the new
    // worktree's directory and Git directory, and it rebuilds the watch when the plan it is given
    // is not the one it holds. This is not a retry, so a failure is not a row that never refreshes.
    void watchGitRepo(repoId, registrationId, JSON.parse(worktrees) as string[])
      .then(() => {
        if (watched.get(repoId) === worktrees && registrations.get(repoId) === registrationId) {
          unwatched.delete(repoId);
        }
      })
      .catch(() => {
        if (watched.get(repoId) !== worktrees || registrations.get(repoId) !== registrationId) return;
        // Keep this plan failed until it changes; workspace replacements with identical checkout
        // IDs must not turn a failure into a silent retry loop.
        watched.delete(repoId);
        registrations.delete(repoId);
        failedPlans.set(repoId, worktrees);
        unwatched.add(repoId);
      })
      .finally(() => {
        starting.delete(repoId);
        const next = pending.get(repoId);
        const hasPending = pending.has(repoId);
        pending.delete(repoId);
        if (hasPending && next === null) void unwatchGitRepo(repoId).catch(() => undefined);
        else if (next !== undefined && next !== null && next !== watched.get(repoId)) register(repoId, next);
      });
  }

  function release(repoId: string): void {
    watched.delete(repoId);
    registrations.delete(repoId);
    failedPlans.delete(repoId);
    unwatched.delete(repoId);
    if (starting.has(repoId)) pending.set(repoId, null);
    else {
      pending.delete(repoId);
      void unwatchGitRepo(repoId).catch(() => undefined);
    }
  }

  void listen<{ repoId: string; checkoutIds: string[]; registrationId: string }>("git-watch-failed", (event) => {
    const { repoId, checkoutIds } = event.payload;
    // The IDs identify the requested plan; the generation separates ABA registrations of it.
    const requestedPlan = checkoutPlanKey(checkoutIds);
    if (watched.get(repoId) !== requestedPlan || registrations.get(repoId) !== event.payload.registrationId) {
      return;
    }
    watched.delete(repoId);
    registrations.delete(repoId);
    failedPlans.set(repoId, requestedPlan);
    unwatched.add(repoId);
  })
    .then((dispose) => {
      if (disposed) dispose();
      else {
        unlistenFailure = dispose;
        listenerReady = true;
        reconcile(latestWanted);
      }
    })
    .catch((cause: unknown) => {
      if (disposed) return;
      listenerFailed = true;
      console.error("Could not listen for Git watcher errors; repositories will stay unwatched:", cause);
      markAllUnwatched(latestWanted);
    });

  onScopeDispose(() => {
    disposed = true;
    unlistenFailure?.();
  });

  watch(
    () => watchPlanSnapshot(toValue(repos)),
    (snapshot) => {
      const wanted = new Map<string, string>(
        (JSON.parse(snapshot) as WatchPlanSnapshot).map(([repoId, checkoutIds]) => [
          repoId,
          checkoutPlanKey(checkoutIds),
        ]),
      );
      latestWanted = wanted;
      if (listenerFailed) markAllUnwatched(wanted);
      else if (listenerReady) reconcile(wanted);
    },
    { immediate: true },
  );

  return unwatched;
}
