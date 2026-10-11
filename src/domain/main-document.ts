export type DocumentMode = "code" | "view";
export type DocumentOrigin = "checkout" | "review-default" | "review-workdir";

/** The copy of the whole-change-set view, shared by the breadcrumb and the diff header. */
export const ALL_CHANGES_LABEL = "All changes";

/** The one thing the main panel shows (D.1). A diff of every changed file has no path. */
export type MainView =
  | { kind: "terminal"; sessionId: string | null }
  | { kind: "document"; path: string; mode: DocumentMode; origin: DocumentOrigin }
  | { kind: "diff"; path: string | null };

/**
 * What `CheckoutUiState` persists about the main panel.
 *
 * The stored shape is Rust's, and its `mainView` only knows a document with a mode, so this
 * projection is what survives a restart. The whole-change-set diff is the one view it cannot
 * name: an empty path is not a safe checkout-relative path and there is no third `mainView`
 * value. So it travels in `diffAllFiles` of its own, which Rust stores as a boolean beside it.
 */
export interface MainViewState {
  mainView: "terminal" | "document";
  document: MainDocument | null;
  diffAllFiles: boolean;
}

export interface MainDocument {
  checkoutId: string;
  path: string;
  origin: DocumentOrigin;
  source: "file" | "change";
  mode: "diff" | DocumentMode;
}

/** The view of the active checkout. Nothing saved for it yet means the terminal. */
export function resolveMainView(mainViews: Record<string, MainView>, checkoutId: string | null): MainView {
  const view = checkoutId ? mainViews[checkoutId] : undefined;
  return view ?? { kind: "terminal", sessionId: null };
}

/** What the last crumb names: the session, the file, or the whole change set. */
export function mainViewLabel(view: MainView, sessionName: string | null): string {
  if (view.kind === "terminal") return sessionName ?? "Terminal";
  if (view.kind === "document") return view.path;
  return view.path ?? ALL_CHANGES_LABEL;
}

export function mainViewToState(view: MainView, checkoutId: string): MainViewState {
  if (view.kind === "terminal") return { mainView: "terminal", document: null, diffAllFiles: false };
  if (view.kind === "document")
    return {
      mainView: "document",
      document: { checkoutId, path: view.path, origin: view.origin, source: "file", mode: view.mode },
      diffAllFiles: false,
    };
  if (view.path === null) return { mainView: "terminal", document: null, diffAllFiles: true };
  return {
    mainView: "document",
    document: { checkoutId, path: view.path, origin: "checkout", source: "change", mode: "diff" },
    diffAllFiles: false,
  };
}

/** Reads a persisted view back. A shape this cannot read is the terminal, never a guess. */
export function mainViewFromState(state: MainViewState): MainView {
  if (state.diffAllFiles) return { kind: "diff", path: null };
  const document = state.document;
  if (state.mainView !== "document" || !document) return { kind: "terminal", sessionId: null };
  return document.mode === "diff"
    ? { kind: "diff", path: document.path }
    : { kind: "document", path: document.path, mode: document.mode, origin: document.origin };
}
