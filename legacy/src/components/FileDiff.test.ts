// @vitest-environment happy-dom
// The diff view is stubbed inline, the way the other component tests stub what they cannot mount.
/* eslint-disable vue/one-component-per-file, vue/require-default-prop */
import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h, provide, reactive, ref } from "vue";
import type { VNodeChild } from "vue";
import type { AgentSession } from "../domain/agent";
import { buildReviewMarkdown } from "../domain/review";
import type { ReviewNote } from "../domain/review";
import { DEFAULT_SETTINGS } from "../domain/settings";
import type { Checkout } from "../domain/workspace";
import type { ActiveGitSnapshot } from "../presentation/active-git-snapshot";
import { REVIEW_SENDER } from "../presentation/review-notes";
import type { NewReviewNoteInput, ReviewSender } from "../presentation/review-notes";
import { theme } from "../presentation/theme";
import { diffRowHeight } from "./use-large-diff";

/** The library's `DiffFile`, as the stub above stands in for it. */
interface DiffFileStub {
  palette: string | undefined;
  notified: number;
  /** The language the stub was told to read the file as, on each side. */
  oldLang: string | undefined;
  newLang: string | undefined;
}

const mocks = vi.hoisted(() => ({
  getGitDiff: vi.fn(),
  getGitDiffPage: vi.fn(),
  /** Every `DiffFile` the component built, in order, so a test can read what it asked for. */
  diffFiles: [] as Array<{ oldLang: string | undefined; newLang: string | undefined; uuid: string | undefined }>,
  /** What the component asked to have read, in order, one entry per diff it drew. */
  prepared: [] as Array<{
    language: string | undefined;
    contents: Record<string, string | undefined>;
    /** How far into the file the diff reaches, which is how much of it is read. */
    lines: number | undefined;
  }>,
  /**
   * The reads a test is holding back, keyed by the text the read was asked for. Keying on the text
   * rather than on a counter is what keeps a test independent of which other tests ran first and left a
   * read of their own in flight.
   */
  held: new Map<string, Promise<void>>(),
  /** Every range the component asked the library to paint its selection over. */
  updateSelectionVisual: vi.fn(),
}));

// The grammar and the reading of the file are one dynamic import away and are covered end to end by
// `diff-highlighter.test.ts`. What is under test here is the wiring: which file is asked to be read,
// with what text, and which of two answers is the one the diff on screen is given.
vi.mock("../lib/diff-highlighter", () => ({
  prepareDiffHighlighting: async (
    language: string | undefined,
    contents: Record<string, string | undefined>,
    lines: number | undefined,
  ) => {
    mocks.prepared.push({ language, contents, lines });
    await mocks.held.get(contents.new ?? "");
    // Each answer is its own object, as the real one is: nothing here is shared between two diffs, and
    // it names the text it was read from so a test can tell whose answer it is looking at.
    return { name: `read:${contents.new ?? ""}`, type: "class" };
  },
}));

vi.mock("../lib/ipc", () => ({
  getGitDiff: mocks.getGitDiff,
  getGitDiffPage: mocks.getGitDiffPage,
}));

// The diff view renders through the library; the send, the header and the notes are what is
// under test here, so only the slots the library would fill are reproduced.
vi.mock("@git-diff-view/vue", async () => {
  const { defineComponent: component, h: createElement, ref: createRef } = await import("vue");
  return {
    DiffFile: class {
      /** What the library is told to paint in, which is the whole of what a palette switch reaches. */
      palette: string | undefined;
      notified = 0;
      constructor(
        _oldFileName: string,
        _oldFileContent: string,
        _newFileName: string,
        _newFileContent: string,
        _diffList: string[],
        oldLang?: string,
        newLang?: string,
        /** The identity each set of hunks is given, which is what tells two checkouts' windows apart. */
        uuid?: string,
      ) {
        mocks.diffFiles.push({ oldLang, newLang, uuid });
      }
      initTheme(theme?: string) {
        this.palette = theme;
      }
      notifyAll() {
        this.notified += 1;
      }
      init() {}
      buildUnifiedDiffLines() {}
      /** The identity the library scopes its rows to when it paints a selection. */
      getId() {
        return this.identity;
      }
      identity = "diff-id";
    },
    // The library paints its own selection band and takes no prop for it, so the range a note
    // covers is drawn through this. Standing in for it records what it was asked to draw.
    updateSelectionVisual_Unified: (...args: unknown[]) => mocks.updateSelectionVisual(...args),
    DiffModeEnum: { Unified: 4 },
    // `diff-highlighter.ts` imports this and reuses it: the diff is highlighted by handing the
    // library an object with this on it. What it returns is never read here, only that it exists.
    processAST: () => ({ syntaxFileObject: {}, syntaxFileLineNumber: 0 }),
    DiffViewWithMultiSelect: component({
      name: "DiffView",
      props: {
        extendData: { type: Object, default: () => ({}) },
        diffFile: { type: Object, default: null },
        registerHighlighter: { type: Object, default: undefined },
        diffViewFontSize: { type: Number, default: undefined },
      },
      setup: (
        props: {
          extendData: { newFile?: Record<string, { data: ReviewNote[] }> };
          registerHighlighter?: unknown;
          diffViewFontSize?: number;
        },
        { slots }: { slots: Record<string, ((payload: never) => VNodeChild) | undefined> },
      ) => {
        const selectedRange = createRef<[number, number] | null>(null);
        return () =>
          // The library wraps everything it draws in this, and it is the element it is asked to
          // paint a selection over, so the rows have to sit inside one for that to be testable.
          createElement("div", { class: "diff-multiselect-wrapper" }, [
            createElement("div", { "data-testid": "diff-view" }, [
              // What the library is handed to highlight with, named so a test can read it: whether one
              // arrives at all is the whole of what `diff-highlighter.ts` is wired up for.
              createElement("span", {
                "data-testid": "diff-highlighter",
                "data-name": (props.registerHighlighter as { name?: string } | undefined)?.name ?? "",
              }),
              createElement("span", { "data-testid": "diff-font-size" }, String(props.diffViewFontSize)),
              // The library draws a row per `@@` header and its own expand buttons inside that row, and
              // the rows of the diff itself carry their lines, their widget and their notes, which is
              // the whole of what the collapse is delegated across.
              createElement("table", {}, [
                createElement("tbody", {}, [
                  createElement("tr", { "data-testid": "hunk-row", "data-line": "1-hunk" }, [
                    createElement("td", { class: "diff-line-hunk" }, "@@ -1 +1 @@"),
                    createElement(
                      "td",
                      {},
                      createElement("button", { class: "diff-widget-tooltip", "data-testid": "hunk-expand" }, "Expand"),
                    ),
                  ]),
                  createElement("tr", { "data-line": "2-add" }, [
                    createElement("td", { "data-testid": "line-number" }, "2"),
                    createElement("td", { class: "diff-line-content" }, [
                      createElement("span", { class: "diff-line-syntax-raw" }, "const after = 2;"),
                      // The note composer and the note cards are rendered into the widget row of the
                      // line they belong to, which is a row of its own.
                      createElement("div", { "data-line": "2-widget-content", "data-testid": "line-widget" }),
                      createElement("div", { "data-line": "2-extend-content", "data-testid": "line-note" }),
                    ]),
                  ]),
                ]),
              ]),
              slots.extend?.({ data: props.extendData?.newFile?.["1"]?.data ?? [] } as never),
              selectedRange.value
                ? slots.widget?.({
                    lineNumber: selectedRange.value[1],
                    fromLineNumber: selectedRange.value[0],
                    side: 2,
                    onClose: () => (selectedRange.value = null),
                  } as never)
                : null,
              createElement(
                "button",
                { "data-testid": "select-diff-range", onClick: () => (selectedRange.value = [1, 3]) },
                "Select lines 1-3",
              ),
              // The library owns the two ends, and nothing here promises it hands them over in order.
              createElement(
                "button",
                { "data-testid": "select-inverted-range", onClick: () => (selectedRange.value = [3, 1]) },
                "Select lines 3-1",
              ),
            ]),
          ]);
      },
    }),
  };
});

vi.mock("reka-ui", async () => {
  const { h: createElement, defineComponent, inject, provide } = await import("vue");
  // The destination menu always renders here, so a target can be picked without driving the open
  // state, the way DocumentPane.test.ts stubs the toolbar's own popover.
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
  // The select is stubbed with its wiring intact: the root holds the value and the rows report
  // a pick back to it, so a destination can be chosen here the way the keyboard would choose it.
  const select = Symbol("select");
  const selectRoot = defineComponent({
    name: "SelectRoot",
    props: { modelValue: String },
    emits: ["update:modelValue"],
    setup(props, { emit, slots }) {
      provide(select, (value: string) => emit("update:modelValue", value));
      return () => createElement("div", slots.default?.());
    },
  });
  const selectItem = defineComponent({
    name: "SelectItem",
    inheritAttrs: false,
    props: { value: String },
    // The row is an option here the way it is in the library, so a query for the destination
    // finds it by the role a screen reader would hear.
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

import FileDiff from "./FileDiff.vue";

/** The session rows, named by the listbox that holds them: the destination select offers rows too. */
const SESSION_OPTION = '[aria-label="Send review to"] [role="option"]';

const checkout: Checkout = {
  id: "checkout:one",
  repoId: "repo:one",
  path: "/one",
  canonicalPath: "/one",
  isPrimary: true,
  changedFiles: 0,
  isMissing: false,
  sessions: [],
};

function snapshot(files: string[] = ["src/app.ts"]): ActiveGitSnapshot {
  return reactive<ActiveGitSnapshot>({
    checkoutId: checkout.id,
    status: {
      branch: "bug/1310-timeline",
      defaultBranch: "main",
      aheadCount: 1,
      files: files.map((path) => ({ path, status: "M" as const })),
    },
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

function note(overrides: Partial<ReviewNote> = {}): ReviewNote {
  return {
    id: "note:1",
    checkoutId: checkout.id,
    path: "src/app.ts",
    side: "new",
    lineStart: 1,
    lineEnd: null,
    content: "revisit this calculation",
    code: "new",
    codeHash: "0000000000000001",
    outdated: false,
    roundId: null,
    status: "draft",
    createdAt: "1",
    updatedAt: "1",
    ...overrides,
  };
}

function session(id: string, overrides: Partial<AgentSession> = {}): AgentSession {
  return {
    id,
    checkoutId: checkout.id,
    title: id,
    running: false,
    idleAt: 1,
    awaitingReply: false,
    agent: null,
    model: null,
    parentId: null,
    outcome: null,
    createdAt: 1,
    updatedAt: 1,
    ...overrides,
  };
}

function reviewApi(notes: ReviewNote[] = []) {
  return reactive({
    notes,
    addNote: vi.fn<(input: NewReviewNoteInput) => Promise<boolean>>(async () => true),
    updateNote: vi.fn(async () => true),
    deleteNote: vi.fn(async () => true),
    verifyAnchors: vi.fn(async () => true),
    clearOutdated: vi.fn(async () => true),
    resolveNote: vi.fn(async () => true),
  });
}

function senderStub(
  sessions: AgentSession[] = [session("ses_one")],
  targetId: string | null = "ses_one",
  unfinishedRounds = 0,
  reviewTarget: "markdown" | "opencode" = "opencode",
) {
  const sessionList = ref(sessions);
  const target = ref(targetId);
  const unfinished = ref(unfinishedRounds);
  const destination = ref(reviewTarget);
  const send = vi.fn(async () => undefined);
  const selectTarget = vi.fn();
  const selectReviewTarget = vi.fn((next: "markdown" | "opencode") => (destination.value = next));
  const sender: ReviewSender = {
    get target() {
      return destination.value;
    },
    get sessions() {
      return sessionList.value;
    },
    get targetId() {
      return target.value;
    },
    get unfinishedRounds() {
      return unfinished.value;
    },
    selectTarget,
    selectReviewTarget,
    send,
  };
  return { sender, send, selectTarget, selectReviewTarget };
}

/**
 * The path the shell hands the diff, which is reactive so a test can open another file in the mount
 * it already has rather than building a second one.
 */
const openedPath = ref<string | null>(null);
/**
 * The checkout the shell hands the diff, reactive for the same reason: the diff is read again when
 * either of these two changes, and a test that needs a second reading of one file needs one of them
 * to move.
 */
const openedCheckout = ref<Checkout>(checkout);

/** The diff is reached through the shell, so the sender is what the shell provides. */
function mountDiff(
  props: Record<string, unknown>,
  sender: ReviewSender | null = senderStub().sender,
  options: { attachTo?: Element } = {},
) {
  // `null` is a path of its own here: it is the whole change set rather than no file at all.
  openedPath.value = "path" in props ? (props.path as string | null) : "src/app.ts";
  openedCheckout.value = checkout;
  const harness = defineComponent({
    setup: () => {
      if (sender) provide(REVIEW_SENDER, sender);
      return () =>
        h(FileDiff, {
          checkout: openedCheckout.value,
          gitSnapshot: snapshot(),
          review: reviewApi(),
          scrollTop: 0,
          ...props,
          path: openedPath.value,
        });
    },
  });
  return mount(harness, options);
}

describe("FileDiff", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    // Not mocks of their own, so `resetAllMocks` leaves them: every test wants only its own files.
    mocks.diffFiles.length = 0;
    mocks.prepared.length = 0;
    mocks.held.clear();
    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "diff --git a/src/app.ts b/src/app.ts\n@@ -1 +1 @@\n-old\n+new\n",
      revision: "src/app.ts#1",
      oldContent: "const before = 1;\n",
      newContent: "const after = 2;\n",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -1 +1 @@" }],
    });
    mocks.getGitDiffPage.mockResolvedValue({ path: "", startLine: 0, totalLines: 0, lines: [] });
  });

  it("collapses a hunk from the header the library draws, and reopens it from the one it leaves", async () => {
    const wrapper = mountDiff({});
    await flushPromises();
    const rendered = () => wrapper.findAll('[data-testid="diff-view"]');
    const toggle = () => wrapper.get('[data-testid="hunk-toggle"]');
    // One header per hunk, and only the library's: it already draws the `@@` row, so a header of our
    // own above an open hunk would be the same header twice and a row of the diff spent on it. The
    // control is in the same element either way and says which state the hunk is in.
    expect(toggle().classes()).toContain("sr-only");
    expect(toggle().attributes("aria-expanded")).toBe("true");
    expect(toggle().text().trim()).toBe("");

    // The library's own expand buttons live inside that row and are left to do their own job.
    await wrapper.get('[data-testid="hunk-expand"]').trigger("click");
    expect(rendered()).toHaveLength(1);

    await wrapper.get('[data-testid="hunk-row"]').trigger("click");
    expect(rendered()).toHaveLength(0);
    // The row that carried the header went with the hunk, so a collapsed one has to get a header back
    // or there is nothing left to open it from. It says so to a screen reader as well as to the eye.
    expect(toggle().attributes("aria-expanded")).toBe("false");
    expect(toggle().classes()).not.toContain("sr-only");
    expect(toggle().text().trim()).toBe("▸ @@ -1 +1 @@");
    // What makes it reachable from the keyboard is that it is a real button and not a row with a click
    // handler on it: a button is in the tab order and answers Enter and Space on its own.
    expect(toggle().element.tagName).toBe("BUTTON");
    expect(toggle().attributes("type")).toBe("button");
    expect(toggle().attributes("tabindex")).toBeUndefined();

    await toggle().trigger("click");
    expect(rendered()).toHaveLength(1);
    expect(toggle().attributes("aria-expanded")).toBe("true");
    wrapper.unmount();
  });

  it("collapses and reopens a hunk from a control the keyboard can reach, and keeps it focused", async () => {
    // Mounted in the document because focus is the claim under test, and a detached tree has no
    // active element to keep.
    const wrapper = mountDiff({}, undefined, { attachTo: document.body });
    await flushPromises();
    const toggle = () => wrapper.get('[data-testid="hunk-toggle"]');
    const control = toggle();
    expect(control.element.tagName).toBe("BUTTON");
    expect(control.attributes("type")).toBe("button");
    expect(control.attributes("tabindex")).toBeUndefined();
    expect(control.attributes("aria-label")).toBe("Collapse hunk @@ -1 +1 @@");
    (control.element as HTMLElement).focus();
    expect(document.activeElement).toBe(control.element);

    // A native button turns Enter and Space into a click, which is the event the handler is on, and
    // happy-dom synthesizes no click from a keydown; so the click is what a key press arrives as and
    // the button is what makes the keyboard send it.
    await control.trigger("click");
    expect(toggle().attributes("aria-expanded")).toBe("false");
    expect(toggle().classes()).not.toContain("sr-only");
    // The control is the same element in both states, so the focus that collapsed the hunk is still on
    // it afterwards and the keyboard is never dropped on the floor by the thing it just did.
    expect(document.activeElement).toBe(control.element);

    await control.trigger("click");
    expect(toggle().attributes("aria-expanded")).toBe("true");
    expect(toggle().classes()).toContain("sr-only");
    expect(document.activeElement).toBe(control.element);
    wrapper.unmount();
  });

  it("collapses a hunk only from its own header row", async () => {
    // The collapse is delegated to the whole hunk, so the guard is the only thing between a click on
    // the diff and a hunk that closes under the user's hand. The library gives every row of a diff
    // its own `data-line`, and only the header row's ends in `-hunk`, so a line, the widget a note
    // is composed in and the cards a note is drawn in are all outside it.
    const wrapper = mountDiff({});
    await flushPromises();
    const collapsed = () => wrapper.get('[data-testid="hunk-toggle"]').attributes("aria-expanded");
    expect(collapsed()).toBe("true");

    await wrapper.get('[data-testid="line-number"]').trigger("click");
    expect(collapsed()).toBe("true");

    await wrapper.get(".diff-line-content").trigger("click");
    expect(collapsed()).toBe("true");

    await wrapper.get('[data-testid="line-widget"]').trigger("click");
    expect(collapsed()).toBe("true");

    await wrapper.get('[data-testid="line-note"]').trigger("click");
    expect(collapsed()).toBe("true");

    // A drag across lines ends where the pointer was let go, and the click the browser sends for it
    // lands on the row the press and the release have in common rather than on either of them. That
    // row is above the header, so it is not the header either.
    await wrapper.get('[data-testid="diff-view"] table').trigger("click");
    expect(collapsed()).toBe("true");

    // The header row is the one that closes it, and the library's own expand buttons inside that row
    // are left to do their own job.
    await wrapper.get('[data-testid="hunk-expand"]').trigger("click");
    expect(collapsed()).toBe("true");

    await wrapper.get('[data-testid="hunk-row"]').trigger("click");
    expect(collapsed()).toBe("false");
    wrapper.unmount();
  });

  it("fills the box it is given, and says so with a height rather than with a position", async () => {
    // The panel hands the diff a height and the diff keeps its own position, because it is the box
    // the composer of a large diff is a layer over. Tailwind emits `.absolute` before `.relative`,
    // so a panel that also said `absolute` would be overruled by the diff's own `relative`, `inset-0`
    // would stop giving it a height, and the diff's scroll container would have nothing to scroll in.
    const wrapper = mountDiff({ class: "h-full" });
    await flushPromises();
    const root = wrapper.get('section[aria-label="File diff"]').classes();
    expect(root).toContain("h-full");
    expect(root).toContain("relative");
    expect(root).not.toContain("absolute");
    // And the scroll container inside it is what a wheel turns, which is the chain the height is for.
    expect(wrapper.get('[aria-label="Diff contents"]').classes()).toContain("overflow-auto");
    wrapper.unmount();
  });

  it("reads a file for the lines the diff reaches, and reads it once however often it reloads", async () => {
    // Reading a file is a pass over all of it on the thread that answers the wheel, and git reports
    // every write in the workdir, so a file being worked on is re-read over and over.
    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "diff --git a/src/app.ts b/src/app.ts\n@@ -40,2 +40,2 @@ fn tail\n-old\n+new\n",
      revision: "src/app.ts#2",
      oldContent: "const before = 1;\n",
      newContent: "const after = 2;\n",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 5,
      hunks: [{ startLine: 0, endLine: 5, title: "@@ -40,2 +40,2 @@ fn tail" }],
    });
    const gitSnapshot = snapshot();
    let releaseFirst = () => {};
    mocks.held.set(
      "const after = 2;\n",
      new Promise<void>((resolve) => {
        releaseFirst = resolve;
      }),
    );
    const wrapper = mountDiff({ gitSnapshot });
    await vi.waitFor(() => expect(mocks.prepared).toHaveLength(1));
    // The header says where the hunk ends, which is as far into the file as the diff paints, so a
    // change at line 40 of a long file is read as the first 42 lines of it and not as the whole file.
    expect(mocks.prepared[0]).toEqual({
      language: "typescript",
      contents: { old: "const before = 1;\n", new: "const after = 2;\n" },
      lines: 42,
    });

    // Three reloads of a file that keeps changing are one reading in flight and one more after it,
    // not four: reading a file is a pass over all of it on the thread that answers the wheel, and the
    // only reading of a burst worth keeping is the last.
    for (const [index, text] of ["newer", "newest", "latest"].entries()) {
      mocks.getGitDiff.mockResolvedValue({
        path: "src/app.ts",
        patch: `diff --git a/src/app.ts b/src/app.ts\n@@ -40,2 +40,2 @@ fn tail\n-old\n+${text}\n`,
        revision: `src/app.ts#3-${text}`,
        oldContent: "const before = 1;\n",
        newContent: `const after = ${text.length};\n`,
        isBinary: false,
        large: false,
        tooLarge: false,
        totalLines: 5,
        hunks: [{ startLine: 0, endLine: 5, title: "@@ -40,2 +40,2 @@ fn tail" }],
      });
      gitSnapshot.statusRevision += 1;
      await vi.waitFor(() => expect(mocks.getGitDiff).toHaveBeenCalledTimes(index + 2));
    }
    expect(mocks.prepared).toHaveLength(1);

    // The reading in flight is answered at last, and the one that was owed behind it is the newest.
    releaseFirst();
    await vi.waitFor(() => expect(mocks.prepared).toHaveLength(2));
    expect(mocks.prepared[1].contents.new).toBe("const after = 6;\n");
    expect(wrapper.get('[data-testid="diff-highlighter"]').attributes("data-name")).toBe("read:const after = 6;\n");
    wrapper.unmount();
  });

  it("reads at the editor's size, and paints the rows the window measures", async () => {
    const row = diffRowHeight(20);
    const wrapper = mountDiff({ editorSettings: { ...DEFAULT_SETTINGS.editor, fontSize: 20 } });
    await flushPromises();
    // The row height is handed to the stylesheet and the arithmetic from the same number, so a row
    // cannot end up taller than the padding that stands in for the rows that are not on screen.
    expect(wrapper.get('[aria-label="Diff contents"]').attributes("style")).toContain(
      `--muster-diff-row-height: ${row}px`,
    );
    expect(wrapper.get('[data-testid="diff-font-size"]').text()).toBe("20");
    wrapper.unmount();

    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "",
      revision: "src/app.ts#4",
      isBinary: false,
      large: true,
      tooLarge: false,
      totalLines: 40,
      hunks: [{ startLine: 0, endLine: 40, title: "@@ -1 +1,40 @@" }],
    });
    mocks.getGitDiffPage.mockResolvedValue({
      path: "src/app.ts",
      startLine: 0,
      totalLines: 40,
      lines: [{ index: 1, kind: "added", text: "+new", oldLineNumber: null, newLineNumber: 1 }],
    });
    const large = mountDiff({ editorSettings: { ...DEFAULT_SETTINGS.editor, fontSize: 20 } });
    await flushPromises();
    expect(large.get('[data-testid="large-diff-row"]').attributes("style")).toContain(`height: ${row}px`);
    // Only a diff long enough to be windowed scrolls on its own: a short file in the change-set stack
    // is read off the stack's one scrollbar, and two for one file is one too many.
    expect(large.get('[aria-label="Diff contents"]').classes()).toContain("diff-viewport-windowed");
    large.unmount();
  });

  it("paints the whole change set without a second scrollbar of its own", async () => {
    const all = mountDiff({ path: null });
    await flushPromises();
    // How a range is picked is a fact about the diff, not about each file of it: repeated down the
    // stack it is a line of chrome per open file saying the same thing.
    expect(all.text()).not.toContain("Drag across line numbers");
    all.unmount();

    const short = mountDiff({});
    await flushPromises();
    // Only a diff long enough to be windowed scrolls on its own, so a file in the stack is read off
    // the stack's one scrollbar and two scrollbars for one file is one too many.
    expect(short.get('[aria-label="Diff contents"]').classes()).not.toContain("diff-viewport-windowed");
    short.unmount();
  });

  it("names the file, or the change set and its branch", async () => {
    const single = mountDiff({});
    await flushPromises();
    expect(single.get("header").text()).toContain("src/app.ts");
    expect(single.get("header").text()).not.toContain("bug/1310-timeline");
    single.unmount();

    const all = mountDiff({ path: null });
    await flushPromises();
    expect(all.get("header").text()).toContain("All changes");
    expect(all.get("header").text()).toContain("bug/1310-timeline");
    all.unmount();
  });

  it("tells the library what language to read each hunk of a file as", async () => {
    // The library decides by taking everything after the last dot, which names no language at all
    // for a `Dockerfile`, so being told is the only way those files get highlighted. It is told the
    // same name the editor reads the file as, which is what makes one file one file's colors.
    const cases: Array<[string, string]> = [
      ["apps/web/src/App.vue", "vue"],
      ["Dockerfile", "docker"],
      ["Makefile", "make"],
      [".prettierrc", "json"],
      ["scripts/build.sh", "shellscript"],
      ["Sources/main.m", "objective-c"],
      ["README.md", "markdown"],
      ["src/app.ts", "typescript"],
      ["src/App.svelte", "svelte"],
    ];
    for (const [path, expected] of cases) {
      mocks.diffFiles.length = 0;
      mocks.getGitDiff.mockResolvedValue({
        path,
        patch: `diff --git a/${path} b/${path}\n@@ -1 +1 @@\n-old\n+new\n`,
        revision: "path#1",
        isBinary: false,
        large: false,
        tooLarge: false,
        totalLines: 3,
        hunks: [{ startLine: 0, endLine: 3, title: "@@ -1 +1 @@" }],
      });
      const wrapper = mountDiff({ path });
      await flushPromises();
      // Both sides of the same file, on every hunk: a hunk has no name of its own to be read by.
      expect(mocks.diffFiles, path).not.toHaveLength(0);
      for (const file of mocks.diffFiles) {
        expect(file.oldLang, path).toBe(expected);
        expect(file.newLang, path).toBe(expected);
      }
      wrapper.unmount();
    }
  });

  it("hands the library the two texts of the file, which is what its grammar reads", async () => {
    // A grammar reads a file, not a hunk of one: the lines inside `<script setup lang="ts">` of a
    // `.vue` file are markup to a grammar that was never shown the tag that opened them. So what
    // arrives with the patch is the whole of each side, under the name Muster calls the language.
    const wrapper = mountDiff({});
    await vi.waitFor(() => expect(mocks.prepared).toHaveLength(1));
    expect(mocks.prepared[0]).toEqual({
      language: "typescript",
      contents: { old: "const before = 1;\n", new: "const after = 2;\n" },
      // As far as this diff reaches, which for a change on the first line is the first two.
      lines: 2,
    });
    wrapper.unmount();
  });

  it("gives each set of hunks an identity, which is what tells two checkouts' windows apart", async () => {
    // The library caches what it read about a window by the text of that window, and two checkouts
    // hold the same path and often the very same lines — so without an identity of its own, the
    // second diff would be drawn with the first one's syntax. It is scoped to the checkout and the
    // path as well as to the reading, because those are what make two files two files.
    const wrapper = mountDiff({});
    await flushPromises();
    expect(mocks.diffFiles).toHaveLength(1);
    // Scoped to the checkout and the path as well as to the reading, because those are what make two
    // files two files, and per hunk because the key it replaces is a hunk's own window.
    for (const file of mocks.diffFiles) expect(file.uuid).toBe(`${checkout.id}:src/app.ts:1:0`);

    // The same file read again is a different reading, and gets an identity of its own rather than
    // reusing the one whose cache the library already holds. The checkout is what moves to have it read
    // again, which is also what scopes the identity to the checkout it belongs to.
    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "diff --git a/src/app.ts b/src/app.ts\n@@ -1 +1 @@\n-old\n+newer\n",
      revision: "src/app.ts#5",
      oldContent: "const before = 1;\n",
      newContent: "const after = 3;\n",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -1 +1 @@" }],
    });
    openedCheckout.value = { ...checkout, id: "checkout:two" };
    await wrapper.setProps({});
    await flushPromises();
    expect(mocks.diffFiles[mocks.diffFiles.length - 1].uuid).toBe("checkout:two:src/app.ts:2:0");
    wrapper.unmount();
  });

  it("asks for nothing for a file whose diff carries no text to read", async () => {
    // A binary diff, a symlink, a diff too large to hold: there is no text, and asking for a reading
    // of what is not there would only hand the library a highlighter that has nothing to say.
    mocks.getGitDiff.mockResolvedValue({
      path: "assets/logo.png",
      patch: "diff --git a/assets/logo.png b/assets/logo.png\n@@ -1 +1 @@\n-old\n+new\n",
      revision: "assets/logo.png#1",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -1 +1 @@" }],
    });
    const wrapper = mountDiff({ path: "assets/logo.png" });
    await flushPromises();
    expect(mocks.prepared).toEqual([]);
    wrapper.unmount();
  });

  it("hands the library a highlighter, which is the only way it highlights with Muster' grammars", async () => {
    // The diff is highlighted by the same grammars the editor reads with, and the library takes a
    // replacement only through this prop. It arrives a moment after the diff does, because a grammar
    // is a dynamic import and a file is read in one, and holding the text back for either would cost
    // more than the flash is worth.
    const wrapper = mountDiff({});
    await vi.waitFor(() =>
      expect(wrapper.find('[data-testid="diff-highlighter"]').attributes("data-name")).toBe("read:const after = 2;\n"),
    );
    wrapper.unmount();
  });

  it("gives the diff on screen the reading of its own file, not one that arrived late", async () => {
    // What the component waits for is a dynamic import and a read of the file, so one file's answer can
    // land after the user has opened another. A highlighter handed over for the file that was left
    // behind is worse than none: the library would be told a highlighter that reads a file which is
    // not the one it is about to be given, and this diff would be drawn with the wrong syntax.
    let releaseFirst = () => {};
    mocks.held.set(
      "const after = 2;\n",
      new Promise<void>((resolve) => {
        releaseFirst = resolve;
      }),
    );
    const wrapper = mountDiff({});
    await vi.waitFor(() => expect(mocks.prepared).toHaveLength(1));
    expect(wrapper.find('[data-testid="diff-highlighter"]').attributes("data-name")).toBe("");

    mocks.getGitDiff.mockResolvedValue({
      path: "src/other.ts",
      patch: "diff --git a/src/other.ts b/src/other.ts\n@@ -1 +1 @@\n-old\n+new\n",
      revision: "src/other.ts#1",
      oldContent: "const other = 1;\n",
      newContent: "const other = 2;\n",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -1 +1 @@" }],
    });
    openedPath.value = "src/other.ts";
    await vi.waitFor(() =>
      expect(wrapper.find('[data-testid="diff-highlighter"]').attributes("data-name")).toBe("read:const other = 2;\n"),
    );

    // The file the user left behind is answered at last, and the diff on screen keeps what it has.
    releaseFirst();
    await flushPromises();
    expect(wrapper.find('[data-testid="diff-highlighter"]').attributes("data-name")).toBe("read:const other = 2;\n");
    wrapper.unmount();
  });

  it("gives a diff read again the reading of that reading, not the one before it", async () => {
    // The same file, read twice, is two answers. The one already on screen is not handed the second
    // reading's highlighter: a diff is drawn from the diff object it was read for, and any other
    // reading is of text the patch it was drawn from never saw.
    const wrapper = mountDiff({});
    await vi.waitFor(() =>
      expect(wrapper.find('[data-testid="diff-highlighter"]').attributes("data-name")).toBe("read:const after = 2;\n"),
    );

    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "diff --git a/src/app.ts b/src/app.ts\n@@ -1 +1 @@\n-old\n+newer\n",
      revision: "src/app.ts#6",
      oldContent: "const before = 1;\n",
      newContent: "const after = 3;\n",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -1 +1 @@" }],
    });
    let releaseSecond = () => {};
    mocks.held.set(
      "const after = 3;\n",
      new Promise<void>((resolve) => {
        releaseSecond = resolve;
      }),
    );
    openedCheckout.value = { ...checkout, id: "checkout:two" };
    await wrapper.setProps({});
    await vi.waitFor(() => expect(mocks.prepared).toHaveLength(2));
    // The reading of the new diff has not landed, so the diff on screen is drawn without one rather
    // than with the previous diff's.
    expect(wrapper.find('[data-testid="diff-highlighter"]').attributes("data-name")).toBe("");

    releaseSecond();
    await flushPromises();
    expect(wrapper.find('[data-testid="diff-highlighter"]').attributes("data-name")).toBe("read:const after = 3;\n");
    wrapper.unmount();
  });

  it("passes the detected grammar for SVG files to the library", async () => {
    mocks.getGitDiff.mockResolvedValue({
      path: "assets/logo.svg",
      patch: "diff --git a/assets/logo.svg b/assets/logo.svg\n@@ -1 +1 @@\n-old\n+new\n",
      revision: "assets/logo.svg#1",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -1 +1 @@" }],
    });
    const wrapper = mountDiff({ path: "assets/logo.svg" });
    await flushPromises();
    // SVG is XML, and the shared source-language table tells both editor and diff to use that grammar.
    for (const file of mocks.diffFiles) {
      expect(file.oldLang).toBe("xml");
      expect(file.newLang).toBe("xml");
    }
    wrapper.unmount();
  });

  it("paints what is open in the theme in effect, and repaints it when the theme changes", async () => {
    const wrapper = mountDiff({});
    await flushPromises();
    const hunk = () => wrapper.getComponent({ name: "DiffView" }).props("diffFile") as DiffFileStub;
    expect(hunk().palette).toBe("dark");

    // A diff already on screen is told, rather than rebuilt: the library reads the theme off the
    // file it was handed, so a switch that only reached a new diff would leave this one painted in
    // the palette it was opened in.
    theme.value = "light";
    await flushPromises();
    expect(hunk().palette).toBe("light");
    expect(hunk().notified).toBe(1);
    wrapper.unmount();
    theme.value = "dark";
  });

  it("does not redraw a diff that git reports unchanged", async () => {
    const gitSnapshot = snapshot();
    const wrapper = mountDiff({ gitSnapshot });
    await flushPromises();

    const diffFile = () => wrapper.getComponent({ name: "DiffView" }).props("diffFile");
    const first = diffFile();
    expect(first).not.toBeNull();

    // Git reports every write in the workdir, not only in the file on screen, so a refresh lands
    // here after any save anywhere. It has to hand the view the same file: a new one takes the
    // open note composer down with it, which closed the composer the moment typing began. The
    // refresh is debounced, so the wait is for the timer rather than for the microtask queue.
    gitSnapshot.statusRevision += 1;
    await vi.waitFor(() => expect(mocks.getGitDiff).toHaveBeenCalledTimes(2));
    expect(diffFile()).toBe(first);

    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "diff --git a/src/app.ts b/src/app.ts\n@@ -1 +1 @@\n-old\n+other\n",
      revision: "src/app.ts#7",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -1 +1 @@" }],
    });
    gitSnapshot.statusRevision += 1;
    await vi.waitFor(() => expect(diffFile()).not.toBe(first));
    // A diff that did move is redrawn, or the view would keep showing lines git no longer has.
    expect(diffFile()).not.toBe(first);
    wrapper.unmount();
  });

  it("hides the header of a diff nested in the change-set stack, so the send is only one", async () => {
    const wrapper = mountDiff({ path: null });
    await flushPromises();
    await wrapper.get(".diff-file-header").trigger("click");
    await vi.waitFor(() => expect(wrapper.findAll('[data-testid="diff-view"]')).toHaveLength(1));
    // The nested diff is a diff of this same component, and the row above it already names the file.
    expect(wrapper.findAll("header")).toHaveLength(1);
    expect(wrapper.findAll('[data-testid="send-review"]')).toHaveLength(1);
    wrapper.unmount();
  });

  it("sends the notes that still need to reach the agent, and counts them", async () => {
    const review = reviewApi([
      note({ id: "note:1", status: "draft" }),
      note({ id: "note:2", status: "sent" }),
      // A resolved note is settled, and an outdated one is held back until asked for.
      note({ id: "note:3", status: "resolved" }),
      note({ id: "note:4", status: "draft", outdated: true }),
    ]);
    const stub = senderStub();
    const wrapper = mountDiff({ review }, stub.sender);
    await flushPromises();

    expect(wrapper.get('[data-testid="send-count"]').text()).toBe("2 notes · 1 draft");
    await wrapper.get('[data-testid="send-review"]').trigger("click");
    expect(stub.send).toHaveBeenCalledWith(["note:1", "note:2"], false);
    wrapper.unmount();
  });

  it("cannot send what the agent has nothing left to hear about", async () => {
    const stub = senderStub();
    const wrapper = mountDiff({ review: reviewApi([note({ id: "note:1", status: "resolved" })]) }, stub.sender);
    await flushPromises();

    expect(wrapper.get('[data-testid="send-review"]').attributes("disabled")).toBeDefined();
    expect(wrapper.find('[data-testid="send-count"]').exists()).toBe(false);
    expect(stub.send).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("holds an outdated note back until the checkbox asks for it", async () => {
    const review = reviewApi([note({ id: "note:1", status: "draft", outdated: true })]);
    const stub = senderStub();
    const wrapper = mountDiff({ review }, stub.sender);
    await flushPromises();

    expect(wrapper.get('[data-testid="send-review"]').attributes("disabled")).toBeDefined();
    await wrapper.get('[data-testid="include-outdated"]').setValue(true);
    await wrapper.get('[data-testid="send-review"]').trigger("click");
    expect(stub.send).toHaveBeenCalledWith(["note:1"], false);
    wrapper.unmount();
  });

  it("offers a destination only above one session, and reports rounds left unconfirmed", async () => {
    const one = senderStub([session("ses_one")]);
    const single = mountDiff({ review: reviewApi([note()]) }, one.sender);
    await flushPromises();
    expect(single.find('[data-testid="send-target"]').exists()).toBe(false);
    single.unmount();

    const longTitle = "Review this session with a descriptive title longer than one line";
    const many = senderStub([session("ses_one"), session("ses_two", { title: longTitle })], "ses_two", 2);
    const wrapper = mountDiff({ review: reviewApi([note()]) }, many.sender);
    await flushPromises();

    expect(wrapper.get('[data-testid="send-review"]').classes()).toContain("muster-button-md");
    expect(wrapper.get('[data-testid="send-target"]').classes()).toContain("muster-select");
    expect(wrapper.find(".session-target-menu").exists()).toBe(true);
    expect(wrapper.get('[data-testid="send-target"]').attributes("title")).toBe(longTitle);
    const options = wrapper.findAll(SESSION_OPTION);
    expect(options.map((option) => option.text())).toEqual(["ses_one", longTitle]);
    expect(options[1].attributes("title")).toBe(longTitle);
    expect(options[1].get(".session-target-label").text()).toBe(longTitle);
    expect(options[1].classes()).toContain("session-target-option");
    // The chosen one is the one that carries the check, so the list says where it already points.
    expect(options[1].attributes("aria-selected")).toBe("true");
    expect(options[1].find("svg").exists()).toBe(true);
    expect(options[0].attributes("aria-selected")).toBe("false");
    expect(wrapper.get('[data-testid="unfinished-rounds"]').text()).toBe("2 rounds not finished");

    await options[0].trigger("click");
    expect(many.selectTarget).toHaveBeenCalledWith("ses_one");
    wrapper.unmount();
  });

  it("exports the notes as markdown without a session, a busy dialog, or a round count", async () => {
    const busy = session("ses_one", { title: "review one", running: true, idleAt: null });
    const stub = senderStub([busy], "ses_one", 3, "markdown");
    const wrapper = mountDiff({ review: reviewApi([note()]) }, stub.sender);
    await flushPromises();

    expect(wrapper.find('[data-testid="send-target"]').exists()).toBe(false);
    expect(wrapper.find('[data-testid="send-busy"]').exists()).toBe(false);
    expect(wrapper.find('[data-testid="unfinished-rounds"]').exists()).toBe(false);
    expect(wrapper.get('[data-testid="send-review"]').text()).toBe("Export as Markdown");
    await wrapper.get('[data-testid="send-review"]').trigger("click");
    expect(stub.send).toHaveBeenCalledWith(["note:1"], false);
    wrapper.unmount();
  });

  it("saves a selected diff range and keeps its range in Markdown export", async () => {
    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "@@ -1,3 +1,3 @@\n first\n-second\n+second changed\n third\n",
      revision: "src/app.ts#8",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 5,
      hunks: [{ startLine: 0, endLine: 5, title: "@@ -1,3 +1,3 @@" }],
    });
    const review = reviewApi();
    const wrapper = mountDiff({ review });
    await flushPromises();

    await wrapper.get('[data-testid="select-diff-range"]').trigger("click");
    expect(wrapper.get('form[aria-label="New review note"]').text()).toContain("new lines 1-3");
    await wrapper.get('textarea[aria-label="Review note"]').setValue("keep these lines together");
    await wrapper.get('form[aria-label="New review note"]').trigger("submit");
    await flushPromises();

    expect(review.addNote).toHaveBeenCalledWith({
      path: "src/app.ts",
      side: "new",
      lineStart: 1,
      lineEnd: 3,
      content: "keep these lines together",
      code: "first\nsecond changed\nthird",
    });
    const saved = review.addNote.mock.calls[0][0];
    const markdown = buildReviewMarkdown(
      [
        note({
          lineStart: saved.lineStart,
          lineEnd: saved.lineEnd ?? null,
          code: saved.code,
          content: saved.content,
        }),
      ],
      { date: "2026-03-14" },
    );
    expect(markdown).toContain("```ts{1-3}\nfirst\nsecond changed\nthird\n```");
    wrapper.unmount();
  });

  it("lets the composer move the top of a range without moving the line the note ends on", async () => {
    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "@@ -1,3 +1,3 @@\n first\n-second\n+second changed\n third\n",
      revision: "src/app.ts#9",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 5,
      hunks: [{ startLine: 0, endLine: 5, title: "@@ -1,3 +1,3 @@" }],
    });
    const review = reviewApi();
    const wrapper = mountDiff({ review });
    await flushPromises();

    // The library picked lines 1-3, so the note ends on line 3 whatever the composer says, and the
    // picker offers every line of the run it could start on, marked as the diff marks them: the
    // two it left alone are on both sides, and the one it added only on the new side.
    await wrapper.get('[data-testid="select-diff-range"]').trigger("click");
    const form = () => wrapper.get('form[aria-label="New review note"]');
    const options = () =>
      form()
        .findAll('[role="option"]')
        .map((row) => row.element.textContent);
    expect(options()).toEqual([" 1", "+2", " 3"]);

    await wrapper.get('textarea[aria-label="Review note"]').setValue("only the last two lines");
    await form().findAll('[role="option"]')[options().indexOf("+2") as number].trigger("click");
    expect(form().text()).toContain("new lines 2-3");

    // The band the library draws is asked to follow the range, which is the one thing the composer
    // cannot do itself: it is the library's own state and it takes no prop for it.
    expect(mocks.updateSelectionVisual).toHaveBeenLastCalledWith(
      expect.anything(),
      { side: "new", startLineNumber: 2, endLineNumber: 3 },
      expect.anything(),
    );

    await form().trigger("submit");
    await flushPromises();

    expect(review.addNote).toHaveBeenCalledWith({
      path: "src/app.ts",
      side: "new",
      lineStart: 2,
      lineEnd: 3,
      content: "only the last two lines",
      code: "second changed\nthird",
    });

    // A saved note leaves no range drawn behind it.
    expect(mocks.updateSelectionVisual).toHaveBeenLastCalledWith(expect.anything(), null, expect.anything());
    wrapper.unmount();
  });

  it("shows one line rather than a range the diff library handed over backwards", async () => {
    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "@@ -1,3 +1,3 @@\n first\n-second\n+second changed\n third\n",
      revision: "src/app.ts#10",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 5,
      hunks: [{ startLine: 0, endLine: 5, title: "@@ -1,3 +1,3 @@" }],
    });
    const review = reviewApi();
    const wrapper = mountDiff({ review });
    await flushPromises();

    // Both ends come from the library, so a range that arrives upside down is not ours to trust.
    // What the composer shows has to be what gets saved, or it shows `lines 3-1` and quietly
    // files a note about line 3 alone.
    await wrapper.get('[data-testid="select-inverted-range"]').trigger("click");
    expect(wrapper.get('form[aria-label="New review note"]').text()).toContain("new line 1");

    await wrapper.get('textarea[aria-label="Review note"]').setValue("the first line alone");
    await wrapper.get('form[aria-label="New review note"]').trigger("submit");
    await flushPromises();

    expect(review.addNote).toHaveBeenCalledWith({
      path: "src/app.ts",
      side: "new",
      lineStart: 1,
      content: "the first line alone",
      code: "first",
    });
    wrapper.unmount();
  });

  it("lets the checkout choose a review destination", async () => {
    const stub = senderStub([session("ses_one")], "ses_one", 0, "markdown");
    const wrapper = mountDiff({ review: reviewApi([note()]) }, stub.sender);
    await flushPromises();

    // The rows render without the list being opened, and they are found through the listbox's
    // own name because the session menu above offers options of its own.
    await wrapper
      .findAll('[aria-label="Review destination"] [role="option"]')
      .find((row) => row.text() === "OpenCode")!
      .trigger("click");
    expect(stub.selectReviewTarget).toHaveBeenCalledWith("opencode");
    expect(wrapper.get('[data-testid="send-review"]').text()).toBe("Send to opencode");
    wrapper.unmount();
  });

  it("the markdown target does not change which notes are included", async () => {
    const review = reviewApi([
      note({ id: "draft" }),
      note({ id: "sent", status: "sent" }),
      note({ id: "resolved", status: "resolved" }),
      note({ id: "outdated", outdated: true }),
    ]);
    const stub = senderStub([], null, 0, "markdown");
    const wrapper = mountDiff({ review }, stub.sender);
    await flushPromises();

    await wrapper.get('[data-testid="send-review"]').trigger("click");
    expect(stub.send).toHaveBeenLastCalledWith(["draft", "sent"], false);
    await wrapper.get('[data-testid="include-outdated"]').setValue(true);
    await wrapper.get('[data-testid="send-review"]').trigger("click");
    expect(stub.send).toHaveBeenLastCalledWith(["draft", "sent", "outdated"], false);
    wrapper.unmount();
  });

  it("lists the newest session first, and only searches a list long enough to need it", async () => {
    const two = senderStub([
      session("ses_old", { title: "old work", updatedAt: 1 }),
      session("ses_new", { title: "new work", updatedAt: 5 }),
    ]);
    const short = mountDiff({ review: reviewApi([note()]) }, two.sender);
    await flushPromises();
    // A send goes to the session that was last used, so the newest one leads the list.
    expect(short.findAll(SESSION_OPTION).map((option) => option.text())).toEqual(["new work", "old work"]);
    expect(short.find('input[aria-label="Search sessions"]').exists()).toBe(false);
    short.unmount();

    const sessions = Array.from({ length: 6 }, (_, index) =>
      session(`ses_${index}`, { title: index === 3 ? "CanvasForm work" : `session ${index}`, updatedAt: index }),
    );
    const six = senderStub(sessions, "ses_0");
    const wrapper = mountDiff({ review: reviewApi([note()]) }, six.sender);
    await flushPromises();

    expect(wrapper.findAll(SESSION_OPTION).map((option) => option.text())).toEqual([
      "session 5",
      "session 4",
      "CanvasForm work",
      "session 2",
      "session 1",
      "session 0",
    ]);
    const search = wrapper.get('input[aria-label="Search sessions"]');
    await search.setValue("canvas");
    expect(wrapper.findAll(SESSION_OPTION).map((option) => option.text())).toEqual(["CanvasForm work"]);
    await search.setValue("nothing");
    expect(wrapper.findAll(SESSION_OPTION)).toHaveLength(0);
    expect(wrapper.get(".menu-note").text()).toBe('No session matches "nothing".');
    wrapper.unmount();
  });

  it("asks what to do when the target is mid-task, and can be talked out of it", async () => {
    const busy = session("ses_one", { title: "review one", running: true, idleAt: null });
    const stub = senderStub([busy]);
    const wrapper = mountDiff({ review: reviewApi([note()]) }, stub.sender);
    await flushPromises();

    await wrapper.get('[data-testid="send-review"]').trigger("click");
    // The agent cannot be interrupted, so the click opens the choice instead of sending.
    expect(stub.send).not.toHaveBeenCalled();
    const choice = wrapper.get('[data-testid="send-busy"]');
    expect(choice.text()).toContain("“review one” is mid-task. Sending now lands inside its current turn");
    expect(choice.text()).toContain("This OpenCode version cannot cancel a turn.");

    await wrapper.get('[data-testid="send-not-now"]').trigger("click");
    expect(wrapper.find('[data-testid="send-busy"]').exists()).toBe(false);
    expect(stub.send).not.toHaveBeenCalled();

    await wrapper.get('[data-testid="send-review"]').trigger("click");
    await wrapper.get('[data-testid="send-queue"]').trigger("click");
    expect(stub.send).toHaveBeenCalledWith(["note:1"], true);
    expect(wrapper.find('[data-testid="send-busy"]').exists()).toBe(false);

    // Landing inside the turn is the other answer, and it is the one the default offers.
    await wrapper.get('[data-testid="send-review"]').trigger("click");
    await wrapper.get('[data-testid="send-now"]').trigger("click");
    expect(stub.send).toHaveBeenLastCalledWith(["note:1"], false);
    wrapper.unmount();
  });

  it("sends straight away when the target is free", async () => {
    const stub = senderStub([session("ses_one"), session("ses_two")]);
    const wrapper = mountDiff({ review: reviewApi([note()]) }, stub.sender);
    await flushPromises();

    await wrapper.get('[data-testid="send-review"]').trigger("click");
    expect(stub.send).toHaveBeenCalledWith(["note:1"], false);
    expect(wrapper.find('[data-testid="send-busy"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("asks the backend once per set of anchor checks, not once per answer", async () => {
    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "@@ -1,2 +1,2 @@\n const a = 1;\n+const b = 2;\n",
      revision: "src/app.ts#11",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 4,
      hunks: [{ startLine: 0, endLine: 4, title: "@@ -1,2 +1,2 @@" }],
    });
    const review = reviewApi([note()]);
    review.verifyAnchors.mockImplementation(async () => {
      review.notes = [...review.notes];
      return true;
    });

    const wrapper = mount(FileDiff, {
      props: { checkout, gitSnapshot: snapshot(), review, path: "src/app.ts", scrollTop: 0 },
    });
    for (let tick = 0; tick < 10; tick += 1) await flushPromises();

    expect(review.verifyAnchors).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it("asks again once the diff's own text moves a note's anchor", async () => {
    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "@@ -1,2 +1,2 @@\n const a = 1;\n+const b = 2;\n",
      revision: "src/app.ts#12",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 4,
      hunks: [{ startLine: 0, endLine: 4, title: "@@ -1,2 +1,2 @@" }],
    });
    const review = reviewApi([note({ lineStart: 2, code: "const b = 2;" })]);
    review.verifyAnchors.mockImplementation(async () => {
      review.notes = [...review.notes];
      return true;
    });
    const git = snapshot();
    const wrapper = mount(FileDiff, {
      props: { checkout, gitSnapshot: git, review, path: "src/app.ts", scrollTop: 0 },
    });
    for (let tick = 0; tick < 5; tick += 1) await flushPromises();
    expect(review.verifyAnchors).toHaveBeenCalledTimes(1);

    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "@@ -1,2 +1,2 @@\n const a = 1;\n+const b = 3;\n",
      revision: "src/app.ts#13",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 4,
      hunks: [{ startLine: 0, endLine: 4, title: "@@ -1,2 +1,2 @@" }],
    });
    git.statusRevision += 1;
    await wrapper.setProps({ gitSnapshot: git });
    await new Promise((resolve) => setTimeout(resolve, 500));
    await flushPromises();

    expect(review.verifyAnchors).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });

  it("draws nothing to send when no shell is above the diff", async () => {
    const wrapper = mount(FileDiff, {
      props: {
        checkout,
        gitSnapshot: snapshot(),
        review: reviewApi([note()]),
        path: "src/app.ts",
        scrollTop: 0,
      },
    });
    await flushPromises();
    expect(wrapper.find('[data-testid="send-review"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("only offers to resolve a note the diff can back, and only after it was delivered", async () => {
    // Hunk mode: the whole patch is in memory, so an absent anchor really is a deleted line.
    mocks.getGitDiff.mockResolvedValue({
      path: "src/app.ts",
      patch: "@@ -1,2 +1,2 @@\n const a = 1;\n+const b = 2;\n",
      revision: "src/app.ts#14",
      isBinary: false,
      large: false,
      tooLarge: false,
      totalLines: 3,
      hunks: [{ startLine: 0, endLine: 3, title: "@@ -1,2 +1,2 @@" }],
    });
    const review = reviewApi([
      note({ id: "note:sent", status: "sent" }),
      note({ id: "note:draft", status: "draft" }),
      note({ id: "note:drifted", status: "sent", outdated: true }),
    ]);
    const wrapper = mountDiff({ review });
    await vi.waitFor(() => expect(wrapper.text()).toContain("revisit this calculation"));

    // Without a verdict the button is not offered: marking something resolved has to mean
    // something was checked, and a draft was never delivered in the first place.
    expect(wrapper.findAll('[aria-label^="Mark note on line"]')).toHaveLength(1);
    expect(wrapper.get('[aria-label="Mark note on line 1 as resolved"]')).toBeTruthy();
    expect(wrapper.get('[data-testid="diff-view"]').text()).toContain("The line this note points at is unchanged.");
    wrapper.unmount();
  });
});
