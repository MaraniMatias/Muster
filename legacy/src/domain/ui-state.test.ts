import { describe, expect, it } from "vitest";
import {
  DEFAULT_APP_LAYOUT,
  DEFAULT_CHECKOUT_UI_STATE,
  normalizeAppLayout,
  normalizeCheckoutUiState,
  needsInspectorDrawer,
  resizeLayoutPanel,
} from "./ui-state";

describe("persisted UI state", () => {
  it("keeps the muster default panel widths", () => {
    expect(DEFAULT_APP_LAYOUT).toEqual({
      mode: "focus",
      sidebarWidth: 240,
      inspectorWidth: 280,
      previewWidth: 360,
    });
  });

  it("round-trips the saved widths and drops the fields the layout no longer has", () => {
    const saved = {
      ...DEFAULT_APP_LAYOUT,
      mode: "split" as const,
      sidebarWidth: 340,
      inspectorWidth: 420,
      previewWidth: 720,
    };
    expect(normalizeAppLayout(JSON.parse(JSON.stringify(saved)))).toEqual(saved);
    expect(
      normalizeAppLayout({
        ...saved,
        sidebarVisible: false,
        focusSnapshot: { sidebarWidth: 300 },
        collapsedRepoIds: ["repo:/one"],
        reduceTransparency: true,
      }),
    ).toEqual(saved);
  });

  it("clamps corrupt dimensions and falls back for a shape that is not a layout", () => {
    // The clamp is validation of the number in hand, not a reading of an older row: a width nobody
    // could have dragged to is brought into the range a pane can actually be.
    expect(normalizeAppLayout({ mode: "split", sidebarWidth: 900, inspectorWidth: -1, previewWidth: 1200 })).toEqual({
      mode: "split",
      sidebarWidth: 500,
      inspectorWidth: 200,
      previewWidth: 900,
    });
    expect(normalizeAppLayout({ mode: "split", sidebarWidth: 400 })).toEqual({
      ...DEFAULT_APP_LAYOUT,
      mode: "split",
      sidebarWidth: 400,
    });
    expect(normalizeAppLayout({})).toEqual(DEFAULT_APP_LAYOUT);
    expect(normalizeAppLayout(null)).toEqual(DEFAULT_APP_LAYOUT);
    expect(normalizeAppLayout([{ mode: "split" }])).toEqual(DEFAULT_APP_LAYOUT);
    // A mode this build does not name is not a guess either way: the window opens focused.
    expect(normalizeAppLayout({ mode: "zen", sidebarWidth: 300 }).mode).toBe("focus");
  });

  it("reads a layout that still carries fields this build dropped", () => {
    // There is no version left to refuse this on, and nothing in it is unreadable: the widths are
    // what the row is for, so they are honoured and the fields this build does not have are dropped
    // on the way out. Refusing the row whole would only cost somebody the arrangement they saved.
    const saved = {
      mode: "split",
      sidebarWidth: 345,
      inspectorWidth: 450,
      previewWidth: 720,
      terminalScrollbar: "auto",
      zoom: 1.2,
    };
    expect(normalizeAppLayout(JSON.parse(JSON.stringify(saved)))).toEqual({
      mode: "split",
      sidebarWidth: 345,
      inspectorWidth: 450,
      previewWidth: 720,
    });
  });

  it("clamps user resizing to the supported range", () => {
    const resized = resizeLayoutPanel(DEFAULT_APP_LAYOUT, "sidebar", 345.4);
    expect(resized.sidebarWidth).toBe(345);
    expect(resized.inspectorWidth).toBe(DEFAULT_APP_LAYOUT.inspectorWidth);
    expect(resizeLayoutPanel(resized, "sidebar", 10).sidebarWidth).toBe(240);
    expect(resizeLayoutPanel(resized, "inspector", 900).inspectorWidth).toBe(480);
    expect(resizeLayoutPanel(resized, "preview", 100).previewWidth).toBe(260);
    expect(resizeLayoutPanel(resized, "preview", 1000).previewWidth).toBe(900);
    expect(resizeLayoutPanel(resized, "inspector", Number.NaN)).toEqual(resized);
  });

  it("asks for the inspector drawer only when the main panel would not fit", () => {
    const defaults = DEFAULT_APP_LAYOUT;
    expect(needsInspectorDrawer(defaults, 949)).toBe(true);
    expect(needsInspectorDrawer(defaults, 950)).toBe(false);
    const widened = resizeLayoutPanel(defaults, "sidebar", 500);
    expect(needsInspectorDrawer(widened, 1210)).toBe(false);
    expect(needsInspectorDrawer(widened, 1209)).toBe(true);
    expect(widened).toMatchObject({ sidebarWidth: 500, inspectorWidth: 280 });
  });

  it("restores checkout document and inspector state while rejecting unsafe paths", () => {
    const saved = {
      ...DEFAULT_CHECKOUT_UI_STATE,
      document: {
        checkoutId: "checkout:/repo",
        path: "docs/guide.md",
        origin: "checkout",
        source: "file",
        mode: "view",
      },
      mainView: "document",
      inspectorTab: "changes",
      selectedFilePath: "docs/guide.md",
      selectedChangePath: "src/main.rs",
      expandedDirectories: ["docs", "src"],
      filesScrollTop: 640,
      changesScrollTop: 480,
      documentScrollTop: 320,
      diffScrollTop: 900,
    };
    expect(normalizeCheckoutUiState(JSON.parse(JSON.stringify(saved)))).toEqual(saved);
    expect(
      normalizeCheckoutUiState({
        document: { ...saved.document, path: "../../outside" },
        selectedFilePath: "../outside",
        expandedDirectories: ["safe", "a/../outside"],
      }),
    ).toMatchObject({ document: null, selectedFilePath: null, expandedDirectories: ["safe"] });
  });

  it("defaults UI flags when absent, and drops documents without a known origin", () => {
    // The panel flags have defaults, but a document without its required root is not guessable.
    expect(
      normalizeCheckoutUiState({
        mainView: "document",
        document: {
          checkoutId: "checkout:/repo",
          path: "docs/guide.md",
          origin: "checkout",
          source: "file",
          mode: "view",
        },
        inspectorTab: "files",
      }),
    ).toMatchObject({
      mainView: "document",
      diffAllFiles: false,
      document: { path: "docs/guide.md", origin: "checkout", source: "file", mode: "view" },
    });
    expect(
      normalizeCheckoutUiState({
        mainView: "document",
        document: { checkoutId: "checkout:/repo", path: "docs/guide.md", source: "file", mode: "view" },
      }),
    ).toMatchObject({ mainView: "document", document: null });
    expect(
      normalizeCheckoutUiState({
        mainView: "document",
        document: {
          checkoutId: "checkout:/repo",
          path: "2026-03-14-1532.md",
          origin: "review-default",
          source: "file",
          mode: "view",
        },
      }),
    ).toMatchObject({ document: { origin: "review-default", path: "2026-03-14-1532.md" } });
    expect(
      normalizeCheckoutUiState({
        mainView: "document",
        document: {
          checkoutId: "checkout:/repo",
          path: "2026-03-14-1532.md",
          origin: "review-workdir",
          source: "file",
          mode: "view",
        },
      }),
    ).toMatchObject({ document: { origin: "review-workdir" } });
    // The old settings-relative origin cannot be safely restored as either pinned location.
    expect(
      normalizeCheckoutUiState({
        mainView: "document",
        document: {
          checkoutId: "checkout:/repo",
          path: "2026-03-14-1532.md",
          origin: "review",
          source: "file",
          mode: "view",
        },
      }),
    ).toMatchObject({ document: null });
    // An unreadable mainView or flag falls to the terminal rather than failing the load.
    expect(normalizeCheckoutUiState({ mainView: "diff", diffAllFiles: "yes" })).toMatchObject({
      mainView: "terminal",
      diffAllFiles: false,
    });
    expect(normalizeCheckoutUiState({ diffAllFiles: true })).toMatchObject({ diffAllFiles: true });
  });

  it("reads a state that carries a version key this build does not write", () => {
    // There is no version to refuse this on any more: what arrives is what this build can make
    // sense of, and it reads. Refusing the row over a key that means nothing here would only throw
    // away a checkout somebody had arranged.
    const carried = { ...DEFAULT_CHECKOUT_UI_STATE, version: 1, mainView: "document" as const, filesScrollTop: 640 };
    expect(normalizeCheckoutUiState(JSON.parse(JSON.stringify(carried)))).toEqual({
      ...DEFAULT_CHECKOUT_UI_STATE,
      mainView: "document",
      filesScrollTop: 640,
    });
  });

  it("falls back to the default state for a shape that is not a state", () => {
    expect(normalizeCheckoutUiState(null)).toEqual(DEFAULT_CHECKOUT_UI_STATE);
    expect(normalizeCheckoutUiState([])).toEqual(DEFAULT_CHECKOUT_UI_STATE);
    expect(normalizeCheckoutUiState("document")).toEqual(DEFAULT_CHECKOUT_UI_STATE);
  });
});
