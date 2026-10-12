// @vitest-environment happy-dom
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { reactive } from "vue";
import type { GitStatus } from "../domain/git";
import { DEFAULT_CHECKOUT_UI_STATE } from "../domain/ui-state";
import type { CheckoutUiState } from "../domain/ui-state";
import type { Checkout, Repo } from "../domain/workspace";
import type { ActiveGitSnapshot } from "../presentation/active-git-snapshot";
import { useToasts } from "../presentation/toasts";

const mocks = vi.hoisted(() => ({
  listCheckoutFiles: vi.fn(),
  getGitCheckoutDiffStats: vi.fn(),
  getGitDiffStats: vi.fn(),
}));

vi.mock("../lib/ipc", () => ({
  listCheckoutFiles: mocks.listCheckoutFiles,
  getGitCheckoutDiffStats: mocks.getGitCheckoutDiffStats,
  getGitDiffStats: mocks.getGitDiffStats,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => undefined) }));

import InspectorPane from "./InspectorPane.vue";

const { toasts, dismiss } = useToasts();

/** Mirrors TREE_ROW_HEIGHT, TREE_WINDOW_SIZE and TREE_OVERSCAN: the window math is only right
 *  when the rendered rows are exactly this tall, so the test pins both sides. */
const ROW_HEIGHT = 26;
const WINDOW_SIZE = 64;
const OVERSCAN = 10;

class TestResizeObserver implements ResizeObserver {
  static instances: TestResizeObserver[] = [];
  readonly targets = new Set<Element>();
  disconnected = false;

  constructor(private callback: ResizeObserverCallback) {
    TestResizeObserver.instances.push(this);
  }

  observe(target: Element) {
    this.targets.add(target);
  }

  unobserve(target: Element) {
    this.targets.delete(target);
  }

  disconnect() {
    this.disconnected = true;
    this.targets.clear();
  }

  resize(target: Element, height: number) {
    this.callback([{ target, contentRect: { height } as DOMRectReadOnly } as ResizeObserverEntry], this);
  }
}

function checkout(id: string, isMissing = false): Checkout {
  return {
    id,
    repoId: `repo:${id}`,
    path: `/${id}`,
    canonicalPath: `/${id}`,
    isPrimary: true,
    changedFiles: 0,
    isMissing,
    sessions: [],
  };
}

function gitSnapshot(checkoutId: string, status: GitStatus | null = null): ActiveGitSnapshot {
  return reactive({
    checkoutId,
    status,
    loading: false,
    statusState: status ? ("ready" as const) : ("error" as const),
    statusError: "",
    changesStatusError: "",
    changesWatchError: "",
    statusRevision: 0,
    statusEventRevision: 0,
    statusEventCheckoutId: null,
  });
}

const repo: Repo = {
  id: "repo:git",
  kind: "git",
  name: "git",
  root: "/repo",
  defaultBranch: "main",
  checkouts: [],
  createdAt: "now",
  lastOpenedAt: "now",
};

function mountInspector(props: {
  checkout: Checkout | null;
  repo?: Repo;
  gitSnapshot?: ActiveGitSnapshot;
  savedState?: CheckoutUiState | null;
  fontScale?: number;
  homePath?: string;
}) {
  return mount(InspectorPane, {
    props: { ...props, gitSnapshot: props.gitSnapshot ?? gitSnapshot(props.checkout?.id ?? "none") },
  });
}

/** The panel publishes its row height, so the test can do the window math with the real value. */
function publishedRowHeight(wrapper: ReturnType<typeof mountInspector>): number {
  const style = wrapper.get("aside").attributes("style") ?? "";
  return Number.parseFloat(/--tree-row-height:\s*([\d.]+)px/.exec(style)?.[1] ?? "0");
}

describe("InspectorPane", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    TestResizeObserver.instances = [];
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    mocks.getGitCheckoutDiffStats.mockResolvedValue({});
    mocks.getGitDiffStats.mockResolvedValue([]);
    for (const toast of [...toasts.value]) dismiss(toast.id);
  });

  afterEach(() => vi.unstubAllGlobals());

  it("draws no edge of its own, so the handle that moves it is the only divider", () => {
    // A border down the panel's left, next to the five pixels of handle that move it, is one line
    // drawn twice. The panels meet on their own backgrounds instead, and the hairline the handle
    // draws is there only while the pointer is on it or a drag is in progress.
    const wrapper = mountInspector({ checkout: checkout("checkout:one") });

    expect(wrapper.get(".app-inspector").classes()).not.toContain("border-l");
    wrapper.unmount();
  });

  it("opens a selected file in the central document and marks the open row per checkout", async () => {
    mocks.listCheckoutFiles.mockImplementation(async (checkoutId: string) => ({
      entries: [{ name: `${checkoutId}.txt`, path: `${checkoutId}.txt`, kind: "file" }],
      truncated: false,
    }));
    const wrapper = mountInspector({ checkout: checkout("checkout:first") });
    await flushPromises();

    const first = wrapper.get('[aria-label="Checkout files"] button');
    expect(first.attributes("aria-current")).toBeUndefined();
    await first.trigger("click");
    expect(wrapper.emitted("openFile")).toEqual([[{ checkoutId: "checkout:first", path: "checkout:first.txt" }]]);
    // E.3: the open file is the row that says so.
    expect(first.classes()).toContain("is-selected");
    expect(first.attributes("aria-current")).toBe("true");

    await wrapper.setProps({ checkout: checkout("checkout:second") });
    await flushPromises();
    await wrapper.get('[aria-label="Checkout files"] button').trigger("click");
    expect(wrapper.emitted("openFile")?.at(-1)).toEqual([
      { checkoutId: "checkout:second", path: "checkout:second.txt" },
    ]);

    await wrapper.setProps({ checkout: checkout("checkout:first") });
    await flushPromises();
    const restored = wrapper.get('[aria-label="Checkout files"] button');
    expect(restored.classes()).toContain("is-selected");
    expect(restored.attributes("aria-current")).toBe("true");
    wrapper.unmount();
  });

  it("shows empty, loading, permission, and missing file-tree states", async () => {
    mocks.listCheckoutFiles.mockResolvedValueOnce({ entries: [], truncated: false });
    const wrapper = mountInspector({ checkout: checkout("empty") });
    expect(wrapper.get('[role="status"]').text()).toContain("Loading files");
    await flushPromises();
    expect(wrapper.text()).toContain("This checkout is empty.");

    mocks.listCheckoutFiles.mockRejectedValueOnce({ code: "permission_denied", message: "denied" });
    await wrapper.setProps({ checkout: checkout("denied") });
    await flushPromises();
    // The root of the tree has no row of its own, so the panel itself says what failed: a toast
    // would expire and leave the tree empty with nothing to explain it.
    expect(wrapper.get('[role="alert"]').text()).toBe("Permission denied while reading this folder or file.");
    expect(toasts.value).toHaveLength(0);

    await wrapper.setProps({ checkout: checkout("gone", true) });
    expect(wrapper.text()).toContain("Checkout is missing.");
    wrapper.unmount();
  });

  it("says nothing under a folder that turned out to be empty", async () => {
    mocks.listCheckoutFiles.mockImplementation(async (_checkoutId: string, path: string) => {
      if (path === ".")
        return { entries: [{ name: "src", path: "src", kind: "directory" as const }], truncated: false };
      return { entries: [], truncated: false };
    });
    const wrapper = mountInspector({ checkout: checkout("hollow") });
    await flushPromises();

    await wrapper.get('[aria-label="Checkout files"] button').trigger("click");
    await flushPromises();

    // The folder stays and says nothing more.
    // A line of prose under every one of them is noise on a real tree.
    expect(wrapper.get('[aria-label="Checkout files"]').text()).not.toContain("Empty folder");
    expect(wrapper.findAll('[aria-label="Checkout files"] .file-row')).toHaveLength(1);
    wrapper.unmount();
  });

  it("quietens a dotfile once and a Git-ignored file twice, on the icon and the name together", async () => {
    mocks.listCheckoutFiles.mockResolvedValueOnce({
      entries: [
        { name: "index.ts", path: "index.ts", kind: "file" as const },
        { name: ".gitignore", path: ".gitignore", kind: "file" as const },
        { name: ".env", path: ".env", kind: "file" as const, ignored: true },
        { name: "build.log", path: "build.log", kind: "file" as const, ignored: true },
        { name: "dist", path: "dist", kind: "directory" as const, ignored: true },
      ],
      truncated: false,
    });
    const wrapper = mountInspector({ checkout: checkout("ramp") });
    await flushPromises();

    /** One row's two halves: the icon it draws with, and the name it writes beside it. */
    const row = (name: string) => {
      const found = wrapper.findAll(".file-row").find((candidate) => candidate.text().includes(name));
      if (!found) throw new Error(`no row for ${name}`);
      return {
        icon: found.get(".file-icon").classes(),
        label: found.get(".file-name").classes(),
      };
    };

    // The icon and the name step together, so a row is never quiet in only one of the two.
    expect(row("index.ts").icon).toContain("file-icon-normal");
    expect(row("index.ts").label).toContain("file-name-normal");
    expect(row(".gitignore").icon).toContain("file-icon-hidden");
    expect(row(".gitignore").label).toContain("file-name-hidden");
    expect(row("build.log").icon).toContain("file-icon-ignored");
    expect(row("build.log").label).toContain("file-name-ignored");

    // Being both a dotfile and ignored is one step down, not two: the ramp has three rungs and no
    // fourth, so the flag from the backend wins over the dot in the name.
    expect(row(".env").icon).toContain("file-icon-ignored");
    expect(row(".env").icon).not.toContain("file-icon-hidden");

    // A directory carries the ramp as well, which is what makes a build output read as present
    // but uninteresting instead of competing with the source next to it.
    expect(row("dist").icon).toContain("file-icon-ignored");
    wrapper.unmount();
  });

  it("toggles a normal folder by clicking its row name", async () => {
    mocks.listCheckoutFiles.mockImplementation(async (_id: string, path: string) => ({
      entries:
        path === "."
          ? [{ name: "src", path: "src", kind: "directory" as const }]
          : [{ name: "main.ts", path: "src/main.ts", kind: "file" as const }],
      truncated: false,
    }));
    const wrapper = mountInspector({ checkout: checkout("toggle-row") });
    await flushPromises();

    const tree = wrapper.get('[aria-label="Checkout files"]');
    const name = tree.get('.folder-name[aria-label="Expand src"]');
    await name.trigger("click");
    await flushPromises();
    expect(tree.find(".folder-toggle").attributes("aria-expanded")).toBe("true");
    expect(tree.text()).toContain("main.ts");

    await tree.get('.folder-name[aria-label="Collapse src"]').trigger("click");
    expect(tree.find(".folder-toggle").attributes("aria-expanded")).toBe("false");
    expect(tree.text()).not.toContain("main.ts");
    wrapper.unmount();
  });

  it("keeps a folder that will not list on its own row", async () => {
    mocks.listCheckoutFiles.mockImplementation(async (_checkoutId: string, path: string) => {
      if (path === ".")
        return { entries: [{ name: "src", path: "src", kind: "directory" as const }], truncated: false };
      throw { code: "permission_denied", message: "denied" };
    });
    const wrapper = mountInspector({ checkout: checkout("folder") });
    await flushPromises();

    await wrapper.get('[aria-label="Checkout files"] button').trigger("click");
    await flushPromises();

    // The row says what failed, and it lasts as long as the row does.
    expect(wrapper.get('[aria-label="Checkout files"]').text()).toContain(
      "Permission denied while reading this folder or file.",
    );
    expect(toasts.value).toHaveLength(0);
    wrapper.unmount();
  });

  it("windows a large tree with the row height the row CSS is built from", async () => {
    const entries = Array.from({ length: 500 }, (_, index) => ({
      name: `file-${index}.txt`,
      path: `file-${index}.txt`,
      kind: "file" as const,
    }));
    mocks.listCheckoutFiles.mockResolvedValue({ entries, truncated: false });
    const wrapper = mountInspector({ checkout: checkout("large") });
    await flushPromises();

    const tree = wrapper.get('[aria-label="Checkout files"]');
    const rowHeight = publishedRowHeight(wrapper);
    expect(rowHeight).toBe(ROW_HEIGHT);
    expect(tree.findAll("button").length).toBe(WINDOW_SIZE);

    (tree.element as HTMLElement).scrollTop = 100 * rowHeight;
    await tree.trigger("scroll");
    const rows = tree.findAll("button");
    expect(rows.length).toBe(WINDOW_SIZE);
    // Window math and the rendered box agree: the first row is `scrollTop / height` minus the
    // overscan, and the padding above it is exactly that many rows tall.
    const first = 100 - OVERSCAN;
    expect(rows[0]!.text()).toContain(`file-${first}.txt`);
    expect(rows.at(-1)!.text()).toContain(`file-${first + WINDOW_SIZE - 1}.txt`);
    expect(tree.get(".file-tree-window").attributes("style")).toContain(`padding-top: ${first * rowHeight}px`);

    (tree.element as HTMLElement).scrollTop = entries.length * rowHeight;
    await tree.trigger("scroll");
    expect(tree.text()).toContain("file-499.txt");
    expect(tree.get(".file-tree-window").text()).not.toContain("file-0.txt");
    wrapper.unmount();
  });

  it("restores virtual scroll positions when each list becomes visible again", async () => {
    const files = Array.from({ length: 200 }, (_, index) => ({
      name: "file-" + index + ".txt",
      path: "file-" + index + ".txt",
      kind: "file" as const,
    }));
    const status: GitStatus = {
      branch: "feature",
      defaultBranch: "main",
      aheadCount: 1,
      files: Array.from({ length: 180 }, (_, index) => ({ path: "changed-" + index + ".txt", status: "M" })),
    };
    const savedFilesTop = 100 * ROW_HEIGHT;
    const savedChangesTop = 70 * ROW_HEIGHT;
    mocks.listCheckoutFiles.mockResolvedValue({ entries: files, truncated: false });
    const wrapper = mountInspector({
      checkout: checkout("restore-scroll"),
      repo,
      gitSnapshot: gitSnapshot("restore-scroll", status),
      savedState: {
        ...DEFAULT_CHECKOUT_UI_STATE,
        filesScrollTop: savedFilesTop,
        changesScrollTop: savedChangesTop,
      },
    });
    await flushPromises();

    const tree = wrapper.get('[aria-label="Checkout files"]');
    const changes = wrapper.get('[aria-label="Changed files"]');
    const treeElement = tree.element as HTMLElement;
    const changesElement = changes.element as HTMLElement;
    const observer = TestResizeObserver.instances.find(
      (candidate) => candidate.targets.has(treeElement) && candidate.targets.has(changesElement),
    );
    expect(observer).toBeDefined();
    if (!observer) throw new Error("Inspector scroll observer was not attached");

    // Hiding a scroll viewport clears its DOM offset while the virtual window still remembers it.
    treeElement.scrollTop = 0;
    observer.resize(treeElement, 0);
    observer.resize(treeElement, 600);
    expect(treeElement.scrollTop).toBe(savedFilesTop);

    // A shorter list can clamp the restored DOM offset; keep the virtual window on that clamped row.
    let actualTreeTop = 0;
    Object.defineProperty(treeElement, "scrollTop", {
      configurable: true,
      get: () => actualTreeTop,
      set: (value: number) => {
        actualTreeTop = Math.min(value, savedFilesTop / 2);
      },
    });
    treeElement.scrollTop = 0;
    observer.resize(treeElement, 0);
    observer.resize(treeElement, 600);
    expect(treeElement.scrollTop).toBe(savedFilesTop / 2);
    await flushPromises();
    expect(tree.findAll("button")[0]!.text()).toContain("file-40.txt");
    expect(wrapper.emitted("updateUiState")?.at(-1)?.[0]).toMatchObject({ filesScrollTop: savedFilesTop / 2 });

    await wrapper.get("#inspector-tab-changes").trigger("click");
    await flushPromises();
    changesElement.scrollTop = 0;
    observer.resize(changesElement, 0);
    observer.resize(changesElement, 600);
    expect(changesElement.scrollTop).toBe(savedChangesTop);

    wrapper.unmount();
    expect(observer.disconnected).toBe(true);
  });

  it("scales the row height with the UI font size, and windows the list at the height it draws", async () => {
    const entries = Array.from({ length: 500 }, (_, index) => ({
      name: `file-${index}.txt`,
      path: `file-${index}.txt`,
      kind: "file" as const,
    }));
    mocks.listCheckoutFiles.mockResolvedValue({ entries, truncated: false });
    // 20px is `UI_FONT_SIZE_LIMITS.max`, so this is the largest row the preference can ask for.
    const wrapper = mountInspector({ checkout: checkout("large"), fontScale: 20 / 14 });
    await flushPromises();

    const tree = wrapper.get('[aria-label="Checkout files"]');
    const rowHeight = publishedRowHeight(wrapper);
    // A row pinned at the design height held the larger label in a line box too short for it.
    expect(rowHeight).toBeGreaterThan(ROW_HEIGHT);
    expect(rowHeight).toBe(Math.round(ROW_HEIGHT * (20 / 14)));

    (tree.element as HTMLElement).scrollTop = 100 * rowHeight;
    await tree.trigger("scroll");
    const first = 100 - OVERSCAN;
    const rows = tree.findAll("button");
    expect(rows[0]!.text()).toContain(`file-${first}.txt`);
    expect(tree.get(".file-tree-window").attributes("style")).toContain(`padding-top: ${first * rowHeight}px`);
    wrapper.unmount();
  });

  it("keeps the tab keyboard navigation and sends a change selection to the main document", async () => {
    mocks.listCheckoutFiles.mockResolvedValue({
      entries: [{ name: "main.ts", path: "src/main.ts", kind: "file" }],
      truncated: false,
    });
    const status: GitStatus = {
      branch: "feature",
      defaultBranch: "main",
      aheadCount: 1,
      files: [{ path: "src/main.ts", status: "M" }],
    };
    const wrapper = mountInspector({ checkout: checkout("git"), repo, gitSnapshot: gitSnapshot("git", status) });
    await flushPromises();
    const sections = wrapper.get('[aria-label="Inspector sections"]').findAll("button");
    expect(sections.map((button) => button.text())).toEqual(["Files", "Changes 1"]);
    expect(sections[0]!.attributes("aria-controls")).toBe("inspector-panel-files");
    expect(sections[1]!.attributes("aria-controls")).toBe("inspector-panel-changes");
    expect(wrapper.get("#inspector-panel-files").attributes("aria-labelledby")).toBe("inspector-tab-files");
    expect(wrapper.get("#inspector-panel-files").attributes("tabindex")).toBe("0");
    expect(wrapper.get("#inspector-panel-changes").attributes("aria-labelledby")).toBe("inspector-tab-changes");

    await wrapper.get('[aria-label="Checkout files"] button').trigger("click");
    await wrapper.get('[aria-label="Inspector sections"]').findAll("button")[0]!.trigger("keydown", { key: "End" });
    await flushPromises();
    expect(wrapper.get("#inspector-tab-changes").attributes("aria-selected")).toBe("true");

    const changes = wrapper.get('[aria-label="Changed files"]');
    (changes.element as HTMLElement).scrollTop = 66;
    await changes.trigger("scroll");
    await flushPromises();
    expect(wrapper.emitted("updateUiState")?.at(-1)?.[0]).toMatchObject({ changesScrollTop: 66 });

    const change = changes.get("button.change-row");
    await change.trigger("click");
    expect(wrapper.emitted("openChange")).toEqual([[{ checkoutId: "git", path: "src/main.ts" }]]);
    expect(change.classes()).toContain("is-selected");
    expect(change.attributes("aria-current")).toBe("true");

    await wrapper.get(".new-item").trigger("click");
    expect(wrapper.emitted("openAllChanges")).toEqual([[{ checkoutId: "git" }]]);

    await wrapper.get("#inspector-tab-files").trigger("click");
    expect(wrapper.get('[aria-label="Checkout files"] button').classes()).toContain("is-selected");
    wrapper.unmount();
  });

  it("groups the changed files by parent directory", async () => {
    mocks.listCheckoutFiles.mockResolvedValue({ entries: [], truncated: false });
    const status: GitStatus = {
      branch: "feature",
      defaultBranch: "main",
      aheadCount: 1,
      files: [
        { path: "src/main.ts", status: "M" },
        { path: "src/lib/io.ts", status: "A" },
        { path: "old/name.ts", oldPath: "src/lib/old.ts", status: "R" },
        { path: "README.md", status: "D" },
        { path: "untracked.ts", status: "??" },
      ],
    };
    const wrapper = mountInspector({
      checkout: checkout("git"),
      repo,
      gitSnapshot: gitSnapshot("git", status),
      savedState: { ...DEFAULT_CHECKOUT_UI_STATE, inspectorTab: "changes" },
    });
    await flushPromises();

    const changes = wrapper.get('[aria-label="Changed files"]');
    // Groups follow the order the status reports its files, root files under "/".
    expect(changes.findAll(".details-group-header").map((header) => header.text())).toEqual([
      "src",
      "src/lib",
      "old",
      "/",
    ]);
    // Every changed file gets a row, status included: E.5 drops the `??` case from the tree
    // decorations, not from the list of what changed.
    const names = changes.findAll("button.change-row").map((row) => row.text());
    expect(names).toEqual(["Mmain.ts", "Aio.ts", "Rname.ts", "DREADME.md", "??untracked.ts"]);
    // A rename keeps its source path reachable without spending a second row on it.
    expect(changes.find("button.change-row[title='src/lib/old.ts → old/name.ts']").exists()).toBe(true);
    wrapper.unmount();
  });

  it("puts each file's own line counts on its change row", async () => {
    mocks.listCheckoutFiles.mockResolvedValue({ entries: [], truncated: false });
    mocks.getGitDiffStats.mockResolvedValue([
      { path: "src/main.ts", status: "M", additions: 12, deletions: 3 },
      { path: "README.md", status: "D", additions: 0, deletions: 8 },
      // Git has no count for a binary file, and the row draws nothing rather than "+0".
      { path: "assets/logo.png", status: "M" },
    ]);
    const status: GitStatus = {
      branch: "feature",
      defaultBranch: "main",
      aheadCount: 1,
      files: [
        { path: "src/main.ts", status: "M" },
        { path: "README.md", status: "D" },
        { path: "assets/logo.png", status: "M" },
      ],
    };
    const wrapper = mountInspector({
      checkout: checkout("git"),
      repo,
      gitSnapshot: gitSnapshot("git", status),
      savedState: { ...DEFAULT_CHECKOUT_UI_STATE, inspectorTab: "changes" },
    });
    await flushPromises();

    const changes = wrapper.get('[aria-label="Changed files"]');
    const rows = changes.findAll("button.change-row");
    expect(rows[0]!.get(".diff-add").text()).toBe("+12");
    expect(rows[0]!.get(".diff-del").text()).toBe("-3");
    // A zero addition has nothing to draw, and the deletion stands on its own.
    expect(rows[1]!.find(".diff-add").exists()).toBe(false);
    expect(rows[1]!.get(".diff-del").text()).toBe("-8");
    expect(rows[2]!.find(".diff-add").exists()).toBe(false);
    expect(rows[2]!.find(".diff-del").exists()).toBe(false);
    // The counts describe the listed files, not a different change set.
    expect(mocks.getGitDiffStats).toHaveBeenCalledWith("git");
    wrapper.unmount();
  });

  it("fetches every saved expanded directory, including nested paths", async () => {
    mocks.listCheckoutFiles.mockImplementation(async (_checkoutId: string, path: string) => ({
      entries:
        path === "."
          ? [{ name: "src", path: "src", kind: "directory" }]
          : path === "src"
            ? [{ name: "nested", path: "src/nested", kind: "directory" }]
            : [{ name: "main.ts", path: "src/nested/main.ts", kind: "file" }],
      truncated: false,
    }));
    const wrapper = mountInspector({
      checkout: checkout("nested"),
      savedState: {
        ...DEFAULT_CHECKOUT_UI_STATE,
        expandedDirectories: ["src", "src/nested"],
      },
    });
    await flushPromises();
    expect(mocks.listCheckoutFiles).toHaveBeenCalledWith("nested", ".");
    expect(mocks.listCheckoutFiles).toHaveBeenCalledWith("nested", "src");
    expect(mocks.listCheckoutFiles).toHaveBeenCalledWith("nested", "src/nested");
    const tree = wrapper.get('[aria-label="Checkout files"]');
    expect(tree.text()).toContain("main.ts");
    // The depth gutter is the sidebar's: 8px plus 14px per level.
    expect(tree.findAll(".file-row").map((row) => row.attributes("style"))).toEqual([
      "padding-left: 8px;",
      "padding-left: 22px;",
      "padding-left: 36px;",
    ]);
    await tree.findAll(".folder-toggle")[0]!.trigger("click");
    expect(wrapper.get('[aria-label="Checkout files"]').findAll(".file-row")).toHaveLength(1);
    wrapper.unmount();
  });

  it("refreshes the file tree when the shared owner reports a Git status event", async () => {
    let resolveRefresh!: (value: {
      entries: { name: string; path: string; kind: "file" }[];
      truncated: boolean;
    }) => void;
    mocks.listCheckoutFiles
      .mockResolvedValueOnce({ entries: [{ name: "before.txt", path: "before.txt", kind: "file" }], truncated: false })
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveRefresh = resolve;
          }),
      );
    const snapshot = gitSnapshot("git");
    const wrapper = mountInspector({ checkout: checkout("git"), repo, gitSnapshot: snapshot });
    await flushPromises();
    expect(wrapper.text()).not.toContain("after.txt");

    snapshot.statusEventCheckoutId = "git";
    snapshot.statusEventRevision += 3;
    await vi.waitFor(() => expect(resolveRefresh).toBeTypeOf("function"));
    expect(wrapper.text()).toContain("before.txt");
    expect(wrapper.text()).not.toContain("Loading files");
    resolveRefresh({
      entries: [
        { name: "before.txt", path: "before.txt", kind: "file" },
        { name: "after.txt", path: "after.txt", kind: "file" },
      ],
      truncated: false,
    });
    await flushPromises();
    expect(wrapper.text()).toContain("after.txt");
    expect(mocks.listCheckoutFiles).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });

  it("keeps long file names intact with their full-path tooltip", async () => {
    const name = "a-very-long-file-name-that-must-remain-visible-with-horizontal-scrolling.txt";
    mocks.listCheckoutFiles.mockResolvedValue({
      entries: [{ name, path: name, kind: "file" }],
      truncated: false,
    });
    const fileCheckout = { ...checkout("long-name"), canonicalPath: "/Users/test/project" };
    const wrapper = mountInspector({ checkout: fileCheckout, homePath: "/Users/test" });
    await flushPromises();

    const row = wrapper.get(".file-row");
    expect(wrapper.get(".tree-width-sizer").attributes("aria-hidden")).toBe("true");
    expect(wrapper.get(".tree-width-sizer").text()).toBe(name);
    expect(row.get(".file-name").text()).toBe(name);
    expect(row.attributes("title")).toBe(`~/project/${name}`);
    wrapper.unmount();
  });

  it("hands a file or a folder to the system on ctrl or cmd, and to the preview otherwise", async () => {
    mocks.listCheckoutFiles.mockResolvedValue({
      entries: [
        { name: "src", path: "src", kind: "directory" },
        { name: "notes.txt", path: "notes.txt", kind: "file" },
      ],
      truncated: false,
    });
    const wrapper = mountInspector({ checkout: checkout("checkout:one") });
    await flushPromises();

    const file = () => wrapper.get(".file-row:not(.file-folder)");
    const folder = () => wrapper.get('.folder-name[aria-label="Expand src"]');

    // Held, both rows go out to the machine and nothing here moves: no selection, no expansion, no
    // view change. A row that opened somewhere else and also changed the preview would be
    // answering a gesture nobody asked.
    for (const modifier of [{ ctrlKey: true }, { metaKey: true }]) {
      await file().trigger("click", modifier);
      expect(wrapper.emitted("openExternalFile")?.at(-1)?.[0]).toEqual({
        checkoutId: "checkout:one",
        path: "notes.txt",
      });
    }
    expect(wrapper.emitted("openFile")).toBeFalsy();

    await folder().trigger("click", { ctrlKey: true });
    expect(wrapper.emitted("openExternalFile")?.at(-1)?.[0]).toEqual({ checkoutId: "checkout:one", path: "src" });
    // The folder did not expand either: it is the same one gesture, not two.
    expect(folder().attributes("aria-label")).toBe("Expand src");

    // Not held, each row is the action it always was.
    await file().trigger("click");
    expect(wrapper.emitted("openFile")?.at(-1)?.[0]).toEqual({ checkoutId: "checkout:one", path: "notes.txt" });
    await folder().trigger("click");
    await flushPromises();
    expect(wrapper.find('.folder-name[aria-label="Collapse src"]').exists()).toBe(true);
    wrapper.unmount();
  });

  it("leaves the chevron and a sticky row out of the external gesture", async () => {
    // The chevron is a twelve-pixel target that is never the thing being pointed at, and a sticky
    // row is a copy of a row further down rather than a row of its own. Neither grows a second
    // meaning for the same click.
    mocks.listCheckoutFiles.mockResolvedValue({
      entries: [{ name: "src", path: "src", kind: "directory" }],
      truncated: false,
    });
    const wrapper = mountInspector({ checkout: checkout("checkout:one") });
    await flushPromises();

    await wrapper.get(".file-folder .folder-toggle").trigger("click", { ctrlKey: true });
    expect(wrapper.emitted("openExternalFile")).toBeFalsy();
    wrapper.unmount();
  });

  /** What one character is worth, in a font that is only that. happy-dom has no text metrics at
   *  all, so the panel's measurers answer with this instead: what is being checked here is which row
   *  the width comes from, not how many pixels a glyph is. */
  const CHAR_PX = 7;
  /** The sizer row's own box around a row's words: the depth indent, the padding, the gutter that
   *  stands in for the chevron and the icon, and the gap beside it. */
  const boxedWidth = (depth: number, text: string) => 8 + depth * 14 + 8 + 37 + 7 + text.length * CHAR_PX;

  function measuredWidth(sizerRow: { attributes(name: string): string | undefined }): number {
    return Number.parseFloat(/width:\s*([\d.]+)px/.exec(sizerRow.attributes("style") ?? "")?.[1] ?? "0");
  }

  describe("the tree is sized without a node per row", () => {
    beforeEach(() => {
      // Only the measurers answer a width, exactly as in the sidebar's own weighing tests.
      Object.defineProperty(HTMLElement.prototype, "scrollWidth", {
        configurable: true,
        get(this: HTMLElement) {
          return this.closest(".tree-width-probe") ? (this.textContent?.length ?? 0) * CHAR_PX : 0;
        },
      });
    });
    afterEach(() => Reflect.deleteProperty(HTMLElement.prototype, "scrollWidth"));

    /** The name that decides the width, and the file carrying it is the last one: at ten thousand
     *  rows it is as far from the viewport as the tree goes. */
    const longest = "a-very-long-file-name-".repeat(8) + "RIGHT-END.txt";

    /** One folder holding `count` files, the one at `widest` carrying that name. The other names are
     *  all the same length, so the row that decides the width is the same one at any size. */
    function treeOf(count: number, widest = count - 1) {
      mocks.listCheckoutFiles.mockImplementation(async (_id: string, path: string) => ({
        entries:
          path === "."
            ? [{ name: "folder", path: "folder", kind: "directory" }]
            : Array.from({ length: count }, (_, index) => ({
                name: index === widest ? longest : `f${String(index).padStart(6, "0")}`,
                path: `folder/${index}`,
                kind: "file",
              })),
        truncated: false,
      }));
      return mountInspector({
        checkout: checkout("width"),
        savedState: { ...DEFAULT_CHECKOUT_UI_STATE, expandedDirectories: ["folder"] },
      });
    }

    it("sizes the whole tree from its widest row, whatever the virtual window holds", async () => {
      const wrapper = treeOf(200, 100);
      await flushPromises();
      const tree = wrapper.get('[aria-label="Checkout files"]');
      const sizer = tree.get(".tree-width-sizer");
      expect(sizer.attributes("aria-hidden")).toBe("true");
      expect(sizer.attributes("inert")).toBeDefined();
      expect(sizer.findAll("button, svg")).toHaveLength(0);
      // One row, and it is the one that decides: the file that is nowhere near the viewport.
      expect(sizer.findAll(".tree-width-row")).toHaveLength(1);
      expect(sizer.text()).toContain(longest);
      expect(measuredWidth(sizer.get(".tree-width-row"))).toBe(boxedWidth(1, longest));
      expect(tree.get(".file-tree-window").text()).not.toContain(longest);
      for (const top of [100 * ROW_HEIGHT, 180 * ROW_HEIGHT, 0]) {
        (tree.element as HTMLElement).scrollTop = top;
        await tree.trigger("scroll");
        expect(measuredWidth(sizer.get(".tree-width-row"))).toBe(boxedWidth(1, longest));
        expect(tree.get(".file-tree-window").findAll(".file-row")).toHaveLength(WINDOW_SIZE);
        const sticky = tree.find(".sticky-folders");
        if (sticky.exists()) {
          expect(sticky.element.parentElement).toBe(tree.get(".file-tree-content").element);
          expect(sticky.attributes("style")).not.toContain("width");
        }
      }
      await tree.get(".folder-toggle").trigger("click");
      await flushPromises();
      expect(measuredWidth(sizer.get(".tree-width-row"))).toBe(boxedWidth(0, "folder"));
      expect(sizer.text()).not.toContain(longest);
      wrapper.unmount();
    });

    it("measures proportional canvas text and invalidates it when fonts load", async () => {
      let fontSize = 10;
      let scrollReads = 0;
      let styleReads = 0;
      let assignedFont = "10px Test";
      const context = {
        set font(value: string) {
          assignedFont = value;
        },
        measureText(text: string) {
          const size = Number.parseFloat(assignedFont);
          return { width: [...text].reduce((sum, char) => sum + size * (char === "W" ? 1 : 0.2), 0) };
        },
      };
      const originalGetContext = HTMLCanvasElement.prototype.getContext;
      const originalFonts = Object.getOwnPropertyDescriptor(document, "fonts");
      const realStyle = window.getComputedStyle;
      HTMLCanvasElement.prototype.getContext = (() =>
        context) as unknown as typeof HTMLCanvasElement.prototype.getContext;
      Object.defineProperty(document, "fonts", {
        configurable: true,
        value: Object.assign(new EventTarget(), { ready: new Promise<void>(() => undefined) }),
      });
      Object.defineProperty(HTMLElement.prototype, "scrollWidth", {
        configurable: true,
        get() {
          scrollReads += 1;
          return 0;
        },
      });
      window.getComputedStyle = ((element: Element, pseudo?: string | null) => {
        if (element.classList.contains("tree-width-name") || element.classList.contains("file-status")) {
          styleReads += 1;
          return {
            ...realStyle.call(window, element, pseudo),
            font: `${fontSize}px Test`,
            letterSpacing: "0px",
          } as CSSStyleDeclaration;
        }
        return realStyle.call(window, element, pseudo);
      }) as typeof window.getComputedStyle;

      try {
        mocks.listCheckoutFiles.mockResolvedValue({
          entries: [{ name: "folder", path: "folder", kind: "directory" }],
          truncated: false,
        });
        mocks.listCheckoutFiles.mockImplementation(async (_id: string, path: string) => ({
          entries:
            path === "."
              ? [{ name: "folder", path: "folder", kind: "directory" }]
              : Array.from({ length: 200 }, (_, index) => ({
                  name: index === 150 ? "WWWW" : index === 100 ? "iiii" : `f${index}`,
                  path: `folder/${index}`,
                  kind: "file",
                })),
          truncated: false,
        }));
        const wrapper = mountInspector({
          checkout: checkout("canvas-width"),
          savedState: { ...DEFAULT_CHECKOUT_UI_STATE, expandedDirectories: ["folder"] },
        });
        await flushPromises();
        const tree = wrapper.get('[aria-label="Checkout files"]');
        const sizer = tree.get(".tree-width-sizer");
        expect(sizer.text()).toContain("WWWW");
        expect(sizer.text()).not.toContain("iiii");
        expect(sizer.findAll(".tree-width-row")).toHaveLength(1);
        const firstWidth = measuredWidth(sizer.get(".tree-width-row"));
        expect(scrollReads).toBe(0);
        expect(styleReads).toBe(1);

        fontSize = 20;
        document.fonts.dispatchEvent(new Event("loadingdone"));
        await flushPromises();
        const secondWidth = measuredWidth(sizer.get(".tree-width-row"));
        expect(secondWidth).toBeGreaterThan(firstWidth);
        expect(styleReads).toBe(2);
        expect(scrollReads).toBe(0);
        wrapper.unmount();
      } finally {
        HTMLCanvasElement.prototype.getContext = originalGetContext;
        window.getComputedStyle = realStyle;
        if (originalFonts) Object.defineProperty(document, "fonts", originalFonts);
        else Reflect.deleteProperty(document, "fonts");
      }
    });

    it("keeps sizing nodes constant at one, ten, and fifty thousand rows", async () => {
      const counts: { rows: number; nodes: number; width: number }[] = [];
      for (const size of [1_000, 10_000, 50_000]) {
        const wrapper = treeOf(size);
        await flushPromises();
        const tree = wrapper.get('[aria-label="Checkout files"]');
        const sizer = tree.get(".tree-width-sizer");
        const widest = sizer.get(".tree-width-row");
        // Every node the sizing machinery costs, the measurers included.
        const sizingNodes = () =>
          tree
            .get(".file-tree-content")
            .element.querySelectorAll(".tree-width-sizer, .tree-width-sizer *, .tree-width-probe, .tree-width-probe *")
            .length;
        counts.push({
          rows: sizer.findAll(".tree-width-row").length,
          nodes: sizingNodes(),
          width: measuredWidth(widest),
        });
        // Scrolling through it keeps the same width and the same nodes.
        (tree.element as HTMLElement).scrollTop = Math.floor(size / 2) * ROW_HEIGHT;
        await tree.trigger("scroll");
        expect(measuredWidth(widest)).toBe(counts.at(-1)!.width);
        expect(sizingNodes()).toBe(counts.at(-1)!.nodes);
        expect(tree.get(".file-tree-window").findAll(".file-row")).toHaveLength(WINDOW_SIZE);
        wrapper.unmount();
      }
      // Width follows the whole tree, while sizing DOM stays independent of its row count.
      expect(counts[1]).toEqual(counts[0]);
      expect(counts[2]).toEqual(counts[0]);
      expect(counts[0]!.rows).toBe(1);
      expect(counts[0]!.width).toBe(boxedWidth(1, longest));
      expect(counts[0]!.nodes).toBe(7);
    });
  });

  it("uses the default sticky setting and tracks ancestors through a nested branch", async () => {
    mocks.listCheckoutFiles.mockImplementation(async (_id: string, path: string) => {
      if (path === ".")
        return { entries: [{ name: "src", path: "src", kind: "directory" as const }], truncated: false };
      if (path === "src")
        return { entries: [{ name: "nested", path: "src/nested", kind: "directory" as const }], truncated: false };
      return {
        entries: Array.from({ length: 80 }, (_, i) => ({
          name: `f${i}`,
          path: `${path}/f${i}`,
          kind: "file" as const,
        })),
        truncated: false,
      };
    });
    const stickyCheckout = { ...checkout("sticky"), canonicalPath: "/Users/test/sticky" };
    const wrapper = mountInspector({ checkout: stickyCheckout, homePath: "/Users/test" });
    await flushPromises();
    await wrapper.get('.file-folder:has(.folder-name[aria-label="Expand src"]) .folder-toggle').trigger("click");
    await flushPromises();
    await wrapper.get('.file-folder:has(.folder-name[aria-label="Expand nested"]) .folder-toggle').trigger("click");
    await flushPromises();
    const tree = wrapper.get('[aria-label="Checkout files"]');
    expect(tree.get('.folder-name[aria-label="Collapse src"]').attributes("title")).toBe("~/sticky/src");
    expect(tree.get('.folder-name[aria-label="Collapse nested"]').attributes("title")).toBe("~/sticky/src/nested");
    expect(tree.findAll("button:not(.sticky-folders button)").length).toBeGreaterThan(0);
    expect(tree.find(".sticky-folders").exists()).toBe(false);
    (tree.element as HTMLElement).scrollTop = 5 * ROW_HEIGHT;
    await tree.trigger("scroll");
    expect(tree.findAll(".sticky-folders .folder-name").map((button) => button.attributes("aria-label"))).toEqual([
      "Show src in tree",
      "Show nested in tree",
    ]);
    expect(tree.findAll(".sticky-folders [title]").map((button) => button.attributes("title"))).toEqual([
      "~/sticky/src",
      "~/sticky/src/nested",
    ]);
    expect(tree.find(".sticky-folders").attributes("style")).toContain(`-${2 * ROW_HEIGHT}px`);
    // The row the stack sits over is itself an open folder: it is drawn where it is and stays out
    // of the stack, and the folder above it is in.
    (tree.element as HTMLElement).scrollTop = ROW_HEIGHT;
    await tree.trigger("scroll");
    expect(tree.findAll(".sticky-folders .folder-name").map((button) => button.attributes("aria-label"))).toEqual([
      "Show src in tree",
    ]);
    (tree.element as HTMLElement).scrollTop = 5 * ROW_HEIGHT;
    await tree.trigger("scroll");
    document.body.append(tree.element.parentElement!);
    await tree.find('.sticky-folders .folder-name[aria-label="Show src in tree"]').trigger("click");
    await flushPromises();
    expect((tree.element as HTMLElement).scrollTop).toBe(0);
    const originalName = tree.find('.folder-name[aria-label="Collapse src"]');
    await vi.waitFor(() => expect(document.activeElement).toBe(originalName.element));
    expect(originalName.attributes("aria-current")).toBe("true");
    expect(
      originalName.element.closest(".file-row")?.querySelector(".folder-toggle")?.getAttribute("aria-expanded"),
    ).toBe("true");

    await originalName.element.closest(".file-row")?.querySelector<HTMLButtonElement>(".folder-toggle")?.click();
    await flushPromises();
    expect(
      originalName.element.closest(".file-row")?.querySelector(".folder-toggle")?.getAttribute("aria-expanded"),
    ).toBe("false");
    expect(wrapper.emitted("updateUiState")?.at(-1)?.[0]).toMatchObject({ expandedDirectories: ["src/nested"] });
    wrapper.unmount();
  });

  it("matches the prefix-scan sticky stack on randomized expanded trees", async () => {
    let seed = 31;
    const random = () => (seed = (seed * 1664525 + 1013904223) >>> 0) / 2 ** 32;
    const directories = new Map<string, { name: string; path: string; kind: "directory" | "file" }[]>();
    const expanded: string[] = [];
    function build(path: string, depth: number) {
      const entries = Array.from({ length: 5 }, (_, index) => {
        const childPath = path === "." ? `d${index}` : `${path}/d${index}`;
        if (depth < 3 && random() < 0.42) {
          expanded.push(childPath);
          return { name: `d${index}`, path: childPath, kind: "directory" as const };
        }
        return { name: `f${index}`, path: `${path}/f${index}`, kind: "file" as const };
      });
      directories.set(path, entries);
      for (const entry of entries) if (entry.kind === "directory") build(entry.path, depth + 1);
      return entries;
    }
    build(".", 0);
    mocks.listCheckoutFiles.mockImplementation(async (_id: string, path: string) => ({
      entries: directories.get(path) ?? [],
      truncated: false,
    }));
    const rows: { entry: { name: string; path: string; kind: "directory" | "file" }; depth: number }[] = [];
    function flatten(path: string, depth: number) {
      for (const entry of directories.get(path) ?? []) {
        rows.push({ entry, depth });
        if (entry.kind === "directory") flatten(entry.path, depth + 1);
      }
    }
    flatten(".", 0);
    const wrapper = mountInspector({
      checkout: checkout("random-tree"),
      savedState: { ...DEFAULT_CHECKOUT_UI_STATE, expandedDirectories: expanded },
    });
    await flushPromises();
    const tree = wrapper.get('[aria-label="Checkout files"]');
    Object.defineProperty(tree.element, "clientHeight", { configurable: true, value: 60 });
    const maxStack = Math.floor((60 - 8) / ROW_HEIGHT);
    for (let pixelTop = ROW_HEIGHT; pixelTop < rows.length * ROW_HEIGHT; pixelTop += 7 * ROW_HEIGHT) {
      (tree.element as HTMLElement).scrollTop = pixelTop;
      await tree.trigger("scroll");
      let top = Math.floor(pixelTop / ROW_HEIGHT);
      let ancestors: { entry: (typeof rows)[number]["entry"]; depth: number }[] = [];
      for (let pass = 0; pass <= maxStack; pass++) {
        ancestors = [];
        for (const row of rows.slice(0, top)) {
          while (ancestors.length && ancestors.at(-1)!.depth >= row.depth) ancestors.pop();
          if (row.entry.kind === "directory") ancestors.push(row);
        }
        const nextTop = Math.floor((pixelTop + (Math.min(maxStack, ancestors.length) + 1) * ROW_HEIGHT) / ROW_HEIGHT);
        if (nextTop <= top) break;
        top = nextTop;
      }
      expect(
        tree.findAll(".sticky-folders .folder-name").map((button) => button.attributes("aria-label")),
        `scroll ${pixelTop}, actual ${tree.element.scrollTop}`,
      ).toEqual(ancestors.slice(-maxStack).map(({ entry }) => `Show ${entry.name} in tree`));
    }
    wrapper.unmount();
  });

  it("keeps a deep stack in a tree far larger than the window, and bounds the stack to what fits", async () => {
    mocks.listCheckoutFiles.mockImplementation(async (_id: string, path: string) => {
      if (path === ".")
        return { entries: [{ name: "src", path: "src", kind: "directory" as const }], truncated: false };
      if (path === "src")
        return { entries: [{ name: "nested", path: "src/nested", kind: "directory" as const }], truncated: false };
      if (path === "src/nested")
        return { entries: [{ name: "deep", path: "src/nested/deep", kind: "directory" as const }], truncated: false };
      return {
        entries: Array.from({ length: 10_000 }, (_, i) => ({
          name: `f${String(i).padStart(5, "0")}`,
          path: `${path}/f${i}`,
          kind: "file" as const,
        })),
        truncated: false,
      };
    });
    const wrapper = mountInspector({ checkout: { ...checkout("big"), canonicalPath: "/Users/test/big" } });
    await flushPromises();
    for (const folder of ["src", "nested", "deep"]) {
      await wrapper
        .get(`.file-folder:has(.folder-name[aria-label="Expand ${folder}"]) .folder-toggle`)
        .trigger("click");
      await flushPromises();
    }
    const tree = wrapper.get('[aria-label="Checkout files"]');
    // A viewport with room for two rows of stack, in a tree of ten thousand.
    Object.defineProperty(tree.element, "clientHeight", { configurable: true, value: 2 * ROW_HEIGHT + 8 });

    for (const fraction of [0.1, 0.5, 0.9]) {
      (tree.element as HTMLElement).scrollTop = Math.floor(10_003 * fraction) * ROW_HEIGHT;
      await tree.trigger("scroll");
      // The stack is the folders open above the row under it, trimmed to what the viewport holds.
      expect(tree.findAll(".sticky-folders .folder-name").map((button) => button.attributes("aria-label"))).toEqual([
        "Show nested in tree",
        "Show deep in tree",
      ]);
      // And none of it costs a node per row: the window is the window and the sizer is one row.
      expect(tree.get(".file-tree-window").findAll(".file-row")).toHaveLength(WINDOW_SIZE);
      expect(tree.get(".tree-width-sizer").findAll(".tree-width-row")).toHaveLength(1);
    }
    wrapper.unmount();
  });
});
