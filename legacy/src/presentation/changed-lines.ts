import { computed, ref, watch } from "vue";
import type { LineRange } from "../lib/changed-lines";
import { changedLineRanges } from "../lib/changed-lines";
import { isIpcError } from "../domain/ipc";
import type { DocumentOrigin } from "../domain/main-document";
import { getGitDiff } from "../lib/ipc";
import type { ActiveGitSnapshot } from "./active-git-snapshot";

/** What the pane is showing, all of it through getters because the pane is what changes its mind. */
export interface ChangedLinesPane {
  /** The checkout the file is in, or undefined while the pane has none to ask Git about. */
  checkoutId: () => string | undefined;
  path: () => string | null;
  /**
   * Which document is on screen. A review document lives outside the checkout, so Git has no status
   * of it and there is nothing of its lines to mark.
   */
  origin: () => DocumentOrigin;
  /** The checkout's status, whose revision is what says the marks on screen may have moved. */
  gitSnapshot: () => ActiveGitSnapshot;
  /** The document on screen, which is what a reading that lands late is checked against. */
  fileIdentity: () => string | null;
  reportCause: (cause: unknown) => void;
}

/**
 * The lines of the file on screen that the checkout has changed, as line numbers of the file itself.
 *
 * A separate concern from the content: the file is read from disk and this is asked of Git, so the
 * two answer at different times and neither waits for the other. Empty means nothing is known yet,
 * which draws exactly the same as "nothing is changed", and both draw as nothing.
 */
export function useChangedLines(pane: ChangedLinesPane) {
  const changedLines = ref<LineRange[]>([]);
  let changedLineGeneration = 0;

  /**
   * Whether Git has something to say about this file, and so whether it is worth asking.
   *
   * The statuses are Git's own two-column code with the blank half dropped, so a file edited in the
   * worktree and never staged reads as `M`, `MM` or `A` depending on what else is in the index — it
   * is never assumed to be one of them. Untracked files are left out even though the backend will
   * happily diff them: every line of a new file is an added line, so marking the whole file says
   * nothing the path in the toolbar does not. A deleted file is left out because there is no new side
   * of it left to mark.
   */
  const changedFileStatus = computed(() => {
    const path = pane.path();
    if (pane.origin() !== "checkout" || path === null) return null;
    const file = pane.gitSnapshot().status?.files.find((entry) => entry.path === path);
    if (!file || file.status === "??" || file.status.endsWith("D")) return null;
    return file.status;
  });

  /** The marks as a lookup, because the read-only renderer asks about one line at a time. */
  const changedLineNumbers = computed(() => {
    const numbers = new Set<number>();
    for (const range of changedLines.value) {
      for (let number = range.start; number <= range.end; number++) numbers.add(number);
    }
    return numbers;
  });

  async function loadChangedLines() {
    const request = ++changedLineGeneration;
    const checkoutId = pane.checkoutId();
    const path = pane.path();
    // Cleared before the question is asked, not after it is answered: these are the line numbers of
    // one file, and the next file's rows are already on screen while this one is still in flight.
    changedLines.value = [];
    if (!changedFileStatus.value || checkoutId === undefined || path === null) return;
    try {
      const diff = await getGitDiff(checkoutId, path);
      if (request !== changedLineGeneration) return;
      changedLines.value = changedLineRanges(diff.patch);
    } catch (cause) {
      if (request !== changedLineGeneration) return;
      changedLines.value = [];
      // A path Git does not count, or one this build cannot diff, is not a failure here: the marks
      // are an addition to a file that reads perfectly well without them.
      if (!isIpcError(cause) || cause.code !== "invalid_path") pane.reportCause(cause);
    }
  }

  // Which lines are changed is asked again on every refresh of the status and on every change of
  // file, and not on the file-activity signal the pane watches: that one re-reads the file, and
  // Git's answer does not come from the file's bytes.
  watch(
    () => [pane.gitSnapshot().statusRevision, pane.fileIdentity()] as const,
    () => void loadChangedLines(),
    { immediate: true },
  );

  return { changedLines, changedLineNumbers };
}
