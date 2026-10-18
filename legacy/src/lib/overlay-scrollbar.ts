/**
 * Where the thumb of a Muster overlay scrollbar goes.
 *
 * The arithmetic is the scrollbar every desktop has had for thirty years: the thumb is as much of
 * the track as the viewport is of the content, never smaller than a thumb you can catch, and its
 * position along the rest of the track is the fraction of the content that is behind you — the same
 * fraction, which is why dragging it moves the viewport by the right amount.
 *
 * This file is the whole job and it is a function over four numbers with no DOM in it, so the
 * arithmetic can be read and tested without a scrolling box attached. The three it is given are the
 * ones the DOM already keeps (`scrollTop`, `scrollHeight`, `clientHeight`), which is what lets the
 * terminal map its scrollback onto the same three numbers and share this rather than keep a copy:
 * lines are pixels here, and a line of scrollback is `scrollHeight`, the visible rows are
 * `clientHeight` and `viewportY` is `scrollTop`.
 *
 * Every number is clamped rather than trusted. A scrollport is measured by a box whose content can
 * change between one measure and the next — a row folds, a virtual list swaps its window, a document
 * finishes loading — and `scrollTop` can sit past the end of a content height that has just shrunk,
 * which would otherwise throw the thumb off the bottom of its own track.
 */

/** A thumb thinner than this cannot be grabbed with a finger or aimed with a mouse. */
const MIN_THUMB_PX = 28;

export interface OverlayScrollport {
  /** How far the content is scrolled from its top, in px. */
  scrollTop: number;
  /** How tall the content is, in px. */
  scrollHeight: number;
  /** How much of it is on screen, in px. */
  clientHeight: number;
}

export interface OverlayScrollbarGeometry {
  /** Where the top of the thumb sits, in px from the top of the track. */
  top: number;
  /** How tall the thumb is, in px. Never more than the track. */
  height: number;
  /** Whether there is anything to scroll, which is also whether a thumb is worth drawing. */
  scrollable: boolean;
  /** The furthest the content can be scrolled from its top, in px. */
  maxOffset: number;
  /** How far the thumb itself can travel down the track, in px. Zero when it fills the track. */
  travel: number;
}

/**
 * The thumb for a viewport somewhere in a scrollport, drawn on a track of a known height.
 *
 * A content height shorter than the viewport is answered as nothing to scroll rather than as a
 * negative travel, and a track with no height answers with no thumb at all: the overlay is in the
 * DOM before it has a size on the frame its pane first appears, and a thumb sized against a track
 * of zero is a correct answer to the wrong question, which the next measure puts right.
 */
export function overlayScrollbarGeometry(scrollport: OverlayScrollport, trackHeight: number): OverlayScrollbarGeometry {
  const viewport = Math.max(1, Math.floor(scrollport.clientHeight));
  const content = Math.max(viewport, Math.floor(scrollport.scrollHeight));
  const maxOffset = Math.max(0, content - viewport);
  const scrollTop = Math.min(maxOffset, Math.max(0, Math.floor(scrollport.scrollTop)));
  const track = Math.max(0, Math.floor(trackHeight));
  if (track === 0) return { top: 0, height: 0, scrollable: maxOffset > 0, maxOffset, travel: 0 };
  if (maxOffset === 0) return { top: 0, height: track, scrollable: false, maxOffset: 0, travel: 0 };
  const height = Math.min(track, Math.max(MIN_THUMB_PX, (viewport / content) * track));
  const travel = track - height;
  return { top: (scrollTop / maxOffset) * travel, height, scrollable: true, maxOffset, travel };
}

/**
 * The scroll position a thumb dropped at `top` stands for, which is the inverse of the arithmetic
 * above and the only thing a drag needs: the pointer says where the thumb is, the box wants a px.
 *
 * A track with no travel has one position, so it answers 0 rather than a division by zero, and the
 * answer is rounded because the arithmetic produces fractions of a pixel and a scroll position that
 * chases a sub-pixel remainder is a thumb that never settles.
 */
export function scrollbarOffsetForTop(top: number, geometry: OverlayScrollbarGeometry): number {
  if (geometry.travel <= 0) return 0;
  const clamped = Math.min(geometry.travel, Math.max(0, top));
  return Math.round((clamped / geometry.travel) * geometry.maxOffset);
}
