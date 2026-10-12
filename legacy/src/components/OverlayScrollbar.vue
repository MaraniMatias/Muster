<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import type { TerminalScrollbarMode } from "../domain/settings";
import {
  overlayScrollbarGeometry,
  scrollbarOffsetForTop,
  type OverlayScrollbarGeometry,
} from "../lib/overlay-scrollbar";

/**
 * Muster' own scrollbar, drawn over a scrolling box that keeps scrolling itself.
 *
 * The box is left alone on purpose: `overflow-y: auto` gives the wheel, the keyboard and the
 * trackpad's own momentum for free, and none of them need this element to exist. What it cannot
 * give is a bar that says how far down the box is — the browser's is either a gutter taken out of
 * every list or an overlay that sits on top of the content with a padding reserved to keep the text
 * out from under it, and a panel wants neither. So the native bar is hidden (in `style.css`, the
 * same way xterm's is) and this draws a five-pixel thumb into the padding that was reserved for it.
 *
 * It is an overlay rather than a participant in layout: it covers the right edge of whatever
 * positioned box it is placed in and costs it no width. That is why the thumb's geometry is a
 * fraction of the content's height rather than a scrollbar the browser sizes — see
 * `overlay-scrollbar.ts`, which is this file's arithmetic with no DOM in it.
 */
const props = withDefaults(
  defineProps<{
    /** The scrolling box this bar stands for. Null until the box is on screen. */
    target: HTMLElement | null;
    /**
     * `auto` shows the thumb while something is moving and takes it away when nothing is, `always`
     * leaves it up so a list that fits is still visibly a list, and `hidden` draws nothing at all.
     */
    mode?: TerminalScrollbarMode;
    label?: string;
  }>(),
  { mode: "auto", label: "Scroll position" },
);

/**
 * How long `auto` leaves the thumb up after the last scroll.
 *
 * Long enough to read where the thumb landed and to catch it again, short enough that a list nobody
 * is scrolling looks like a list with no scrollbar.
 */
const FADE_MS = 900;

/** What an arrow key moves the box by, which is a row a person can see change. */
const ARROW_PX = 48;

/** What a wheel line is worth in px, for the gesture that arrives measured in lines. */
const LINE_PX = 16;

const track = ref<HTMLElement | null>(null);
const thumb = ref({ top: 0, height: 0 });
/** Whether `auto` currently has the thumb up. */
const lit = ref(false);
const maximum = ref(0);
const position = ref(0);

let geometry: OverlayScrollbarGeometry = { top: 0, height: 0, scrollable: false, maxOffset: 0, travel: 0 };
let attached: HTMLElement | null = null;
let observed = new Set<Element>();
let resizeObserver: ResizeObserver | undefined;
let childObserver: MutationObserver | undefined;
let fadeTimer: number | undefined;
let drag: { pointerId: number; grabOffset: number } | null = null;
let wheelRemainderPx = 0;

const enabled = computed(() => props.mode !== "hidden");
/** `always` has no fade to get to, so its thumb is simply there. */
const visible = computed(() => props.mode === "always" || lit.value);

/**
 * Puts the track over the box it measures.
 *
 * The bar is drawn where the content is rather than filling whatever panel it happens to be a child
 * of, so one placed at the end of a pane covers the list and not the toolbar above it. Both rects
 * are in the same visual space, which is why the offset is read from them rather than from
 * `offsetTop`: a box nested inside something positioned of its own would answer that question about
 * the wrong ancestor.
 */
function place() {
  const rail = track.value;
  const box = attached;
  const host = rail?.parentElement;
  if (!rail || !box || !host) return;
  const railTop = `${box.getBoundingClientRect().top - host.getBoundingClientRect().top}px`;
  const railHeight = `${box.getBoundingClientRect().height}px`;
  if (rail.style.top === railTop && rail.style.height === railHeight) return;
  rail.style.top = railTop;
  rail.style.height = railHeight;
}

/**
 * Reads the box and puts the thumb where it belongs.
 *
 * The track's own height is measured rather than assumed because it is the height of the list it
 * belongs to, and writing the style only when the numbers moved is what keeps this cheap enough to
 * call on every scroll event.
 */
function update() {
  const box = attached;
  if (!box) return;
  place();
  geometry = overlayScrollbarGeometry(
    { scrollTop: box.scrollTop, scrollHeight: box.scrollHeight, clientHeight: box.clientHeight },
    track.value?.clientHeight ?? 0,
  );
  position.value = Math.min(geometry.maxOffset, Math.max(0, box.scrollTop));
  maximum.value = geometry.maxOffset;
  // A box whose whole content fits on screen has nothing to say with a thumb, so in `auto` it does
  // not get to flash one. `always` still draws it: a full-height thumb is an honest answer, and a
  // person who asked for a permanent scrollbar is entitled to see that there is nothing to scroll.
  if (!geometry.scrollable) lit.value = false;
  if (geometry.top === thumb.value.top && geometry.height === thumb.value.height) return;
  thumb.value = { top: geometry.top, height: geometry.height };
}

/**
 * `auto` shows the thumb while something is moving and takes it away when nothing is.
 *
 * The timer restarts on every scroll instead of running once, so a wheel held down keeps the bar up
 * for as long as it is held and the fade happens when the gesture stops. Dragging counts as moving,
 * and so does the wheel over the track, or the thumb would fade out from under a pointer that is
 * still holding it.
 */
function wake() {
  if (!enabled.value || props.mode === "always" || !geometry.scrollable) return;
  lit.value = true;
  if (fadeTimer !== undefined) window.clearTimeout(fadeTimer);
  fadeTimer = undefined;
  if (drag) return;
  fadeTimer = window.setTimeout(() => {
    fadeTimer = undefined;
    lit.value = false;
  }, FADE_MS);
}

function onScroll() {
  update();
  wake();
}

/** Moves the box to where a thumb dropped at `top` would stand, and re-reads it. */
function scrollTo(top: number) {
  const box = attached;
  if (!box) return;
  box.scrollTop = scrollbarOffsetForTop(top, geometry);
  update();
}

/**
 * The thumb is a fraction of the content's height, so a row folding, a file opening or a virtual
 * list swapping its window changes it without anything scrolling. The children are watched for that:
 * their boxes are what grow, and watching them is cheaper than watching every descendant of a diff.
 * `childList` on the box itself is what a list adding or removing rows fires, and it is also what
 * changes which children are worth watching.
 */
function watchChildren() {
  const box = attached;
  if (!box || !resizeObserver) return;
  const next = new Set<Element>(Array.from(box.children));
  for (const child of observed) if (!next.has(child)) resizeObserver.unobserve(child);
  for (const child of next) resizeObserver.observe(child);
  observed = next;
}

function attach(box: HTMLElement | null) {
  detach();
  attached = box;
  if (!box) return;
  box.addEventListener("scroll", onScroll, { passive: true });
  resizeObserver = new ResizeObserver(() => update());
  // The track is observed beside the box: on the frame the panel first appears the box can already
  // have a size while the track, which is drawn after it, does not, and a thumb sized against a
  // track of zero stays wrong until one of the two is measured again.
  resizeObserver.observe(box);
  if (track.value) resizeObserver.observe(track.value);
  watchChildren();
  childObserver = new MutationObserver(watchChildren);
  childObserver.observe(box, { childList: true });
  update();
}

function cancelDrag() {
  const pointerId = drag?.pointerId;
  drag = null;
  // The capture is released rather than left to the browser, so a panel that is torn down mid-drag
  // does not leave this holding a pointer id that belongs to a box that is no longer there.
  if (pointerId !== undefined && track.value?.hasPointerCapture(pointerId)) {
    track.value.releasePointerCapture(pointerId);
  }
}

function detach() {
  attached?.removeEventListener("scroll", onScroll);
  cancelDrag();
  resizeObserver?.disconnect();
  resizeObserver = undefined;
  childObserver?.disconnect();
  childObserver = undefined;
  observed = new Set();
  attached = null;
  if (fadeTimer !== undefined) window.clearTimeout(fadeTimer);
  fadeTimer = undefined;
  lit.value = false;
}

function onPointerDown(event: PointerEvent) {
  const rail = track.value;
  if (event.button !== 0 || !rail) return;
  update();
  if (!geometry.scrollable) return;
  // The track and the pointer are in the same units here — unlike the terminal, whose track is
  // inside a pane scaled by the zoom setting — so the offset is read straight off the rect.
  const y = event.clientY - rail.getBoundingClientRect().top;
  // A press on the thumb takes hold of it wherever it was caught, so the thumb does not jump to
  // centre itself under the pointer. A press on the empty track puts the middle of the thumb there
  // instead, which is what every other scrollbar does and is what makes clicking above or below the
  // thumb a jump rather than a press that does nothing at all.
  const grabOffset = y >= geometry.top && y <= geometry.top + geometry.height ? y - geometry.top : geometry.height / 2;
  drag = { pointerId: event.pointerId, grabOffset };
  rail.setPointerCapture(event.pointerId);
  scrollTo(y - grabOffset);
  wake();
  event.preventDefault();
}

function onPointerMove(event: PointerEvent) {
  const rail = track.value;
  if (!rail || !drag || drag.pointerId !== event.pointerId) return;
  scrollTo(event.clientY - rail.getBoundingClientRect().top - drag.grabOffset);
  wake();
  event.preventDefault();
}

function onPointerUp(event: PointerEvent) {
  if (!drag || drag.pointerId !== event.pointerId) return;
  cancelDrag();
  wake();
}

function onLostPointerCapture(event: PointerEvent) {
  if (!drag || drag.pointerId !== event.pointerId) return;
  drag = null;
  wake();
}

/**
 * The bar is a sibling of the box rather than part of it, so a wheel over it never reaches the box's
 * own scrolling. The event's line or page delta is preserved rather than applied as pixels, so a
 * small trackpad gesture over a five-pixel strip is not a screenful.
 */
function onWheel(event: WheelEvent) {
  const box = attached;
  if (!box || !geometry.scrollable || event.deltaY === 0) return;
  event.preventDefault();
  const before = box.scrollTop;
  if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE) {
    box.scrollTop = before + Math.trunc(event.deltaY) * box.clientHeight;
  } else {
    const deltaPx = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? event.deltaY * LINE_PX : event.deltaY;
    wheelRemainderPx += deltaPx;
    const px = Math.trunc(wheelRemainderPx);
    if (px === 0) return;
    wheelRemainderPx -= px;
    box.scrollTop = before + px;
  }
  // A box already at an end ignores the nudge, and a remainder kept past that is a nudge owed twice.
  if (box.scrollTop === before) wheelRemainderPx = 0;
  update();
  wake();
}

/**
 * Keyboard scrolling for a bar that is not a scroller.
 *
 * The box answers the keys on its own while it has focus, and this is for when the bar itself has
 * it, which is the one case where a pointer-only affordance is unreachable.
 */
function onKeydown(event: KeyboardEvent) {
  const box = attached;
  if (!box || !geometry.scrollable) return;
  const page = Math.max(1, box.clientHeight - LINE_PX);
  const next = (() => {
    switch (event.key) {
      case "ArrowUp":
        return box.scrollTop - ARROW_PX;
      case "ArrowDown":
        return box.scrollTop + ARROW_PX;
      case "PageUp":
        return box.scrollTop - page;
      case "PageDown":
        return box.scrollTop + page;
      case "Home":
        return 0;
      case "End":
        return geometry.maxOffset;
      default:
        return null;
    }
  })();
  if (next === null) return;
  event.preventDefault();
  box.scrollTop = Math.min(geometry.maxOffset, Math.max(0, next));
  update();
  wake();
}

watch(() => props.target, attach);
onMounted(() => attach(props.target));
onBeforeUnmount(detach);

/** Re-measures after a change this component cannot see, such as a list that grew without scrolling. */
defineExpose({ refresh: update });
</script>

<template>
  <div
    v-if="enabled"
    ref="track"
    class="overlay-scrollbar"
    :class="{ 'overlay-scrollbar-idle': !visible }"
    role="scrollbar"
    :aria-label="label"
    aria-orientation="vertical"
    aria-valuemin="0"
    :aria-valuemax="maximum"
    :aria-valuenow="position"
    tabindex="0"
    @keydown="onKeydown"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerUp"
    @lostpointercapture="onLostPointerCapture"
    @wheel="onWheel"
  >
    <div class="overlay-scrollbar-thumb" :style="{ top: `${thumb.top}px`, height: `${thumb.height}px` }" />
  </div>
</template>
