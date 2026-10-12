// @vitest-environment happy-dom
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h, nextTick, ref } from "vue";
import type { Ref } from "vue";
import { DEFAULT_APP_LAYOUT, DEFAULT_CHECKOUT_UI_STATE } from "../domain/ui-state";
import type { AppLayoutState, CheckoutUiState } from "../domain/ui-state";
import { useLayoutPersistence } from "./layout-persistence";

const mocks = vi.hoisted(() => ({
  saveAppLayout: vi.fn(),
  saveCheckoutUiState: vi.fn(),
  causes: [] as unknown[],
}));

vi.mock("../lib/ipc", () => ({
  saveAppLayout: mocks.saveAppLayout,
  saveCheckoutUiState: mocks.saveCheckoutUiState,
}));

function checkoutUiState(documentScrollTop: number): CheckoutUiState {
  return { ...DEFAULT_CHECKOUT_UI_STATE, documentScrollTop };
}

interface Host {
  wrapper: ReturnType<typeof mount>;
  appLayout: Ref<AppLayoutState>;
  appLayoutReady: Ref<boolean>;
  checkoutUiStates: Ref<Record<string, CheckoutUiState>>;
  saves: ReturnType<typeof useLayoutPersistence>;
}

/** A host that mounts the persistence the way the window does: on the arrangement it holds. */
function host(): Host {
  const appLayout = ref<AppLayoutState>({ ...DEFAULT_APP_LAYOUT });
  const appLayoutReady = ref(true);
  const checkoutUiStates = ref<Record<string, CheckoutUiState>>({});
  let saves!: ReturnType<typeof useLayoutPersistence>;
  const wrapper = mount(
    defineComponent({
      setup() {
        saves = useLayoutPersistence({
          appLayout,
          appLayoutReady,
          checkoutUiStates,
          reportCause: (cause) => mocks.causes.push(cause),
        });
        return () => h("div", "layout");
      },
    }),
  );
  return { wrapper, appLayout, appLayoutReady, checkoutUiStates, saves };
}

/** How long a change waits before it costs a write. */
const UI_STATE_WRITE_DEBOUNCE = 250;

describe("useLayoutPersistence", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.causes.length = 0;
    mocks.saveAppLayout.mockResolvedValue(undefined);
    mocks.saveCheckoutUiState.mockResolvedValue(undefined);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("writes nothing until the layout that was loaded is the one on screen", async () => {
    vi.useFakeTimers();
    const panel = host();
    panel.appLayoutReady.value = false;

    panel.appLayout.value = { ...DEFAULT_APP_LAYOUT, previewWidth: 420 };
    await vi.advanceTimersByTimeAsync(UI_STATE_WRITE_DEBOUNCE);
    expect(mocks.saveAppLayout).not.toHaveBeenCalled();

    // The defaults the window opens with are not an answer the reader gave, so writing them back
    // would turn a failed read into a lost layout.
    panel.appLayoutReady.value = true;
    panel.appLayout.value = { ...DEFAULT_APP_LAYOUT, previewWidth: 421 };
    await vi.advanceTimersByTimeAsync(UI_STATE_WRITE_DEBOUNCE);
    expect(mocks.saveAppLayout).toHaveBeenCalledTimes(1);

    panel.wrapper.unmount();
  });

  it("collapses a burst of layout changes into the one write it amounts to", async () => {
    vi.useFakeTimers();
    const panel = host();

    for (const previewWidth of [400, 410, 420]) {
      panel.appLayout.value = { ...DEFAULT_APP_LAYOUT, previewWidth };
      await vi.advanceTimersByTimeAsync(50);
    }
    expect(mocks.saveAppLayout).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(UI_STATE_WRITE_DEBOUNCE);
    await flushPromises();
    // Dragging a splitter is hundreds of changes that all say the same thing.
    expect(mocks.saveAppLayout).toHaveBeenCalledTimes(1);
    expect(mocks.saveAppLayout).toHaveBeenLastCalledWith({ ...DEFAULT_APP_LAYOUT, previewWidth: 420 });

    panel.wrapper.unmount();
  });

  it("keeps one timer per checkout so a panel in one workdir is not made to wait behind another's", async () => {
    vi.useFakeTimers();
    const panel = host();
    panel.checkoutUiStates.value = {
      "checkout:a": checkoutUiState(300),
      "checkout:b": checkoutUiState(400),
    };

    panel.saves.scheduleCheckoutUiSave("checkout:a");
    panel.saves.scheduleCheckoutUiSave("checkout:b");
    panel.saves.scheduleCheckoutUiSave("checkout:b");
    await vi.advanceTimersByTimeAsync(UI_STATE_WRITE_DEBOUNCE);
    await flushPromises();

    expect(mocks.saveCheckoutUiState).toHaveBeenCalledTimes(2);
    expect(mocks.saveCheckoutUiState).toHaveBeenCalledWith("checkout:a", checkoutUiState(300));
    expect(mocks.saveCheckoutUiState).toHaveBeenCalledWith("checkout:b", checkoutUiState(400));

    panel.wrapper.unmount();
  });

  it("writes what is still pending when a close asks for it to be flushed", async () => {
    vi.useFakeTimers();
    const panel = host();
    panel.checkoutUiStates.value = { "checkout:a": checkoutUiState(300) };
    panel.appLayout.value = { ...DEFAULT_APP_LAYOUT, previewWidth: 420 };
    panel.saves.scheduleCheckoutUiSave("checkout:a");
    await nextTick();

    const flushed = panel.saves.flushUiStateWrites();
    await flushPromises();

    // Neither had waited out its window, and a window that is gone cannot be asked again.
    expect(mocks.saveAppLayout).toHaveBeenCalledTimes(1);
    expect(mocks.saveCheckoutUiState).toHaveBeenCalledTimes(1);

    await vi.runAllTimersAsync();
    await flushed;
    // The timers are dropped rather than merely expired, so the same write is not made twice.
    expect(mocks.saveAppLayout).toHaveBeenCalledTimes(1);
    expect(mocks.saveCheckoutUiState).toHaveBeenCalledTimes(1);

    panel.wrapper.unmount();
  });

  it("waits for a write that is still on the other side of the bridge", async () => {
    vi.useFakeTimers();
    const panel = host();
    let asked = false;
    mocks.saveAppLayout.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          asked = true;
          setTimeout(resolve, 10);
        }),
    );

    panel.appLayout.value = { ...DEFAULT_APP_LAYOUT, previewWidth: 420 };
    await vi.advanceTimersByTimeAsync(UI_STATE_WRITE_DEBOUNCE);
    expect(asked).toBe(true);

    let settled = false;
    const flushed = panel.saves.flushUiStateWrites().then(() => {
      settled = true;
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(settled).toBe(false);

    await vi.runAllTimersAsync();
    await flushed;
    expect(settled).toBe(true);

    panel.wrapper.unmount();
  });

  it("reports a failed write and keeps the ones after it", async () => {
    vi.useFakeTimers();
    const panel = host();
    const failure = new Error("the disk said no");
    mocks.saveAppLayout.mockRejectedValueOnce(failure);

    panel.appLayout.value = { ...DEFAULT_APP_LAYOUT, previewWidth: 420 };
    await vi.advanceTimersByTimeAsync(UI_STATE_WRITE_DEBOUNCE);
    await flushPromises();
    expect(mocks.causes).toEqual([failure]);

    // A rejected write must not take the queue down with it: the next one is still owed.
    panel.appLayout.value = { ...DEFAULT_APP_LAYOUT, previewWidth: 500 };
    await vi.advanceTimersByTimeAsync(UI_STATE_WRITE_DEBOUNCE);
    await flushPromises();
    expect(mocks.saveAppLayout).toHaveBeenCalledTimes(2);

    panel.wrapper.unmount();
  });

  it("writes nothing for a panel that is gone", async () => {
    vi.useFakeTimers();
    const panel = host();
    panel.appLayout.value = { ...DEFAULT_APP_LAYOUT, previewWidth: 420 };
    panel.checkoutUiStates.value = { "checkout:a": checkoutUiState(300) };
    panel.saves.scheduleCheckoutUiSave("checkout:a");

    panel.wrapper.unmount();
    await vi.advanceTimersByTimeAsync(UI_STATE_WRITE_DEBOUNCE * 4);
    await flushPromises();

    expect(mocks.saveAppLayout).not.toHaveBeenCalled();
    expect(mocks.saveCheckoutUiState).not.toHaveBeenCalled();
  });
});
