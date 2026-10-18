import { onScopeDispose, watch } from "vue";
import type { Ref } from "vue";
import { normalizeAppLayout, normalizeCheckoutUiState } from "../domain/ui-state";
import type { AppLayoutState, CheckoutUiState } from "../domain/ui-state";
import { saveAppLayout, saveCheckoutUiState } from "../lib/ipc";

/** What is arranged on screen, all of it through refs because the arrangement is the panel's own. */
export interface LayoutPersistenceLayout {
  /** The window's arrangement, which is written on every change to it. */
  appLayout: Ref<AppLayoutState>;
  /**
   * Whether that arrangement is the one that was loaded. Nothing is written before it is: the
   * defaults the window opens with are not an answer the reader gave, and writing them back would
   * turn a failed read into a lost layout.
   */
  appLayoutReady: Ref<boolean>;
  /** What each checkout's panels are showing, which is written per checkout. */
  checkoutUiStates: Ref<Record<string, CheckoutUiState>>;
  reportCause: (cause: unknown) => void;
}

/**
 * How long a change to the arrangement waits before it is written.
 *
 * Dragging a splitter moves one number per frame and scrolling a file moves another per event, and
 * the write is over the bridge to a file on disk. Without a window, a drag is hundreds of writes
 * that all say the same thing, and the last one is the only one that was ever worth making.
 */
const UI_STATE_WRITE_DEBOUNCE = 250;

/**
 * The panels' own arrangement, written to disk on its own schedule.
 *
 * The layout and every checkout's panel state are one concern because they are one file each and
 * both answer to the same question: what the window looked like when it was last closed. Every
 * write goes through one queue so they cannot land out of order, and a close flushes what is
 * pending rather than losing it, because a window that is gone cannot be asked again.
 */
export function useLayoutPersistence(layout: LayoutPersistenceLayout) {
  const { appLayout, appLayoutReady, checkoutUiStates, reportCause } = layout;
  let uiLayoutSaveTimer: number | undefined;
  const checkoutUiSaveTimers = new Map<string, number>();
  let uiStateWriteQueue: Promise<void> = Promise.resolve();

  /**
   * One write at a time and in the order it was asked for: the layout file and a checkout's own
   * state are written by the same backend, and a slow write must not be overtaken by a later one
   * describing an arrangement that came after it.
   */
  function scheduleUiWrite(write: () => Promise<void>) {
    const pending = uiStateWriteQueue
      .catch(() => {})
      .then(write)
      .catch((cause: unknown) => {
        reportCause(cause);
      });
    uiStateWriteQueue = pending;
  }

  function scheduleAppLayoutSave() {
    if (!appLayoutReady.value) return;
    if (uiLayoutSaveTimer !== undefined) window.clearTimeout(uiLayoutSaveTimer);
    uiLayoutSaveTimer = window.setTimeout(() => {
      uiLayoutSaveTimer = undefined;
      const state = normalizeAppLayout(JSON.parse(JSON.stringify(appLayout.value)));
      scheduleUiWrite(() => saveAppLayout(state));
    }, UI_STATE_WRITE_DEBOUNCE);
  }

  /**
   * One timer per checkout, so a reader moving a panel in one workdir is not made to wait behind
   * another workdir's scroll, and each of them is written only when that workdir settles.
   */
  function scheduleCheckoutUiSave(checkoutId: string) {
    const previousTimer = checkoutUiSaveTimers.get(checkoutId);
    if (previousTimer !== undefined) window.clearTimeout(previousTimer);
    checkoutUiSaveTimers.set(
      checkoutId,
      window.setTimeout(() => {
        checkoutUiSaveTimers.delete(checkoutId);
        const state = normalizeCheckoutUiState(JSON.parse(JSON.stringify(checkoutUiStates.value[checkoutId])));
        scheduleUiWrite(() => saveCheckoutUiState(checkoutId, state));
      }, UI_STATE_WRITE_DEBOUNCE),
    );
  }

  /**
   * Writes what is still pending and waits for it to land, which is what a close asks for: the
   * window stays alive until this returns, so a debounced change made seconds earlier is not the
   * one thing a launch cannot recover.
   */
  async function flushUiStateWrites() {
    if (uiLayoutSaveTimer !== undefined) {
      window.clearTimeout(uiLayoutSaveTimer);
      uiLayoutSaveTimer = undefined;
      const state = normalizeAppLayout(JSON.parse(JSON.stringify(appLayout.value)));
      scheduleUiWrite(() => saveAppLayout(state));
    }
    for (const [checkoutId, timer] of checkoutUiSaveTimers) {
      window.clearTimeout(timer);
      const state = normalizeCheckoutUiState(JSON.parse(JSON.stringify(checkoutUiStates.value[checkoutId])));
      scheduleUiWrite(() => saveCheckoutUiState(checkoutId, state));
    }
    checkoutUiSaveTimers.clear();
    await uiStateWriteQueue;
  }

  watch(appLayout, () => scheduleAppLayoutSave(), { deep: true });

  onScopeDispose(() => {
    // A pending timer is a write to a layout this window will never be asked about again.
    if (uiLayoutSaveTimer !== undefined) window.clearTimeout(uiLayoutSaveTimer);
    for (const timer of checkoutUiSaveTimers.values()) window.clearTimeout(timer);
    checkoutUiSaveTimers.clear();
  });

  return { scheduleCheckoutUiSave, flushUiStateWrites };
}
