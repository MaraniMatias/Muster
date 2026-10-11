// @vitest-environment happy-dom
// Test doubles intentionally colocate small component shells and omit production prop defaults.
/* eslint-disable vue/one-component-per-file */
import { flushPromises, mount } from "@vue/test-utils";
import type { VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { defineComponent, reactive, ref } from "vue";
import type { VNodeChild } from "vue";
import type { GitStatus } from "../domain/git";
import type { DocumentMode, MainView } from "../domain/main-document";
import { buildReviewMarkdown } from "../domain/review";
import type { ReviewNote } from "../domain/review";
import type { Checkout, Repo } from "../domain/workspace";
import type { ActiveGitSnapshot } from "../presentation/active-git-snapshot";
import { useToasts } from "../presentation/toasts";
import DocumentPane from "./DocumentPane.vue";
import { failureAnswer, runFormat } from "../lib/prettier-format";
import type { FormatAnswer, FormatCommand } from "../lib/prettier-format";
import FileDiff from "./FileDiff.vue";
import InspectorPane from "./InspectorPane.vue";
import MainPane from "./MainPane.vue";

const { toasts, dismiss } = useToasts();

/**
 * Stands in for the Prettier worker: the same answer, on this thread.
 *
 * This environment has no `Worker`, and what these tests are about is what the pane does with a
 * formatted document. The reply waits a microtask because a real worker answers on another task,
 * and the pane must not come to depend on the answer arriving first.
 */
class InlinePrettierWorker {
  private listener: ((event: { data: FormatAnswer }) => void) | null = null;

  /** How long the worker takes to answer, so a test can let the reader act before it does. */
  static delay = 0;

  addEventListener(type: string, listener: (event: { data: FormatAnswer }) => void): void {
    if (type === "message") this.listener = listener;
  }

  removeEventListener(): void {}

  terminate(): void {}

  postMessage(command: FormatCommand): void {
    void new Promise((resolve) => window.setTimeout(resolve, InlinePrettierWorker.delay)).then(async () => {
      const answer = await runFormat(command.request).then(
        (text): FormatAnswer => ({ id: command.id, text }),
        (error: unknown): FormatAnswer => failureAnswer(command.id, error),
      );
      this.listener?.({ data: answer });
    });
  }
}

beforeEach(() => {
  InlinePrettierWorker.delay = 0;
});

const mocks = vi.hoisted(() => ({
  listCheckoutFiles: vi.fn(),
  readCheckoutFile: vi.fn(),
  readCheckoutMedia: vi.fn(),
  writeCheckoutFile: vi.fn(),
  readPrettierConfig: vi.fn(),
  getReviewFolder: vi.fn(),
  readCheckoutMarkdownImage: vi.fn(),
  getGitDiff: vi.fn(),
  getGitDiffPage: vi.fn(),
  getGitCheckoutDiffStats: vi.fn(async () => ({})),
  getGitDiffStats: vi.fn(async () => []),
  loadTerminalLayout: vi.fn(async () => null),
  saveTerminalLayout: vi.fn(async () => undefined),
}));

vi.mock("../lib/ipc", () => ({
  listCheckoutFiles: mocks.listCheckoutFiles,
  readCheckoutFile: mocks.readCheckoutFile,
  readCheckoutMedia: mocks.readCheckoutMedia,
  writeCheckoutFile: mocks.writeCheckoutFile,
  readPrettierConfig: mocks.readPrettierConfig,
  getReviewFolder: mocks.getReviewFolder,
  readCheckoutMarkdownImage: mocks.readCheckoutMarkdownImage,
  getGitDiff: mocks.getGitDiff,
  getGitDiffPage: mocks.getGitDiffPage,
  // The mounted InspectorPane reads the diff counts; it only renders them when present.
  getGitCheckoutDiffStats: mocks.getGitCheckoutDiffStats,
  getGitDiffStats: mocks.getGitDiffStats,
  // MainPane imports the terminal pane; only its stub is mounted.
  loadTerminalLayout: mocks.loadTerminalLayout,
  saveTerminalLayout: mocks.saveTerminalLayout,
}));

vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeText: vi.fn(),
}));

vi.mock("reka-ui", async () => {
  const { h: createElement, defineComponent, inject, provide } = await import("vue");
  // The document toolbar's popover always renders here, so a grammar can be picked without
  // driving the open state, the way App.test.ts stubs the titlebar's own popover. Plain options
  // objects rather than defineComponent, which would be one more component in a file that already
  // warns about them.
  const passThrough = (name: string) => ({
    name,
    setup:
      (
        _props: unknown,
        context: { attrs: Record<string, unknown>; slots: Record<string, (() => unknown) | undefined> },
      ) =>
      () =>
        createElement("div", context.attrs, context.slots.default?.() as never),
  });
  // The note composer's line picker is the same select as the destination menu, stubbed with its
  // wiring intact so a range's first line can be chosen here the way the keyboard would choose it.
  const select = Symbol("select");
  const selectRoot = defineComponent({
    name: "SelectRoot",
    props: { modelValue: { type: String, default: "" } },
    emits: ["update:modelValue"],
    setup(props, { emit, slots }) {
      provide(select, (value: string) => emit("update:modelValue", value));
      return () => createElement("div", slots.default?.());
    },
  });
  const selectItem = defineComponent({
    name: "SelectItem",
    inheritAttrs: false,
    props: { value: { type: String, default: "" } },
    setup(props, { attrs, slots }) {
      const pick = inject<((value: string) => void) | undefined>(select);
      return () =>
        createElement(
          "button",
          { role: "option", ...attrs, onClick: () => pick?.(props.value ?? "") },
          slots.default?.(),
        );
    },
  });
  return {
    PopoverRoot: passThrough("PopoverRoot"),
    PopoverTrigger: passThrough("PopoverTrigger"),
    // The list is portalled out of the pane so the CodeMirror view underneath cannot paint over
    // it. The portal is the library's and is the one thing here that is worth stubbing as itself:
    // a pass-through stands in for it, and where the content lands is not this file's business.
    PopoverPortal: passThrough("PopoverPortal"),
    PopoverContent: passThrough("PopoverContent"),
    SelectRoot: selectRoot,
    SelectTrigger: passThrough("SelectTrigger"),
    SelectValue: passThrough("SelectValue"),
    SelectPortal: passThrough("SelectPortal"),
    SelectContent: passThrough("SelectContent"),
    SelectViewport: passThrough("SelectViewport"),
    SelectItem: selectItem,
    SelectItemText: passThrough("SelectItemText"),
    SelectItemIndicator: passThrough("SelectItemIndicator"),
  };
});

vi.mock("@git-diff-view/vue", async () => {
  const { defineComponent: component, h: createElement } = await import("vue");
  return {
    DiffFile: class {
      initTheme() {}
      init() {}
      buildUnifiedDiffLines() {}
    },
    DiffModeEnum: { Unified: 4 },
    // Renders the review slots the way the real diff view does for the active line.
    DiffViewWithMultiSelect: component({
      name: "DiffView",
      props: {
        diffViewAddWidget: { type: Boolean, default: false },
        extendData: { type: Object, default: () => ({}) },
      },
      setup:
        (
          props: {
            diffViewAddWidget: boolean;
            extendData: { newFile?: Record<string, { data: ReviewNote[] }> };
          },
          { slots }: { slots: Record<string, ((payload: never) => VNodeChild) | undefined> },
        ) =>
        () =>
          createElement("div", { "data-testid": "diff-view" }, [
            String(props.diffViewAddWidget),
            Object.keys(props.extendData?.newFile ?? {}).join(","),
            slots.extend?.({ data: props.extendData?.newFile?.["1"]?.data ?? [] } as never),
            slots.widget?.({ lineNumber: 1, fromLineNumber: 1, side: 2, onClose: () => undefined } as never),
          ]),
    }),
  };
});

function checkout(id: string): Checkout {
  return {
    id,
    repoId: "repo:repo",
    path: `/${id}`,
    canonicalPath: `/${id}`,
    isPrimary: true,
    changedFiles: 0,
    isMissing: false,
    sessions: [],
  };
}

function snapshot(checkoutId: string, files: GitStatus["files"] = []): ActiveGitSnapshot {
  return reactive<ActiveGitSnapshot>({
    checkoutId,
    status: { branch: "feature", defaultBranch: "main", aheadCount: 1, files },
    loading: false,
    statusState: "ready",
    statusError: "",
    changesStatusError: "",
    changesWatchError: "",
    statusRevision: 0,
    statusEventRevision: 0,
    statusEventCheckoutId: null,
  });
}

function reviewNote(overrides: Partial<ReviewNote> = {}): ReviewNote {
  return {
    id: "note:1",
    checkoutId: "checkout:repo",
    path: "src/app.ts",
    side: "new",
    lineStart: 1,
    lineEnd: null,
    content: "revisit this calculation",
    code: "+new",
    codeHash: "0000000000000001",
    outdated: false,
    roundId: null,
    status: "draft",
    createdAt: "1",
    updatedAt: "1",
    ...overrides,
  };
}

function reviewApi() {
  return reactive({
    notes: [] as ReviewNote[],
    addNote: vi.fn(async () => true),
    updateNote: vi.fn(async () => true),
    deleteNote: vi.fn(async () => true),
    verifyAnchors: vi.fn(async () => true),
    clearOutdated: vi.fn(async () => true),
    resolveNote: vi.fn(async () => true),
  });
}

const repo: Repo = {
  id: "repo:repo",
  kind: "git",
  name: "repo",
  root: "/repo",
  defaultBranch: "main",
  checkouts: [],
  createdAt: "now",
  lastOpenedAt: "now",
};

function documentPaneProps(
  path: string | null,
  mode: DocumentMode = "code",
  currentCheckout = checkout("checkout:one"),
  gitSnapshot = snapshot(currentCheckout.id),
) {
  return { checkout: currentCheckout, path, origin: "checkout" as const, mode, gitSnapshot };
}

/** The grammar rows, by the label each one shows. */
function languageRowLabels(wrapper: VueWrapper): (string | undefined)[] {
  return wrapper
    .get('[role="listbox"]')
    .findAll('[role="option"]')
    .map((row) => row.findAll("span")[0]?.text());
}

async function pickLanguage(wrapper: VueWrapper, label: string) {
  const row = wrapper
    .get('[role="listbox"]')
    .findAll('[role="option"]')
    .find((option) => option.text().startsWith(label));
  if (!row) throw new Error(`no row for ${label}`);
  await row.trigger("click");
}

describe("media documents", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    let index = 0;
    vi.spyOn(URL, "createObjectURL").mockImplementation(() => "blob:media-" + ++index);
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    mocks.readCheckoutMedia.mockResolvedValue(new Blob([new Uint8Array([0, 255])], { type: "image/png" }));
  });
  // Several tests here take over Blob.prototype.slice to watch how the payload is read. Restored
  // after each one so the next test spies on the real method: a second spy over a live one is the
  // same spy, and an implementation delegating to what it replaced calls itself.
  afterEach(() => vi.restoreAllMocks());

  it.each(["png", "jpg", "jpeg", "gif", "webp", "avif", "ico", "bmp", "mp4", "webm", "mov", "ogv"])(
    "opens %s as media even with restored Code mode",
    async (ext) => {
      const wrapper = mount(DocumentPane, {
        props: documentPaneProps(
          "asset." + ext,
          "code",
          checkout("checkout:one"),
          snapshot("checkout:one", [{ path: "asset." + ext, status: "M" }]),
        ),
      });
      await flushPromises();
      expect(wrapper.find(".media-preview img, .media-preview video").exists()).toBe(true);
      expect(wrapper.find('[aria-label="Document mode"]').exists()).toBe(false);
      expect(wrapper.find('[role="listbox"]').exists()).toBe(false);
      expect(wrapper.find('[aria-label="Source code"]').exists()).toBe(false);
      expect(mocks.readCheckoutFile).not.toHaveBeenCalled();
      expect(mocks.getGitDiff).not.toHaveBeenCalled();
      wrapper.unmount();
      expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:media-1");
    },
  );

  it("refreshes only named media (or unknown batches), revokes replacements and surfaces decoding errors", async () => {
    let payload = new Uint8Array([0, 255]);
    mocks.readCheckoutMedia.mockImplementation(async () => new Blob([payload], { type: "image/png" }));
    const wrapper = mount(DocumentPane, { props: documentPaneProps("asset.png", "view") });
    await flushPromises();
    await wrapper.setProps({ refreshRevision: 1, refreshPaths: ["unrelated.ts"] });
    expect(mocks.readCheckoutMedia).toHaveBeenCalledTimes(1);
    // The same bytes again: there is nothing to reinstall, so nothing is revoked.
    await wrapper.setProps({ refreshRevision: 2, refreshPaths: ["asset.png"] });
    await flushPromises();
    expect(mocks.readCheckoutMedia).toHaveBeenCalledTimes(2);
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();
    // One byte different, same length and same type. Only reading the payload says so.
    payload = new Uint8Array([0, 254]);
    await wrapper.setProps({ refreshRevision: 3, refreshPaths: [] });
    await flushPromises();
    expect(mocks.readCheckoutMedia).toHaveBeenCalledTimes(3);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:media-1");
    await wrapper.get(".media-preview img").trigger("error");
    expect(wrapper.get('[role="alert"]').text()).toContain("could not be decoded");
    wrapper.unmount();
  });

  it("keeps the object URL and the playback when a refresh reads the same bytes", async () => {
    const wrapper = mount(DocumentPane, { props: documentPaneProps("clip.mp4", "view") });
    await flushPromises();
    const pause = vi.spyOn(wrapper.get("video").element as HTMLVideoElement, "pause");
    await wrapper.setProps({ refreshRevision: 1, refreshPaths: [] });
    await flushPromises();
    // The file was read again — that is how it is known to be unchanged — and the video is still
    // the one that was playing, on the URL it was already playing from.
    expect(mocks.readCheckoutMedia).toHaveBeenCalledTimes(2);
    expect(wrapper.get("video").attributes("src")).toBe("blob:media-1");
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();
    expect(pause).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("reads one near-cap payload at a time, in slices no larger than the compare chunk", async () => {
    const cap = 64 * 1024 * 1024 - 1;
    const bytes = new Uint8Array(cap);
    bytes[cap - 1] = 7;
    mocks.readCheckoutMedia.mockResolvedValue(new Blob([bytes], { type: "video/mp4" }));
    const slice = Blob.prototype.slice;
    let largest = 0;
    vi.spyOn(Blob.prototype, "slice").mockImplementation(function (this: Blob, start, end, type) {
      largest = Math.max(largest, (end ?? this.size) - (start ?? 0));
      return slice.call(this, start, end, type);
    });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("clip.mp4", "view") });
    await flushPromises();
    await wrapper.setProps({ refreshRevision: 1, refreshPaths: [] });
    await flushPromises();
    // The payload is nearly the whole video cap, and deciding whether it changed never asks for
    // more than one chunk of it at a time.
    expect(largest).toBeLessThanOrEqual(1024 * 1024);
    expect(largest).toBeGreaterThan(0);
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it("holds a burst behind the comparison in flight, abandons it, and commits only the newest", async () => {
    const slice = Blob.prototype.slice;
    const arrayBuffer = Blob.prototype.arrayBuffer;
    let arm = false;
    let held = 0;
    let chunks = 0;
    const release: (() => void)[] = [];
    vi.spyOn(Blob.prototype, "slice").mockImplementation(function (this: Blob, start, end, type) {
      const part = slice.call(this, start, end, type);
      part.arrayBuffer = () => {
        const read = arrayBuffer.call(part);
        chunks += 1;
        if (!arm) return read;
        held += 1;
        return new Promise<ArrayBuffer>((done) => release.push(() => void read.then(done)));
      };
      return part;
    });
    // Four chunks, and the payloads differ only in the last one: a comparison that stops early
    // stops on something other than the answer.
    let reads = 0;
    mocks.readCheckoutMedia.mockImplementation(async () => {
      reads += 1;
      const bytes = new Uint8Array(4 * 1024 * 1024);
      bytes[bytes.length - 1] = reads;
      return new Blob([bytes], { type: "image/png" });
    });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("asset.png", "view") });
    await flushPromises();
    arm = true;
    await wrapper.setProps({ refreshRevision: 1, refreshPaths: [] });
    await flushPromises();
    // The second load has read the file and is comparing it, its first chunk held open.
    expect(mocks.readCheckoutMedia).toHaveBeenCalledTimes(2);
    expect(held).toBe(1);
    for (const revision of [2, 3, 4]) {
      await wrapper.setProps({ refreshRevision: revision, refreshPaths: revision === 4 ? ["asset.png"] : [] });
      await flushPromises();
    }
    // Three more loads were asked for and not one of them started: the queue covers the comparison
    // too, so a burst cannot stack a payload per refresh behind the one being checked.
    expect(mocks.readCheckoutMedia).toHaveBeenCalledTimes(2);
    arm = false;
    release.splice(0).forEach((open) => open());
    await flushPromises();
    // The superseded comparison read the chunk it was held on, its partner, and then stopped at the
    // next position instead of reading the other three: four reads rather than the sixteen two
    // full comparisons of a four-chunk payload would take. The two loads after it were abandoned
    // without reading at all, and the one that names the file read last and is what is installed.
    expect(chunks).toBe(10);
    expect(mocks.readCheckoutMedia).toHaveBeenCalledTimes(3);
    const installed = vi.mocked(URL.createObjectURL).mock.calls.at(-1)![0] as Blob;
    expect(new Uint8Array(await installed.arrayBuffer()).at(-1)).toBe(3);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:media-1");
    wrapper.unmount();
  });

  it("discards a media comparison that lands after the reader moved on", async () => {
    const slice = Blob.prototype.slice;
    const arrayBuffer = Blob.prototype.arrayBuffer;
    let release!: () => void;
    const gate = new Promise<void>((done) => (release = done));
    vi.spyOn(Blob.prototype, "slice").mockImplementation(function (this: Blob, start, end, type) {
      const part = slice.call(this, start, end, type);
      // The comparison reads its chunks through here, and here one is held open across a
      // navigation so it finishes against a pane that is showing something else.
      part.arrayBuffer = () => gate.then(() => arrayBuffer.call(part));
      return part;
    });
    mocks.readCheckoutMedia.mockImplementation(async (_id: string, path: string) =>
      path.endsWith(".mp4")
        ? new Blob([new Uint8Array([2])], { type: "video/mp4" })
        : new Blob([new Uint8Array([1])], { type: "image/png" }),
    );
    const wrapper = mount(DocumentPane, { props: documentPaneProps("asset.png", "view") });
    await flushPromises();
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    await wrapper.setProps({ refreshRevision: 1, refreshPaths: [] });
    await flushPromises();
    await wrapper.setProps({ path: "clip.mp4" });
    release();
    await flushPromises();
    // The comparison that named asset.png answered after clip.mp4 took the pane: it is thrown away,
    // so the video is the second URL and not the third.
    expect(URL.createObjectURL).toHaveBeenCalledTimes(2);
    expect(wrapper.get("video").attributes("src")).toBe("blob:media-2");
    expect(wrapper.find(".media-preview img").exists()).toBe(false);
    wrapper.unmount();
  });

  it("discards stale requests after navigation and unmount", async () => {
    let resolve!: (blob: Blob) => void;
    mocks.readCheckoutMedia.mockImplementationOnce(
      () =>
        new Promise<Blob>((done) => {
          resolve = done;
        }),
    );
    const wrapper = mount(DocumentPane, { props: documentPaneProps("old.png", "view") });
    await wrapper.setProps({ path: "new.png" });
    await flushPromises();
    resolve(new Blob(["stale"]));
    await flushPromises();
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    wrapper.unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(1);
    mocks.readCheckoutMedia.mockImplementationOnce(
      () =>
        new Promise<Blob>((done) => {
          resolve = done;
        }),
    );
    const pending = mount(DocumentPane, { props: documentPaneProps("pending.png", "view") });
    pending.unmount();
    resolve(new Blob(["late"]));
    await flushPromises();
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
  });

  it("shows loading/read failures and revokes failed refreshes", async () => {
    mocks.readCheckoutMedia.mockRejectedValueOnce(new Error("Media read failed"));
    const wrapper = mount(DocumentPane, { props: documentPaneProps("asset.png", "view") });
    expect(wrapper.text()).toContain("Loading file");
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain("Media read failed");
    await wrapper.setProps({ refreshRevision: 1 });
    await flushPromises();
    mocks.readCheckoutMedia.mockRejectedValueOnce(new Error("Gone"));
    await wrapper.setProps({ refreshRevision: 2 });
    await flushPromises();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:media-1");
    wrapper.unmount();
  });

  it("pauses video when the retained pane is hidden or replaced", async () => {
    const wrapper = mount(DocumentPane, { props: documentPaneProps("clip.mp4", "view") });
    await flushPromises();
    const element = wrapper.get("video").element as HTMLVideoElement;
    const pause = vi.spyOn(element, "pause");
    expect(element.controls).toBe(true);
    expect(element.hasAttribute("playsinline")).toBe(true);
    expect(element.autoplay).toBe(false);
    await wrapper.get('[data-testid="close-preview"]').trigger("click");
    expect(pause).toHaveBeenCalled();
    pause.mockClear();
    await wrapper.setProps({ path: null, mode: "code" });
    expect(pause).toHaveBeenCalled();
    await wrapper.setProps({ path: "clip.mp4", mode: "view" });
    await flushPromises();
    expect(mocks.readCheckoutMedia).toHaveBeenCalledTimes(1);
    const nextPause = vi.spyOn(wrapper.get("video").element as HTMLVideoElement, "pause");
    await wrapper.setProps({ path: "asset.png" });
    expect(nextPause).toHaveBeenCalled();
    wrapper.unmount();
  });

  it.each(["terminal", "diff"])("pauses video when MainPane hides it for %s", async (destination) => {
    const currentCheckout = checkout("checkout:one");
    const wrapper = mount(MainPane, {
      props: {
        checkout: currentCheckout,
        view: { kind: "document", path: "clip.mp4", mode: "view", origin: "checkout" },
        ready: true,
        gitSnapshot: snapshot(currentCheckout.id),
        review: reviewApi(),
        activeSessionId: null,
        isOpening: false,
      },
      global: { stubs: { SessionPane: true, FileDiff: true } },
    });
    await flushPromises();
    const pause = vi.spyOn(wrapper.get("video").element as HTMLVideoElement, "pause");
    const pane = wrapper.getComponent(DocumentPane);
    await wrapper.setProps({
      view: destination === "terminal" ? { kind: "terminal", sessionId: null } : { kind: "diff", path: "file.ts" },
    });
    expect(wrapper.getComponent(DocumentPane).element).toBe(pane.element);
    expect(pause).toHaveBeenCalled();
    expect(pane.props("path")).toBeNull();
    expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).toBe("none");
    wrapper.unmount();
  });

  it("renders SVG only as an image and previews the unsaved XML draft", async () => {
    const source = '<svg xmlns="http://www.w3.org/2000/svg"><rect width="1" /></svg>';
    mocks.readCheckoutFile.mockResolvedValue({ path: "icon.svg", content: source });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("icon.svg", "code") });
    await vi.waitFor(() => expect(wrapper.find(".cm-editor").exists()).toBe(true));
    expect(wrapper.get(".code-editor-host").attributes("data-language")).toBe("xml");
    expect(wrapper.get<HTMLButtonElement>('[data-testid="format-file"]').element.disabled).toBe(false);
    const { EditorView } = await import("@codemirror/view");
    const editor = EditorView.findFromDOM(wrapper.get(".cm-editor").element as HTMLElement)!;
    const draft = source.replace('width="1"', 'width="2"');
    editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: draft } });
    await wrapper.setProps({ mode: "view" });
    await flushPromises();
    expect(wrapper.find('[aria-label="Document mode"]').exists()).toBe(true);
    expect(wrapper.find(".media-preview svg").exists()).toBe(false);
    expect(wrapper.get(".media-preview img").attributes("src")).toBe("blob:media-1");
    const blob = vi.mocked(URL.createObjectURL).mock.calls[0]![0] as Blob;
    expect(await blob.text()).toBe(draft);
    expect(mocks.readCheckoutFile).toHaveBeenCalledTimes(1);
    await wrapper.setProps({ mode: "code" });
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:media-1");
    wrapper.unmount();
  });

  it.each(["<html><!-- <svg> --></html>", '<svg xmlns="http://www.w3.org/2000/svg">', '<svg xmlns="wrong"/>'])(
    "rejects a non-SVG root or malformed XML in View: %s",
    async (content) => {
      mocks.readCheckoutFile.mockResolvedValue({ path: "bad.svg", content });
      const wrapper = mount(DocumentPane, { props: documentPaneProps("bad.svg", "view") });
      await flushPromises();
      expect(wrapper.get('[role="alert"]').text()).toContain("not a valid SVG");
      expect(URL.createObjectURL).not.toHaveBeenCalled();
      await wrapper.setProps({ mode: "code" });
      await vi.waitFor(() => expect(wrapper.find(".cm-editor").exists()).toBe(true));
      const { EditorView } = await import("@codemirror/view");
      const editor = EditorView.findFromDOM(wrapper.get(".cm-editor").element as HTMLElement)!;
      expect(editor.state.doc.toString()).toBe(content);
      editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: "<svg" } });
      await flushPromises();
      await wrapper.get('button[aria-label="Save"]').trigger("click");
      await flushPromises();
      expect(mocks.writeCheckoutFile).toHaveBeenCalledWith("checkout:one", "bad.svg", "<svg", content, "checkout");
      await wrapper.setProps({ mode: "view" });
      await flushPromises();
      expect(wrapper.get('[role="alert"]').text()).toContain("not a valid SVG");
      expect(URL.createObjectURL).not.toHaveBeenCalled();
      wrapper.unmount();
    },
  );
});

describe("DocumentPane", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    mocks.listCheckoutFiles.mockResolvedValue({ entries: [], truncated: false });

    mocks.readCheckoutMarkdownImage.mockResolvedValue({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgo=",
      sizeBytes: 8,
    });
    mocks.getGitDiffPage.mockResolvedValue({ path: "", startLine: 0, totalLines: 0, lines: [] });
    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "diff --git a/src/app.ts b/src/app.ts\n@@ -1 +1 @@\n-old\n+new\n",
      revision: "src/app.ts#1",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -1 +1 @@" }],
    });
    mocks.getReviewFolder.mockResolvedValue({
      path: "/Users/dev/.muster/tmp/reviews",
      storage: "default",
      files: 0,
      bytes: 0,
    });
    vi.stubGlobal("Worker", InlinePrettierWorker);
    for (const toast of [...toasts.value]) dismiss(toast.id);
  });

  it("opens a file selection in the central document, rendering Markdown by default and allowing Code mode", async () => {
    const view = ref<MainView>({ kind: "terminal", sessionId: null });
    const currentCheckout = checkout("checkout:one");
    const gitSnapshot = snapshot(currentCheckout.id, [{ path: "src/app.ts", status: "M" }]);
    mocks.listCheckoutFiles.mockResolvedValue({
      entries: [
        { name: "readme.md", path: "docs/readme.md", kind: "file" },
        { name: "app.ts", path: "src/app.ts", kind: "file" },
      ],
      truncated: false,
    });
    mocks.readCheckoutFile.mockImplementation(async (_checkoutId: string, path: string) => ({
      path,
      content: path.endsWith(".md") ? "# Muster\n\n[unsafe](javascript:alert(1))" : "const answer: number = 42;",
    }));
    const harness = defineComponent({
      components: { InspectorPane, MainPane },
      setup() {
        function openFile(file: { checkoutId: string; path: string }) {
          view.value = { kind: "document", path: file.path, mode: "view", origin: "checkout" };
        }
        function openChange(file: { checkoutId: string; path: string }) {
          view.value = { kind: "diff", path: file.path };
        }
        return { view, currentCheckout, gitSnapshot, review: reviewApi(), openFile, openChange };
      },
      data: () => ({ repo }),
      template: `<div>
        <InspectorPane :checkout="currentCheckout" :repo="repo" :git-snapshot="gitSnapshot" @open-file="openFile" @open-change="openChange" />
        <MainPane
          :checkout="currentCheckout"
          :view="view"
          :ready="true"
          :git-snapshot="gitSnapshot"
          :review="review"
          :active-session-id="null"
          :is-opening="false"
          @update-document-mode="view = { ...view, mode: $event }"
        />
      </div>`,
    });
    const wrapper = mount(harness, {
      global: { stubs: { SessionPane: true } },
    });
    await flushPromises();
    await wrapper.get('[aria-label="Checkout files"] button').trigger("click");
    await vi.waitFor(() => expect(wrapper.find('[aria-label="File contents"] article').exists()).toBe(true));
    expect(wrapper.get('[aria-label="File contents"] article').text()).toContain("Muster");
    expect(wrapper.get('[aria-label="File contents"]').element.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(mocks.readCheckoutFile).toHaveBeenCalledWith(currentCheckout.id, "docs/readme.md", "checkout");

    await wrapper
      .get('[aria-label="Document mode"]')
      .findAll("button")
      .find((button) => button.text() === "Code")!
      .trigger("click");
    // Code is the editor, so the Code view only has its text once CodeMirror is mounted.
    await vi.waitFor(() => expect(wrapper.find(".cm-line").exists()).toBe(true));
    expect(wrapper.get(".cm-content").text()).toContain("# Muster");

    await wrapper.get('[aria-label="Inspector sections"]').findAll("button")[1].trigger("click");
    expect(wrapper.text()).toContain("docs/readme.md");
    await wrapper.get('[aria-label="Changed files"] button').trigger("click");
    await flushPromises();
    // A change is the diff view now, not a mode of the document view (F.1, F.3).
    expect(mocks.getGitDiff).toHaveBeenCalledWith(currentCheckout.id, "src/app.ts");
    expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).toBe("none");
    expect((wrapper.get("#main-view-diff").element as HTMLElement).style.display).not.toBe("none");
    expect(wrapper.find('[data-testid="diff-view"]').exists()).toBe(true);

    view.value = { kind: "diff", path: null };
    await vi.waitFor(() => expect(wrapper.get(".diff-file-header").text()).toContain("src/app.ts"));
    expect(wrapper.get('[aria-label="File diff"] header').text()).toContain("All changes");
    // The file's own diff is a diff of this same component, loaded the same lazy way.
    await wrapper.get(".diff-file-header").trigger("click");
    await vi.waitFor(() => expect(wrapper.findAll('[data-testid="diff-view"]')).toHaveLength(1));
    wrapper.unmount();
  });

  it("navigates safe relative Markdown document links within the active checkout", async () => {
    const checkoutId = "checkout:markdown-links";
    mocks.readCheckoutFile.mockResolvedValue({ path: "docs/start.md", content: "[Next guide](../guide.md)" });
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps("docs/start.md", "view", checkout(checkoutId)),
    });
    await vi.waitFor(() => expect(wrapper.find('[aria-label="File contents"] article').exists()).toBe(true));
    await wrapper.get('[aria-label="File contents"] a[href="../guide.md"]').trigger("click");
    expect(wrapper.emitted("openMarkdownLink")).toEqual([["guide.md"]]);
    wrapper.unmount();
  });

  it("hands a web link to the browser and keeps the document's own links to itself", async () => {
    // A web link names nothing in this checkout, and the preview is not a browser that could
    // follow one: `open_url` on the other end is the only thing that can open it, and it refuses
    // anything that is not a page. Which is why `mailto:` is not in here either.
    const checkoutId = "checkout:web-links";
    mocks.readCheckoutFile.mockResolvedValue({
      path: "docs/start.md",
      content: "[Site](https://example.com/guide) and [write](mailto:someone@example.com)",
    });
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps("docs/start.md", "view", checkout(checkoutId)),
    });
    await vi.waitFor(() => expect(wrapper.find('[aria-label="File contents"] article').exists()).toBe(true));

    await wrapper.get('[aria-label="File contents"] a[href="https://example.com/guide"]').trigger("click");

    expect(wrapper.emitted("openExternalUrl")).toEqual([["https://example.com/guide"]]);
    expect(wrapper.emitted("openMarkdownLink")).toBeUndefined();
    wrapper.unmount();
  });

  it("leaves a link that names an app rather than a page to that app", async () => {
    const checkoutId = "checkout:app-links";
    mocks.readCheckoutFile.mockResolvedValue({
      path: "docs/start.md",
      content: "[write](mailto:someone@example.com)",
    });
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps("docs/start.md", "view", checkout(checkoutId)),
    });
    await vi.waitFor(() => expect(wrapper.find('[aria-label="File contents"] article').exists()).toBe(true));

    await wrapper.get('[aria-label="File contents"] a[href="mailto:someone@example.com"]').trigger("click");

    // Nothing here opens it, which is the honest answer: this key is not an email client's.
    expect(wrapper.emitted("openExternalUrl")).toBeUndefined();
    expect(wrapper.emitted("openMarkdownLink")).toBeUndefined();
    wrapper.unmount();
  });

  it("copies the absolute checkout path and reports success", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const answer = 42;" });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/app.ts") });
    await flushPromises();

    await wrapper.get('[aria-label="Copy file path"]').trigger("click");

    expect(writeText).toHaveBeenCalledWith("/checkout:one/src/app.ts");
    expect(toasts.value.at(-1)?.message).toBe("File path copied.");
    wrapper.unmount();
  });

  it("copying the path of an exported file yields the path that exists", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "notes.md", content: "# Review" });
    const wrapper = mount(DocumentPane, {
      props: { ...documentPaneProps("notes.md", "view"), origin: "review-default" },
    });
    await flushPromises();

    await wrapper.get('[aria-label="Copy file path"]').trigger("click");

    expect(mocks.getReviewFolder).toHaveBeenCalledWith("checkout:one", "default");
    expect(writeText).toHaveBeenCalledWith("/Users/dev/.muster/tmp/reviews/notes.md");
    wrapper.unmount();
  });

  it("same-name review files from different pinned roots keep separate drafts", async () => {
    mocks.readCheckoutFile.mockImplementation(async (_checkoutId: string, path: string, origin: string) => ({
      path,
      content:
        origin === "review-default"
          ? "default baseline"
          : origin === "review-workdir"
            ? "workdir baseline"
            : "checkout baseline",
    }));
    const wrapper = mount(DocumentPane, { props: documentPaneProps("notes.md") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));
    const edit = async (text: string) => {
      const editor = wrapper.get(".cm-content").element as HTMLElement;
      editor.textContent = text;
      editor.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: "t" }));
      await flushPromises();
    };

    await edit("checkout draft");
    await wrapper.setProps({ origin: "review-default" });
    await vi.waitFor(() => expect(wrapper.get(".cm-content").text()).toContain("default baseline"));
    await edit("default draft");
    await wrapper.setProps({ origin: "review-workdir" });
    await vi.waitFor(() => expect(wrapper.get(".cm-content").text()).toContain("workdir baseline"));
    await edit("workdir draft");
    await wrapper.setProps({ origin: "review-default" });
    await vi.waitFor(() => expect(wrapper.get(".cm-content").text()).toContain("default draft"));
    await wrapper.setProps({ origin: "checkout" });
    await vi.waitFor(() => expect(wrapper.get(".cm-content").text()).toContain("checkout draft"));

    expect(mocks.readCheckoutFile).toHaveBeenCalledWith("checkout:one", "notes.md", "review-default");
    expect(mocks.readCheckoutFile).toHaveBeenCalledWith("checkout:one", "notes.md", "review-workdir");
    wrapper.unmount();
  });

  it("keeps read, format, save and copy on the pinned workdir origin", async () => {
    const source = "This   is   a review";
    mocks.readCheckoutFile.mockResolvedValue({ path: "notes.md", content: source });
    mocks.readPrettierConfig.mockResolvedValue(null);
    mocks.writeCheckoutFile.mockResolvedValue(undefined);
    mocks.getReviewFolder.mockImplementation(async (_checkoutId: string, storage: string) => ({
      path: storage === "workdir" ? "/checkout:one/.muster/reviews" : "/Users/dev/.muster/tmp/reviews",
      storage,
      files: 0,
      bytes: 0,
    }));
    const wrapper = mount(DocumentPane, {
      props: { ...documentPaneProps("notes.md"), origin: "review-workdir" },
    });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));
    expect(mocks.readCheckoutFile).toHaveBeenCalledWith("checkout:one", "notes.md", "review-workdir");

    await wrapper.get('[data-testid="format-file"]').trigger("click");
    await vi.waitFor(() => expect(wrapper.find('[aria-label="Unsaved changes"]').exists()).toBe(true));
    expect(mocks.readPrettierConfig).toHaveBeenCalledWith("checkout:one", "notes.md", "review-workdir");

    const editor = wrapper.get(".cm-content").element as HTMLElement;
    editor.textContent = "# Saved review";
    editor.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: "d" }));
    await flushPromises();
    await wrapper.get('button[aria-label="Save"]').trigger("click");
    await flushPromises();
    expect(mocks.writeCheckoutFile).toHaveBeenCalledWith(
      "checkout:one",
      "notes.md",
      "# Saved review",
      expect.any(String),
      "review-workdir",
    );

    await wrapper.get('[aria-label="Copy file path"]').trigger("click");
    expect(mocks.getReviewFolder).toHaveBeenCalledWith("checkout:one", "workdir");
    expect(writeText).toHaveBeenCalledWith("/checkout:one/.muster/reviews/notes.md");
    wrapper.unmount();
  });

  it("an exported Markdown preview does not request images from the checkout", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "notes.md", content: "![image](image.png)" });
    const wrapper = mount(DocumentPane, {
      props: { ...documentPaneProps("notes.md", "view"), origin: "review-default" },
    });
    await vi.waitFor(() => expect(wrapper.find('[aria-label="File contents"] article').exists()).toBe(true));

    expect(mocks.readCheckoutMarkdownImage).not.toHaveBeenCalled();
    expect(wrapper.text()).not.toContain("Some Markdown images were missing");
    wrapper.unmount();
  });

  it("keeps a Code draft visible and saves it with its original content", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const answer = 42;" });
    mocks.writeCheckoutFile.mockResolvedValue(undefined);
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/app.ts") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    const editor = wrapper.get(".cm-content").element as HTMLElement;
    editor.textContent = "const answer = 43;";
    editor.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: "3" }));
    await flushPromises();

    expect(wrapper.find('[aria-label="Unsaved changes"]').exists()).toBe(true);
    await wrapper.get('button[aria-label="Save"]').trigger("click");
    await flushPromises();
    expect(mocks.writeCheckoutFile).toHaveBeenCalledWith(
      "checkout:one",
      "src/app.ts",
      "const answer = 43;",
      "const answer = 42;",
      "checkout",
    );
    expect(wrapper.find('[aria-label="Unsaved changes"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("offers Prettier for a grammar it can parse and says why it cannot for the rest", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/main.rs", content: "fn main() {}" });
    const rust = mount(DocumentPane, { props: documentPaneProps("src/main.rs") });
    await vi.waitFor(() => expect(rust.find(".cm-content").exists()).toBe(true));
    const refused = rust.get<HTMLButtonElement>('[data-testid="format-file"]');
    expect(refused.element.disabled).toBe(true);
    expect(refused.attributes("title")).toBe("Prettier does not format Rust.");
    rust.unmount();

    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const x=1" });
    const typescript = mount(DocumentPane, { props: documentPaneProps("src/app.ts") });
    await vi.waitFor(() => expect(typescript.find(".cm-content").exists()).toBe(true));
    const offered = typescript.get<HTMLButtonElement>('[data-testid="format-file"]');
    expect(offered.element.disabled).toBe(false);
    expect(offered.text()).toBe("");
    expect(offered.attributes("aria-label")).toBe("Format with Prettier");
    expect(offered.attributes("title")).toContain("Save it or Cancel it");
    typescript.unmount();
  });

  it("formats the file into a draft that Save writes against the bytes it read", async () => {
    const source = "const   answer   =   42";
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: source });
    mocks.readPrettierConfig.mockResolvedValue({
      path: "app.ts",
      options: { overrides: [{ files: "*.ts", options: { printWidth: 120 } }] },
    });
    mocks.writeCheckoutFile.mockResolvedValue(undefined);
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/app.ts") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));
    expect(wrapper.find('[aria-label="Unsaved changes"]').exists()).toBe(false);

    await wrapper.get('[data-testid="format-file"]').trigger("click");
    await vi.waitFor(() => expect(wrapper.find('[aria-label="Unsaved changes"]').exists()).toBe(true));
    const { EditorView } = await import("@codemirror/view");
    const editor = EditorView.findFromDOM(wrapper.get(".cm-editor").element as HTMLElement)!;
    expect(editor.state.doc.toString()).toBe("const answer = 42;\n");
    expect(mocks.readPrettierConfig).toHaveBeenCalledWith("checkout:one", "src/app.ts", "checkout");
    // Formatting is a proposal: nothing reaches the disk until the reader says so.
    expect(mocks.writeCheckoutFile).not.toHaveBeenCalled();

    await wrapper.get('button[aria-label="Save"]').trigger("click");
    await flushPromises();
    expect(mocks.writeCheckoutFile).toHaveBeenCalledWith(
      "checkout:one",
      "src/app.ts",
      "const answer = 42;\n",
      source,
      "checkout",
    );
    expect(wrapper.find('[aria-label="Unsaved changes"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("throws a formatted document away on Cancel the way it throws away a typed one", async () => {
    const source = "const   answer   =   42";
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: source });
    mocks.writeCheckoutFile.mockResolvedValue(undefined);
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/app.ts") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    await wrapper.get('[data-testid="format-file"]').trigger("click");
    await vi.waitFor(() => expect(wrapper.find('[aria-label="Unsaved changes"]').exists()).toBe(true));
    await wrapper.get('button[aria-label="Cancel"]').trigger("click");
    await flushPromises();

    const { EditorView } = await import("@codemirror/view");
    const editor = EditorView.findFromDOM(wrapper.get(".cm-editor").element as HTMLElement)!;
    expect(editor.state.doc.toString()).toBe(source);
    expect(wrapper.find('[aria-label="Unsaved changes"]').exists()).toBe(false);
    expect(mocks.writeCheckoutFile).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("throws away a formatting it could not apply to what is on screen now", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const   x=1" });
    mocks.readPrettierConfig.mockResolvedValue(null);
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/app.ts") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    // The worker takes seconds on a large file, and the window stays live while it works, so the
    // reader can keep typing. Applying the answer would throw those keystrokes away.
    InlinePrettierWorker.delay = 25;
    const click = wrapper.get('[data-testid="format-file"]').trigger("click");
    const { EditorView } = await import("@codemirror/view");
    const editor = EditorView.findFromDOM(wrapper.get(".cm-editor").element as HTMLElement)!;
    editor.dispatch({ changes: { from: editor.state.doc.length, insert: "\n// typed while it worked" } });
    await click;
    await vi.waitFor(() =>
      expect(toasts.value.at(-1)?.message).toBe("The file changed while Prettier was working. Format it again."),
    );

    const text = editor.state.doc.toString();
    expect(text).toContain("typed while it worked");
    expect(text).toContain("const   x=1");
    expect(wrapper.find('[aria-label="Unsaved changes"]').exists()).toBe(true);
    expect(wrapper.get('[data-testid="format-file"]').text()).toBe("");
    wrapper.unmount();
  });

  it("throws away a formatting for a file the reader has navigated away from", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const   x=1" });
    mocks.readPrettierConfig.mockResolvedValue(null);
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/app.ts") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    // The worker takes seconds on a large file, and the window stays live while it works, so the
    // reader can open another file before it answers. The answer belongs to a document that is no
    // longer the one on screen, and applying it would put the old file's text in the new one's pane.
    InlinePrettierWorker.delay = 40;
    await wrapper.get('[data-testid="format-file"]').trigger("click");
    await wrapper.setProps({ path: "src/other.ts" });
    await vi.waitFor(() =>
      expect(toasts.value.at(-1)?.message).toBe("The file changed while Prettier was working. Format it again."),
    );

    // The pane is showing the other file, and neither document was written.
    expect(mocks.writeCheckoutFile).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it.each(["edit and undo", "language and back", "editor replacement"])(
    "discards stale formatting after %s",
    async (race) => {
      const source = "const   x=1";
      mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: source });
      let resolveConfig!: (value: null) => void;
      mocks.readPrettierConfig.mockReturnValue(
        new Promise((resolve) => {
          resolveConfig = resolve;
        }),
      );
      const wrapper = mount(DocumentPane, { props: documentPaneProps("src/app.ts") });
      await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));
      await wrapper.get('[data-testid="format-file"]').trigger("click");
      const { EditorView } = await import("@codemirror/view");
      if (race === "edit and undo") {
        const editor = EditorView.findFromDOM(wrapper.get(".cm-editor").element as HTMLElement)!;
        editor.dispatch({ changes: { from: editor.state.doc.length, insert: " " } });
        editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: source } });
      } else if (race === "language and back") {
        await pickLanguage(wrapper, "Plain text");
        await pickLanguage(wrapper, "Auto");
      } else {
        await wrapper.setProps({ mode: "view" });
        await flushPromises();
        await wrapper.setProps({ mode: "code" });
      }
      await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));
      resolveConfig(null);
      await vi.waitFor(() =>
        expect(toasts.value.at(-1)?.message).toBe("The file changed while Prettier was working. Format it again."),
      );
      const editor = EditorView.findFromDOM(wrapper.get(".cm-editor").element as HTMLElement)!;
      expect(editor.state.doc.toString()).toBe(source);
      expect(mocks.writeCheckoutFile).not.toHaveBeenCalled();
      wrapper.unmount();
    },
  );

  it("leaves the editor registering keystrokes after a failed format", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const = ;" });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/app.ts") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));
    const { EditorView } = await import("@codemirror/view");
    const editor = EditorView.findFromDOM(wrapper.get(".cm-editor").element as HTMLElement)!;

    await wrapper.get('[data-testid="format-file"]').trigger("click");
    await vi.waitFor(() => expect(toasts.value.length).toBeGreaterThan(0));

    // A failure must not leave the pane holding a flag that makes every later edit look like one of
    // its own, which is what would silently stop the draft from ever registering again.
    editor.dispatch({ changes: { from: editor.state.doc.length, insert: "x" } });
    await flushPromises();
    expect(editor.state.doc.toString()).toBe("const = ;x");
    wrapper.unmount();
  });

  it("names the place Prettier could not read the file instead of formatting it anyway", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const = ;" });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/app.ts") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    await wrapper.get('[data-testid="format-file"]').trigger("click");
    await vi.waitFor(() => expect(toasts.value.at(-1)?.message).toBe("Variable declaration expected. (1:7)"));
    expect(wrapper.find('[aria-label="Unsaved changes"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("offers View/Code only for a Markdown file, and no mode control for anything else", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "docs/readme.md", content: "# Current file" });
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps("docs/readme.md", "view"),
    });
    await flushPromises();

    expect(
      wrapper
        .get('[aria-label="Document mode"]')
        .findAll("button")
        .map((button) => button.text()),
    ).toEqual(["View", "Code"]);

    await wrapper
      .get('[aria-label="Document mode"]')
      .findAll("button")
      .find((button) => button.text() === "Code")!
      .trigger("click");
    expect(wrapper.emitted("updateMode")?.at(-1)).toEqual(["code"]);
    wrapper.unmount();
  });

  it("shows no mode control for a file that has no modes to switch", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const answer: number = 42;" });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/app.ts") });
    await flushPromises();

    expect(wrapper.find('[aria-label="Document mode"]').exists()).toBe(false);
    expect(wrapper.text()).not.toContain("Wrap");
    expect(wrapper.emitted("updateMode")).toBeUndefined();
    // A grammar can still be forced onto a file that has no modes, which is the whole point.
    expect(wrapper.get('[data-testid="language-trigger"]').text()).toBe("TypeScript");
    wrapper.unmount();
  });

  it("puts the grammar control on the same row, before the mode Markdown has", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "docs/readme.md", content: "# Current file" });
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps("docs/readme.md", "code"),
    });
    await flushPromises();

    const language = wrapper.get('[aria-label="Highlight language"]');
    const mode = wrapper.get('[aria-label="Document mode"]');
    // In Auto the button says what the extension already decided, and it is not a mode: a grammar
    // applies to a non-Markdown file too, so it cannot live inside that group.
    expect(wrapper.get('[data-testid="language-trigger"]').text()).toBe("Markdown");
    expect(language.element.compareDocumentPosition(mode.element) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // The picker is a crumb that opens a list, not a button: the same text-and-cursor control the
    // titlebar's crumbs use, and none of the surface the mode group beside it wears.
    const trigger = wrapper.get('[data-testid="language-trigger"]');
    expect(trigger.classes()).toContain("text-menu-control");
    expect(trigger.classes()).not.toContain("document-mode-button");
    expect(language.classes()).not.toContain("document-mode-control");
    // And the list behind it is the menu the titlebar opens, the same one the worktree menu is.
    expect(wrapper.find(".muster-menu").exists()).toBe(true);
    // The rows scroll, not the field that filters them.
    expect(wrapper.get('[role="listbox"]').classes()).toContain("muster-menu-scroll");
    expect(wrapper.find(".muster-menu-search").exists()).toBe(true);
    expect(wrapper.get('[role="option"]').classes()).toContain("menu-item");
    expect(mode.findAll("button").map((button) => button.text())).toEqual(["View", "Code"]);

    // A Markdown preview renders its own fences, so there is no grammar left to choose there.
    await wrapper.setProps({ mode: "view" });
    expect(wrapper.find('[data-testid="language-trigger"]').exists()).toBe(false);
    expect(wrapper.find('[aria-label="Document mode"]').exists()).toBe(true);
    wrapper.unmount();
  });

  it("forces a grammar onto a file whose extension says nothing about it", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "notes.txt", content: "def answer():\n    return 42" });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("notes.txt") });
    await flushPromises();

    // Nothing is applied yet, and the toolbar says so instead of naming a grammar that is not on.
    expect(wrapper.get('[data-testid="language-trigger"]').text()).toBe("Auto (no highlighting)");
    expect(wrapper.get('[aria-label="Source code"]').attributes("data-language")).toBe("plaintext");

    const search = wrapper.get('input[aria-label="Search highlight languages"]');
    // A search reads the label, the grammar name, or the suffix a file wears.
    await search.setValue("yml");
    expect(languageRowLabels(wrapper)).toEqual(["Auto", "Plain text", "YAML"]);
    expect(search.attributes("aria-expanded")).toBe("false");
    // The rows are chosen with the pointer, so no row claims to be the one under the keyboard.
    expect(wrapper.find(".is-active").exists()).toBe(false);

    await search.setValue("pyt");
    await pickLanguage(wrapper, "Python");
    await vi.waitFor(() =>
      expect(wrapper.get('[aria-label="Source code"]').attributes("data-language")).toBe("python"),
    );
    expect(wrapper.get('[data-testid="language-trigger"]').text()).toBe("Python");

    await search.setValue("zzz");
    // Auto and plain text are the ways back out, so a search never takes them away.
    expect(languageRowLabels(wrapper)).toEqual(["Auto", "Plain text"]);
    expect(wrapper.get('[role="listbox"]').text()).toContain("No language matches");
    wrapper.unmount();
  });

  it("reads Auto as the extension says and can be put back to plain text", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const answer: number = 42;" });
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps("src/app.ts", "code"),
    });
    const editor = () => wrapper.get('[aria-label="Source code"]').attributes("data-language");
    await vi.waitFor(() => expect(editor()).toBe("typescript"));
    await pickLanguage(wrapper, "Plain text");
    await flushPromises();
    expect(editor()).toBe("plaintext");
    expect(wrapper.get('[data-testid="language-trigger"]').text()).toBe("Plain text");

    await pickLanguage(wrapper, "Auto");
    await vi.waitFor(() => expect(editor()).toBe("typescript"));
    expect(wrapper.get('[data-testid="language-trigger"]').text()).toBe("TypeScript");
    wrapper.unmount();
  });

  it("keeps each file's grammar to itself", async () => {
    mocks.readCheckoutFile.mockImplementation(async (_checkoutId: string, path: string) => ({
      path,
      content: path.endsWith(".ts") ? "const answer = 42;" : "def answer():\n    return 42",
    }));
    const wrapper = mount(DocumentPane, { props: documentPaneProps("notes.txt") });
    await flushPromises();
    await pickLanguage(wrapper, "Python");
    const editor = () => wrapper.get('[aria-label="Source code"]').attributes("data-language");
    await vi.waitFor(() => expect(editor()).toBe("python"));

    // The next file starts from its own extension, not from the last choice.
    await wrapper.setProps({ path: "src/app.ts" });
    await flushPromises();
    expect(wrapper.get('[data-testid="language-trigger"]').text()).toBe("TypeScript");
    await vi.waitFor(() => expect(editor()).toBe("typescript"));

    // And the file that was forced keeps its grammar for as long as the window lives.
    await wrapper.setProps({ path: "notes.txt" });
    await flushPromises();
    expect(wrapper.get('[data-testid="language-trigger"]').text()).toBe("Python");
    await vi.waitFor(() => expect(editor()).toBe("python"));
    wrapper.unmount();
  });

  it("offers a way back to the terminal from the toolbar", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/example.ts", content: "const a = 1;" });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/example.ts", "code") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    const close = wrapper.get('[data-testid="close-preview"]');
    expect(close.attributes("aria-label")).toBe("Close preview");
    await close.trigger("click");

    expect(wrapper.emitted("close")).toHaveLength(1);
    wrapper.unmount();
  });

  it("puts the close button on the toolbar, past the controls that read the file", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "docs/readme.md", content: "# Muster" });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("docs/readme.md", "view") });
    await vi.waitFor(() => expect(wrapper.find(".markdown-preview").exists()).toBe(true));

    // The last control of the row, so the controls that change what this file is read as stay
    // together on its own side of the toolbar and the way out of it does not join them.
    const controls = wrapper.get(".document-toolbar").findAll("button, [role='group']");
    expect(controls.at(-1)?.attributes("data-testid")).toBe("close-preview");
    wrapper.unmount();
  });

  it("marks the lines the checkout has changed, and only those", async () => {
    mocks.readCheckoutFile.mockResolvedValue({
      path: "src/example.ts",
      content: ["const a = 1;", "const b = 2;", "const c = 3;", "const d = 4;"].join("\n"),
    });
    mocks.getGitDiff.mockResolvedValue({
      path: "src/example.ts",
      // A three-line window whose middle line is the one this change wrote.
      patch: [
        "diff --git a/src/example.ts b/src/example.ts",
        "@@ -1,3 +1,3 @@",
        " const a = 1;",
        "+const b = 2;",
        " const c = 3;",
      ].join("\n"),
      revision: "src/example.ts#1",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 5,
      hunks: [{ startLine: 0, endLine: 5, title: "@@ -1,3 +1,3 @@" }],
    });
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps(
        "src/example.ts",
        "code",
        checkout("checkout:one"),
        snapshot("checkout:one", [{ path: "src/example.ts", status: "M" }]),
      ),
    });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    await vi.waitFor(() =>
      expect(wrapper.findAll(".muster-changed-line").map((line) => line.text())).toEqual(["const b = 2;"]),
    );
    expect(wrapper.find(".muster-changed-line-marker").exists()).toBe(true);
    wrapper.unmount();
  });

  it("asks Git about nothing when the file is not one it has changed", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/example.ts", content: "const a = 1;" });
    // A clean file is not in the status at all, and asking about it would be answered with a refusal.
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/example.ts", "code") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    expect(mocks.getGitDiff).not.toHaveBeenCalled();
    expect(wrapper.find(".muster-changed-line").exists()).toBe(false);
    wrapper.unmount();
  });

  it("does not mark every line of an untracked file", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/new.ts", content: "const a = 1;\nconst b = 2;" });
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps(
        "src/new.ts",
        "code",
        checkout("checkout:one"),
        snapshot("checkout:one", [{ path: "src/new.ts", status: "??" }]),
      ),
    });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    // Every line of a new file is an added line, so the marks would cover all of it and say nothing
    // the path in the toolbar does not.
    expect(mocks.getGitDiff).not.toHaveBeenCalled();
    expect(wrapper.find(".muster-changed-line").exists()).toBe(false);
    wrapper.unmount();
  });

  it("marks the lines in the read-only view too, where there is no editor", async () => {
    mocks.readCheckoutFile.mockResolvedValue({
      path: "src/example.ts",
      content: ["const a = 1;", "const b = 2;", "const c = 3;"].join("\n"),
    });
    mocks.getGitDiff.mockResolvedValue({
      path: "src/example.ts",
      patch: [
        "diff --git a/src/example.ts b/src/example.ts",
        "@@ -2,2 +2,3 @@",
        " const b = 2;",
        "+const c = 3;",
        " const d = 4;",
      ].join("\n"),
      revision: "src/example.ts#2",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 5,
      hunks: [],
    });
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps(
        "src/example.ts",
        "view",
        checkout("checkout:one"),
        snapshot("checkout:one", [{ path: "src/example.ts", status: "M" }]),
      ),
    });
    await vi.waitFor(() => expect(wrapper.findAll(".shiki .line").length).toBeGreaterThan(0));

    await vi.waitFor(() =>
      expect(wrapper.findAll(".muster-changed-line").map((row) => row.text().replace(/^\d+/, ""))).toEqual([
        "const c = 3;",
      ]),
    );
    wrapper.unmount();
  });

  it("keeps a file readable when Git cannot say anything about it", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/example.ts", content: "const a = 1;" });
    mocks.getGitDiff.mockRejectedValue(new Error("git exploded"));
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps(
        "src/example.ts",
        "code",
        checkout("checkout:one"),
        snapshot("checkout:one", [{ path: "src/example.ts", status: "M" }]),
      ),
    });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    // The marks are an addition to a file that reads perfectly well without them, so a failure to
    // get them is not a failure to open the file.
    expect(wrapper.find(".cm-content").text()).toContain("const a = 1;");
    expect(wrapper.find(".muster-changed-line").exists()).toBe(false);
    wrapper.unmount();
  });

  it("says nothing about a path Git refuses, which is a file it has not changed", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/example.ts", content: "const a = 1;" });
    mocks.getGitDiff.mockRejectedValue(Object.assign(new Error("not a changed file"), { code: "invalid_path" }));
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps(
        "src/example.ts",
        "code",
        checkout("checkout:one"),
        snapshot("checkout:one", [{ path: "src/example.ts", status: "M" }]),
      ),
    });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));
    await flushPromises();

    // The one refusal that is an answer rather than a fault, so it is not worth a toast either.
    expect(toasts.value).toHaveLength(0);
    expect(wrapper.find(".muster-changed-line").exists()).toBe(false);
    wrapper.unmount();
  });

  it("highlights the read view with Shiki and displays line numbers", async () => {
    mocks.readCheckoutFile.mockResolvedValue({
      path: "src/example.ts",
      content: "const answer: number = 42;\nreturn answer;",
    });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/example.ts", "view") });
    await vi.waitFor(() => expect(wrapper.findAll(".shiki .line")).toHaveLength(2));
    const source = wrapper.get('[aria-label="Source code"]');
    expect(source.findAll(".shiki .line").map((line) => line.text())).toEqual([
      "const answer: number = 42;",
      "return answer;",
    ]);
    expect(source.findAll(".shiki .line > span").length).toBeGreaterThan(0);
    expect(source.findAll(".source-line-number").map((number) => number.text())).toEqual(["1", "2"]);
    wrapper.unmount();
  });

  it("gives the Code view to the editor, with its lines and its gutter", async () => {
    mocks.readCheckoutFile.mockResolvedValue({
      path: "src/example.ts",
      content: "const answer: number = 42;\nreturn answer;",
    });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("src/example.ts", "code") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));
    const source = wrapper.get(".code-editor-host");
    // One renderer for the Code view: Shiki draws nothing behind the editor.
    expect(wrapper.find(".shiki").exists()).toBe(false);
    expect(wrapper.findAll(".cm-line").length).toBeGreaterThan(0);
    expect(source.get(".cm-content").text()).toContain("const answer: number = 42;");
    expect(source.get(".cm-lineNumbers").text()).toContain("1");
    wrapper.unmount();
  });

  it("draws the editor at the size and the ligatures the settings ask for", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/example.ts", content: "const a = 1;" });
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("src/example.ts", "code"),
        editorSettings: {
          fontSize: 17,
          ligatures: false,
          cursorBlink: true,
          indentation: { useSpaces: true, size: 2 },
        },
      },
    });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    // The host is a template ref that does not exist until a Code view is on the pane, so a
    // preference that was already loaded when the component was created has to be applied again
    // once the host is there. Reading it off the host rather than the editor is the whole point:
    // CodeMirror builds its theme once and nothing reconfigures it for a preference.
    const host = wrapper.get<HTMLElement>(".code-editor-host").element;
    expect(host.style.getPropertyValue("--muster-editor-font-size")).toBe("17px");
    expect(host.style.getPropertyValue("--muster-editor-ligatures")).toBe("none");

    await wrapper.setProps({
      editorSettings: { fontSize: 20, ligatures: true, cursorBlink: true, indentation: { useSpaces: true, size: 2 } },
    });
    expect(host.style.getPropertyValue("--muster-editor-font-size")).toBe("20px");
    expect(host.style.getPropertyValue("--muster-editor-ligatures")).toBe("normal");
    wrapper.unmount();
  });

  it("leaves the caret blinking or still without touching the editor", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/example.ts", content: "const a = 1;" });
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("src/example.ts", "code"),
        editorSettings: {
          fontSize: 13,
          ligatures: true,
          cursorBlink: false,
          indentation: { useSpaces: true, size: 2 },
        },
      },
    });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    // CodeMirror blinks the cursor layer itself, so this is a declaration on the host and not a
    // setting reconfigured into the editor: a preference that rebuilt the editor would take the
    // document, the undo history and the caret with it to stop a light blinking.
    const host = wrapper.get<HTMLElement>(".code-editor-host").element;
    expect(host.style.getPropertyValue("--muster-editor-cursor-blink")).toBe("paused");
    expect(wrapper.find(".cm-content").text()).toContain("const a = 1;");

    await wrapper.setProps({
      editorSettings: { fontSize: 13, ligatures: true, cursorBlink: true, indentation: { useSpaces: true, size: 2 } },
    });
    expect(host.style.getPropertyValue("--muster-editor-cursor-blink")).toBe("running");
    // The same editor, still: `cm-content` is not rebuilt, so the text it holds is the one above.
    expect(wrapper.find(".cm-content").text()).toContain("const a = 1;");
    wrapper.unmount();
  });

  it("uses compact source rendering above the line bound", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "logs/output.txt", content: Array(5001).fill("line").join("\n") });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("logs/output.txt", "view") });
    await flushPromises();
    const source = wrapper.get('[aria-label="Source code"]');
    expect(source.findAll(".source-line-number")).toHaveLength(1);
    expect(source.get(".source-line-number").text().split("\n")).toHaveLength(5001);
    wrapper.unmount();
  });

  it("ignores an older file read after switching checkout and document", async () => {
    let resolveOld!: (value: { path: string; content: string }) => void;
    mocks.readCheckoutFile.mockImplementation((_checkoutId: string, path: string) => {
      if (path === "old.ts") return new Promise((resolve) => (resolveOld = resolve));
      return Promise.resolve({ path, content: "new checkout content" });
    });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("old.ts") });
    await wrapper.setProps({
      checkout: checkout("checkout:new"),
      path: "new.ts",
      gitSnapshot: snapshot("checkout:new"),
    });
    await flushPromises();
    resolveOld({ path: "old.ts", content: "stale old checkout content" });
    await flushPromises();
    expect(wrapper.text()).toContain("new checkout content");
    expect(wrapper.text()).not.toContain("stale old checkout content");
    wrapper.unmount();
  });

  it("hides the previous file while the next one loads, and keeps it once loaded", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "a.ts", content: "const fileA = true;" });
    let resolveB!: (value: { path: string; content: string }) => void;
    mocks.readCheckoutFile.mockImplementation((_checkoutId: string, path: string) =>
      path === "a.ts"
        ? Promise.resolve({ path, content: "const fileA = true;" })
        : new Promise((resolve) => {
            resolveB = resolve;
          }),
    );
    const wrapper = mount(DocumentPane, { props: documentPaneProps("a.ts") });
    await vi.waitFor(() => expect(wrapper.text()).toContain("const fileA = true;"));

    await wrapper.setProps({ path: "b.ts" });
    await vi.waitFor(() => expect(wrapper.get('[role="status"]').text()).toBe("Loading file…"));
    // E.3: the file that was open is not what stays on screen.
    expect(wrapper.text()).not.toContain("const fileA = true;");

    resolveB({ path: "b.ts", content: "const fileB = true;" });
    await vi.waitFor(() => expect(wrapper.text()).toContain("const fileB = true;"));
    expect(wrapper.text()).not.toContain("const fileA = true;");
    wrapper.unmount();
  });

  it("refreshes a changed file from file activity alone while keeping its reading position", async () => {
    mocks.readCheckoutFile
      .mockResolvedValueOnce({ path: "src/app.ts", content: "const first = true;" })
      .mockResolvedValueOnce({ path: "src/app.ts", content: "const second = true;" });
    const currentCheckout = checkout("checkout:one");
    const gitSnapshot = snapshot(currentCheckout.id);
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("src/app.ts", "code", currentCheckout, gitSnapshot),
        refreshRevision: 0,
      },
    });
    await flushPromises();
    const viewport = wrapper.get('[aria-label="File contents"]');
    (viewport.element as HTMLElement).scrollTop = 120;
    (viewport.element as HTMLElement).scrollLeft = 40;
    await viewport.trigger("scroll");

    // App.vue maps checkout-file-activity to this revision; no status event accompanies it.
    await wrapper.setProps({ refreshRevision: 1 });
    await flushPromises();
    expect(gitSnapshot.statusEventRevision).toBe(0);
    expect(mocks.readCheckoutFile).toHaveBeenCalledTimes(2);
    expect(wrapper.text()).toContain("const second = true;");
    expect((wrapper.get('[aria-label="File contents"]').element as HTMLElement).scrollTop).toBe(120);
    expect((wrapper.get('[aria-label="File contents"]').element as HTMLElement).scrollLeft).toBe(40);
    wrapper.unmount();
  });

  it("fills the box it is given, and says so with a height rather than with a position", async () => {
    // The panel hands the document a height and the document keeps its own position, because it is
    // the box the overlay scrollbars' rails are absolutely positioned against. Tailwind emits
    // `.absolute` before `.relative`, so a panel that also said `absolute` would be overruled by
    // this pane's own `relative`, `inset-0` would stop giving it a height, and the pane's scroll
    // container would have nothing to scroll in: the document drawn at full length under a panel
    // that clips it, with the wheel turning nothing and no bar to say where it had got to.
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const value = 1;" });
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps("src/app.ts", "code", checkout("checkout:one")),
      attrs: { class: "h-full" },
    });
    await flushPromises();
    const root = wrapper.get("main").classes();
    expect(root).toContain("h-full");
    expect(root).toContain("relative");
    expect(root).not.toContain("absolute");
    // And the scroll container inside it is what a wheel turns, which is the chain the height is for.
    expect(wrapper.get('[aria-label="File contents"]').classes()).toContain("overflow-auto");
    wrapper.unmount();
  });

  it("refreshes a figure the document references when its own text did not change", async () => {
    mocks.readCheckoutFile.mockResolvedValue({
      path: "docs/readme.md",
      content: "![preview](images/pic.png)\n\n# Muster",
    });
    mocks.readCheckoutMarkdownImage.mockResolvedValue({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgo=",
      sizeBytes: 8,
    });
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("docs/readme.md", "view"),
        refreshRevision: 0,
        refreshPaths: [],
      },
    });
    await vi.waitFor(() =>
      expect(wrapper.get(".markdown-preview img").attributes("src")).toBe("data:image/png;base64,iVBORw0KGgo="),
    );

    // Another process replaced the figure and left the document exactly as it was, which is the
    // whole of the staleness: the bytes match, and what the page draws no longer does.
    mocks.readCheckoutMarkdownImage.mockResolvedValue({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgp=",
      sizeBytes: 8,
    });
    await wrapper.setProps({ refreshRevision: 1, refreshPaths: ["docs/images/pic.png"] });
    await vi.waitFor(() =>
      expect(wrapper.get(".markdown-preview img").attributes("src")).toBe("data:image/png;base64,iVBORw0KGgp="),
    );
    expect(mocks.readCheckoutFile).toHaveBeenCalledTimes(2);
    // The command is handed the URL as the document spells it and resolves it itself: the path the
    // activity names is the resolved one, and the pane never reads a file by it.
    expect(mocks.readCheckoutMarkdownImage).toHaveBeenLastCalledWith(
      "checkout:one",
      "docs/readme.md",
      "images/pic.png",
    );
    wrapper.unmount();
  });

  it("leaves a preview and its figures alone when the activity moved something else", async () => {
    mocks.readCheckoutFile.mockResolvedValue({
      path: "docs/readme.md",
      content: "![preview](images/pic.png)\n\n# Muster",
    });
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("docs/readme.md", "view"),
        refreshRevision: 0,
        refreshPaths: [],
      },
    });
    await vi.waitFor(() => expect(wrapper.get(".markdown-preview img").attributes("src")).toBeTruthy());
    const figure = wrapper.get(".markdown-preview img").element;
    const page = wrapper.get(".markdown-preview").html();

    await wrapper.setProps({ refreshRevision: 1, refreshPaths: ["src/app.ts"] });
    await flushPromises();

    // Nothing that draws the page was asked again, and nothing was handed back to `v-html`, so no
    // renderer ran over it either.
    expect(mocks.readCheckoutMarkdownImage).toHaveBeenCalledTimes(1);
    expect(wrapper.get(".markdown-preview img").element).toBe(figure);
    expect(wrapper.get(".markdown-preview").html()).toBe(page);
    wrapper.unmount();
  });

  it("asks every figure in use again when a batch could not say what it moved", async () => {
    mocks.readCheckoutFile.mockResolvedValue({
      path: "docs/readme.md",
      content: "![one](one.png)\n\n![two](two.png)",
    });
    mocks.readCheckoutMarkdownImage.mockResolvedValue({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgo=",
      sizeBytes: 8,
    });
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("docs/readme.md", "view"),
        refreshRevision: 0,
        refreshPaths: [],
      },
    });
    await vi.waitFor(() => expect(mocks.readCheckoutMarkdownImage).toHaveBeenCalledTimes(2));

    // No paths at all is a batch past the watcher's own budget, which is not a checkout nothing
    // touched: every reference in use is read again, and no more than those.
    mocks.readCheckoutMarkdownImage.mockResolvedValue({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgp=",
      sizeBytes: 8,
    });
    await wrapper.setProps({ refreshRevision: 1, refreshPaths: [] });
    await vi.waitFor(() => expect(mocks.readCheckoutMarkdownImage).toHaveBeenCalledTimes(4));
    expect(wrapper.get(".markdown-preview img").attributes("src")).toBe("data:image/png;base64,iVBORw0KGgp=");
    wrapper.unmount();
  });

  it("refreshes a figure the superseded read was triggered by, not only the one that replaced it", async () => {
    mocks.readCheckoutFile.mockResolvedValue({
      path: "docs/readme.md",
      content: "![preview](images/pic.png)\n\n# Muster",
    });
    mocks.readCheckoutMarkdownImage.mockResolvedValue({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgo=",
      sizeBytes: 8,
    });
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("docs/readme.md", "view"),
        refreshRevision: 0,
        refreshPaths: [],
      },
    });
    await vi.waitFor(() =>
      expect(wrapper.get(".markdown-preview img").attributes("src")).toBe("data:image/png;base64,iVBORw0KGgo="),
    );

    // The read that named the figure is still out when the activity for another file lands, and the
    // read that lands is the one that gets to name what brought it here. What the first one was
    // told about is only its own paths, so it has to have handed them over before it was sent.
    let resolveRead!: (value: { path: string; content: string }) => void;
    mocks.readCheckoutFile.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveRead = resolve;
        }),
    );
    await wrapper.setProps({ refreshRevision: 1, refreshPaths: ["docs/images/pic.png"] });

    mocks.readCheckoutMarkdownImage.mockResolvedValue({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgp=",
      sizeBytes: 8,
    });
    await wrapper.setProps({ refreshRevision: 2, refreshPaths: ["src/app.ts"] });
    resolveRead({ path: "docs/readme.md", content: "![preview](images/pic.png)\n\n# Muster" });

    // Both batches are spent as one, so the figure the superseded read named is asked again and the
    // batch that named nothing the page draws reads nothing itself.
    await vi.waitFor(() =>
      expect(wrapper.get(".markdown-preview img").attributes("src")).toBe("data:image/png;base64,iVBORw0KGgp="),
    );
    expect(mocks.readCheckoutMarkdownImage).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });

  it("asks every figure in use again when the batch that could not say what it moved is the superseded read", async () => {
    mocks.readCheckoutFile.mockResolvedValue({
      path: "docs/readme.md",
      content: "![one](one.png)\n\n![two](two.png)",
    });
    mocks.readCheckoutMarkdownImage.mockResolvedValue({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgo=",
      sizeBytes: 8,
    });
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("docs/readme.md", "view"),
        refreshRevision: 0,
        refreshPaths: [],
      },
    });
    await vi.waitFor(() => expect(mocks.readCheckoutMarkdownImage).toHaveBeenCalledTimes(2));

    // A batch that could not say what it moved is not a batch that moved nothing, and the read it
    // triggered is not the read that lands, so it has to be holding the figures' share of it too.
    let resolveRead!: (value: { path: string; content: string }) => void;
    mocks.readCheckoutFile.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveRead = resolve;
        }),
    );
    await wrapper.setProps({ refreshRevision: 1, refreshPaths: [] });

    mocks.readCheckoutMarkdownImage.mockResolvedValue({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgp=",
      sizeBytes: 8,
    });
    await wrapper.setProps({ refreshRevision: 2, refreshPaths: ["src/app.ts"] });
    resolveRead({ path: "docs/readme.md", content: "![one](one.png)\n\n![two](two.png)" });

    // Naming fewer paths than the batch before it does not undo it: both references in use are read
    // again, and no more than those.
    await vi.waitFor(() => expect(mocks.readCheckoutMarkdownImage).toHaveBeenCalledTimes(4));
    expect(wrapper.get(".markdown-preview img").attributes("src")).toBe("data:image/png;base64,iVBORw0KGgp=");
    wrapper.unmount();
  });

  it("warns and drops a figure whose file is gone, and takes the warning back when it returns", async () => {
    mocks.readCheckoutFile.mockResolvedValue({
      path: "docs/readme.md",
      content: "![preview](images/pic.png)\n\n# Muster",
    });
    mocks.readCheckoutMarkdownImage.mockResolvedValue({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgo=",
      sizeBytes: 8,
    });
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("docs/readme.md", "view"),
        refreshRevision: 0,
        refreshPaths: [],
      },
    });
    await vi.waitFor(() => expect(wrapper.get(".markdown-preview img").attributes("src")).toBeTruthy());

    mocks.readCheckoutMarkdownImage.mockRejectedValue(new Error("could not inspect image"));
    await wrapper.setProps({ refreshRevision: 1, refreshPaths: ["docs/images/pic.png"] });
    await vi.waitFor(() => expect(wrapper.text()).toContain("Some Markdown images were missing"));
    expect(wrapper.get(".markdown-preview img").attributes("src")).toBeUndefined();
    expect(wrapper.text()).not.toContain("Rendering preview");

    mocks.readCheckoutMarkdownImage.mockResolvedValue({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgp=",
      sizeBytes: 8,
    });
    await wrapper.setProps({ refreshRevision: 2, refreshPaths: ["docs/images/pic.png"] });
    await vi.waitFor(() =>
      expect(wrapper.get(".markdown-preview img").attributes("src")).toBe("data:image/png;base64,iVBORw0KGgp="),
    );
    expect(wrapper.text()).not.toContain("Some Markdown images were missing");
    wrapper.unmount();
  });

  it("publishes no figure that a newer document has already replaced", async () => {
    mocks.readCheckoutFile.mockResolvedValue({
      path: "docs/readme.md",
      content: "![preview](images/pic.png)\n\n# Muster",
    });
    mocks.readCheckoutMarkdownImage.mockResolvedValueOnce({
      mimeType: "image/png",
      dataBase64: "iVBORw0KGgo=",
      sizeBytes: 8,
    });
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("docs/readme.md", "view"),
        refreshRevision: 0,
        refreshPaths: [],
      },
    });
    await vi.waitFor(() => expect(mocks.readCheckoutMarkdownImage).toHaveBeenCalledTimes(1));

    let reImage!: (value: { mimeType: string; dataBase64: string; sizeBytes: number }) => void;
    mocks.readCheckoutMarkdownImage.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          reImage = resolve;
        }),
    );
    await wrapper.setProps({ refreshRevision: 1, refreshPaths: ["docs/images/pic.png"] });
    await vi.waitFor(() => expect(mocks.readCheckoutMarkdownImage).toHaveBeenCalledTimes(2));

    // The reader moves on while that read is still out.
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const moved = true;" });
    await wrapper.setProps({ path: "src/app.ts", mode: "code" });
    await vi.waitFor(() => expect(wrapper.text()).toContain("const moved = true;"));

    reImage({ mimeType: "image/png", dataBase64: "b3RocmU=", sizeBytes: 8 });
    await flushPromises();
    // It answers for a page nobody is on, so it is not put up anywhere.
    expect(wrapper.html()).not.toContain("b3RocmU=");
    wrapper.unmount();
  });

  it("reads a file once when status and file activity arrive together", async () => {
    const currentCheckout = checkout("checkout:one");
    const gitSnapshot = snapshot(currentCheckout.id);
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const before = true;" });
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("src/app.ts", "code", currentCheckout, gitSnapshot),
        refreshRevision: 0,
      },
    });
    await flushPromises();

    mocks.readCheckoutFile.mockClear();
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const after = true;" });
    gitSnapshot.statusEventCheckoutId = currentCheckout.id;
    gitSnapshot.statusEventRevision += 1;
    await wrapper.setProps({ refreshRevision: 1 });
    await flushPromises();

    expect(mocks.readCheckoutFile).toHaveBeenCalledOnce();
    expect(wrapper.text()).toContain("const after = true;");
    wrapper.unmount();
  });

  it("keeps a file it cannot read on screen while the checkout reads it again", async () => {
    // A reason it cannot be read is not something to take away and hand back during a refresh.
    mocks.readCheckoutFile.mockRejectedValue({ code: "binary_file", message: "not utf-8" });
    const wrapper = mount(DocumentPane, { props: documentPaneProps("assets/unsupported.bin") });
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toBe("This file is binary or is not valid UTF-8.");

    let reRead!: (value: { path: string; content: string }) => void;
    mocks.readCheckoutFile.mockImplementation(
      () =>
        new Promise((resolve) => {
          reRead = resolve;
        }),
    );
    await wrapper.setProps({ refreshRevision: 1 });
    expect(wrapper.get('[role="alert"]').text()).toBe("This file is binary or is not valid UTF-8.");
    expect(wrapper.text()).not.toContain("Loading file");

    reRead({ path: "assets/unsupported.bin", content: "readable now" });
    await flushPromises();
    expect(wrapper.text()).toContain("readable now");
    expect(wrapper.find('[role="alert"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("gives the next file a Code view of its own after one that could not be read", async () => {
    // A reason a file cannot be read is the whole panel for a moment, so the Code view it took
    // down went with it. The editor that lived in it must not survive: the next file that opens
    // would find one still naming itself, reuse it instead of building its own, and show nothing.
    mocks.readCheckoutFile.mockImplementation(async (_checkoutId: string, path: string) => {
      if (path === "assets/unsupported.bin") throw { code: "binary_file", message: "not utf-8" };
      return { path, content: "node_modules/\n" };
    });
    const wrapper = mount(DocumentPane, { props: documentPaneProps(".gitignore") });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));

    await wrapper.setProps({ path: "assets/unsupported.bin" });
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toBe("This file is binary or is not valid UTF-8.");
    expect(wrapper.find(".cm-editor").exists()).toBe(false);

    await wrapper.setProps({ path: ".gitignore" });
    await vi.waitFor(() => expect(wrapper.find(".cm-content").exists()).toBe(true));
    expect(wrapper.get(".cm-content").text()).toBe("node_modules/");
    wrapper.unmount();
  });

  it("leaves an unchanged document exactly where the reader left it", async () => {
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const same = true;" });
    const wrapper = mount(DocumentPane, {
      props: { ...documentPaneProps("src/app.ts"), refreshRevision: 0 },
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("const same = true;"));
    const source = wrapper.get('[aria-label="Source code"]').element;
    const viewport = wrapper.get('[aria-label="File contents"]');
    (viewport.element as HTMLElement).scrollTop = 96;
    await viewport.trigger("scroll");

    await wrapper.setProps({ refreshRevision: 1 });
    await flushPromises();
    expect(mocks.readCheckoutFile).toHaveBeenCalledTimes(2);
    // The same node, so nothing was taken down and built again, and the offset is still the
    // reader's: the bytes were the ones already on screen.
    expect(wrapper.get('[aria-label="Source code"]').element).toBe(source);
    expect(wrapper.text()).not.toContain("Highlighting source");
    expect((wrapper.get('[aria-label="File contents"]').element as HTMLElement).scrollTop).toBe(96);
    wrapper.unmount();
  });

  it("keeps the rendered Markdown page on screen while the file is read again", async () => {
    mocks.readCheckoutFile.mockResolvedValueOnce({
      path: "docs/readme.md",
      content: "![pic](pic.png)\n\n# First title",
    });
    const wrapper = mount(DocumentPane, {
      props: { ...documentPaneProps("docs/readme.md", "view"), refreshRevision: 0 },
    });
    await vi.waitFor(() => expect(wrapper.get(".markdown-preview").text()).toContain("First title"));

    let reRead!: (value: { path: string; content: string }) => void;
    let reImage!: (value: { mimeType: string; dataBase64: string; sizeBytes: number }) => void;
    mocks.readCheckoutFile.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          reRead = resolve;
        }),
    );
    mocks.readCheckoutMarkdownImage.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          reImage = resolve;
        }),
    );
    await wrapper.setProps({ refreshRevision: 1 });
    reRead({ path: "docs/readme.md", content: "![pic](pic.png)\n\n# Second title" });
    // The new page is being put together, image and all, so this is the window in which the one
    // the reader is on has to still be the one on screen.
    await vi.waitFor(() => expect(mocks.readCheckoutMarkdownImage).toHaveBeenCalledTimes(2));
    expect(wrapper.get(".markdown-preview").text()).toContain("First title");
    expect(wrapper.text()).not.toContain("Rendering preview");

    reImage({ mimeType: "image/png", dataBase64: "iVBORw0KGgo=", sizeBytes: 8 });
    await vi.waitFor(() => expect(wrapper.get(".markdown-preview").text()).toContain("Second title"));
    wrapper.unmount();
  });

  it("restores the Markdown reading position after relative images finish decoding", async () => {
    const descriptor = Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, "decode");
    const decode = vi.fn(function (this: HTMLImageElement) {
      const viewport = this.closest(".document-pane")?.querySelector('[aria-label="File contents"]') as HTMLElement;
      viewport.scrollTop = 0;
      return Promise.resolve();
    });
    Object.defineProperty(HTMLImageElement.prototype, "decode", { configurable: true, value: decode });
    mocks.readCheckoutFile.mockResolvedValue({ path: "docs/readme.md", content: "![preview](image.png)" });
    const wrapper = mount(DocumentPane, {
      props: {
        ...documentPaneProps("docs/readme.md", "view"),
        readingPosition: { top: 240, left: 12 },
      },
    });
    try {
      await vi.waitFor(() => expect(decode).toHaveBeenCalledOnce());
      expect((wrapper.get('[aria-label="File contents"]').element as HTMLElement).scrollTop).toBe(240);
      expect((wrapper.get('[aria-label="File contents"]').element as HTMLElement).scrollLeft).toBe(12);
    } finally {
      wrapper.unmount();
      if (descriptor) Object.defineProperty(HTMLImageElement.prototype, "decode", descriptor);
      else delete (HTMLImageElement.prototype as { decode?: () => Promise<void> }).decode;
    }
  });

  it("does not read a file that is gone from the working tree", async () => {
    const gitSnapshot = snapshot("checkout:deleted", [{ path: "gone.ts", status: "D" }]);
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps("gone.ts", "code", checkout("checkout:deleted"), gitSnapshot),
    });
    await flushPromises();
    expect(mocks.readCheckoutFile).not.toHaveBeenCalled();
    expect(wrapper.text()).toContain("This file was deleted");
    wrapper.unmount();
  });

  it("keeps the current file readable after its change is removed from Git status", async () => {
    const checkoutId = "checkout:cleaned";
    const gitSnapshot = snapshot(checkoutId, [{ path: "src/app.ts", status: "M" }]);
    mocks.readCheckoutFile.mockResolvedValue({ path: "src/app.ts", content: "const stillHere = true;" });
    const wrapper = mount(DocumentPane, {
      props: documentPaneProps("src/app.ts", "code", checkout(checkoutId), gitSnapshot),
    });
    await flushPromises();
    expect(wrapper.text()).toContain("const stillHere = true;");

    gitSnapshot.status!.files = [];
    gitSnapshot.statusRevision += 1;
    await flushPromises();
    expect(wrapper.text()).toContain("const stillHere = true;");
    expect(wrapper.text()).not.toContain("current file is unavailable");
    wrapper.unmount();
  });

  it("shows the whole change set as one diff per changed file", async () => {
    const checkoutId = "checkout:all-changes";
    const gitSnapshot = snapshot(checkoutId, [
      { path: "src/app.ts", status: "M", additions: 3, deletions: 1 },
      { path: "README.md", status: "A" },
    ]);
    mocks.getGitDiff.mockImplementation(async (_checkoutId: string, path: string) => ({
      path,
      patch: `diff --git a/${path} b/${path}\n@@ -1 +1 @@\n-old\n+new\n`,
      revision: "path#1",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -1 +1 @@" }],
    }));
    const wrapper = mount(FileDiff, {
      props: {
        checkout: checkout(checkoutId),
        gitSnapshot,
        review: reviewApi(),
        path: null,
        scrollTop: 0,
      },
    });
    await flushPromises();

    // Nothing is diffed until a file is opened: the backend pages one path at a time.
    expect(wrapper.get("header").text()).toContain("All changes");
    expect(wrapper.get("header").text()).toContain("feature");
    const headers = wrapper.findAll(".diff-file-header");
    expect(headers.map((header) => header.text())).toEqual(["Msrc/app.ts+3-1", "AREADME.md"]);
    expect(mocks.getGitDiff).not.toHaveBeenCalled();

    await headers[0]!.trigger("click");
    await vi.waitFor(() => expect(mocks.getGitDiff).toHaveBeenCalledWith(checkoutId, "src/app.ts"));
    expect(wrapper.findAll('[data-testid="diff-view"]')).toHaveLength(1);
    expect(mocks.getGitDiff).toHaveBeenCalledTimes(1);

    // A note made in a file of the change set belongs to that file, so the stack comments.
    const review = reviewApi();
    review.notes = [reviewNote({ id: "note:1", path: "src/app.ts", lineStart: 1 })];
    await wrapper.setProps({ review });
    await headers[1]!.trigger("click");
    await vi.waitFor(() => expect(mocks.getGitDiff).toHaveBeenCalledWith(checkoutId, "README.md"));
    expect(wrapper.text()).toContain("revisit this calculation");
    wrapper.unmount();
  });

  it("says the change set is loading, failing, or empty from the Git status", async () => {
    const checkoutId = "checkout:changes-status";
    const loading = snapshot(checkoutId);
    loading.statusState = "loading";
    const wrapper = mount(FileDiff, {
      props: {
        checkout: checkout(checkoutId),
        gitSnapshot: loading,
        review: reviewApi(),
        path: null,
        scrollTop: 0,
      },
    });
    await flushPromises();
    expect(wrapper.get('[role="status"]').text()).toBe("Loading changes…");

    const failing = snapshot(checkoutId);
    failing.statusState = "error";
    failing.changesStatusError = "could not read the index";
    await wrapper.setProps({ gitSnapshot: failing });
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toBe("could not read the index");

    await wrapper.setProps({ gitSnapshot: snapshot(checkoutId) });
    await flushPromises();
    expect(wrapper.get('[role="status"]').text()).toBe("No changed files.");
    wrapper.unmount();
  });

  it("draws a diff that cannot be read in the panel, where it does not expire", async () => {
    const checkoutId = "checkout:diff-error";
    mocks.getGitDiff.mockRejectedValueOnce(new Error("git is not available"));
    const wrapper = mount(FileDiff, {
      props: {
        checkout: checkout(checkoutId),
        gitSnapshot: snapshot(checkoutId, [{ path: "src/app.ts", status: "M" }]),
        review: reviewApi(),
        path: "src/app.ts",
        scrollTop: 0,
      },
    });
    await flushPromises();

    // Nothing else would be on screen, so the reason is drawn rather than left to a toast.
    expect(wrapper.get('[role="alert"]').text()).toBe("git is not available");
    expect(toasts.value).toHaveLength(0);
    wrapper.unmount();
  });

  it("pages a large diff lazily", async () => {
    const checkoutId = "checkout:large-diff";
    const gitSnapshot = snapshot(checkoutId, [{ path: "large.txt", status: "M" }]);
    mocks.getGitDiff.mockResolvedValue({
      path: "large.txt",
      patch: "",
      revision: "large.txt#1",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: 2,
      hunks: [{ startLine: 0, endLine: 2, title: "@@ -1 +1 @@" }],
    });
    mocks.getGitDiffPage.mockResolvedValue({
      path: "large.txt",
      startLine: 0,
      totalLines: 2,
      lines: [
        { index: 0, kind: "hunk", text: "@@ -1 +1 @@", oldLineNumber: null, newLineNumber: null },
        { index: 1, kind: "added", text: "+new line", oldLineNumber: null, newLineNumber: 1 },
      ],
    });
    const wrapper = mount(FileDiff, {
      props: {
        checkout: checkout(checkoutId),
        gitSnapshot,
        review: reviewApi(),
        path: "large.txt",
        scrollTop: 0,
      },
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("+new line"));
    expect(mocks.getGitDiffPage).toHaveBeenCalledWith(checkoutId, "large.txt", 0, 32);
    wrapper.unmount();
  });

  it("keeps a large diff's pages across a status refresh that moved no line", async () => {
    const checkoutId = "checkout:large-diff-unchanged";
    const gitSnapshot = snapshot(checkoutId, [{ path: "large.txt", status: "M" }]);
    mocks.getGitDiff.mockResolvedValue({
      path: "large.txt",
      patch: "",
      revision: "large.txt#2",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: 2,
      hunks: [{ startLine: 0, endLine: 2, title: "@@ -1 +1 @@" }],
    });
    mocks.getGitDiffPage.mockResolvedValue({
      path: "large.txt",
      startLine: 0,
      totalLines: 2,
      lines: [
        { index: 0, kind: "hunk", text: "@@ -1 +1 @@", oldLineNumber: null, newLineNumber: null },
        { index: 1, kind: "added", text: "+new line", oldLineNumber: null, newLineNumber: 1 },
      ],
    });
    const wrapper = mount(FileDiff, {
      props: { checkout: checkout(checkoutId), gitSnapshot, review: reviewApi(), path: "large.txt", scrollTop: 0 },
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("+new line"));

    // The agent writing any other file in the workdir moves statusRevision with this one on
    // screen. The patch is the diff, so an equal patch means the pages already fetched are
    // still the pages git has, and dropping them is what put a large diff in a permanent
    // "Loading diff page" cycle: each refresh blanked the window and the next arrived before
    // the refill had landed. The call count is the assertion that carries it, since the
    // refetch lands fast enough to go unnoticed; putting the reset back at the top of
    // loadDiff, where it ran before the patch was known, fails this on the first bump.
    gitSnapshot.statusRevision += 1;
    await vi.waitFor(() => expect(mocks.getGitDiff).toHaveBeenCalledTimes(2));
    expect(wrapper.text()).toContain("+new line");
    expect(wrapper.text()).not.toContain("Loading diff page");
    expect(mocks.getGitDiffPage).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it("restores a diff position independently and reports later scrolling", async () => {
    const checkoutId = "checkout:diff-position";
    const wrapper = mount(FileDiff, {
      props: {
        checkout: checkout(checkoutId),
        gitSnapshot: snapshot(checkoutId, [{ path: "src/app.ts", status: "M" }]),
        review: reviewApi(),
        path: "src/app.ts",
        scrollTop: 72,
      },
    });
    await flushPromises();
    const viewport = wrapper.get('[aria-label="Diff contents"]');
    expect((viewport.element as HTMLElement).scrollTop).toBe(72);
    (viewport.element as HTMLElement).scrollTop = 144;
    await viewport.trigger("scroll");
    expect(wrapper.emitted("scrollPositionChanged")).toEqual([[144]]);
    wrapper.unmount();
  });

  it("feeds saved notes to the hunk renderer so they appear under their line", async () => {
    const checkoutId = "checkout:hunk-notes";
    const gitSnapshot = snapshot(checkoutId, [{ path: "src/app.ts", status: "M" }]);
    const note = reviewNote({ id: "note:1", lineStart: 1 });
    const review = reviewApi();
    review.notes = [note, reviewNote({ id: "note:2", path: "src/other.ts", lineStart: 5 })];
    const wrapper = mount(FileDiff, {
      props: { checkout: checkout(checkoutId), gitSnapshot, review, path: "src/app.ts", scrollTop: 0 },
    });
    await flushPromises();

    const view = wrapper.get('[data-testid="diff-view"]');
    expect(view.text()).toContain("true");
    expect(view.text()).toContain("1");
    expect(wrapper.find('[data-testid="diff-view"]').exists()).toBe(true);
    wrapper.unmount();
  });

  it("saves a note from the hunk widget with the code of the commented line", async () => {
    const checkoutId = "checkout:hunk-widget";
    const gitSnapshot = snapshot(checkoutId, [{ path: "src/app.ts", status: "M" }]);
    const review = reviewApi();
    const wrapper = mount(FileDiff, {
      props: { checkout: checkout(checkoutId), gitSnapshot, review, path: "src/app.ts", scrollTop: 0 },
    });
    await flushPromises();

    const form = wrapper.get('form[aria-label="New review note"]');
    await form.get('textarea[aria-label="Review note"]').setValue("guard the user");
    await form.trigger("submit");
    await flushPromises();

    expect(review.addNote).toHaveBeenCalledWith({
      path: "src/app.ts",
      side: "new",
      lineStart: 1,
      content: "guard the user",
      code: "new",
    });
    wrapper.unmount();
  });

  it("adds an inline review note to a virtualized diff row with the captured code", async () => {
    const checkoutId = "checkout:large-note";
    const gitSnapshot = snapshot(checkoutId, [{ path: "large.txt", status: "M" }]);
    mocks.getGitDiff.mockResolvedValue({
      path: "large.txt",
      patch: "@@ -0,0 +1,2 @@\n+first line\n+second line\n",
      revision: "large.txt#3",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -0,0 +1,2 @@" }],
    });
    mocks.getGitDiffPage.mockResolvedValue({
      path: "large.txt",
      startLine: 0,
      totalLines: 3,
      lines: [
        { index: 0, kind: "hunk", text: "@@ -0,0 +1,2 @@", oldLineNumber: null, newLineNumber: null },
        { index: 1, kind: "added", text: "+first line", oldLineNumber: null, newLineNumber: 1 },
        { index: 2, kind: "added", text: "+second line", oldLineNumber: null, newLineNumber: 2 },
      ],
    });
    const review = reviewApi();
    const wrapper = mount(FileDiff, {
      props: {
        checkout: checkout(checkoutId),
        gitSnapshot,
        review,
        path: "large.txt",
        scrollTop: 0,
      },
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("+first line"));

    await wrapper.get('[aria-label="Add review note on line 1"]').trigger("click");
    const textarea = wrapper.get('textarea[aria-label="Review note"]');
    await textarea.setValue("revisit this calculation");
    await wrapper.get('form[aria-label="New review note"]').trigger("submit");
    await flushPromises();

    expect(review.addNote).toHaveBeenCalledWith({
      path: "large.txt",
      side: "new",
      lineStart: 1,
      content: "revisit this calculation",
      code: "first line",
    });
    wrapper.unmount();
  });

  it("writes a range the way GitLab does: the note ends on the line its + was on", async () => {
    const checkoutId = "checkout:range-note";
    const gitSnapshot = snapshot(checkoutId, [{ path: "large.txt", status: "M" }]);
    mocks.getGitDiff.mockResolvedValue({
      path: "large.txt",
      patch: "@@ -0,0 +1,3 @@\n+first line\n+second line\n+third line\n",
      revision: "large.txt#4",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: 4,
      hunks: [{ startLine: 0, endLine: 4, title: "@@ -0,0 +1,3 @@" }],
    });
    mocks.getGitDiffPage.mockResolvedValue({
      path: "large.txt",
      startLine: 0,
      totalLines: 4,
      lines: [
        { index: 0, kind: "hunk", text: "@@ -0,0 +1,3 @@", oldLineNumber: null, newLineNumber: null },
        { index: 1, kind: "added", text: "+first line", oldLineNumber: null, newLineNumber: 1 },
        { index: 2, kind: "added", text: "+second line", oldLineNumber: null, newLineNumber: 2 },
        { index: 3, kind: "added", text: "+third line", oldLineNumber: null, newLineNumber: 3 },
      ],
    });
    const review = reviewApi();
    const wrapper = mount(FileDiff, {
      props: {
        checkout: checkout(checkoutId),
        gitSnapshot,
        review,
        path: "large.txt",
        scrollTop: 0,
      },
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("+first line"));

    // The + fixes where the note ends, so it goes on line 3 first; the + on line 1 then says
    // where the range starts, which is the one thing a note cannot say by growing downwards.
    await wrapper.get('[aria-label="Add review note on line 3"]').trigger("click");
    await wrapper.get('[aria-label="Add review note on line 1"]').trigger("click");
    expect(wrapper.get("form[aria-label='New review note']").text()).toContain("new lines 1-3");

    await wrapper.get('textarea[aria-label="Review note"]').setValue("these three lines belong together");
    await wrapper.get('form[aria-label="New review note"]').trigger("submit");
    await flushPromises();

    expect(review.addNote).toHaveBeenCalledWith({
      path: "large.txt",
      side: "new",
      lineStart: 1,
      lineEnd: 3,
      content: "these three lines belong together",
      code: "first line\nsecond line\nthird line",
    });
    wrapper.unmount();
  });

  it("widens a one-line note from the composer itself, without going back to the diff", async () => {
    const checkoutId = "checkout:composer-range";
    const gitSnapshot = snapshot(checkoutId, [{ path: "large.txt", status: "M" }]);
    mocks.getGitDiff.mockResolvedValue({
      path: "large.txt",
      patch: "@@ -0,0 +1,3 @@\n+first line\n+second line\n+third line\n",
      revision: "large.txt#5",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: 4,
      hunks: [{ startLine: 0, endLine: 4, title: "@@ -0,0 +1,3 @@" }],
    });
    mocks.getGitDiffPage.mockResolvedValue({
      path: "large.txt",
      startLine: 0,
      totalLines: 4,
      lines: [
        { index: 0, kind: "hunk", text: "@@ -0,0 +1,3 @@", oldLineNumber: null, newLineNumber: null },
        { index: 1, kind: "added", text: "+first line", oldLineNumber: null, newLineNumber: 1 },
        { index: 2, kind: "added", text: "+second line", oldLineNumber: null, newLineNumber: 2 },
        { index: 3, kind: "added", text: "+third line", oldLineNumber: null, newLineNumber: 3 },
      ],
    });
    const review = reviewApi();
    const wrapper = mount(FileDiff, {
      props: { checkout: checkout(checkoutId), gitSnapshot, review, path: "large.txt", scrollTop: 0 },
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("+first line"));

    // One + and the note is on that line, but the composer already says where it starts and ends:
    // the + fixed the end, and the top of the range is a decision the composer takes, not the diff.
    await wrapper.get('[aria-label="Add review note on line 3"]').trigger("click");
    const form = wrapper.get('form[aria-label="New review note"]');
    expect(form.text()).toContain("new line 3");
    // Every line of the run the note could start on, marked as the gutter marks it. The mark is read
    // off the markup rather than off the trimmed text, because a line the diff left alone is a
    // space and that is the whole of what tells it from the others.
    const options = () => form.findAll('[role="option"]').map((row) => row.element.textContent);
    expect(options()).toEqual(["+1", "+2", "+3"]);

    await form.get('textarea[aria-label="Review note"]').setValue("the last two lines");
    await form.findAll('[role="option"]')[options().indexOf("+2") as number].trigger("click");
    // Widening the range is not a new note, so what was written stays written.
    expect(form.text()).toContain("new lines 2-3");
    expect((form.get('textarea[aria-label="Review note"]').element as HTMLTextAreaElement).value).toBe(
      "the last two lines",
    );
    await form.trigger("submit");
    await flushPromises();

    expect(review.addNote).toHaveBeenCalledWith({
      path: "large.txt",
      side: "new",
      lineStart: 2,
      lineEnd: 3,
      content: "the last two lines",
      code: "second line\nthird line",
    });
    wrapper.unmount();
  });

  it("offers a range over the lines the diff removed, on the numbering they were removed from", async () => {
    const checkoutId = "checkout:removed-range";
    const gitSnapshot = snapshot(checkoutId, [{ path: "large.txt", status: "M" }]);
    mocks.getGitDiff.mockResolvedValue({
      path: "large.txt",
      patch: "@@ -1,3 +1,2 @@\n first\n-second\n-third\n+two thirds\n",
      revision: "large.txt#6",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: 5,
      hunks: [{ startLine: 0, endLine: 5, title: "@@ -1,3 +1,2 @@" }],
    });
    mocks.getGitDiffPage.mockResolvedValue({
      path: "large.txt",
      startLine: 0,
      totalLines: 5,
      lines: [
        { index: 0, kind: "hunk", text: "@@ -1,3 +1,2 @@", oldLineNumber: null, newLineNumber: null },
        { index: 1, kind: "context", text: " first", oldLineNumber: 1, newLineNumber: 1 },
        { index: 2, kind: "removed", text: "-second", oldLineNumber: 2, newLineNumber: null },
        { index: 3, kind: "removed", text: "-third", oldLineNumber: 3, newLineNumber: null },
        { index: 4, kind: "added", text: "+two thirds", oldLineNumber: null, newLineNumber: 2 },
      ],
    });
    const review = reviewApi();
    const wrapper = mount(FileDiff, {
      props: { checkout: checkout(checkoutId), gitSnapshot, review, path: "large.txt", scrollTop: 0 },
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("-second"));

    // A line the diff removed only ever had a number on the old side, so a note on one is walked
    // there: the picker offers the old numbers, each marked the way the gutter marks it, and never
    // a new-side line that happens to sit at the same distance from the end.
    await wrapper.get('[aria-label="Add review note on line 3"]').trigger("click");
    const form = wrapper.get('form[aria-label="New review note"]');
    expect(form.text()).toContain("old line 3");
    const options = () => form.findAll('[role="option"]').map((row) => row.element.textContent);
    expect(options()).toEqual([" 1", "-2", "-3"]);

    await form.findAll('[role="option"]')[options().indexOf("-2") as number].trigger("click");
    await form.get('textarea[aria-label="Review note"]').setValue("both of these go");
    await form.trigger("submit");
    await flushPromises();

    expect(review.addNote).toHaveBeenCalledWith({
      path: "large.txt",
      side: "old",
      lineStart: 2,
      lineEnd: 3,
      content: "both of these go",
      code: "second\nthird",
    });
    wrapper.unmount();
  });

  it("starts a second note when the + is not above the one already open", async () => {
    const checkoutId = "checkout:second-note";
    const gitSnapshot = snapshot(checkoutId, [{ path: "large.txt", status: "M" }]);
    mocks.getGitDiff.mockResolvedValue({
      path: "large.txt",
      patch: "@@ -0,0 +1,3 @@\n+first line\n+second line\n+third line\n",
      revision: "large.txt#7",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: 4,
      hunks: [{ startLine: 0, endLine: 4, title: "@@ -0,0 +1,3 @@" }],
    });
    mocks.getGitDiffPage.mockResolvedValue({
      path: "large.txt",
      startLine: 0,
      totalLines: 4,
      lines: [
        { index: 0, kind: "hunk", text: "@@ -0,0 +1,3 @@", oldLineNumber: null, newLineNumber: null },
        { index: 1, kind: "added", text: "+first line", oldLineNumber: null, newLineNumber: 1 },
        { index: 2, kind: "added", text: "+second line", oldLineNumber: null, newLineNumber: 2 },
        { index: 3, kind: "added", text: "+third line", oldLineNumber: null, newLineNumber: 3 },
      ],
    });
    const review = reviewApi();
    const wrapper = mount(FileDiff, {
      props: { checkout: checkout(checkoutId), gitSnapshot, review, path: "large.txt", scrollTop: 0 },
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("+first line"));

    // A range only grows upwards, so a + on the same line or below the open one is not part of
    // the note being written: it is a second note, and the first one was never a range at all.
    await wrapper.get('[aria-label="Add review note on line 1"]').trigger("click");
    await wrapper.get("textarea[aria-label='Review note']").setValue("about line 1");
    await wrapper.get('[aria-label="Add review note on line 3"]').trigger("click");
    expect(wrapper.get("form[aria-label='New review note']").text()).toContain("new line 3");

    // The text of the note that was open is gone with it, rather than carried into a range.
    expect((wrapper.get('textarea[aria-label="Review note"]').element as HTMLTextAreaElement).value).toBe("");
    await wrapper.get('textarea[aria-label="Review note"]').setValue("about line 3");
    await wrapper.get('form[aria-label="New review note"]').trigger("submit");
    await flushPromises();

    expect(review.addNote).toHaveBeenCalledWith({
      path: "large.txt",
      side: "new",
      lineStart: 3,
      content: "about line 3",
      code: "third line",
    });
    expect(review.addNote).not.toHaveBeenCalledWith(expect.objectContaining({ content: "about line 1" }));
    wrapper.unmount();
  });

  it("keeps range code from every loaded diff page when selection spans a scroll", async () => {
    const checkoutId = "checkout:range-pages";
    const gitSnapshot = snapshot(checkoutId, [{ path: "large.ts", status: "M" }]);
    const lines = [
      { index: 0, kind: "hunk" as const, text: "@@ -0,0 +1,70 @@", oldLineNumber: null, newLineNumber: null },
      ...Array.from({ length: 70 }, (_, index) => ({
        index: index + 1,
        kind: "added" as const,
        text: `+line ${index + 1}`,
        oldLineNumber: null,
        newLineNumber: index + 1,
      })),
    ];
    const code = Array.from({ length: 70 }, (_, index) => `line ${index + 1}`).join("\n");
    mocks.getGitDiff.mockResolvedValue({
      path: "large.ts",
      patch: "",
      revision: "large.ts#1",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: lines.length,
      hunks: [{ startLine: 0, endLine: lines.length, title: "@@ -0,0 +1,70 @@" }],
    });
    mocks.getGitDiffPage.mockImplementation(async (_checkout: string, path: string, start: number, count: number) => ({
      path,
      startLine: start,
      totalLines: lines.length,
      lines: lines.slice(start, start + count),
    }));
    const review = reviewApi();
    const wrapper = mount(FileDiff, {
      props: { checkout: checkout(checkoutId), gitSnapshot, review, path: "large.ts", scrollTop: 0 },
    });
    // The window holds the rows on screen and the overscan either side of them, so the far end of a
    // 70-row diff is in the DOM only once the user has scrolled to it. Both ends have to be loaded
    // for the range's code to be whole, and the composer is held by the panel rather than by its row
    // so it is still there when the scroll that finishes the range has left its own line behind.
    await vi.waitFor(() => expect(wrapper.text()).toContain("+line 1"));

    // The note ends on line 70, so the + goes there; line 1 is the top of the range and is picked
    // on the way back, which is the scroll the composer has to survive.
    const viewport = wrapper.get('[aria-label="Diff contents"]');
    (viewport.element as HTMLElement).scrollTop = 70 * 22;
    await viewport.trigger("scroll");
    await vi.waitFor(() => expect(wrapper.text()).toContain("+line 70"));
    await wrapper.get('[aria-label="Add review note on line 70"]').trigger("click");
    (viewport.element as HTMLElement).scrollTop = 0;
    await viewport.trigger("scroll");
    await vi.waitFor(() => expect(wrapper.text()).toContain("+line 1"));
    await wrapper.get('[aria-label="Add review note on line 1"]').trigger("click");

    await vi.waitFor(() =>
      expect(wrapper.get('form[aria-label="New review note"]').text()).toContain("new lines 1-70"),
    );
    await wrapper.get('textarea[aria-label="Review note"]').setValue("review the whole block");
    await wrapper.get('form[aria-label="New review note"]').trigger("submit");
    await flushPromises();

    expect(review.addNote).toHaveBeenCalledWith({
      path: "large.ts",
      side: "new",
      lineStart: 1,
      lineEnd: 70,
      content: "review the whole block",
      code,
    });
    const markdown = buildReviewMarkdown(
      [reviewNote({ path: "large.ts", lineStart: 1, lineEnd: 70, code, content: "review the whole block" })],
      { date: "2026-03-14" },
    );
    expect(markdown).toContain(`\`\`\`ts{1-70}\n${code}\n\`\`\``);
    wrapper.unmount();
  });

  it("rejects a range when virtualized pages needed for its code were skipped", async () => {
    const checkoutId = "checkout:range-unloaded";
    const gitSnapshot = snapshot(checkoutId, [{ path: "large.ts", status: "M" }]);
    const lines = [
      { index: 0, kind: "hunk" as const, text: "@@ -0,0 +1,520 @@", oldLineNumber: null, newLineNumber: null },
      ...Array.from({ length: 520 }, (_, index) => ({
        index: index + 1,
        kind: "added" as const,
        text: `+line ${index + 1}`,
        oldLineNumber: null,
        newLineNumber: index + 1,
      })),
    ];
    mocks.getGitDiff.mockResolvedValue({
      path: "large.ts",
      patch: "",
      revision: "large.ts#2",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: lines.length,
      hunks: [{ startLine: 0, endLine: lines.length, title: "@@ -0,0 +1,520 @@" }],
    });
    mocks.getGitDiffPage.mockImplementation(async (_checkout: string, path: string, start: number, count: number) => ({
      path,
      startLine: start,
      totalLines: lines.length,
      lines: lines.slice(start, start + count),
    }));
    const review = reviewApi();
    const wrapper = mount(FileDiff, {
      props: { checkout: checkout(checkoutId), gitSnapshot, review, path: "large.ts", scrollTop: 0 },
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("+line 1"));

    const viewport = wrapper.get('[aria-label="Diff contents"]');
    (viewport.element as HTMLElement).scrollTop = 500 * 22;
    await viewport.trigger("scroll");
    await vi.waitFor(() => expect(wrapper.text()).toContain("+line 500"));
    await wrapper.get('[aria-label="Add review note on line 500"]').trigger("click");
    (viewport.element as HTMLElement).scrollTop = 0;
    await viewport.trigger("scroll");
    await wrapper.get('[aria-label="Add review note on line 1"]').trigger("click");

    const form = wrapper.get('form[aria-label="New review note"]');
    await form.get('textarea[aria-label="Review note"]').setValue("review this range");
    await form.trigger("submit");

    expect(form.get('[role="alert"]').text()).toContain("Some selected lines are unavailable");
    expect((form.get('textarea[aria-label="Review note"]').element as HTMLTextAreaElement).value).toBe(
      "review this range",
    );
    expect(review.addNote).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("reports the anchor text of visible notes and holds drifted ones back", async () => {
    const checkoutId = "checkout:anchor-verify";
    const gitSnapshot = snapshot(checkoutId, [{ path: "large.txt", status: "M" }]);
    mocks.getGitDiff.mockResolvedValue({
      path: "large.txt",
      patch: "@@ -0,0 +1,2 @@\n+first line\n+second line\n",
      revision: "large.txt#8",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -0,0 +1,2 @@" }],
    });
    mocks.getGitDiffPage.mockResolvedValue({
      path: "large.txt",
      startLine: 0,
      totalLines: 3,
      lines: [
        { index: 0, kind: "hunk", text: "@@ -0,0 +1,2 @@", oldLineNumber: null, newLineNumber: null },
        { index: 1, kind: "added", text: "+first line", oldLineNumber: null, newLineNumber: 1 },
        { index: 2, kind: "added", text: "+second line", oldLineNumber: null, newLineNumber: 2 },
      ],
    });
    const review = reviewApi();
    review.notes = [
      reviewNote({ id: "note:1", path: "large.txt", lineStart: 1, code: "first line" }),
      reviewNote({ id: "note:2", path: "large.txt", lineStart: 2, code: "an older line", outdated: true }),
    ];
    const wrapper = mount(FileDiff, {
      props: {
        checkout: checkout(checkoutId),
        gitSnapshot,
        review,
        path: "large.txt",
        scrollTop: 0,
      },
    });
    await vi.waitFor(() =>
      expect(review.verifyAnchors).toHaveBeenCalledWith("large.txt", [
        { id: "note:1", currentCode: "first line" },
        { id: "note:2", currentCode: "second line" },
      ]),
    );

    // A drifted note says so, and accepting it is the only way out.
    expect(wrapper.text()).toContain("outdated");
    await wrapper.get('[aria-label="Accept note on line 2 even though the line changed"]').trigger("click");
    expect(review.clearOutdated).toHaveBeenCalledWith("note:2");
    wrapper.unmount();
  });

  it("judges each note from the diff and only offers resolve when it can back it", async () => {
    const checkoutId = "checkout:verdict";
    const gitSnapshot = snapshot(checkoutId, [{ path: "src/app.ts", status: "M" }]);
    // Hunk mode: the whole patch is in memory, so an absent anchor really is a deleted line.
    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "@@ -1,2 +1,2 @@\n const a = 1;\n+const b = 2;\n",
      revision: "src/app.ts#2",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -1,2 +1,2 @@" }],
    });
    const review = reviewApi();
    review.notes = [
      reviewNote({ id: "note:kept", lineStart: 1, status: "sent" }),
      reviewNote({ id: "note:gone", lineStart: 40, status: "sent" }),
    ];
    const wrapper = mount(FileDiff, {
      props: {
        checkout: checkout(checkoutId),
        gitSnapshot,
        review,
        path: "src/app.ts",
        scrollTop: 0,
      },
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("revisit this calculation"));

    // Unchanged and deleted can both be resolved, and they are reported apart.
    expect(wrapper.text()).toContain("The line this note points at is unchanged.");
    expect(wrapper.text()).toContain("The line this note points at no longer exists.");
    const resolvable = wrapper.findAll('[aria-label^="Mark note on line"]');
    expect(resolvable.map((button) => button.attributes("aria-label"))).toEqual([
      "Mark note on line 1 as resolved",
      "Mark note on line 40 as resolved",
    ]);
    wrapper.unmount();
  });

  it("offers to resolve an attended note and not a drifted one", async () => {
    const checkoutId = "checkout:resolve-note";
    const gitSnapshot = snapshot(checkoutId, [{ path: "large.txt", status: "M" }]);
    mocks.getGitDiff.mockResolvedValue({
      path: "large.txt",
      patch: "@@ -0,0 +1,2 @@\n+first line\n+second line\n",
      revision: "large.txt#9",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -0,0 +1,2 @@" }],
    });
    mocks.getGitDiffPage.mockResolvedValue({
      path: "large.txt",
      startLine: 0,
      totalLines: 3,
      lines: [
        { index: 0, kind: "hunk", text: "@@ -0,0 +1,2 @@", oldLineNumber: null, newLineNumber: null },
        { index: 1, kind: "added", text: "+first line", oldLineNumber: null, newLineNumber: 1 },
        { index: 2, kind: "added", text: "+second line", oldLineNumber: null, newLineNumber: 2 },
      ],
    });
    const review = reviewApi();
    review.notes = [
      reviewNote({ id: "note:1", path: "large.txt", lineStart: 1, status: "sent" }),
      // A draft was never delivered, so there is nothing to resolve.
      reviewNote({ id: "note:2", path: "large.txt", lineStart: 2, status: "draft" }),
      reviewNote({ id: "note:3", path: "large.txt", lineStart: 2, status: "sent", outdated: true }),
    ];
    const wrapper = mount(FileDiff, {
      props: {
        checkout: checkout(checkoutId),
        gitSnapshot,
        review,
        path: "large.txt",
        scrollTop: 0,
      },
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("+first line"));

    // Only the delivered, non-drifted note can be resolved, and only by the user.
    const resolveButtons = wrapper.findAll('[aria-label="Mark note on line 1 as resolved"]');
    expect(resolveButtons).toHaveLength(1);
    await resolveButtons[0].trigger("click");
    expect(review.resolveNote).toHaveBeenCalledWith("note:1");
    wrapper.unmount();
  });

  it("retains the selected diff on status refresh and handles binary and over-limit changes", async () => {
    const checkoutId = "checkout:diff-status";
    const gitSnapshot = snapshot(checkoutId, [{ path: "src/app.ts", status: "M" }]);
    const wrapper = mount(FileDiff, {
      props: {
        checkout: checkout(checkoutId),
        gitSnapshot,
        review: reviewApi(),
        path: "src/app.ts",
        scrollTop: 0,
      },
    });
    await flushPromises();
    expect(mocks.getGitDiff).toHaveBeenCalledTimes(1);
    // Debounced: a burst of writes is one reload, so the second call waits out the timer.
    gitSnapshot.statusRevision += 1;
    await vi.waitFor(() => expect(mocks.getGitDiff).toHaveBeenCalledTimes(2));
    wrapper.unmount();

    for (const result of [
      { isBinary: true, tooLarge: false, message: "Binary file; text diff is unavailable." },
      { isBinary: false, tooLarge: true, message: "This diff exceeds safe preview limits" },
    ]) {
      const isolatedSnapshot = snapshot(checkoutId, [{ path: "src/app.ts", status: "M" }]);
      mocks.getGitDiff.mockResolvedValueOnce({
        path: "src/app.ts",
        patch: "",
        revision: "src/app.ts#3",
        isBinary: result.isBinary,
        large: false,
        tooLarge: result.tooLarge,
        totalLines: 0,
        hunks: [],
      });
      const special = mount(FileDiff, {
        props: {
          checkout: checkout(checkoutId),
          gitSnapshot: isolatedSnapshot,
          review: reviewApi(),
          path: "src/app.ts",
          scrollTop: 0,
        },
      });
      await vi.waitFor(() => expect(special.text()).toContain(result.message));
      special.unmount();
    }
  });
});
