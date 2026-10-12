/**
 * How far the whole window is scaled, and the three keys that move it.
 *
 * The window is scaled with `zoom` on the root element rather than with the webview's own zoom,
 * because a webview zoom is applied by the compositor: the terminal's WebGL canvas is resampled
 * rather than redrawn, and a terminal app that gets soft the moment somebody makes the text bigger
 * has answered the wrong question. Every other surface in this app is DOM, so a layout zoom
 * re-rasterizes all of it and only the terminal needs to be told about it.
 *
 * The steps are the ones a person can hold a key down on, and they are walked by index rather than
 * by adding a tenth: `0.1 + 0.1 + 0.1` is not `0.3`, and a value that lands between two steps is a
 * value the label and the stored layout cannot both name.
 */
export const ZOOM_STEPS = [0.8, 0.9, 1, 1.1, 1.2, 1.3, 1.4, 1.5] as const;

export type Zoom = (typeof ZOOM_STEPS)[number];

export const DEFAULT_ZOOM: Zoom = 1;

/** Which direction a key asks for: `-1` out, `1` in, `0` back to 100%. */
export type ZoomDirection = 1 | -1 | 0;

/**
 * The name of the key that opened the zoom, for the message that says how to undo it.
 *
 * It comes from the key that was actually pressed rather than from the platform, because the
 * handler needs a modifier either way: a bare `0` has to be able to type a `0` into the search
 * field, the editor and the terminal. By the time the shortcut is recognized the platform has
 * therefore already said which of the two it is, and no user-agent sniffing is needed to agree
 * with it.
 */
export type ZoomModifier = "Cmd" | "Ctrl";

/** The part of a key event the shortcut reads, so a plain object answers the same as an event. */
export interface ZoomKey {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
}

function nearestStep(value: number): Zoom {
  let nearest: Zoom = ZOOM_STEPS[0];
  for (const step of ZOOM_STEPS) {
    // Strictly less, so a value exactly between two steps takes the smaller one instead of
    // whichever end of the list happens to be walked last.
    if (Math.abs(step - value) < Math.abs(nearest - value)) nearest = step;
  }
  return nearest;
}

/**
 * The step a saved zoom means, or 100% for anything this build cannot draw.
 *
 * A saved value is clamped into the steps rather than trusted: a layout from a build with a wider
 * range, or a file somebody edited, is closer to a step than to "unusable", and the answer is only
 * ever read as a scale factor.
 */
export function normalizeZoom(value: unknown): Zoom {
  if (typeof value !== "number" || !Number.isFinite(value)) return DEFAULT_ZOOM;
  return nearestStep(value);
}

/** The step one place along from `current`, or 100% for the reset. */
export function zoomStep(current: number, direction: ZoomDirection): Zoom {
  // A reset is a step and not a distance: it is 100% from anywhere, which is the whole difference
  // between it and a walk that happens to have nowhere left to go.
  if (direction === 0) return DEFAULT_ZOOM;
  const index = ZOOM_STEPS.indexOf(nearestStep(current)) + direction;
  return ZOOM_STEPS[Math.min(ZOOM_STEPS.length - 1, Math.max(0, index))]!;
}

/**
 * What a key asks of the zoom, or nothing at all.
 *
 * `=` is taken with `+` because they are one key on a US layout, where `+` arrives as `=` with
 * shift held down, and a layout that spells the sign somewhere else sends a character that is
 * neither. The modifier is required rather than optional: without it the `0` here is a `0` somebody
 * is typing.
 */
export function zoomKeyFor(event: ZoomKey): ZoomDirection | undefined {
  if (!event.metaKey && !event.ctrlKey) return undefined;
  if (event.altKey) return undefined;
  if (event.key === "+" || event.key === "=") return 1;
  if (event.key === "-" || event.key === "_") return -1;
  if (event.key === "0") return 0;
  return undefined;
}

/**
 * The one line that says where the window is scaled to and how to put it back.
 *
 * The percentage is the fact; a message that only said the zoom changed would be the one thing a
 * person pressing the key five times cannot work out from looking at the screen.
 */
export function zoomLabel(zoom: number, modifier: ZoomModifier): string {
  return `Zoom ${Math.round(zoom * 100)}% (${modifier} 0 to reset)`;
}
