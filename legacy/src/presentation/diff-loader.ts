import { DiffFile } from "@git-diff-view/vue";
import type { DiffFileHighlighter } from "@git-diff-view/vue";
import { computed, nextTick, onScopeDispose, ref, shallowRef, watch } from "vue";
import type { Ref } from "vue";
import type { GitFileDiff } from "../domain/git";
import { isIpcError } from "../domain/ipc";
import { getGitDiff } from "../lib/ipc";
import { detectedLanguageName } from "../lib/source-languages";
import type { ActiveGitSnapshot } from "./active-git-snapshot";
import { theme } from "./theme";

/**
 * What the loading of one diff needs to know about the panel that draws it.
 *
 * All of it is read through getters because it belongs to the component: a diff is asked for as
 * soon as the panel names a file, and the panel is what changes its mind.
 */
export interface DiffLoaderSource {
  /** The checkout whose diff is on screen. */
  checkoutId: () => string;
  /** The file on screen, or null while the whole change set is stacked in this panel. */
  path: () => string | null;
  /** Where the diff has to open when it is not a refresh of the one already on screen. */
  scrollTop: () => number;
  /** The element the diff is scrolled in, which the position above is restored on. */
  viewport: Ref<HTMLElement | null>;
  /** The checkout's status, whose revision is what says the diff on screen may have moved. */
  gitSnapshot: () => ActiveGitSnapshot;
  /**
   * The page window of a large diff. A thunk rather than the window itself because this owns the
   * state `useLargeDiff` is built from, so it is built first and reaches the window afterwards.
   */
  pages: () => DiffPageWindow;
  /** Told when a diff is on screen, so the panel can answer what to do with the row it names. */
  onReady: (path: string) => void;
}

/** The two things a large diff's own loader needs from a diff that has just landed or moved. */
export interface DiffPageWindow {
  /**
   * A moved patch moves every line after the edit, so the pages on hand are stale and go. An
   * unmoved one leaves the line numbers they are indexed by exactly as they were, and dropping them
   * is what put a virtualized diff in a permanent "Loading diff page" loop: each refresh blanked
   * the window and the next one arrived before the refill had landed.
   */
  reset: () => void;
  /** How much of the diff is on screen has to exist in it, which is what the pages below ask for. */
  loadVisiblePages: () => void;
}

function errorText(error: unknown): string {
  return isIpcError(error) ? error.message : error instanceof Error ? error.message : String(error);
}

/**
 * The diff on screen, and everything that has to line up for it to be the right one.
 *
 * Three things answer at their own time and none of them waits for another: Git's diff of the file,
 * the grammar that file is read with, and the status revision that says at any moment the diff is
 * already stale. They are one concern rather than three because a reading is worth nothing unless
 * it is of the file, the checkout and the patch that are on screen, so the generations that keep a
 * late answer off a moved-on diff live next to the state they guard rather than in the markup that
 * happens to draw it.
 */
export function useDiffLoader(source: DiffLoaderSource) {
  const { checkoutId, path, scrollTop, viewport, gitSnapshot, pages, onReady } = source;
  const diff = shallowRef<GitFileDiff | null>(null);
  const diffHunks = shallowRef<Array<{ title: string; file: DiffFile }>>([]);
  const collapsedHunks = ref<number[]>([]);
  const diffState = ref<"idle" | "loading" | "ready" | "error">("idle");
  /** Why the diff is not on screen. It is the panel's whole content, so it is drawn there. */
  const diffError = ref("");
  const diffScrollTop = ref(scrollTop());
  const selectedPath = ref<string | null>(null);
  /**
   * The reading the file on screen is highlighted with, once it has arrived, and the diff it was read
   * for. Keyed on that diff because a path is not an identity: two checkouts hold their own
   * `src/App.vue`, and two versions of one file are two readings of it. See `loadHighlighter`.
   */
  const loadedHighlighter = shallowRef<{ source: GitFileDiff; highlighter: DiffFileHighlighter }>();
  /**
   * The highlighter handed to the library, and only while it is the one read for the diff on screen: a
   * reading that arrives after the user has moved on is worth nothing, and handing the library one that
   * cannot read the language it is about to be given sends the file through the wrong grammar.
   */
  const diffHighlighter = computed(() =>
    loadedHighlighter.value?.source === diff.value ? loadedHighlighter.value.highlighter : undefined,
  );

  /** Bumped by anything that makes the diff on screen a different one, and read by every answer. */
  let diffGeneration = 0;
  /** The reading of a file in flight, so the one that answers is the one the diff on screen asked for. */
  let highlighterRequest = 0;
  /** Names each set of hunks, which is what tells two checkouts' identical windows apart. */
  let diffIdentity = 0;
  let mounted = true;

  /**
   * One `DiffFile` per hunk, which is why a hunk is handed the file's language on both sides: a hunk
   * has no name of its own to be read by.
   *
   * The library decides a language by taking everything after the last dot of the path, which names
   * one for a `src/app.vue` and nothing at all for a `Dockerfile` or a `.prettierrc`. Told the
   * language, it highlights those too. A path no grammar is detected for leaves the library to guess,
   * rather than claiming a language that does not exist.
   *
   * Each is given an identity of its own, which the library keys its own reading of a window by instead
   * of by the text of that window. Two diffs of two checkouts hold the same path and often the very same
   * lines (the same run of placeholder newlines and the same hunk) and one cache would hand the
   * second whatever it read for the first, which for the two of them is a different file's syntax. It is
   * an identity per hunk rather than per file because the key it replaces is the window's text, and two
   * hunks of one file are two different windows.
   */
  function createHunks(filePath: string, patch: string) {
    const lang = detectedLanguageName(filePath);
    const identity = `${checkoutId()}:${filePath}:${++diffIdentity}`;
    const preamble: string[] = [];
    const sections: Array<{ title: string; patch: string }> = [];
    let current: string[] | null = null;
    let title = "";
    for (const line of patch.split("\n")) {
      if (line.startsWith("@@")) {
        if (current) sections.push({ title, patch: [...preamble, ...current].join("\n") });
        current = [line];
        title = line;
      } else if (current) current.push(line);
      else preamble.push(line);
    }
    if (current) sections.push({ title, patch: [...preamble, ...current].join("\n") });
    return sections.map((section, index) => {
      const file = new DiffFile(
        `a/${filePath}`,
        "",
        `b/${filePath}`,
        "",
        [section.patch],
        lang,
        lang,
        `${identity}:${index}`,
      );
      file.initTheme(theme.value);
      file.init();
      file.buildUnifiedDiffLines();
      return { title: section.title, file };
    });
  }

  /** What one `@@` header says: where each side of the change starts, and how many lines it covers. */
  const HUNK_HEADER = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;

  /**
   * The last line of the file that any hunk of this diff reaches, or nothing when a header says
   * something this cannot read.
   *
   * The library builds each hunk's window out of the file's own lines, from the first to the one that
   * hunk ends on, filling in the lines the change does not touch, so those are the only lines of the
   * file a grammar is ever asked about. A header that cannot be read leaves the answer unknown, and an
   * unknown answer is no limit at all: the whole file is read, which is what it always was.
   */
  function lastLineShown(hunks: GitFileDiff["hunks"]): number | undefined {
    let last = 0;
    for (const hunk of hunks) {
      const header = HUNK_HEADER.exec(hunk.title);
      if (!header) return undefined;
      // A count git leaves off is one line, which is what `@@ -7 +7 @@` says.
      const count = (at: string | undefined) => (at === undefined ? 1 : Number(at));
      last = Math.max(last, Number(header[1]) + count(header[2]), Number(header[3]) + count(header[4]));
    }
    return last;
  }

  /** One reading of one file, and the last ask for that same file that arrived while it ran. */
  interface HighlighterFlight {
    /** The file this flight is reading, which is what says another ask is of the same one. */
    key: string;
    /** The last ask for that same file that arrived while it ran, or nothing. */
    queued: { checkoutId: string; source: GitFileDiff } | null;
  }

  /**
   * One reading of one file at a time, and the reading of that same file that was asked for while
   * another was running as soon as the running one lands.
   *
   * Reading a file is a pass over all of it on the thread that also answers the wheel, and git reports
   * every write in the workdir, so a file being worked on is asked for over and over. A second pass
   * over the same file nobody is reading yet costs as much as the first and answers nothing sooner,
   * and the only one of such a burst worth keeping is the last. Another file is a different matter: the
   * user has moved to it and is waiting, and the reading of the file they left is one whose answer is
   * already worth nothing, so that one goes ahead rather than behind.
   *
   * The slot belongs to the flight that took it, and so does the reading worth keeping behind it, so
   * only the flight that still holds the slot ends it and takes its own queue out of it. A reading
   * that has been overtaken is of a file nobody is looking at, and a `finally` that cleared the slot
   * anyway would drop the slot of the reading on screen, whose answer would then not know that a
   * reading of its own file is already running and would pay for a second pass over that same file.
   * It would start whatever was queued behind it as well, which is that overtaken flight's own queue
   * and nobody else's.
   */
  let highlighterReading: HighlighterFlight | null = null;

  /**
   * Reads the grammar the file on screen is highlighted with, and the file itself for that grammar to
   * read, once the diff it is drawn from has arrived.
   *
   * Deliberately not awaited before the hunks are built: a grammar is a dynamic import and reading a
   * file is a pass over it, and waiting on either would hold back the diff text the user opened the
   * file for. Until they land the library highlights the way it always has, and `diffHighlighter`
   * changing is what makes it repaint in the same colors the editor reads the same file in. A file
   * whose language no grammar is loaded for, or whose text came back too large or not at all, simply
   * keeps the library's own highlighter, which is what it does today.
   *
   * The two sides are the whole of the file rather than the hunks of the diff, which is the only thing
   * a grammar can read: a hunk is a fragment, and the lines inside `<script setup lang="ts">` of a
   * `.vue` file are markup to a grammar that was never shown the tag that opened them. As far as the
   * diff reaches, though: a grammar is only asked about the lines the library builds a window out of,
   * which run from the first line of the file to the one the last hunk ends on, so a change near the
   * top of a large file is read as the top of that file and not as all of it.
   *
   * What comes back belongs to this diff and to nothing else, and it is published only after the guard
   * below: a reading that lands after the user has opened another file, or after this one has been read
   * again, is dropped rather than handed to a diff it is not of.
   */
  async function loadHighlighter(readingCheckoutId: string, reading: GitFileDiff) {
    const language = detectedLanguageName(reading.path);
    if (language === undefined) return;
    // Which file this ask is of, because a path alone does not name one: two checkouts hold their own
    // `src/App.vue`, and one reading at a time is one reading per file. The other checkout's file is
    // not this one, so holding it behind a reading of that one is holding what the user is waiting on
    // behind a file nobody is looking at.
    const key = `${readingCheckoutId}:${reading.path}`;
    // Another ask for the file already being read. The last of such a burst is the only one of it
    // worth keeping: the ones before it are of versions of the diff nobody is going to be shown.
    if (highlighterReading?.key === key) {
      highlighterReading.queued = { checkoutId: readingCheckoutId, source: reading };
      return;
    }
    const request = ++highlighterRequest;
    const flight: HighlighterFlight = { key, queued: null };
    highlighterReading = flight;
    try {
      const { prepareDiffHighlighting } = await import("../lib/diff-highlighter");
      const highlighter = await prepareDiffHighlighting(
        language,
        { old: reading.oldContent, new: reading.newContent },
        lastLineShown(reading.hunks),
      );
      // What this waits for is a dynamic import and a read of the file, either of which can land
      // after the user has opened another file or after this one has been read again, and either of
      // which is worth nothing to a diff that is no longer the one on screen.
      if (!highlighter || !mounted || request !== highlighterRequest) return;
      if (checkoutId() !== readingCheckoutId || diff.value !== reading) return;
      loadedHighlighter.value = { source: reading, highlighter };
    } catch {
      // A grammar that is not there, or one that fails to load, leaves the library to highlight the
      // file its own way. Neither is worth a toast: the diff is already on screen without them.
    } finally {
      // The reading on screen keeps the slot while it runs, whatever lands before it. A flight that
      // no longer holds it is of a file the user has left, so it takes nothing out of the slot and
      // leaves its own queue behind rather than starting a read of that file for nobody.
      if (highlighterReading === flight) {
        highlighterReading = null;
        const queued = flight.queued;
        flight.queued = null;
        if (queued) void loadHighlighter(queued.checkoutId, queued.source);
      }
    }
  }

  /**
   * One diff, asked for by path, and everything that has to be true of the answer for it to be
   * painted: still the path on screen, still the checkout it was asked of, and not older than
   * anything that has been asked for since.
   */
  async function loadDiff(selected: string, preservePosition = false) {
    const request = ++diffGeneration;
    const requestCheckoutId = checkoutId();
    const oldScrollTop = preservePosition ? diffScrollTop.value : scrollTop();
    // Whether this reload is the same file again. A different one resets the pages through the
    // path watcher inside the composable, so it is the only case left to cover here.
    const sameFile = selectedPath.value === selected;
    // E.3: a different file is a different selection, so the previous diff goes away rather
    // than sitting under the loading state. A refresh of the same file keeps its place.
    const keepPreviousDiff = diff.value !== null && sameFile;
    selectedPath.value = selected;
    if (!keepPreviousDiff) {
      diff.value = null;
      diffHunks.value = [];
      collapsedHunks.value = [];
    }
    diffScrollTop.value = oldScrollTop;
    diffError.value = "";
    diffState.value = "loading";
    try {
      const result = await getGitDiff(requestCheckoutId, selected);
      if (
        !mounted ||
        request !== diffGeneration ||
        selectedPath.value !== selected ||
        checkoutId() !== requestCheckoutId ||
        path() !== selected
      )
        return;
      // A refresh that changed nothing must not redraw. The diff view keeps the open note
      // composer in state that a new DiffFile identity wipes, and git reports every write in the
      // workdir, not just in the file on screen: rebuilding on each one closed the composer the
      // moment the user started typing, for a diff that had not moved.
      //
      // What is compared is the backend's revision of the diff rather than the patch, because the
      // patch cannot answer it: a large, a too large and a binary diff carry none, so two different
      // ones compared equal, the stale result stayed installed, and the pages went on serving the
      // content the first one had. A diff that moved changes its revision; one that has not, does
      // not, whatever else the workdir did.
      if (result.revision !== diff.value?.revision) {
        if (sameFile) pages().reset();
        diff.value = result;
        collapsedHunks.value = [];
        if (!result.isBinary && !result.symlinkTarget && result.patch.includes("@@")) {
          diffHunks.value = createHunks(selected, result.patch);
          void loadHighlighter(requestCheckoutId, result);
        }
      }
      diffState.value = "ready";
      onReady(selected);
      await nextTick();
      if (request !== diffGeneration || checkoutId() !== requestCheckoutId || path() !== selected) return;
      if (viewport.value) viewport.value.scrollTop = oldScrollTop;
      if (result.large && !result.tooLarge && !result.isBinary) pages().loadVisiblePages();
    } catch (error) {
      if (
        !mounted ||
        request !== diffGeneration ||
        selectedPath.value !== selected ||
        checkoutId() !== requestCheckoutId ||
        path() !== selected
      )
        return;
      // A diff that cannot be read leaves the panel with nothing to show, so the reason is
      // drawn in it: a toast would expire and leave an empty panel unexplained.
      diffError.value = errorText(error);
      diffState.value = "error";
    }
  }

  watch(
    () => [checkoutId(), path()] as const,
    ([, file]) => {
      if (file !== null) void loadDiff(file);
    },
    { immediate: true, flush: "sync" },
  );

  // Git reports every write in the workdir, on a 220ms debounce, and an agent writing a file
  // produces a steady stream of them. Reloading on each one spends the whole stream fetching a
  // diff the user never sees land, so a burst collapses into the single reload it amounts to.
  const STATUS_REFRESH_DEBOUNCE = 400;

  watch(
    () => gitSnapshot().statusRevision,
    (revision, previous, onCleanup) => {
      const file = path();
      if (revision === previous || file === null) return;
      if (gitSnapshot().checkoutId !== checkoutId() || !gitSnapshot().status) return;
      const timer = setTimeout(() => {
        if (gitSnapshot().status?.files.some((changed) => changed.path === file)) {
          void loadDiff(file, true);
          return;
        }
        diffGeneration += 1;
        diff.value = null;
        diffHunks.value = [];
        diffScrollTop.value = 0;
        diffError.value = "This file is no longer in the current Git changes.";
        diffState.value = "error";
      }, STATUS_REFRESH_DEBOUNCE);
      onCleanup(() => clearTimeout(timer));
    },
  );

  onScopeDispose(() => {
    mounted = false;
    diffGeneration += 1;
    // A reading queued for a file nobody is looking at any more is a whole pass over it for nothing,
    // and a reading already running drops its queue when it lands: that is what keeps a panel that
    // was unmounted with work pending from starting the reading that was waiting behind it.
    if (highlighterReading) highlighterReading.queued = null;
  });

  return { diff, diffHunks, collapsedHunks, diffState, diffError, diffScrollTop, selectedPath, diffHighlighter };
}
