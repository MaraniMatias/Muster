// @vitest-environment happy-dom
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h, nextTick, ref } from "vue";
import type { Ref } from "vue";
import type { GitFileDiff, GitStatus } from "../domain/git";
import type { ActiveGitSnapshot } from "./active-git-snapshot";
import { useDiffLoader } from "./diff-loader";
import type { DiffPageWindow } from "./diff-loader";

const mocks = vi.hoisted(() => ({
  getGitDiff: vi.fn(),
  /** Every set of hunks the loader built, so a test can read what the library was handed. */
  diffFiles: [] as Array<{ oldLang: string | undefined; uuid: string | undefined }>,
  /**
   * The reads a test is holding back, keyed by the text the read was asked for. Keying on the text
   * rather than on a counter is what keeps a test independent of which other tests ran first and
   * left a read of their own in flight.
   */
  held: new Map<string, Promise<void>>(),
  /** Every grammar the loader asked to be read, and both sides it asked about. */
  prepared: [] as Array<{ language: string | undefined; contents: Record<string, string | undefined> }>,
}));

// The library's `DiffFile` builds the hunks the markup draws. What is under test here is the
// wiring: which file is asked of Git, which of two answers lands, and which reading is published.
vi.mock("@git-diff-view/vue", () => ({
  DiffFile: class {
    constructor(
      _oldFileName: string,
      _oldFileContent: string,
      _newFileName: string,
      _newFileContent: string,
      _diffList: string[],
      oldLang?: string,
      _newLang?: string,
      uuid?: string,
    ) {
      mocks.diffFiles.push({ oldLang, uuid });
    }
    initTheme() {}
    init() {}
    buildUnifiedDiffLines() {}
  },
}));

vi.mock("../lib/diff-highlighter", () => ({
  prepareDiffHighlighting: async (language: string | undefined, contents: Record<string, string | undefined>) => {
    mocks.prepared.push({ language, contents });
    await mocks.held.get(contents.new ?? "");
    return { name: `read:${contents.new ?? ""}`, type: "class" };
  },
}));

vi.mock("../lib/ipc", () => ({
  getGitDiff: mocks.getGitDiff,
}));

function diff(overrides: Partial<GitFileDiff> = {}): GitFileDiff {
  const fixture: GitFileDiff = {
    path: "src/app.vue",
    patch: "@@ -1 +1 @@\n-old\n+new",
    revision: "",
    oldContent: "old",
    newContent: "new",
    isBinary: false,
    large: false,
    tooLarge: false,
    totalLines: 2,
    hunks: [{ startLine: 1, endLine: 1, title: "@@ -1 +1 @@" }],
    ...overrides,
  };
  // The backend hashes the lines it read, so a fixture's revision stands for its content unless a
  // test says otherwise: a fixture whose patch moved is a diff that moved, and one that carries no
  // patch at all has to be told apart by whatever the test gives it instead.
  return { ...fixture, revision: overrides.revision ?? fixture.patch };
}

function status(...files: string[]): GitStatus {
  return {
    branch: "main",
    head: "abc123",
    defaultBranch: "main",
    aheadCount: 0,
    files: files.map((path) => ({ path, status: "M" })),
  };
}

function snapshot(overrides: Partial<ActiveGitSnapshot> = {}): ActiveGitSnapshot {
  return {
    checkoutId: "checkout:a",
    status: status("src/app.vue"),
    loading: false,
    statusState: "ready",
    statusError: "",
    changesStatusError: "",
    changesWatchError: "",
    statusRevision: 0,
    statusEventRevision: 0,
    statusEventCheckoutId: null,
    ...overrides,
  };
}

/** A promise a test releases by hand, for work that must finish on its own schedule. */
function gate() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}

interface Host {
  wrapper: ReturnType<typeof mount>;
  loader: ReturnType<typeof useDiffLoader>;
  /** What the panel drew, which is what a user would be looking at. */
  text: () => string;
  path: Ref<string | null>;
  /** The checkout on screen, which is half of what names the file a reading is of. */
  checkout: Ref<string>;
  gitSnapshot: Ref<ActiveGitSnapshot>;
  viewport: Ref<HTMLElement | null>;
  pages: DiffPageWindow;
  ready: string[];
}

/**
 * A host that mounts the loader the way the panel does and draws only what the loader published, so
 * a test asserts on what is on screen rather than on what a call happened to return.
 */
function host(): Host {
  const path = ref<string | null>("src/app.vue");
  const checkout = ref("checkout:a");
  const gitSnapshot = ref(snapshot());
  const viewport = ref<HTMLElement | null>(null);
  const pages: DiffPageWindow = { reset: vi.fn(), loadVisiblePages: vi.fn() };
  const ready: string[] = [];
  let loader!: ReturnType<typeof useDiffLoader>;
  const wrapper = mount(
    defineComponent({
      setup() {
        loader = useDiffLoader({
          checkoutId: () => checkout.value,
          path: () => path.value,
          scrollTop: () => 0,
          viewport,
          gitSnapshot: () => gitSnapshot.value,
          pages: () => pages,
          onReady: (readyPath) => ready.push(readyPath),
        });
        return () =>
          h(
            "div",
            [
              loader.diff.value ? `diff:${loader.diff.value.path}` : `diff:${loader.diffState.value}`,
              `|hunks:${loader.diffHunks.value.length}`,
              `|highlighter:${(loader.diffHighlighter.value as { name?: string } | undefined)?.name ?? "none"}`,
            ].join(""),
          );
      },
    }),
  );
  return { wrapper, loader, text: () => wrapper.text(), path, checkout, gitSnapshot, viewport, pages, ready };
}

/** How long a burst of status revisions waits before it costs a round trip. */
const STATUS_REFRESH_DEBOUNCE = 400;

describe("useDiffLoader", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.held.clear();
    mocks.diffFiles.length = 0;
    mocks.prepared.length = 0;
    mocks.getGitDiff.mockResolvedValue(diff());
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("asks Git for the file the panel names and draws what it answers", async () => {
    const panel = host();
    await flushPromises();

    expect(mocks.getGitDiff).toHaveBeenCalledWith("checkout:a", "src/app.vue");
    expect(panel.text()).toBe("diff:src/app.vue|hunks:1|highlighter:read:new");
    expect(panel.ready).toEqual(["src/app.vue"]);
    expect(mocks.diffFiles).toEqual([
      // The file's language on both sides, because a hunk has no name of its own to be read by.
      { oldLang: "vue", uuid: expect.any(String) },
    ]);

    panel.wrapper.unmount();
  });

  it("leaves a stale answer off a diff the panel has already moved on from", async () => {
    const held = gate();
    mocks.getGitDiff.mockImplementationOnce(() => held.promise);
    const panel = host();
    await flushPromises();
    expect(panel.text()).toBe("diff:loading|hunks:0|highlighter:none");

    // The user opens another file while the first answer is still on its way.
    mocks.getGitDiff.mockResolvedValue(diff({ path: "src/other.vue", newContent: "other" }));
    panel.path.value = "src/other.vue";
    await flushPromises();
    expect(panel.text()).toBe("diff:src/other.vue|hunks:1|highlighter:read:other");

    // The first answer lands last and is worth nothing: it is of a file nobody is looking at.
    held.release();
    await flushPromises();
    expect(panel.text()).toBe("diff:src/other.vue|hunks:1|highlighter:read:other");

    panel.wrapper.unmount();
  });

  it("drops an answer that lands after the panel is gone", async () => {
    const held = gate();
    mocks.getGitDiff.mockImplementationOnce(() => held.promise);
    const panel = host();
    await flushPromises();
    expect(panel.text()).toBe("diff:loading|hunks:0|highlighter:none");

    panel.wrapper.unmount();
    held.release();
    await flushPromises();

    // The panel is unmounted, so a late answer neither paints nor reports itself ready.
    expect(panel.text()).toBe("diff:loading|hunks:0|highlighter:none");
    expect(panel.ready).toEqual([]);
  });

  it("drops a reading that was asked for a file nobody is looking at any more", async () => {
    const held = gate();
    mocks.held.set("new", held.promise);
    const panel = host();
    await flushPromises();
    expect(panel.text()).toBe("diff:src/app.vue|hunks:1|highlighter:none");

    panel.wrapper.unmount();
    held.release();
    await flushPromises();

    // A reading queued for a file nobody is looking at any more is a whole pass over it for nothing.
    expect(mocks.prepared).toHaveLength(1);
    expect(panel.loader.diffHighlighter.value).toBeUndefined();
  });

  it("collapses a burst of status revisions into the one reload it amounts to", async () => {
    vi.useFakeTimers();
    const panel = host();
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.getGitDiff).toHaveBeenCalledTimes(1);

    for (const revision of [1, 2, 3]) {
      panel.gitSnapshot.value = snapshot({ statusRevision: revision });
      await nextTick();
    }

    // Nothing has been asked yet: three writes in the workdir are one reload, not three.
    expect(mocks.getGitDiff).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(STATUS_REFRESH_DEBOUNCE);
    await flushPromises();

    expect(mocks.getGitDiff).toHaveBeenCalledTimes(2);
    expect(panel.text()).toBe("diff:src/app.vue|hunks:1|highlighter:read:new");

    panel.wrapper.unmount();
  });

  it("says the file left the change set rather than leaving an empty diff behind", async () => {
    vi.useFakeTimers();
    const panel = host();
    await vi.advanceTimersByTimeAsync(0);
    expect(panel.text()).toBe("diff:src/app.vue|hunks:1|highlighter:read:new");

    panel.gitSnapshot.value = snapshot({ statusRevision: 1, status: status() });
    await nextTick();
    await vi.advanceTimersByTimeAsync(STATUS_REFRESH_DEBOUNCE);
    await flushPromises();

    expect(mocks.getGitDiff).toHaveBeenCalledTimes(1);
    expect(panel.text()).toBe("diff:error|hunks:0|highlighter:none");

    panel.wrapper.unmount();
  });

  it("keeps one reading of a file in flight and reads the last of the rest as soon as it lands", async () => {
    vi.useFakeTimers();
    const held = gate();
    mocks.held.set("first", held.promise);
    mocks.getGitDiff
      .mockResolvedValueOnce(diff({ patch: "@@ -1 +1 @@\n-old\n+first", newContent: "first" }))
      .mockResolvedValueOnce(diff({ patch: "@@ -2 +2 @@\n-old\n+second", newContent: "second" }))
      .mockResolvedValueOnce(diff({ patch: "@@ -3 +3 @@\n-old\n+newest", newContent: "newest" }));
    const panel = host();
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first"]);

    // Two more diffs of the same file arrive while its reading is still running. Neither is read
    // again: a second pass over a file nobody is reading yet costs as much as the first.
    for (const revision of [1, 2]) {
      panel.gitSnapshot.value = snapshot({ statusRevision: revision });
      await nextTick();
      await vi.advanceTimersByTimeAsync(STATUS_REFRESH_DEBOUNCE);
      await flushPromises();
    }
    expect(mocks.prepared).toHaveLength(1);

    held.release();
    await flushPromises();

    // Only the last of the burst was worth keeping, and it is read as soon as the running one lands.
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first", "newest"]);
    expect(panel.text()).toBe("diff:src/app.vue|hunks:1|highlighter:read:newest");

    panel.wrapper.unmount();
  });

  it("never hands the library a reading taken for a diff that is no longer on screen", async () => {
    const left = gate();
    const arrived = gate();
    mocks.held.set("new", left.promise);
    mocks.held.set("other", arrived.promise);
    const panel = host();
    await flushPromises();

    mocks.getGitDiff.mockResolvedValue(diff({ path: "src/other.vue", newContent: "other" }));
    panel.path.value = "src/other.vue";
    await flushPromises();

    // The reading of the file the panel left lands last, and it is worth nothing to the one on
    // screen: handing it over would paint the new diff through the old file's grammar.
    left.release();
    await flushPromises();
    expect(panel.text()).toBe("diff:src/other.vue|hunks:1|highlighter:none");

    arrived.release();
    await flushPromises();
    expect(panel.text()).toBe("diff:src/other.vue|hunks:1|highlighter:read:other");

    panel.wrapper.unmount();
  });

  it("leaves the reading of the file on screen alone when another one lands behind it", async () => {
    vi.useFakeTimers();
    const left = gate();
    const other = gate();
    mocks.held.set("first", left.promise);
    mocks.held.set("other", other.promise);
    mocks.getGitDiff
      .mockResolvedValueOnce(diff({ patch: "@@ -1 +1 @@\n-old\n+first", newContent: "first" }))
      .mockResolvedValueOnce(diff({ patch: "@@ -2 +2 @@\n-old\n+second", newContent: "second" }))
      .mockResolvedValueOnce(diff({ path: "src/other.vue", patch: "@@ -1 +1 @@\n-old\n+other", newContent: "other" }))
      .mockResolvedValue(
        diff({ path: "src/other.vue", patch: "@@ -2 +2 @@\n-old\n+moved", oldContent: "old2", newContent: "moved" }),
      );
    const panel = host();
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first"]);

    // Another version of the file on screen arrives while its first reading is still running. It is
    // the same file, so it waits behind the reading rather than paying for a pass over it twice.
    panel.gitSnapshot.value = snapshot({ statusRevision: 1 });
    await nextTick();
    await vi.advanceTimersByTimeAsync(STATUS_REFRESH_DEBOUNCE);
    await flushPromises();
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first"]);

    // The user opens another file before either has landed. That is a file of its own and the one
    // they are waiting on, so it is read while the other still runs.
    panel.path.value = "src/other.vue";
    await flushPromises();
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first", "other"]);

    // The reading of the file they left lands first. It ends nothing: the reading on screen keeps the
    // slot it took, and the reading queued behind that one is of the file nobody is looking at.
    left.release();
    await flushPromises();
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first", "other"]);
    expect(panel.text()).toBe("diff:src/other.vue|hunks:1|highlighter:none");

    // Which is what keeps the next refresh of the file on screen from starting a second pass over it
    // while one of its own is already running.
    panel.gitSnapshot.value = snapshot({ statusRevision: 2, status: status("src/app.vue", "src/other.vue") });
    await nextTick();
    await vi.advanceTimersByTimeAsync(STATUS_REFRESH_DEBOUNCE);
    await flushPromises();
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first", "other"]);

    // The reading on screen lands, and only then is the version of it that arrived meanwhile read.
    other.release();
    await flushPromises();
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first", "other", "moved"]);
    expect(panel.text()).toBe("diff:src/other.vue|hunks:1|highlighter:read:moved");

    panel.wrapper.unmount();
  });

  it("reads another checkout's file of the same name rather than waiting for the one it holds", async () => {
    const left = gate();
    mocks.held.set("first", left.promise);
    mocks.getGitDiff.mockResolvedValueOnce(diff({ patch: "@@ -1 +1 @@\n-old\n+first", newContent: "first" }));
    const panel = host();
    await flushPromises();
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first"]);

    // The panel moves to another checkout, which holds its own file under the same name. That file
    // is the one the user is waiting on, and the reading running is of the other checkout's one.
    mocks.getGitDiff.mockResolvedValue(
      diff({ patch: "@@ -2 +2 @@\n-old\n+second", oldContent: "old2", newContent: "second" }),
    );
    panel.checkout.value = "checkout:b";
    panel.gitSnapshot.value = snapshot({ checkoutId: "checkout:b" });
    await flushPromises();

    expect(mocks.getGitDiff).toHaveBeenLastCalledWith("checkout:b", "src/app.vue");
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first", "second"]);

    // The reading of the checkout the panel left lands last and is worth nothing to the file now on
    // screen, which is a different file that happens to have the same name.
    left.release();
    await flushPromises();
    expect(panel.text()).toBe("diff:src/app.vue|hunks:1|highlighter:read:second");

    panel.wrapper.unmount();
  });

  it("starts no reading for a queue the panel left behind", async () => {
    vi.useFakeTimers();
    const left = gate();
    mocks.held.set("first", left.promise);
    mocks.getGitDiff
      .mockResolvedValueOnce(diff({ patch: "@@ -1 +1 @@\n-old\n+first", newContent: "first" }))
      .mockResolvedValueOnce(diff({ patch: "@@ -2 +2 @@\n-old\n+second", newContent: "second" }));
    const panel = host();
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first"]);

    // Another version of it arrives while the first is still being read, and waits behind it.
    panel.gitSnapshot.value = snapshot({ statusRevision: 1 });
    await nextTick();
    await vi.advanceTimersByTimeAsync(STATUS_REFRESH_DEBOUNCE);
    await flushPromises();
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first"]);

    // The panel goes away with the reading running and the one behind it still unread.
    panel.wrapper.unmount();
    left.release();
    await flushPromises();

    // The reading lands on a panel nobody is looking at, so it neither paints nor starts the reading
    // waiting behind it: a whole pass over the file for nothing.
    expect(mocks.prepared.map((read) => read.contents.new)).toEqual(["first"]);
    expect(panel.loader.diffHighlighter.value).toBeUndefined();
  });

  it("hands a large diff's page window over to the loader once it is on screen", async () => {
    vi.useFakeTimers();
    const panel = host();
    panel.viewport.value = document.createElement("div");
    await vi.advanceTimersByTimeAsync(0);
    // A diff too small to page asks for nothing.
    expect(panel.pages.loadVisiblePages).not.toHaveBeenCalled();
    expect(panel.pages.reset).not.toHaveBeenCalled();

    mocks.getGitDiff.mockResolvedValue(
      diff({
        path: "src/big.vue",
        large: true,
        totalLines: 900,
        patch: "@@ -1,900 +1,900 @@\n-old\n+new",
        hunks: [{ startLine: 1, endLine: 900, title: "@@ -1,900 +1,900 @@" }],
      }),
    );
    panel.path.value = "src/big.vue";
    await flushPromises();
    // A first read of a file has no pages to drop, so only the window is asked for.
    expect(panel.pages.loadVisiblePages).toHaveBeenCalledTimes(1);
    expect(panel.pages.reset).not.toHaveBeenCalled();

    mocks.getGitDiff.mockResolvedValue(
      diff({
        path: "src/big.vue",
        large: true,
        totalLines: 901,
        patch: "@@ -1,901 +1,901 @@\n-old\n+moved",
        hunks: [{ startLine: 1, endLine: 901, title: "@@ -1,901 +1,901 @@" }],
      }),
    );
    panel.gitSnapshot.value = snapshot({ statusRevision: 1, status: status("src/app.vue", "src/big.vue") });
    await nextTick();
    await vi.advanceTimersByTimeAsync(STATUS_REFRESH_DEBOUNCE);
    await flushPromises();
    // A patch that moved drops the pages, because a moved patch moves every line after the edit.
    expect(panel.pages.reset).toHaveBeenCalledTimes(1);

    panel.wrapper.unmount();
  });

  it("installs a second large diff of the same file, which the empty patch alone could not tell apart", async () => {
    // The shape the backend actually answers a large diff with: no patch at all, because a diff past
    // the patch's cap drops the bytes it had collected. So every large diff compared equal on the
    // patch, the stale answer stayed installed, and the pages went on serving the previous diff's
    // lines while the file behind them moved.
    vi.useFakeTimers();
    mocks.getGitDiff.mockResolvedValue(
      diff({
        path: "src/big.vue",
        patch: "",
        large: true,
        totalLines: 6001,
        hunks: [{ startLine: 1, endLine: 6001, title: "@@ -1,6001 +1,6001 @@" }],
        revision: "6001",
      }),
    );
    const panel = host();
    panel.path.value = "src/big.vue";
    await flushPromises();
    expect(panel.loader.diff.value?.totalLines).toBe(6001);
    expect(panel.pages.reset).not.toHaveBeenCalled();
    const reads = mocks.getGitDiff.mock.calls.length;

    mocks.getGitDiff.mockResolvedValue(
      diff({
        path: "src/big.vue",
        patch: "",
        large: true,
        totalLines: 7001,
        hunks: [{ startLine: 1, endLine: 7001, title: "@@ -1,7001 +1,7001 @@" }],
        revision: "7001",
      }),
    );
    panel.gitSnapshot.value = snapshot({ statusRevision: 1, status: status("src/app.vue", "src/big.vue") });
    await nextTick();
    await vi.advanceTimersByTimeAsync(STATUS_REFRESH_DEBOUNCE);
    await flushPromises();

    expect(mocks.getGitDiff.mock.calls.length).toBe(reads + 1);
    expect(panel.loader.diff.value?.totalLines).toBe(7001);
    // A diff that moved drops the pages, or they keep serving the lines of the one before it.
    expect(panel.pages.reset).toHaveBeenCalledTimes(1);

    panel.wrapper.unmount();
  });

  it("keeps the pages of a large diff that a refresh reported again without having moved", async () => {
    // The other half of the same rule, and what the composer-preservation above rests on: an answer
    // that is the same diff is not a new one. Dropping the pages on every write in the workdir blanked
    // the window and then refilled it, which is the cycle the pages are dropped to avoid.
    vi.useFakeTimers();
    const large = {
      path: "src/big.vue",
      patch: "",
      large: true,
      totalLines: 6001,
      hunks: [{ startLine: 1, endLine: 6001, title: "@@ -1,6001 +1,6001 @@" }],
      revision: "6001",
    };
    mocks.getGitDiff.mockResolvedValue(diff(large));
    const panel = host();
    panel.path.value = "src/big.vue";
    await flushPromises();
    const reads = mocks.getGitDiff.mock.calls.length;

    mocks.getGitDiff.mockResolvedValue(diff(large));
    panel.gitSnapshot.value = snapshot({ statusRevision: 1, status: status("src/app.vue", "src/big.vue") });
    await nextTick();
    await vi.advanceTimersByTimeAsync(STATUS_REFRESH_DEBOUNCE);
    await flushPromises();

    expect(mocks.getGitDiff.mock.calls.length).toBe(reads + 1);
    expect(panel.loader.diff.value?.totalLines).toBe(6001);
    expect(panel.pages.reset).not.toHaveBeenCalled();

    panel.wrapper.unmount();
  });

  it("installs a symlink diff whose target was repointed", async () => {
    // A symlink carries no patch either, so its identity cannot come from one: the target is the
    // whole of what is drawn, and a link repointed while the user is looking at it has to move the
    // view rather than leave the old target on screen.
    vi.useFakeTimers();
    mocks.getGitDiff.mockResolvedValue(diff({ patch: "", symlinkTarget: "old/target", revision: "link-old" }));
    const panel = host();
    await vi.advanceTimersByTimeAsync(0);
    expect(panel.loader.diff.value?.symlinkTarget).toBe("old/target");
    const reads = mocks.getGitDiff.mock.calls.length;

    mocks.getGitDiff.mockResolvedValue(diff({ patch: "", symlinkTarget: "new/target", revision: "link-new" }));
    panel.gitSnapshot.value = snapshot({ statusRevision: 1 });
    await nextTick();
    await vi.advanceTimersByTimeAsync(STATUS_REFRESH_DEBOUNCE);
    await flushPromises();

    expect(mocks.getGitDiff.mock.calls.length).toBe(reads + 1);
    expect(panel.loader.diff.value?.symlinkTarget).toBe("new/target");

    panel.wrapper.unmount();
  });

  it("does not redraw a refresh that changed nothing", async () => {
    vi.useFakeTimers();
    const panel = host();
    await vi.advanceTimersByTimeAsync(0);
    const identity = mocks.diffFiles[0]?.uuid;

    panel.gitSnapshot.value = snapshot({ statusRevision: 1 });
    await nextTick();
    await vi.advanceTimersByTimeAsync(STATUS_REFRESH_DEBOUNCE);
    await flushPromises();

    // A diff that had not moved keeps its hunks: rebuilding them closed the note composer the
    // moment the user started typing.
    expect(mocks.getGitDiff).toHaveBeenCalledTimes(2);
    expect(mocks.diffFiles).toHaveLength(1);
    expect(mocks.diffFiles[0]?.uuid).toBe(identity);
    expect(panel.ready).toEqual(["src/app.vue", "src/app.vue"]);

    panel.wrapper.unmount();
  });
});
