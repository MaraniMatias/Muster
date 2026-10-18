// @vitest-environment happy-dom
import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import OverlayScrollbar from "./OverlayScrollbar.vue";

/**
 * A scrolling box and a host to draw its bar in, with the layout jsdom does not have.
 *
 * The geometry is a 480px viewport over 2,400px of content on a 480px track, which is the same set of
 * numbers `overlay-scrollbar.test.ts` uses: a thumb of 96px over a travel of 384px, so every position
 * in this file comes out whole.
 */
const TRACK = 480;
const CONTENT = 2_400;
const VIEWPORT = 480;

for (const [property, value] of [
  ["clientHeight", TRACK],
  ["clientWidth", 320],
] as const) {
  Object.defineProperty(HTMLElement.prototype, property, { value, configurable: true });
}

type ResizeCallback = () => void;
const observers: { callback: ResizeCallback; targets: Element[] }[] = [];

class MockResizeObserver {
  callback: ResizeCallback;
  targets: Element[] = [];

  constructor(callback: ResizeCallback) {
    this.callback = callback;
    observers.push(this);
  }

  observe(target: Element) {
    this.targets.push(target);
  }

  unobserve(target: Element) {
    this.targets = this.targets.filter((element) => element !== target);
  }

  disconnect() {
    this.targets = [];
  }
}

interface Harness {
  box: HTMLElement;
  scrollTo(px: number): void;
  grow(px: number): void;
  fireResize(): void;
}

function harness(options: { scrollHeight?: number } = {}): Harness {
  const host = document.createElement("div");
  const box = document.createElement("div");
  const mountPoint = document.createElement("div");
  host.append(box, mountPoint);
  document.body.append(host);
  let scrollTop = 0;
  let scrollHeight = options.scrollHeight ?? CONTENT;
  Object.defineProperties(box, {
    scrollHeight: { get: () => scrollHeight, configurable: true },
    scrollTop: {
      get: () => scrollTop,
      // A box clamps a scroll it is given past either end and says nothing about it, and the bar
      // reads that silence as the end it is at. This mock clamps for the same reason, and fires the
      // event the way the browser does, because that is what the bar listens to.
      set: (value: number) => {
        scrollTop = Math.min(Math.max(0, scrollHeight - VIEWPORT), Math.max(0, value));
        box.dispatchEvent(new Event("scroll"));
      },
      configurable: true,
    },
  });
  box.getBoundingClientRect = () => ({ top: 32, height: VIEWPORT }) as DOMRect;
  mountPoint.getBoundingClientRect = () => ({ top: 0 }) as DOMRect;
  return {
    box,
    scrollTo: (px: number) => {
      box.scrollTop = px;
    },
    grow: (px: number) => {
      scrollHeight = px;
    },
    fireResize: () => observers.forEach((observer) => observer.callback()),
  };
}

beforeEach(() => {
  observers.length = 0;
  vi.stubGlobal("ResizeObserver", MockResizeObserver);
});

afterEach(() => {
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
});

describe("overlay scrollbar", () => {
  it("draws no element at all in the hidden mode", () => {
    const { box } = harness();
    const wrapper = mount(OverlayScrollbar, { props: { target: box, mode: "hidden" } });

    // Not merely invisible: the browser's bar is off in this box, so a hidden one would leave the
    // panel with nothing to say how far down it is and nothing to scroll by hand either.
    expect(wrapper.find(".overlay-scrollbar").exists()).toBe(false);
    wrapper.unmount();
  });

  it("sizes the thumb to the content and moves it with the scroll position", async () => {
    const { box, scrollTo } = harness();
    const wrapper = mount(OverlayScrollbar, { props: { target: box, mode: "always" } });
    await wrapper.vm.$nextTick();

    expect(wrapper.find(".overlay-scrollbar-idle").exists()).toBe(false);
    expect(wrapper.get(".overlay-scrollbar").attributes()).toMatchObject({
      role: "scrollbar",
      "aria-orientation": "vertical",
      "aria-valuemin": "0",
      "aria-valuemax": "1920",
      "aria-valuenow": "0",
      tabindex: "0",
    });
    // A fifth of 2,400px is on screen, so the thumb is a fifth of the track and starts at the top.
    expect(wrapper.get(".overlay-scrollbar-thumb").attributes("style")).toBe("top: 0px; height: 96px;");

    scrollTo(960);
    await wrapper.vm.$nextTick();

    // Halfway down the content is halfway down the thumb's travel, which is the whole point of
    // deriving one from the other: the thumb is where the content is, not near it.
    expect(wrapper.get(".overlay-scrollbar-thumb").attributes("style")).toBe("top: 192px; height: 96px;");
    expect(wrapper.get(".overlay-scrollbar").attributes("aria-valuenow")).toBe("960");
    wrapper.unmount();
  });

  it("leaves `always` showing a full-height thumb when there is nothing to scroll", async () => {
    const { box } = harness({ scrollHeight: VIEWPORT });
    const wrapper = mount(OverlayScrollbar, { props: { target: box, mode: "always" } });
    await wrapper.vm.$nextTick();

    // The whole list fits, so there is no position to be anywhere but this one. A person who asked
    // for a permanent scrollbar is told there is nothing to scroll.
    expect(wrapper.get(".overlay-scrollbar-thumb").attributes("style")).toBe("top: 0px; height: 480px;");
    expect(wrapper.get(".overlay-scrollbar").attributes("aria-valuemax")).toBe("0");
    wrapper.unmount();
  });

  it("lights up on a scroll in `auto` and takes the thumb away when the scrolling stops", async () => {
    vi.useFakeTimers();
    try {
      const { box, scrollTo } = harness();
      const wrapper = mount(OverlayScrollbar, { props: { target: box } });
      await wrapper.vm.$nextTick();

      // A panel nobody is scrolling looks like a panel with no scrollbar, which is what `auto` is
      // for. The bar is drawn over the padding rather than in it, so a bar that fades costs nothing.
      expect(wrapper.find(".overlay-scrollbar-idle").exists()).toBe(true);

      scrollTo(480);
      await wrapper.vm.$nextTick();
      expect(wrapper.find(".overlay-scrollbar-idle").exists()).toBe(false);

      await vi.advanceTimersByTimeAsync(900);
      expect(wrapper.find(".overlay-scrollbar-idle").exists()).toBe(true);

      // The timer restarts on every scroll rather than running once, so a gesture held down keeps
      // the bar up for as long as it is held.
      scrollTo(600);
      await vi.advanceTimersByTimeAsync(600);
      scrollTo(700);
      await vi.advanceTimersByTimeAsync(600);
      expect(wrapper.find(".overlay-scrollbar-idle").exists()).toBe(false);
      await vi.advanceTimersByTimeAsync(900);
      expect(wrapper.find(".overlay-scrollbar-idle").exists()).toBe(true);
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("never flashes a thumb in `auto` for a box that fits", async () => {
    vi.useFakeTimers();
    try {
      const { box, scrollTo } = harness({ scrollHeight: VIEWPORT });
      const wrapper = mount(OverlayScrollbar, { props: { target: box } });
      await wrapper.vm.$nextTick();

      scrollTo(0);
      await vi.advanceTimersByTimeAsync(1000);

      expect(wrapper.find(".overlay-scrollbar-idle").exists()).toBe(true);
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("draws itself over the box it measures rather than over the panel it is a child of", async () => {
    const { box } = harness();
    const wrapper = mount(OverlayScrollbar, { props: { target: box, mode: "always" } });
    await wrapper.vm.$nextTick();

    // The bar is placed at the end of a pane so that nothing has to be wrapped to give it an edge to
    // sit on, which only holds if it takes its place from the box: 32px down, 480px tall.
    expect(wrapper.get(".overlay-scrollbar").attributes("style")).toBe("top: 32px; height: 480px;");
    wrapper.unmount();
  });

  it("re-measures when the content grows or the panel is resized", async () => {
    const { box, grow, fireResize } = harness();
    const wrapper = mount(OverlayScrollbar, { props: { target: box, mode: "always" } });
    await wrapper.vm.$nextTick();
    expect(wrapper.get(".overlay-scrollbar-thumb").attributes("style")).toContain("height: 96px;");

    // A list that grows without anything scrolling — a row folding, a file opening, a virtual list
    // swapping its window — has to shorten the thumb, or the bar claims there is less below than
    // there is.
    grow(4_800);
    fireResize();
    await wrapper.vm.$nextTick();

    expect(wrapper.get(".overlay-scrollbar-thumb").attributes("style")).toContain("height: 48px;");
    expect(observers[0]!.targets).toContain(box);
    wrapper.unmount();
  });

  it("scrolls to the position a dragged thumb stands for, and jumps on a press beside it", async () => {
    const { box } = harness();
    const wrapper = mount(OverlayScrollbar, { props: { target: box, mode: "always" } });
    await wrapper.vm.$nextTick();
    const track = wrapper.get(".overlay-scrollbar");

    // Caught 24px down a thumb that starts at the top of the track, so the drag keeps the offset it
    // was caught at instead of snapping the thumb's middle under the pointer.
    await track.trigger("pointerdown", { clientY: 24, pointerId: 1 });
    expect(box.scrollTop).toBe(0);
    await track.trigger("pointermove", { clientY: 200, pointerId: 1 });
    expect(box.scrollTop).toBe(880);
    expect(wrapper.get(".overlay-scrollbar-thumb").attributes("style")).toBe("top: 176px; height: 96px;");
    await track.trigger("pointerup", { clientY: 200, pointerId: 1 });

    // A press on the empty track puts the middle of the thumb under the pointer instead, which is
    // what every other scrollbar does and what makes clicking above or below the thumb a jump rather
    // than a press that does nothing: 300px is 252 above the thumb's middle, which is 1,260 of 1,920.
    await track.trigger("pointerdown", { clientY: 300, pointerId: 2 });
    expect(box.scrollTop).toBe(1_260);
    // A press from a second pointer is not the one being dragged.
    await track.trigger("pointermove", { clientY: 100, pointerId: 3 });
    expect(box.scrollTop).toBe(1_260);
    wrapper.unmount();
  });

  it("keeps the wheel's own measure instead of turning every tick into a page", async () => {
    const { box, scrollTo } = harness();
    const wrapper = mount(OverlayScrollbar, { props: { target: box, mode: "always" } });
    await wrapper.vm.$nextTick();
    const track = wrapper.get(".overlay-scrollbar");
    scrollTo(0);

    // The bar is a sibling of the box rather than part of it, so a wheel over it never reaches the
    // box's own scrolling and the delta is applied here instead, in the measure it arrived in.
    const pixels = new WheelEvent("wheel", { deltaY: 10, cancelable: true });
    track.element.dispatchEvent(pixels);
    expect(pixels.defaultPrevented).toBe(true);
    expect(box.scrollTop).toBe(10);

    const lines = new WheelEvent("wheel", {
      deltaY: 3,
      deltaMode: WheelEvent.DOM_DELTA_LINE,
      cancelable: true,
    });
    track.element.dispatchEvent(lines);
    expect(box.scrollTop).toBe(58);

    const page = new WheelEvent("wheel", { deltaY: 1, deltaMode: WheelEvent.DOM_DELTA_PAGE, cancelable: true });
    track.element.dispatchEvent(page);
    expect(box.scrollTop).toBe(538);

    // A box already at an end ignores the nudge, and a remainder kept past that is owed twice.
    scrollTo(1_920);
    track.element.dispatchEvent(new WheelEvent("wheel", { deltaY: 100, cancelable: true }));
    expect(box.scrollTop).toBe(1_920);
    track.element.dispatchEvent(new WheelEvent("wheel", { deltaY: 100, cancelable: true }));
    expect(box.scrollTop).toBe(1_920);
    wrapper.unmount();
  });

  it("scrolls from the keyboard for a pointer-only affordance", async () => {
    const { box, scrollTo } = harness();
    const wrapper = mount(OverlayScrollbar, { props: { target: box, mode: "always" } });
    await wrapper.vm.$nextTick();
    const track = wrapper.get(".overlay-scrollbar");
    scrollTo(0);

    await track.trigger("keydown", { key: "ArrowDown" });
    expect(box.scrollTop).toBe(48);
    await track.trigger("keydown", { key: "PageDown" });
    expect(box.scrollTop).toBe(512);
    await track.trigger("keydown", { key: "End" });
    expect(box.scrollTop).toBe(1_920);
    await track.trigger("keydown", { key: "PageUp" });
    expect(box.scrollTop).toBe(1_456);
    await track.trigger("keydown", { key: "Home" });
    expect(box.scrollTop).toBe(0);
    // A key that is not the bar's is the box's business, not this one's.
    await track.trigger("keydown", { key: "a" });
    expect(box.scrollTop).toBe(0);
    wrapper.unmount();
  });

  it("lets go of the box and of a held drag when it goes away", async () => {
    const { box, scrollTo } = harness();
    const wrapper = mount(OverlayScrollbar, { props: { target: box, mode: "always" } });
    await wrapper.vm.$nextTick();
    const track = wrapper.get(".overlay-scrollbar");
    await track.trigger("pointerdown", { clientY: 24, pointerId: 1 });

    wrapper.unmount();

    // A pane torn down mid-drag must not leave this holding a listener on a box that is no longer
    // on screen, or a resize observer measuring a detached one.
    expect(observers[0]!.targets).toEqual([]);
    expect(() => scrollTo(100)).not.toThrow();
  });

  it("follows the box to a different one", async () => {
    const { box, scrollTo } = harness();
    const other = harness({ scrollHeight: 4_800 }).box;
    const wrapper = mount(OverlayScrollbar, { props: { target: box, mode: "always" } });
    await wrapper.vm.$nextTick();

    await wrapper.setProps({ target: other });
    await wrapper.vm.$nextTick();
    // A pane whose list was replaced is a box the bar has to be measuring, not the one it was
    // measuring a moment ago: 4,800px of content is a tenth of the track rather than a fifth.
    expect(wrapper.get(".overlay-scrollbar-thumb").attributes("style")).toContain("height: 48px;");

    other.scrollTop = 1_200;
    await wrapper.vm.$nextTick();
    expect(wrapper.get(".overlay-scrollbar").attributes("aria-valuenow")).toBe("1200");

    // The box it left behind is no longer the thing a scroll has to move the thumb for.
    scrollTo(1_920);
    expect(wrapper.get(".overlay-scrollbar").attributes("aria-valuenow")).toBe("1200");
    wrapper.unmount();
  });
});
