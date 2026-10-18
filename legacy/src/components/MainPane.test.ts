// @vitest-environment happy-dom
// Test doubles intentionally omit production prop defaults: this file asserts the class each pane is
// handed, and a real terminal would be a whole PTY to stand in for nothing.
import { mount } from "@vue/test-utils";
import { defineComponent, reactive } from "vue";
import { describe, expect, it, vi } from "vitest";
import type { Checkout } from "../domain/workspace";
import type { ActiveGitSnapshot } from "../presentation/active-git-snapshot";
import MainPane from "./MainPane.vue";

// The terminal pane drags xterm in with it, which needs a browser global this file has no use for.
// Only the class it is handed is asserted here, so the pane behind it is a name and a box.
vi.mock("./SessionPane.vue", () => ({
  default: defineComponent({ name: "SessionPane", render: () => null }),
}));

/**
 * The panel hands each pane a box, and how it says so is a contract rather than a style choice.
 *
 * Tailwind emits `.absolute` before `.relative` in the same layer at equal specificity, so a pane
 * whose own root says `relative` silently overrules an `absolute inset-0` handed down to it. Its
 * own `relative` is what the overlay scrollbars' absolutely positioned rails need an ancestor for,
 * so it is not going away, and `inset-0` under `position: relative` only offsets a box: it does not
 * stretch one. The pane is left content-sized, its scroll container has nothing to scroll in, the
 * document is drawn at full length under a panel that clips it, and the wheel turns nothing.
 *
 * So a pane that says `relative` has to be given a height, and one that does not can keep taking
 * `absolute inset-0`. Nothing here can measure a height — happy-dom has no layout engine — so what
 * is pinned is the class each pane is handed, which is the half that actually decides it.
 */

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

function snapshot(checkoutId: string): ActiveGitSnapshot {
  return reactive<ActiveGitSnapshot>({
    checkoutId,
    status: { branch: "feature", defaultBranch: "main", aheadCount: 0, files: [] },
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

/** The panel with every pane inside it stubbed, so what is asserted is what it hands each one. */
function mountPanel(view: "document" | "diff" | "terminal") {
  const activeCheckout = checkout("checkout:one");
  return mount(MainPane, {
    props: {
      checkout: activeCheckout,
      checkouts: [activeCheckout],
      view:
        view === "document"
          ? ({ kind: "document", path: "src/app.ts", mode: "code", origin: "checkout" } as const)
          : view === "diff"
            ? ({ kind: "diff", path: "src/app.ts" } as const)
            : ({ kind: "terminal", sessionId: null } as const),
      ready: true,
      gitSnapshot: snapshot(activeCheckout.id),
      review: {
        notes: [],
        addNote: async () => true,
        updateNote: async () => true,
        deleteNote: async () => true,
        verifyAnchors: async () => true,
        clearOutdated: async () => true,
        resolveNote: async () => true,
      },
      activeSessionId: null,
      isOpening: false,
    },
    shallow: true,
  });
}

describe("MainPane", () => {
  it("gives the document pane a height, because its own root would overrule a position", () => {
    const document = mountPanel("document").getComponent({ name: "DocumentPane" });

    expect(document.classes()).toContain("h-full");
    expect(document.classes()).not.toContain("absolute");
  });

  it("gives the diff pane the height, for the reason the document pane's does", () => {
    const diff = mountPanel("diff").getComponent({ name: "FileDiff" });

    expect(diff.classes()).toContain("h-full");
    expect(diff.classes()).not.toContain("absolute");
  });

  it("still gives the terminal a position, because its own root does not overrule one", () => {
    const terminal = mountPanel("terminal").getComponent({ name: "SessionPane" });

    expect(terminal.classes()).toContain("absolute");
    expect(terminal.classes()).toContain("inset-0");
  });

  it("passes the terminal order up, because the sidebar draws this list and nothing else", () => {
    // The pane below is the only place a checkout's terminal order is decided. This component did
    // not carry the event any further, so the sidebar fell back to the order the database hands out
    // forever: a reorder was saved to the layout, and then drawn from somewhere else entirely —
    // persisted, correct, and a drag that did not move anything.
    const wrapper = mountPanel("terminal");
    wrapper.getComponent({ name: "SessionPane" }).vm.$emit("sessionOrder", "checkout:one", ["b", "a"]);

    expect(wrapper.emitted("sessionOrder")).toEqual([["checkout:one", ["b", "a"]]]);
  });
});
