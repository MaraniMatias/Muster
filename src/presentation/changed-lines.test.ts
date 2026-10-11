// @vitest-environment happy-dom
import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h, nextTick, ref } from "vue";
import type { Ref } from "vue";
import type { GitFileDiff, GitStatus } from "../domain/git";
import type { DocumentOrigin } from "../domain/main-document";
import type { ActiveGitSnapshot } from "./active-git-snapshot";
import { useChangedLines } from "./changed-lines";

const mocks = vi.hoisted(() => ({
  getGitDiff: vi.fn(),
  /** Every diff a reading asked of Git for, in order, so a test can see what was asked and when. */
  asked: [] as Array<{ checkoutId: string; path: string }>,
  /** The failures a test wants, keyed by path. */
  failing: new Map<string, unknown>(),
}));

vi.mock("../lib/ipc", () => ({
  getGitDiff: mocks.getGitDiff,
}));

function patch(...lines: string[]): GitFileDiff {
  return {
    path: "src/app.ts",
    patch: lines.join("\n"),
    revision: lines.join("\n"),
    isBinary: false,
    large: false,
    tooLarge: false,
    totalLines: 8,
    hunks: [],
  };
}

function status(...files: Array<[string, string]>): GitStatus {
  return {
    branch: "main",
    head: "abc123",
    defaultBranch: "main",
    aheadCount: 0,
    files: files.map(([path, status]) => ({ path, status })),
  };
}

function snapshot(overrides: Partial<ActiveGitSnapshot> = {}): ActiveGitSnapshot {
  return {
    checkoutId: "checkout:a",
    status: status(["src/app.ts", "M"]),
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
function gate(answer: GitFileDiff) {
  let release!: (value: GitFileDiff) => void;
  const promise = new Promise<GitFileDiff>((resolve) => {
    release = resolve;
  });
  return { promise, release: () => release(answer) };
}

interface Host {
  wrapper: ReturnType<typeof mount>;
  lines: ReturnType<typeof useChangedLines>;
  text: () => string;
  path: Ref<string | null>;
  origin: Ref<DocumentOrigin>;
  checkoutId: Ref<string | undefined>;
  gitSnapshot: Ref<ActiveGitSnapshot>;
  fileIdentity: Ref<string | null>;
  causes: unknown[];
}

function host(): Host {
  const path = ref<string | null>("src/app.ts");
  const origin = ref<DocumentOrigin>("checkout");
  const checkoutId = ref<string | undefined>("checkout:a");
  const gitSnapshot = ref(snapshot());
  const fileIdentity = ref<string | null>("checkout:a\0checkout\0src/app.ts");
  const causes: unknown[] = [];
  let lines!: ReturnType<typeof useChangedLines>;
  const wrapper = mount(
    defineComponent({
      setup() {
        lines = useChangedLines({
          checkoutId: () => checkoutId.value,
          path: () => path.value,
          origin: () => origin.value,
          gitSnapshot: () => gitSnapshot.value,
          fileIdentity: () => fileIdentity.value,
          reportCause: (cause) => causes.push(cause),
        });
        return () => h("div", `marks:${[...lines.changedLineNumbers.value].join(",") || "none"}`);
      },
    }),
  );
  return { wrapper, lines, text: () => wrapper.text(), path, origin, checkoutId, gitSnapshot, fileIdentity, causes };
}

describe("useChangedLines", () => {
  beforeEach(() => {
    mocks.getGitDiff.mockReset();
    mocks.asked.length = 0;
    mocks.failing.clear();
    mocks.getGitDiff.mockImplementation(async (checkoutId: string, path: string) => {
      mocks.asked.push({ checkoutId, path });
      const failure = mocks.failing.get(path);
      if (failure) throw failure;
      return patch("@@ -1,4 +1,4 @@", "-a", "+b", " c", "-d", "+e");
    });
  });

  it("asks Git for the lines of the file on screen and shows them as line numbers", async () => {
    const panel = host();
    await flushPromises();

    expect(mocks.asked).toEqual([{ checkoutId: "checkout:a", path: "src/app.ts" }]);
    expect(panel.lines.changedLines.value).toEqual([
      { start: 1, end: 1 },
      { start: 3, end: 3 },
    ]);
    expect(panel.text()).toBe("marks:1,3");

    panel.wrapper.unmount();
  });

  it("asks nothing for a file Git has nothing to say about", async () => {
    const panel = host();
    await flushPromises();
    expect(mocks.asked).toHaveLength(1);

    panel.gitSnapshot.value = snapshot({ status: status(["src/app.ts", "??"]) });
    await nextTick();
    await flushPromises();

    // Marking every line of a new file says nothing the path in the toolbar does not.
    expect(mocks.asked).toHaveLength(1);
    expect(panel.text()).toBe("marks:none");

    panel.wrapper.unmount();
  });

  it("asks nothing for a file whose new side is gone", async () => {
    const panel = host();
    await flushPromises();
    expect(mocks.asked).toHaveLength(1);

    panel.gitSnapshot.value = snapshot({ status: status(["src/app.ts", "MD"]) });
    await nextTick();
    await flushPromises();

    expect(mocks.asked).toHaveLength(1);
    expect(panel.text()).toBe("marks:none");

    panel.wrapper.unmount();
  });

  it("asks nothing for a review document, which lives outside the checkout", async () => {
    const panel = host();
    await flushPromises();
    expect(mocks.asked).toHaveLength(1);

    panel.path.value = "review/notes.md";
    panel.origin.value = "review-workdir";
    panel.fileIdentity.value = "checkout:a\0review-workdir\0review/notes.md";
    await nextTick();
    await flushPromises();

    // Git has no status of a document that is not in the checkout, so there is nothing to mark.
    expect(mocks.asked).toHaveLength(1);
    expect(panel.text()).toBe("marks:none");

    panel.wrapper.unmount();
  });

  it("leaves a stale answer off the marks of the file on screen", async () => {
    const held = gate(patch("@@ -1,6 +1,6 @@", "-a", "+x", " b", " c", " d", "-e", "+y", " f"));
    mocks.getGitDiff.mockImplementationOnce(() => held.promise);
    const panel = host();
    await flushPromises();
    expect(panel.text()).toBe("marks:none");

    // The user opens another file while the first answer is still on its way.
    mocks.getGitDiff.mockResolvedValueOnce(patch("@@ -1,3 +1,3 @@", " a", "-b", "+c"));
    panel.path.value = "src/other.ts";
    panel.fileIdentity.value = "checkout:a\0checkout\0src/other.ts";
    panel.gitSnapshot.value = snapshot({ status: status(["src/app.ts", "M"], ["src/other.ts", "M"]) });
    await nextTick();
    await flushPromises();
    expect(panel.text()).toBe("marks:2");

    // The first answer lands last and is worth nothing: it is of lines of a file nobody is reading.
    held.release();
    await flushPromises();
    expect(panel.text()).toBe("marks:2");

    panel.wrapper.unmount();
  });

  it("clears the marks before the question is asked rather than after it is answered", async () => {
    const held = gate(patch("@@ -1,6 +1,6 @@", "-a", "+x", " b", " c", " d", "-e", "+y", " f"));
    const panel = host();
    await flushPromises();
    expect(panel.text()).toBe("marks:1,3");

    mocks.getGitDiff.mockImplementationOnce(() => held.promise);
    panel.gitSnapshot.value = snapshot({ statusRevision: 1 });
    await nextTick();
    await flushPromises();

    // These are the line numbers of one file, and the next file's rows are already on screen while
    // this one is still in flight.
    expect(panel.text()).toBe("marks:none");
    held.release();
    await flushPromises();
    expect(panel.text()).toBe("marks:1,5");

    panel.wrapper.unmount();
  });

  it("says nothing about a path Git does not count", async () => {
    mocks.failing.set("src/app.ts", { code: "invalid_path", message: "not a path" });
    const panel = host();
    await flushPromises();

    // The marks are an addition to a file that reads perfectly well without them.
    expect(panel.causes).toEqual([]);
    expect(panel.text()).toBe("marks:none");

    panel.wrapper.unmount();
  });

  it("reports a failure that is not about the path at all", async () => {
    mocks.failing.set("src/app.ts", new Error("git is not there"));
    const panel = host();
    await flushPromises();

    expect(panel.causes).toHaveLength(1);
    expect(panel.text()).toBe("marks:none");

    panel.wrapper.unmount();
  });
});
