import { describe, expect, it } from "vitest";
import { ALL_CHANGES_LABEL, mainViewFromState, mainViewLabel, mainViewToState, resolveMainView } from "./main-document";
import type { MainView } from "./main-document";

const terminal: MainView = { kind: "terminal", sessionId: "session:one" };
const document: MainView = { kind: "document", path: "docs/guide.md", mode: "view", origin: "checkout" };
const reviewDocument: MainView = {
  kind: "document",
  path: "2026-03-14-1532.md",
  mode: "view",
  origin: "review-default",
};
const fileDiff: MainView = { kind: "diff", path: "src/main.rs" };
const allDiff: MainView = { kind: "diff", path: null };

describe("checkout-scoped main views", () => {
  it("keeps one view per checkout and falls back to the terminal", () => {
    const mainViews = { "checkout:docs": document, "checkout:terminal": terminal };
    expect(resolveMainView(mainViews, "checkout:docs")).toBe(document);
    expect(resolveMainView(mainViews, "checkout:terminal")).toBe(terminal);
    // Nothing saved yet, or no checkout at all, is the terminal rather than a guess.
    expect(resolveMainView(mainViews, "checkout:unknown")).toEqual({ kind: "terminal", sessionId: null });
    expect(resolveMainView(mainViews, null)).toEqual({ kind: "terminal", sessionId: null });
  });

  it("names the view the last crumb shows", () => {
    expect(mainViewLabel(terminal, "Terminal 1")).toBe("Terminal 1");
    expect(mainViewLabel({ kind: "terminal", sessionId: null }, null)).toBe("Terminal");
    expect(mainViewLabel(document, "Terminal 1")).toBe("docs/guide.md");
    expect(mainViewLabel(fileDiff, "Terminal 1")).toBe("src/main.rs");
    expect(mainViewLabel(allDiff, "Terminal 1")).toBe(ALL_CHANGES_LABEL);
  });

  it("round-trips every view through the persisted state", () => {
    for (const view of [document, fileDiff, allDiff]) {
      const state = mainViewToState(view, "checkout:one");
      // What Rust validates: a known mainView, and a document that is a safe relative path.
      if (state.mainView === "document") {
        expect(state.document).toMatchObject({ checkoutId: "checkout:one" });
        expect(state.document!.path).not.toBe("");
      } else {
        expect(state.document).toBeNull();
      }
      expect(mainViewFromState(state)).toEqual(view);
    }
  });

  it.each(["review-default", "review-workdir"] as const)(
    "round-trips a %s document through the persisted state",
    (origin) => {
      const view: MainView = { ...reviewDocument, origin };
      const state = mainViewToState(view, "checkout:one");
      expect(state.document).toMatchObject({ origin, path: "2026-03-14-1532.md" });
      expect(mainViewFromState(state)).toEqual(view);
    },
  );

  it("saves the terminal as the terminal, without the session the workspace already owns", () => {
    expect(mainViewToState(terminal, "checkout:one")).toEqual({
      mainView: "terminal",
      document: null,
      diffAllFiles: false,
    });
    expect(mainViewFromState(mainViewToState(terminal, "checkout:one"))).toEqual({
      kind: "terminal",
      sessionId: null,
    });
  });

  it("reads a change set saved as its own flag, and a terminal for anything unreadable", () => {
    expect(mainViewFromState({ mainView: "terminal", document: null, diffAllFiles: true })).toEqual(allDiff);
    expect(mainViewFromState({ mainView: "document", document: null, diffAllFiles: false })).toEqual({
      kind: "terminal",
      sessionId: null,
    });
    // A file document is told apart from a file diff by its mode.
    expect(
      mainViewFromState({
        mainView: "document",
        document: {
          checkoutId: "checkout:one",
          path: "src/main.rs",
          origin: "checkout",
          source: "change",
          mode: "diff",
        },
        diffAllFiles: false,
      }),
    ).toEqual(fileDiff);
  });
});
