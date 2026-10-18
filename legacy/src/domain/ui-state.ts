import type { MainDocument, MainViewState } from "./main-document";

/**
 * How the window is arranged, and nothing else.
 *
 * These are the shapes a person arrives at by dragging and then leaves alone, which is why they
 * live in the database next to the workspace they belong to. The preferences (sizes, ligatures,
 * the scale) are in `src/domain/settings.ts`, in `~/.muster/config.yml`.
 *
 * No version rides along: `SCHEMA_VERSION` is the only ladder in the database, so a shape this
 * build cannot read is the default window, never a version someone gets to compare.
 */
export interface AppLayoutState {
  mode: "focus" | "split";
  sidebarWidth: number;
  inspectorWidth: number;
  previewWidth: number;
}

export interface CheckoutUiState extends MainViewState {
  inspectorTab: "files" | "changes";
  selectedFilePath: string | null;
  selectedChangePath: string | null;
  expandedDirectories: string[];
  filesScrollTop: number;
  changesScrollTop: number;
  documentScrollTop: number;
  documentScrollLeft: number;
  diffScrollTop: number;
}

export const SIDEBAR_WIDTH_LIMITS = { min: 240, max: 500 } as const;
export const INSPECTOR_WIDTH_LIMITS = { min: 200, max: 480 } as const;
export const PREVIEW_WIDTH_LIMITS = { min: 260, max: 900 } as const;
/** What the main panel needs before the inspector can no longer sit beside it. */
const MIN_MAIN_WIDTH = 430;

export const DEFAULT_APP_LAYOUT: AppLayoutState = {
  mode: "focus",
  sidebarWidth: 240,
  inspectorWidth: 280,
  previewWidth: 360,
};

export const DEFAULT_CHECKOUT_UI_STATE: CheckoutUiState = {
  document: null,
  mainView: "terminal",
  diffAllFiles: false,
  inspectorTab: "files",
  selectedFilePath: null,
  selectedChangePath: null,
  expandedDirectories: [],
  filesScrollTop: 0,
  changesScrollTop: 0,
  documentScrollTop: 0,
  documentScrollLeft: 0,
  diffScrollTop: 0,
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function boundedNumber(value: unknown, fallback: number, min: number, max: number): number {
  return typeof value === "number" && Number.isFinite(value) ? Math.min(max, Math.max(min, value)) : fallback;
}

function safePath(value: unknown): value is string {
  if (typeof value !== "string" || value.length === 0 || value.length > 4096 || value.startsWith("/")) return false;
  return value.split(/[\\/]/).every((part) => part !== ".." && part !== "." && part.length > 0);
}

function safeReviewPath(value: unknown): value is string {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.length > 4096 ||
    value.startsWith("/") ||
    /^[A-Za-z]:/.test(value) ||
    !value.endsWith(".md") ||
    value.includes("\\") ||
    value.includes("\0")
  )
    return false;
  return value.split("/").every((part) => part !== ".." && part !== "." && part.length > 0);
}

/**
 * Reads a saved layout back into the shape this build writes.
 *
 * There is no version to compare here, so a shape this build does not read is the default window,
 * and a dimension that arrives corrupt is clamped into the range a pane can actually be dragged
 * to. That clamp is validation of the number in hand, not a reading of an older row.
 */
export function normalizeAppLayout(value: unknown): AppLayoutState {
  if (!isRecord(value)) return { ...DEFAULT_APP_LAYOUT };
  return {
    mode: value.mode === "split" ? "split" : "focus",
    sidebarWidth: boundedNumber(
      value.sidebarWidth,
      DEFAULT_APP_LAYOUT.sidebarWidth,
      SIDEBAR_WIDTH_LIMITS.min,
      SIDEBAR_WIDTH_LIMITS.max,
    ),
    inspectorWidth: boundedNumber(
      value.inspectorWidth,
      DEFAULT_APP_LAYOUT.inspectorWidth,
      INSPECTOR_WIDTH_LIMITS.min,
      INSPECTOR_WIDTH_LIMITS.max,
    ),
    previewWidth: boundedNumber(
      value.previewWidth,
      DEFAULT_APP_LAYOUT.previewWidth,
      PREVIEW_WIDTH_LIMITS.min,
      PREVIEW_WIDTH_LIMITS.max,
    ),
  };
}

/**
 * Reads a saved checkout state back into the shape this build writes.
 *
 * Every field has its own reading: a flag falls to the flag that means the same thing, a path
 * that is not safe to open is not opened, and a shape that is not an object at all is the
 * default state. None of that is a version comparison, because there is no version to compare.
 */
export function normalizeCheckoutUiState(value: unknown): CheckoutUiState {
  if (!isRecord(value)) return { ...DEFAULT_CHECKOUT_UI_STATE };
  const rawDocument = value.document;
  const document =
    isRecord(rawDocument) &&
    typeof rawDocument.path === "string" &&
    (rawDocument.origin === "review-default" ||
      rawDocument.origin === "review-workdir" ||
      rawDocument.origin === "checkout") &&
    (rawDocument.origin === "checkout" ? safePath(rawDocument.path) : safeReviewPath(rawDocument.path)) &&
    typeof rawDocument.checkoutId === "string" &&
    (rawDocument.source === "file" || rawDocument.source === "change") &&
    (rawDocument.mode === "diff" || rawDocument.mode === "view" || rawDocument.mode === "code") &&
    (rawDocument.origin === "checkout" || (rawDocument.source === "file" && rawDocument.mode !== "diff"))
      ? ({
          checkoutId: rawDocument.checkoutId,
          path: rawDocument.path,
          origin: rawDocument.origin,
          source: rawDocument.source,
          mode: rawDocument.mode,
        } satisfies MainDocument)
      : null;
  const expandedDirectories = Array.isArray(value.expandedDirectories)
    ? value.expandedDirectories.filter(safePath).slice(0, 2000)
    : [];
  return {
    document,
    mainView: value.mainView === "document" ? "document" : "terminal",
    diffAllFiles: value.diffAllFiles === true,
    inspectorTab: value.inspectorTab === "changes" ? "changes" : "files",
    selectedFilePath: safePath(value.selectedFilePath) ? value.selectedFilePath : null,
    selectedChangePath: safePath(value.selectedChangePath) ? value.selectedChangePath : null,
    expandedDirectories: [...new Set(expandedDirectories)],
    filesScrollTop: boundedNumber(value.filesScrollTop, 0, 0, 10_000_000),
    changesScrollTop: boundedNumber(value.changesScrollTop, 0, 0, 10_000_000),
    documentScrollTop: boundedNumber(value.documentScrollTop, 0, 0, 10_000_000),
    documentScrollLeft: boundedNumber(value.documentScrollLeft, 0, 0, 10_000_000),
    diffScrollTop: boundedNumber(value.diffScrollTop, 0, 0, 10_000_000),
  };
}

export function resizeLayoutPanel(
  layout: AppLayoutState,
  panel: "sidebar" | "inspector" | "preview",
  width: number,
): AppLayoutState {
  if (!Number.isFinite(width)) return layout;
  const limits =
    panel === "sidebar" ? SIDEBAR_WIDTH_LIMITS : panel === "inspector" ? INSPECTOR_WIDTH_LIMITS : PREVIEW_WIDTH_LIMITS;
  const clamped = Math.round(Math.min(limits.max, Math.max(limits.min, width)));
  if (panel === "sidebar") return { ...layout, sidebarWidth: clamped };
  return panel === "inspector" ? { ...layout, inspectorWidth: clamped } : { ...layout, previewWidth: clamped };
}

export function needsInspectorDrawer(layout: AppLayoutState, viewportWidth: number): boolean {
  return viewportWidth < layout.sidebarWidth + layout.inspectorWidth + MIN_MAIN_WIDTH;
}
