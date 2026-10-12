import { listen } from "@tauri-apps/api/event";
import { onScopeDispose, reactive, toValue, watch } from "vue";
import type { MaybeRefOrGetter } from "vue";
import type { GitDiffStats } from "../domain/git";
import type { Repo } from "../domain/workspace";
import { getGitCheckoutDiffStats, getGitDiffStats } from "../lib/ipc";

/** What one row of the Changes tab shows. A file Git has no count for keeps both absent, so
 *  the row draws no number rather than a zero it did not measure. */
export interface DiffStatsFile {
  additions?: number;
  deletions?: number;
}

export interface DiffStats {
  /** The totals of every registered Git checkout, keyed by checkout id. One call covers the
   *  whole sidebar, which is why no view walks the checkouts one at a time. A checkout Git
   *  cannot answer for is absent, so its row shows nothing rather than a false clean bill. */
  readonly checkoutTotals: Readonly<Record<string, GitDiffStats>>;
  /** The counts of the active checkout's changed files, keyed by path. */
  readonly fileCounts: Readonly<Record<string, DiffStatsFile>>;
}

/**
 * The `+N`/`-N` a workdir row and a Changes row show.
 *
 * The counts are refreshed by the same `git-status-changed` signal that refreshes the file
 * list, and against the same base ref, so a number can never sit on a row describing a
 * different change set than the one the panel lists. The store is shared by every caller: the
 * sidebar and the inspector want the same data, and one listener plus one call per change
 * serves both.
 *
 * The base ref is resolved in Rust, from the checkout's own default branch. Nothing about it
 * crosses the IPC boundary from the WebView, and no path or ref is accepted as input here.
 */
const state = reactive({
  checkoutTotals: {} as Record<string, GitDiffStats>,
  fileCounts: {} as Record<string, DiffStatsFile>,
});

/**
 * How long a change waits for company before it costs a round trip.
 *
 * Each checkout's watcher already holds a change back until its directory has been quiet for
 * 220ms, so this is not about one checkout saving a file. It is about the several checkouts
 * and repos reporting in the same breath: a commit in one worktree moves the merge base every
 * sibling diffs against, so one save speaks for a whole repo. Without a window, each of those
 * answers is its own sweep of every checkout in the workspace, and they all run at once.
 */
const REFRESH_DEBOUNCE_MS = 120;

/** The checkout whose files the Changes tab lists. Every caller names the same one. */
let activeCheckoutId: string | null = null;
let consumers = 0;
/** Bumped when the last reader leaves. Anything still on its way belongs to the readers that
 *  asked for it, whether that is a subscription Tauri has not handed back yet or a sweep still
 *  waiting on Git, so its late answer is dropped instead of landing on whoever is reading now.
 *  It moves when the count reaches zero and not on the way back up: a reader that arrives while
 *  the previous cycle is still in flight is exactly the case where the old answer must not
 *  paint. */
let lifecycle = 0;
let unlisten: (() => void) | undefined;
let refreshTimer: ReturnType<typeof setTimeout> | undefined;
let refreshing = false;
let refreshAgain = false;

export function useDiffStats(repos: MaybeRefOrGetter<Repo[]>, checkoutId: MaybeRefOrGetter<string | null>): DiffStats {
  if (++consumers === 1) void startListening();

  // Each caller decides on its own what counts as a change, because the sidebar watches
  // every checkout while the inspector watches only the one it is showing. Two callers
  // asking at once collapse into one call below, so a redundant ask costs nothing.
  watch(
    [
      // Only the set of checkouts matters here, not the rest of the workspace: a terminal
      // opening in a checkout is not a change to what the sidebar has to ask Git about.
      () =>
        toValue(repos)
          .flatMap((repo) => repo.checkouts)
          .map((checkout) => `${checkout.id}:${checkout.isMissing ? 1 : 0}`)
          .sort()
          .join(","),
      () => toValue(checkoutId),
    ],
    ([, currentCheckoutId]) => {
      if (currentCheckoutId !== activeCheckoutId) {
        activeCheckoutId = currentCheckoutId;
        // Counts belong to the checkout they were counted in, and a path can be the same
        // in two checkouts, so the previous checkout's numbers never outlive the selection.
        state.fileCounts = {};
      }
      // Not debounced: a row that just appeared is showing numbers it does not have, and
      // the guard below keeps this from running beside another sweep.
      refreshNow();
    },
    { immediate: true },
  );

  onScopeDispose(() => {
    if (--consumers > 0) return;
    lifecycle += 1;
    unlisten?.();
    unlisten = undefined;
    clearTimeout(refreshTimer);
    refreshTimer = undefined;
    // The last reader is gone, so nothing may answer with numbers nobody is showing.
    activeCheckoutId = null;
    state.checkoutTotals = {};
    state.fileCounts = {};
  });

  return state;
}

async function startListening() {
  const startedIn = lifecycle;
  try {
    const dispose = await listen<string[]>("git-status-changed", () => scheduleRefresh());
    // The last reader can be gone before the subscription exists, and a new one may have
    // registered another in the meantime. A listener from a finished lifecycle would go on
    // refreshing a store that was just emptied, and outlive the readers it was registered for.
    if (lifecycle !== startedIn) dispose();
    else unlisten = dispose;
  } catch {
    // Without the event the counts refresh on selection and on workspace changes only, which
    // is stale but never wrong.
  }
}

/** A burst of changes is one round trip, and the sweep behind it is never asked to run twice
 *  at once: a refresh already on its way collects the change and the answer behind it. */
function scheduleRefresh() {
  if (refreshTimer !== undefined) return;
  refreshTimer = setTimeout(() => {
    refreshTimer = undefined;
    refreshNow();
  }, REFRESH_DEBOUNCE_MS);
}

function refreshNow() {
  if (refreshing) {
    refreshAgain = true;
    return;
  }
  refreshing = true;
  void refresh().finally(() => {
    refreshing = false;
    if (!refreshAgain) return;
    refreshAgain = false;
    refreshNow();
  });
}

async function refresh() {
  const startedIn = lifecycle;
  const checkoutId = activeCheckoutId;
  // One failure must not take the other half down: a checkout with no readable counts leaves
  // the sidebar totals it already had, and the Changes tab loses only its numbers.
  const [totals, files] = await Promise.all([
    getGitCheckoutDiffStats().catch(() => null),
    checkoutId ? getGitDiffStats(checkoutId).catch(() => null) : Promise.resolve(null),
  ]);
  // The last reader left while Git was answering, and leaving empties the store. These numbers
  // are about nobody now, so they are dropped rather than read by whoever mounts next.
  if (lifecycle !== startedIn) return;
  if (totals) {
    for (const id of Object.keys(state.checkoutTotals)) delete state.checkoutTotals[id];
    Object.assign(state.checkoutTotals, totals);
  }
  // The sweep was started for one checkout and the panel may have moved on while it ran. A path
  // is only the same path in two checkouts by accident, so numbers counted against a selection
  // that is gone are dropped rather than shown on rows describing another change set.
  if (files && checkoutId === activeCheckoutId) {
    const counts: Record<string, DiffStatsFile> = {};
    for (const file of files) counts[file.path] = { additions: file.additions, deletions: file.deletions };
    state.fileCounts = counts;
  }
}
