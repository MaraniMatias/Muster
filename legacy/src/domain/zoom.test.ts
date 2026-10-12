import { describe, expect, it } from "vitest";
import { DEFAULT_ZOOM, ZOOM_STEPS, normalizeZoom, zoomKeyFor, zoomLabel, zoomStep } from "./zoom";
import type { ZoomKey } from "./zoom";

const key = (overrides: Partial<ZoomKey> = {}): ZoomKey => ({
  key: "0",
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  ...overrides,
});

describe("zoom", () => {
  it("walks the steps by index, so a held key never lands between two of them", () => {
    const held = [1, 1, 1, 1].reduce((zoom) => zoomStep(zoom, 1), 1);
    expect(held).toBe(1.4);
    // Every answer is a step the list already names, which is the whole point of walking the
    // index: nothing accumulates an error on the way there.
    const walked = [1, 1, 1, 1, 1, 1, 1].reduce<number[]>((seen, _, index) => {
      seen.push(zoomStep(index === 0 ? 1 : seen[index - 1]!, 1));
      return seen;
    }, []);
    expect(walked.every((zoom) => ZOOM_STEPS.includes(zoom as never))).toBe(true);
  });

  it("stops at the ends instead of running off them", () => {
    expect(zoomStep(1.5, 1)).toBe(1.5);
    expect(zoomStep(0.8, -1)).toBe(0.8);
  });

  it("puts a reset back to 100% from anywhere, not from where it happened to be", () => {
    for (const zoom of ZOOM_STEPS) expect(zoomStep(zoom, 0)).toBe(1);
  });

  it("clamps what was saved into a step it can draw, and calls anything else 100%", () => {
    expect(normalizeZoom(4)).toBe(1.5);
    expect(normalizeZoom(0.01)).toBe(0.8);
    expect(normalizeZoom(1.16)).toBe(1.2);
    expect(normalizeZoom(Number.NaN)).toBe(DEFAULT_ZOOM);
    expect(normalizeZoom("1.2")).toBe(DEFAULT_ZOOM);
    expect(normalizeZoom(undefined)).toBe(DEFAULT_ZOOM);
  });

  it("reads the sign the layout produced rather than the one the keyboard has", () => {
    expect(zoomKeyFor(key({ key: "+", metaKey: true }))).toBe(1);
    expect(zoomKeyFor(key({ key: "=", metaKey: true }))).toBe(1);
    expect(zoomKeyFor(key({ key: "-", ctrlKey: true }))).toBe(-1);
    expect(zoomKeyFor(key({ key: "_", ctrlKey: true }))).toBe(-1);
    expect(zoomKeyFor(key({ key: "0", metaKey: true }))).toBe(0);
  });

  it("leaves a `0` and a `+` that are being typed alone", () => {
    expect(zoomKeyFor(key({ key: "0" }))).toBeUndefined();
    expect(zoomKeyFor(key({ key: "+", metaKey: true, altKey: true }))).toBeUndefined();
    expect(zoomKeyFor(key({ key: "9", metaKey: true }))).toBeUndefined();
    expect(zoomKeyFor(key({ key: "a", ctrlKey: true }))).toBeUndefined();
  });

  it("says the scale and names the key that was pressed", () => {
    expect(zoomLabel(1.2, "Cmd")).toBe("Zoom 120% (Cmd 0 to reset)");
    expect(zoomLabel(1, "Ctrl")).toBe("Zoom 100% (Ctrl 0 to reset)");
  });
});
