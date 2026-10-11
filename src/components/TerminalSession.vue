<script setup lang="ts">
import { Channel } from "@tauri-apps/api/core";
import { emit as emitAppEvent } from "@tauri-apps/api/event";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import ConfirmDialog from "./ConfirmDialog.vue";
import { isIpcError } from "../domain/ipc";
import { terminalHasProcess } from "../domain/workspace";
import type { TerminalSessionStatus } from "../domain/workspace";
import type { TerminalCursorStyle, TerminalScrollbarMode } from "../domain/settings";
import {
  closeTerminal,
  createTerminal,
  getTerminalStatus,
  launchPiReviewTerminal,
  resizeTerminal,
  writeTerminal,
} from "../lib/ipc";
import type { ReviewLaunch } from "../lib/ipc";
import type { ReviewRound } from "../domain/review";
import {
  attachTerminalRenderer,
  createMusterTerminal,
  musterTerminalTheme,
  setTerminalLigatures,
  enableTerminalSelectionCopy,
  preloadTerminalFonts,
  terminalFontSize,
  watchTerminalRendererRecovery,
} from "../lib/muster-terminal";
import { registerFilePathLinks } from "../lib/terminal-file-links";
import { enableClickToMoveCursor } from "../lib/terminal-click-cursor";
import type { ClickToMoveCursor } from "../lib/terminal-click-cursor";
import { watchKeyboardProtocol } from "../lib/terminal-keys";
import { createPtyOutputWriter } from "../lib/terminal-renderer";
import type { PtyOutputWriter } from "../lib/terminal-renderer";
import { registerShellIntegration } from "../lib/terminal-shell-integration";
import { scrollbarOffsetForTop, terminalScrollbarGeometry } from "../lib/terminal-scrollbar";
import type { TerminalScrollbarGeometry } from "../lib/terminal-scrollbar";
import { theme } from "../presentation/theme";
import { useToasts } from "../presentation/toasts";

const props = withDefaults(
  defineProps<{
    checkoutId: string;
    /** The session's own name, which the confirmation names: a terminal asked about by what it is
     * called, not by an id no reader has ever seen. */
    name?: string;
    active: boolean;
    visible?: boolean;
    focused?: boolean;
    scrollbar?: TerminalScrollbarMode;
    fontSize?: number;
    ligatures?: boolean;
    /**
     * Whether a selection gesture copies what it selected. Read inside the copy callback rather than
     * at registration, which is what makes it reach the terminals already open, and it is the copy
     * alone that is gated: the selection gesture itself stays, because reading and highlighting a
     * line is not something this preference turns off.
     */
    selectionCopy?: boolean;
    cursorBlink?: boolean;
    cursorStyle?: TerminalCursorStyle;
    zoom?: number;
    /**
     * Set when this terminal is a Pi started to receive a review rather than a shell.
     *
     * Read once, when the session starts: it says what the terminal was opened to become, and a
     * terminal that is already running is not the same one re-decided.
     */
    launch?: ReviewLaunch;
  }>(),
  {
    visible: true,
    name: undefined,
    focused: false,
    scrollbar: "hidden",
    fontSize: 16,
    ligatures: true,
    selectionCopy: true,
    cursorBlink: true,
    cursorStyle: "block",
    zoom: 1,
    launch: undefined,
  },
);
const emit = defineEmits<{
  created: [result: Awaited<ReturnType<typeof createTerminal>>];
  /**
   * The round a launched Pi's terminal is holding.
   *
   * A separate answer from `created` because it means something different: the terminal existing
   * is not the round arriving, which is why the round travels with it and stays pending.
   */
  reviewLaunched: [round: ReviewRound];
  closed: [workspace: Awaited<ReturnType<typeof closeTerminal>>];
  statusChanged: [status: TerminalSessionStatus];
  failed: [message: string];
  openFile: [path: string];
  openExternalUrl: [url: string];
}>();

const terminalElement = ref<HTMLElement | null>(null);
const scrollbarTrack = ref<HTMLElement | null>(null);
const scrollbarThumb = ref({ top: 0, height: 0 });
const scrollbarLit = ref(false);
const scrollbarPosition = ref(0);
const scrollbarMaximum = ref(0);
const state = ref<TerminalSessionStatus>({ state: "running", foregroundProcess: false });
const error = ref<string | null>(null);
const sessionUnavailable = ref(false);
const closing = ref(false);
const terminal = createMusterTerminal(props.fontSize, props.cursorBlink, props.cursorStyle, props.zoom);
const fit = new FitAddon();
terminal.loadAddon(fit);
const { pushCause } = useToasts();

/** The scrollbar is Muster' own and only exists in the two modes that draw one. */
const scrollbarEnabled = computed(() => props.scrollbar !== "hidden");
/** `always` has no fade to get to, so its thumb is simply there. */
const scrollbarVisible = computed(() => props.scrollbar === "always" || scrollbarLit.value);

let sessionId: string | null = null;
let terminalTitle: string | null = null;
/**
 * What the shell's last command exited with, and the only failure signal a live session has.
 *
 * Held outside `state` and merged back into every status because `updateStatus` replaces the whole
 * object on each 750ms poll, which is exactly what `terminalTitle` above does and for the same
 * reason: a field the backend never sends has to survive the poll that replaces the backend's answer
 * with it. `undefined` is a real value here and not the same as absent — it is what says "the last
 * command is running", which is what takes the red off the sidebar's glyph.
 */
let lastCommandExit: number | undefined;
let shellIntegration: { dispose(): void } | undefined;
let channel: Channel<ArrayBuffer> | undefined;
let resizeObserver: ResizeObserver | undefined;
let statusTimer: number | undefined;
let statusPollInFlight = false;
let statusPollError: string | null = null;
const MAX_TERMINAL_INPUT_BYTES = 1024 * 1024;
const MAX_QUEUED_TERMINAL_INPUTS = 128;
let queuedTerminalInputBytes = 0;
let queuedTerminalInputRequests = 0;
let inputGeneration = 0;
let inputQueue: Promise<void> = Promise.resolve();
let resizeQueue: Promise<void> = Promise.resolve();
let resizeScheduled = false;
let disposed = false;
let started = false;
let terminalReady = false;
// The registration that copies a selection, and the handle that takes it back off the terminal.
let selectionCopyDisposer: { dispose(): void } | undefined;
// The click that moves the shell's own cursor. Created in `onMounted` and told about the prompt by
// the shell-integration callback, which is registered before it and fires for the very first prompt.
let clickCursor: ClickToMoveCursor | undefined;
let fileLinks: { dispose(): void } | undefined;
let outputWriter: PtyOutputWriter | undefined;
let rendererRecovery: { dispose(): void } | undefined;
let latestSize = { cols: 0, rows: 0 };
/** What the queue is working on: the last size handed to the PTY, or the one it refused. */
let attemptedSize = { cols: 0, rows: 0 };
let geometry: TerminalScrollbarGeometry = { top: 0, height: 0, scrollable: false, maxOffset: 0, travel: 0 };
let scrollbarFadeTimer: number | undefined;
let scrollbarDrag: { pointerId: number; grabOffset: number } | null = null;
let wheelRemainderPx = 0;

/**
 * How long `auto` leaves the thumb up after the last scroll.
 *
 * Long enough to read where the thumb landed and to catch it again, short enough that a terminal
 * nobody is scrolling looks like a terminal with no scrollbar. Which is what the setting is for
 * people who scrolled once and decided they did not need one.
 */
const SCROLLBAR_FADE_MS = 900;

/**
 * How often a session is asked how it is doing.
 *
 * A terminal on screen is watched closely, because that is where an exit and the name of the process
 * in front of the shell are read: an exit closes the session, and the rest is what the pane and its
 * row in the sidebar say. A terminal nobody is looking at still has to keep that row honest, so it
 * asks rarely rather than not at all. Every pane stays mounted with `v-show`, so at the fast pace
 * ten hidden terminals are ~800 backend calls a minute, each one validating ownership in SQLite and
 * inspecting a process, none of it for a terminal that is `display: none`.
 */
const STATUS_POLL_MS = 750;
const BACKGROUND_STATUS_POLL_MS = 5000;
const UNAVAILABLE_STATUS_RETRY_MS = 5000;

/**
 * Where the reader behind the PTY is told how much of its output this window has parsed.
 *
 * This is the only message in a terminal that runs backwards. Everything else about a session is
 * an answer, and the reader cannot be asked for more output while it has no way to be told where
 * the output it already sent has got to — which is why nothing here waits for one: a report that
 * never arrives costs throughput for five seconds and then the reader carries on on its own.
 */
const OUTPUT_FLOW_EVENT = "terminal-output-flow";

/** What the window has parsed in total, and the last figure it was reported as. */
let processedOutput = 0;
let reportedOutput = -1;

/**
 * Tells the reader how much of the output it sent has been parsed here.
 *
 * A total and not a pending count, because the reader counts its own output from the moment it
 * hands it over: what it needs from here is the same number counted from this side, and only the
 * difference between the two covers the chunks still in the channel. A session with no id yet
 * cannot be named, so the answer waits for one rather than being sent to nobody — the writer keeps
 * counting either way, and `startSession` flushes what it has the moment the session exists.
 */
function reportOutputFlow() {
  if (disposed || !sessionId || reportedOutput === processedOutput) return;
  reportedOutput = processedOutput;
  void emitAppEvent(OUTPUT_FLOW_EVENT, { sessionId, parsed: processedOutput }).catch(() => {});
}

function errorMessage(cause: unknown) {
  return isIpcError(cause) ? cause.message : cause instanceof Error ? cause.message : String(cause);
}

function showError(cause: unknown) {
  statusPollError = null;
  error.value = errorMessage(cause);
}

function showStatusPollError(cause: unknown) {
  const message = errorMessage(cause);
  if (error.value !== message) error.value = message;
  statusPollError = message;
}

function clearStatusPollError() {
  if (statusPollError !== null && error.value === statusPollError) error.value = null;
  statusPollError = null;
}

/**
 * Reads the scrollback and puts the thumb where it belongs.
 *
 * The track's own height is measured rather than assumed because the pane is whatever height the
 * splitter and the layout leave it, and the arithmetic is in `terminal-scrollbar.ts` over four
 * numbers. Writing the style only when the numbers moved is what keeps this cheap enough to call
 * on every scroll: a terminal that prints for a minute fires this on every line, and a scrollback
 * whose length is not a multiple of anything will otherwise re-render the pane at the same size
 * a thousand times.
 */
function updateScrollbar() {
  if (!scrollbarEnabled.value) return;
  const scrollback = terminal.buffer.active;
  geometry = terminalScrollbarGeometry(
    { length: scrollback.length, viewportY: scrollback.viewportY, rows: terminal.rows },
    scrollbarTrack.value?.clientHeight ?? 0,
  );
  scrollbarPosition.value = Math.min(geometry.maxOffset, Math.max(0, scrollback.viewportY));
  scrollbarMaximum.value = geometry.maxOffset;
  // A terminal whose whole history fits on screen has nothing to say with a thumb, so in `auto` it
  // does not get to flash one. `always` still draws it: a full-height thumb is an honest answer,
  // and a person who asked for a permanent scrollbar is entitled to see that there is nothing to
  // scroll rather than to wonder whether the setting took.
  if (!geometry.scrollable) scrollbarLit.value = false;
  if (geometry.top === scrollbarThumb.value.top && geometry.height === scrollbarThumb.value.height) return;
  scrollbarThumb.value = { top: geometry.top, height: geometry.height };
}

/**
 * `auto` shows the thumb while something is moving and takes it away when nothing is.
 *
 * The timer restarts on every scroll instead of running once, so a build that prints for a minute
 * keeps its scrollbar up for that minute (the thumb genuinely is moving the whole time) and the
 * fade happens when the output stops. Dragging counts as scrolling, and so does the wheel over
 * the track, or the thumb would fade out from under a pointer that is still holding it.
 */
function wakeScrollbar() {
  if (!scrollbarEnabled.value || props.scrollbar === "always" || !geometry.scrollable) return;
  scrollbarLit.value = true;
  if (scrollbarFadeTimer !== undefined) window.clearTimeout(scrollbarFadeTimer);
  scrollbarFadeTimer = undefined;
  if (scrollbarDrag) return;
  scrollbarFadeTimer = window.setTimeout(() => {
    scrollbarFadeTimer = undefined;
    scrollbarLit.value = false;
  }, SCROLLBAR_FADE_MS);
}

/** Moves the viewport to where a thumb dropped at `top` would stand, and re-reads it. */
function scrollbarTo(top: number) {
  terminal.scrollToLine(scrollbarOffsetForTop(top, geometry));
  updateScrollbar();
}

function onScrollbarPointerDown(event: PointerEvent) {
  if (event.button !== 0) return;
  const track = scrollbarTrack.value;
  if (!track) return;
  updateScrollbar();
  if (!geometry.scrollable) return;
  // The track is inside the scaled window and the pointer is not, so the offset is divided back
  // into the track's own units: the geometry it is compared against is measured in those.
  const y = (event.clientY - track.getBoundingClientRect().top) / props.zoom;
  // A press on the thumb takes hold of it wherever it was caught, so the thumb does not jump to
  // centre itself under the pointer. A press on the empty track puts the middle of the thumb there
  // instead, which is what every other scrollbar does and is what makes clicking above or below
  // the thumb a jump rather than a press that does nothing at all.
  const grabOffset = y >= geometry.top && y <= geometry.top + geometry.height ? y - geometry.top : geometry.height / 2;
  scrollbarDrag = { pointerId: event.pointerId, grabOffset };
  track.setPointerCapture(event.pointerId);
  scrollbarTo(y - grabOffset);
  wakeScrollbar();
  event.preventDefault();
}

function onScrollbarPointerMove(event: PointerEvent) {
  const track = scrollbarTrack.value;
  if (!track || !scrollbarDrag || scrollbarDrag.pointerId !== event.pointerId) return;
  scrollbarTo((event.clientY - track.getBoundingClientRect().top) / props.zoom - scrollbarDrag.grabOffset);
  wakeScrollbar();
  event.preventDefault();
}

function onScrollbarPointerUp(event: PointerEvent) {
  if (!scrollbarDrag || scrollbarDrag.pointerId !== event.pointerId) return;
  const pointerId = scrollbarDrag.pointerId;
  scrollbarDrag = null;
  // The capture is released rather than left to the browser, so a view that is torn down mid-drag
  // does not leave this holding a pointer id that belongs to a pane that is no longer there.
  if (scrollbarTrack.value?.hasPointerCapture(pointerId)) {
    scrollbarTrack.value.releasePointerCapture(pointerId);
  }
  wakeScrollbar();
}

function onScrollbarLostPointerCapture(event: PointerEvent) {
  if (!scrollbarDrag || scrollbarDrag.pointerId !== event.pointerId) return;
  scrollbarDrag = null;
  wakeScrollbar();
}

function cancelScrollbarDrag() {
  const pointerId = scrollbarDrag?.pointerId;
  scrollbarDrag = null;
  if (pointerId !== undefined && scrollbarTrack.value?.hasPointerCapture(pointerId)) {
    scrollbarTrack.value.releasePointerCapture(pointerId);
  }
}

/**
 * The overlay is a sibling of the terminal rather than part of it, so a wheel over it never
 * reaches xterm's own viewport. Preserve the event's line or pixel delta instead of turning a
 * small trackpad gesture into a screenful, and accumulate pixel deltas until they cover a row.
 */
function onScrollbarWheel(event: WheelEvent) {
  if (!geometry.scrollable || event.deltaY === 0) return;
  event.preventDefault();
  const cellHeight = (scrollbarTrack.value?.clientHeight ?? 0) / terminal.rows;
  if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE) {
    wheelRemainderPx = 0;
    terminal.scrollLines(Math.trunc(event.deltaY) * terminal.rows);
    return;
  }
  const deltaPx = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? event.deltaY * cellHeight : event.deltaY;
  wheelRemainderPx += deltaPx;
  const lines = Math.trunc(wheelRemainderPx / Math.max(1, cellHeight));
  if (lines === 0) return;
  wheelRemainderPx -= lines * cellHeight;
  terminal.scrollLines(lines);
  wakeScrollbar();
}

function onScrollbarKeydown(event: KeyboardEvent) {
  if (!geometry.scrollable) return;
  const page = Math.max(1, terminal.rows - 1);
  const next = (() => {
    switch (event.key) {
      case "ArrowUp":
        return scrollbarPosition.value - 1;
      case "ArrowDown":
        return scrollbarPosition.value + 1;
      case "PageUp":
        return scrollbarPosition.value - page;
      case "PageDown":
        return scrollbarPosition.value + page;
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
  terminal.scrollToLine(Math.min(geometry.maxOffset, Math.max(0, next)));
  updateScrollbar();
  wakeScrollbar();
}

/**
 * Puts the status poll on the clock the pane's own visibility asks for, and takes it away when the
 * session has nothing left to report.
 *
 * One function owns the timer, so every change of pace and every end of it goes through the same
 * place. The interval is rebuilt rather than re-timed, so a terminal that has just come back to the
 * foreground starts asking at its own pace from now instead of waiting out the slow one first.
 */
function syncStatusPolling() {
  if (statusTimer !== undefined) {
    window.clearInterval(statusTimer);
    statusTimer = undefined;
  }
  if (disposed || !sessionId || state.value.state !== "running") return;
  const pace = sessionUnavailable.value
    ? UNAVAILABLE_STATUS_RETRY_MS
    : props.active && props.visible
      ? STATUS_POLL_MS
      : BACKGROUND_STATUS_POLL_MS;
  statusTimer = window.setInterval(() => void pollStatus(), pace);
}

function updateStatus(status: TerminalSessionStatus) {
  // Preserve the shell integration result across status polls, whose payload omits it; otherwise the
  // sidebar's glyph would flicker back to grey between polls.
  state.value = {
    ...status,
    ...(terminalTitle !== null && { terminalTitle }),
    ...(lastCommandExit !== undefined && { lastCommandExit }),
  };
  emit("statusChanged", state.value);
  if (status.state !== "exited") return;
  // Nothing left to poll for, and asking anyway would only produce errors for a session the backend
  // no longer has.
  syncStatusPolling();
  // A shell that has exited has nothing left to say, and holding its last screen open is worse
  // than losing it: the panel shows a frozen frame that looks like a hung app, the session stays
  // in the sidebar with a live-looking entry, and the only way out is the close button on a pane
  // the user has to go looking for. `exit` and `:q` are how a shell is ended on purpose, so the
  // session goes with it. The close asks the backend once more before acting, finds the process
  // already gone, and so never reaches the confirmation that stopping a live process needs: that
  // question stays where it belongs, on the close the user asked for.
  //
  // This runs for any status report, including the one `requestClose` makes on its own way out,
  // and the `closing` guard there turns that second call into a no-op instead of a loop.
  void requestClose();
}

async function pollStatus() {
  if (disposed || !sessionId || statusPollInFlight) return;
  // A read that comes back after the pane is gone, or after the session it asked about is no longer
  // this component's, has nobody left to report to: publishing it writes state on an unmounted
  // component and can ask a session that is on its way out to close itself. The checkout and session
  // ids are captured before the ask and compared after it, so a reply is only ever applied to the
  // session that asked for it.
  const id = sessionId;
  const checkoutId = props.checkoutId;
  statusPollInFlight = true;
  try {
    const status = await getTerminalStatus(checkoutId, id);
    if (disposed || sessionId !== id || props.checkoutId !== checkoutId) return;
    if (sessionUnavailable.value) {
      sessionUnavailable.value = false;
      syncStatusPolling();
    }
    clearStatusPollError();
    updateStatus(status);
  } catch (cause) {
    if (disposed || sessionId !== id || props.checkoutId !== checkoutId) return;
    showStatusPollError(cause);
    if (
      isIpcError(cause) &&
      (cause.code === "terminal_session_missing" || cause.code === "terminal_ownership_mismatch")
    ) {
      sessionUnavailable.value = true;
      syncStatusPolling();
    }
  } finally {
    statusPollInFlight = false;
    // If ownership changed while the old request was pending, ask for the current session now
    // rather than waiting for the next interval. The one-in-flight guard still bounds IPC calls.
    if (
      !disposed &&
      !closing.value &&
      state.value.state === "running" &&
      sessionId &&
      (sessionId !== id || props.checkoutId !== checkoutId)
    ) {
      void pollStatus();
    }
  }
}

function fitActiveView() {
  if (!terminalReady || !props.active || !props.visible || !terminalElement.value) return;
  if (terminalElement.value.clientWidth === 0 || terminalElement.value.clientHeight === 0) return;
  fit.fit();
  // The thumb is a fraction of the pane, so it is measured where the pane is measured, and this is
  // where the pane changes size. It goes before the column recovery below rather than after it
  // because that recovery gives columns back and keeps the rows: a terminal that fits is the same
  // height as one that does not, so there is nothing about the thumb that changes after it.
  updateScrollbar();
  // FitAddon reserves 14px for an overview ruler whenever scrollback is enabled, even though
  // this terminal hides the ruler and scrollbar. Recover those columns without clipping TUIs.
  const screen = terminal.element?.querySelector(".xterm-screen");
  if (!screen) return;
  const cellWidth = screen.getBoundingClientRect().width / terminal.cols;
  if (!cellWidth) return;
  const cols = Math.floor(terminalElement.value.getBoundingClientRect().width / cellWidth);
  if (cols > terminal.cols) terminal.resize(cols, terminal.rows);
}

/**
 * Whether a resize has anywhere to go.
 *
 * A session that is closing or already unmounted has no pane left to size: the PTY is either about
 * to be shut down or already is, and a send after that is a call the backend refuses at best. This
 * is the same guard `queueInput` uses, for the same reason: once the session is on its way out,
 * nothing about the layout is worth another round trip.
 */
function resizeAllowed() {
  return !closing.value && !disposed && state.value.state === "running";
}

function queueResize(cols: number, rows: number) {
  latestSize = { cols, rows };
  if (!sessionId || !resizeAllowed() || resizeScheduled) return;
  resizeScheduled = true;
  resizeQueue = resizeQueue
    .catch(() => {})
    .then(async () => {
      while (sessionId && resizeAllowed()) {
        const size = latestSize;
        const id = sessionId;
        attemptedSize = size;
        await resizeTerminal(props.checkoutId, id, size.cols, size.rows);
        if (size.cols === latestSize.cols && size.rows === latestSize.rows) break;
      }
    })
    .catch(showError)
    .finally(() => {
      resizeScheduled = false;
      // A drain that ended because the session is going away has nothing left to correct, and
      // re-queuing here would put a resize behind the close.
      if (!sessionId || !resizeAllowed()) return;
      // Otherwise, a measurement that landed after the loop's last comparison and before here is
      // refused by the guard above while the queue is still draining, and nothing sends it
      // afterwards. The pane would keep the size it just had while the space around it had already
      // changed, which is what a shell wrapping to the old width looks like after the layout
      // moves. A size the backend refused counts as attempted, so it is not asked again.
      if (attemptedSize.cols !== latestSize.cols || attemptedSize.rows !== latestSize.rows) {
        queueResize(latestSize.cols, latestSize.rows);
      }
    });
}

function queueInput(value: string): Promise<boolean> {
  if (!sessionId || closing.value || state.value.state !== "running") return Promise.resolve(false);
  if (!value) return Promise.resolve(true);
  if (value.length > MAX_TERMINAL_INPUT_BYTES) {
    showError("Terminal input exceeds the 1 MiB limit.");
    return Promise.resolve(false);
  }
  const id = sessionId;
  const checkoutId = props.checkoutId;
  const generation = inputGeneration;
  const bytes = new TextEncoder().encode(value);
  if (
    bytes.length > MAX_TERMINAL_INPUT_BYTES ||
    queuedTerminalInputBytes + bytes.length > MAX_TERMINAL_INPUT_BYTES ||
    queuedTerminalInputRequests >= MAX_QUEUED_TERMINAL_INPUTS
  ) {
    showError("Terminal input queue is full; wait for pending input to finish.");
    return Promise.resolve(false);
  }
  queuedTerminalInputBytes += bytes.length;
  queuedTerminalInputRequests += 1;
  const write = inputQueue
    .then(async () => {
      if (disposed || generation !== inputGeneration) return false;
      if (sessionId !== id || props.checkoutId !== checkoutId) {
        showError("Queued terminal input was discarded because the session changed.");
        return false;
      }
      await writeTerminal(checkoutId, id, bytes);
      return true;
    })
    .finally(() => {
      queuedTerminalInputBytes -= bytes.length;
      queuedTerminalInputRequests -= 1;
    });
  inputQueue = write.then(
    () => undefined,
    (cause) => {
      inputGeneration += 1;
      if (disposed) return;
      const discardedBytes = queuedTerminalInputBytes;
      const discardedRequests = queuedTerminalInputRequests;
      showError(
        discardedRequests > 0
          ? `${errorMessage(cause)}; discarded ${discardedRequests} queued writes (${discardedBytes} bytes). Inspect the terminal state before typing again.`
          : cause,
      );
    },
  );
  return write.catch(() => false);
}

/**
 * The question a close has to ask, and the answer it is waiting for.
 *
 * A terminal with a process in front of it is the only close that can lose work: the build, the
 * server and the agent are all writing output that nothing has read yet, and stopping them takes it
 * away. A shell at a prompt is the same button and nothing to ask about, which is why the status
 * alone is not the test and `terminalHasProcess` is.
 *
 * It is asked in the app's own dialog rather than the browser's, because a native confirm cannot be
 * styled, is not announced the way the rest of the app announces, and on some platforms renders
 * behind the window it belongs to.
 */
const pendingClose = ref<TerminalSessionStatus | null>(null);

/** What the question names: the program that would stop, in this terminal. */
const closeQuestion = computed(() => {
  const status = pendingClose.value;
  const program = status?.foregroundApp;
  const terminal = props.name ?? "this terminal";
  return {
    title: "Stop the running process?",
    message: program
      ? `“${terminal}” is running ${program}. Closing it stops the process, and anything it has not written yet is lost.`
      : `“${terminal}” still has a process running in it. Closing it stops the process, and anything it has not written yet is lost.`,
  };
});

async function requestClose() {
  if (!sessionId || closing.value) return false;
  closing.value = true;
  try {
    const actualStatus = await getTerminalStatus(props.checkoutId, sessionId);
    // Recorded but not published: a process that ended between two polls has already reported its
    // own ending, and this read exists to be sure before stopping something, not to say it twice.
    state.value = actualStatus;
    // The read is the whole of the guard. A failure to read it falls to the catch below and closes
    // nothing: "I could not tell whether a build was running" is not the same as "none was", and
    // stopping the process anyway is the one answer that cannot be taken back.
    if (terminalHasProcess(actualStatus)) {
      pendingClose.value = actualStatus;
      closing.value = false;
      return false;
    }
    await stopSession();
    return true;
  } catch (cause) {
    showError(cause);
    closing.value = false;
    return false;
  }
}

/**
 * How long a close waits for queued input and resize requests before proceeding.
 *
 * PTY writes have their own deadline, but an IPC response or resize can still fail to settle. The
 * bound keeps close responsive; lifecycle checks prevent unsent input from targeting a moved or
 * unmounted session.
 */
const CLOSE_DRAIN_MS = 1_000;

/**
 * The drain, or the bound, whichever lands first.
 *
 * Subscribed from the start, so a queue that answers after the bound still has somewhere to answer:
 * a failure this wait has already stopped watching for is the terminal's own to show, not a
 * rejection with nothing behind it.
 */
function drainBeforeClose(drain: Promise<void>): Promise<boolean> {
  let bound!: ReturnType<typeof setTimeout>;
  let timedOut = false;
  const giveUp = new Promise<void>(
    (resolve) =>
      (bound = setTimeout(() => {
        timedOut = true;
        resolve();
      }, CLOSE_DRAIN_MS)),
  );
  return Promise.race([drain, giveUp])
    .then(() => !timedOut)
    .finally(() => clearTimeout(bound));
}

/** The close itself, once nothing is left to ask about. */
async function stopSession(): Promise<void> {
  try {
    // One wait over both, because the resize is only worth waiting for behind the input, and
    // because the guard in `queueResize` refuses new work the moment closing begins: from here on,
    // neither queue can hand back a replacement while this waits.
    if (!(await drainBeforeClose(inputQueue.then(() => resizeQueue)))) {
      inputGeneration += 1;
      if (queuedTerminalInputRequests > 0) {
        const message =
          "Terminal input was still pending when close timed out; it may have been partially delivered, and queued input was discarded.";
        showError(message);
        pushCause(message);
      }
    }
    emit("closed", await closeTerminal(props.checkoutId, sessionId!));
  } catch (cause) {
    showError(cause);
    closing.value = false;
  }
}

async function answerClose(confirmed: boolean) {
  const status = pendingClose.value;
  pendingClose.value = null;
  // Cancel is the whole of the answer that matters: nothing is stopped, the PTY keeps running and
  // the terminal keeps its output. The close is not left half-done for the next attempt to find.
  if (!confirmed || !status) {
    closing.value = false;
    return;
  }
  await stopSession();
}

/** Resolve the shell's current directory without waiting for the slower status poll. */
async function refreshFileLinkWorkingDirectory(checkoutId: string): Promise<string | null> {
  const id = sessionId;
  if (disposed || closing.value || !id || props.checkoutId !== checkoutId) return null;
  try {
    const status = await getTerminalStatus(checkoutId, id);
    if (disposed || closing.value || sessionId !== id || props.checkoutId !== checkoutId) return null;
    return status.state === "running" ? (status.workingDirectory ?? null) : null;
  } catch {
    return null;
  }
}

/** Register links against the checkout and directory this live terminal currently belongs to. */
function registerFileLinks() {
  fileLinks?.dispose();
  fileLinks = registerFilePathLinks(terminal, {
    get checkoutId() {
      return props.checkoutId;
    },
    // Retain the polled value as fallback; path lookups use the authoritative reader below.
    get workingDirectory() {
      return state.value.workingDirectory;
    },
    refreshWorkingDirectory: refreshFileLinkWorkingDirectory,
    open: (path) => emit("openFile", path),
    // A page is the browser's to open, which is where `curl`'s own output goes: the webview is
    // not a browser, so the address travels back over the bridge like the preview's links do.
    openUrl: (url) => emit("openExternalUrl", url),
  });
}

/**
 * Tells an idle shell to change directory, and answers whether it did.
 *
 * `false` means the directory was left alone, and the caller says so out loud rather than
 * pretending the move changed anything: a shell with a command in front of it cannot be told
 * anything without feeding that command, and a session that has exited has no shell left to tell.
 * A rejected `cd` write also returns false; its actual error stays in this terminal's alert. The
 * `cd` is written as a line the shell reads rather than asked of the backend, because a PTY has no
 * way to move a process that is already running.
 */
async function changeDirectory(path: string): Promise<boolean> {
  if (!sessionId || closing.value) return false;
  // Read rather than believe: the polled state can be most of a second old, and a build that
  // started since the last poll would have the `cd` fed to it instead of read by a shell.
  const status = await getTerminalStatus(props.checkoutId, sessionId);
  if (status.state !== "running" || terminalHasProcess(status)) return false;
  // A directory is typed into the shell as input, so anything that could end the line or run a
  // second command is refused rather than quoted into shape.
  if (/[\n\r\0]/.test(path)) return false;
  return queueInput(`cd '${path.replaceAll("'", `'\\''`)}'\n`);
}

/** A moved terminal keeps its process, so which tree a path may name is a thing that changes. */
watch(
  () => props.checkoutId,
  () => {
    if (terminalReady) registerFileLinks();
    if (!closing.value && state.value.state === "running") void pollStatus();
  },
);

/**
 * A shell that `cd`s keeps the terminal and changes what a bare name on a line means, so the
 * links are put back on the directory it is in now rather than the one it was in when the screen
 * was last drawn. xterm holds on to the links it has already been given until the provider changes,
 * and a `LICENSE` underlined under the old directory is a name that opens the wrong file.
 */
watch(
  () => state.value.workingDirectory,
  (directory, was) => {
    if (terminalReady && directory !== was) registerFileLinks();
  },
);

defineExpose({ requestClose, changeDirectory, focus: () => terminal.focus() });

async function startSession() {
  if (started || disposed || !terminalReady || (!props.active && !props.launch) || !terminalElement.value) return;
  started = true;
  fitActiveView();
  // One writer for the whole session, before the first byte can arrive: chunks are joined and
  // handed to xterm a write at a time, and how much of its output xterm has parsed is how far the
  // reader behind it is to let it come. Losing bytes is not an option here — a TUI drawn from a
  // dropped chunk is a TUI that is wrong on screen — so the writer queues rather than ever
  // discarding.
  outputWriter ??= createPtyOutputWriter(terminal, (parsed) => {
    processedOutput = parsed;
    reportOutputFlow();
  });
  channel = new Channel<ArrayBuffer>();
  channel.onmessage = (buffer) => {
    // Read before the writer, because what the program said about its own keyboard arrives in the
    // same output as everything it draws, and a modified key is answered differently depending on
    // whether it has already said so.
    keyboardProtocol.read(buffer);
    outputWriter?.push(buffer);
  };
  const initialSize = { cols: terminal.cols || 80, rows: terminal.rows || 24 };
  const launch = props.launch;
  try {
    // The same session, the same output channel and the same PTY either way: what differs is only
    // what runs in it. A review Pi is handed its notes through this call and is never written to
    // afterwards, so nothing here is a place input could be injected.
    let created: Awaited<ReturnType<typeof createTerminal>>;
    // The round the backend recorded for a Pi it just started. Carried alongside rather than read
    // off the answer because a terminal existing says a Pi was started, not that it read anything:
    // the round arrives pending, and its own state is what says so.
    let launchedRound: ReviewRound | null = null;
    if (launch) {
      const launched = await launchPiReviewTerminal(
        launch,
        props.checkoutId,
        initialSize.cols,
        initialSize.rows,
        channel,
      );
      created = launched;
      launchedRound = launched.round;
    } else {
      created = await createTerminal(props.checkoutId, initialSize.cols, initialSize.rows, channel);
    }
    if (disposed) {
      await closeTerminal(props.checkoutId, created.session.id);
      return;
    }
    sessionId = created.session.id;
    if (launchedRound) emit("reviewLaunched", launchedRound);
    // The writer may already have counted output the backend sent before the session had a name
    // to be answered under, and the first report is what lets the reader's gate be opened by an
    // answer rather than by its own timeout.
    reportOutputFlow();
    emit("created", created);
    if (
      latestSize.cols &&
      latestSize.rows &&
      (latestSize.cols !== initialSize.cols || latestSize.rows !== initialSize.rows)
    ) {
      queueResize(terminal.cols, terminal.rows);
    }
    if (props.visible && props.active && props.focused) terminal.focus();
    await pollStatus();
    syncStatusPolling();
  } catch (cause) {
    showError(cause);
    emit("failed", error.value ?? "Could not start terminal");
  }
}

terminal.onData(queueInput);

// The same queue is how a question reaches the program, so a key answer and a keyboard answer are
// one path: both are bytes going down to whoever is behind the PTY.
const keyboardProtocol = watchKeyboardProtocol((data) => void queueInput(data));

/**
 * Shift+Enter is a different key from Enter, and xterm.js 6 cannot say so: its keyboard mapping
 * turns Return into `\r` without ever looking at the shift, while the same mapping does read the
 * shift for Tab. So behind a reader that binds the two to different things — OpenCode sends the
 * prompt on `return` and breaks the line on `shift+return` — Shift+Enter arrives as the return and
 * the prompt goes instead of the line.
 *
 * Which encoding it is sent in is the reader's to have chosen. A program that pushed the flags of
 * the keyboard protocol is reading keys in the CSI-u form, and gets the key with its modifier
 * attached: `ESC [ 13 ; 2 u`, 13 being the key and 2 the shift, one bit above the base value. A
 * program that never said anything gets the line ending that is also what `ctrl+j` sends and what
 * every program treats as Enter, so a shell still runs the line instead of printing an escape it
 * cannot read. Either way `false` comes back, so xterm sends no carriage return as well.
 */
function forwardShiftEnter(event: KeyboardEvent): boolean {
  if (event.key !== "Enter" || !event.shiftKey) return true;
  // A shift on its own is the key this is about. With another modifier held it is that other
  // shortcut, and a terminal that cannot even name the modifiers is not where to decide what they
  // should have been.
  if (event.ctrlKey || event.altKey || event.metaKey) return true;
  // The character event is swallowed along with the keydown. xterm 6 registers its keydown listener
  // without the preventDefault that would stop the browser from raising one, so the press is looked
  // at twice, and letting the second look through is where xterm's own carriage return came from:
  // sent right beside the key that goes out here, which a reader holding both as "return" answers
  // by sending the message and breaking the line at once.
  if (event.type === "keyup") return true;
  if (event.type === "keydown") void queueInput(keyboardProtocol.csiU ? "\u001b[13;2u" : "\n");
  return false;
}
terminal.attachCustomKeyEventHandler(forwardShiftEnter);

terminal.onTitleChange((title) => {
  // PTY titles are untrusted text: keep them short and strip control characters before exposing them to UI.
  terminalTitle =
    title
      .replace(/\p{Cc}/gu, "")
      .trim()
      .slice(0, 80) || null;
  state.value = { ...state.value, terminalTitle };
  emit("statusChanged", state.value);
});
terminal.onResize(({ cols, rows }) => queueResize(cols, rows));
// Registered at setup rather than in `onMounted` because the hook the backend installed is already
// answering from the shell's very first prompt, and output is delivered over the channel from the
// moment the session is created: a handler registered after the first paint would miss the exit code
// of whatever ran first.
shellIntegration = registerShellIntegration(terminal, (event) => {
  // The prompt boundaries are what say whether anything on screen is editable, and they are answered
  // from inside this handler on purpose: the cell a prompt ended on is the one the terminal's cursor
  // was on as that marker was parsed, and every later read of it is a cursor that has moved on.
  if (event.kind === "input-started") clickCursor?.inputStarted();
  if (event.kind === "command-started") clickCursor?.commandStarted();
  // Only a command starting clears the failure: red means "the last command you ran failed", and
  // once another command is in front of you that is no longer what the row is telling you.
  //
  // A prompt arriving does NOT clear it, which is the whole point of naming the event rather than
  // clearing on everything that is not a finish. The prompt is printed the moment the command ends —
  // before the next line can be typed — so clearing there wiped the exit code a failing command had
  // just reported, every single time, and the row could never be red. The prompt says the shell is
  // ready for the next line; it says nothing about the line that just ran.
  if (event.kind === "command-started") lastCommandExit = undefined;
  if (event.kind === "command-finished") lastCommandExit = event.exitCode;
  state.value = { ...state.value, lastCommandExit };
  emit("statusChanged", state.value);
});
// xterm fires this whenever the viewport moves, whether a wheel, a drag, Shift+PageUp or output
// arriving at the bottom caused it, and it does not promise what the payload is (some paths send
// the new position, some send an object wrapping it). So the position is read back off the buffer
// rather than taken from the event. Output is the reason this is worth subscribing to at all: a
// scrollback that grows has to shrink its thumb, and nothing else says so.
terminal.onScroll(() => {
  updateScrollbar();
  wakeScrollbar();
});

/**
 * Picking a mode is a change to what the pane draws, so the thumb is measured again once the
 * overlay is really there: the element only exists in the modes that draw one, and a thumb sized
 * against a track that was not in the document yet would be sized against nothing.
 */
watch(
  () => props.scrollbar,
  async (mode) => {
    if (scrollbarFadeTimer !== undefined) window.clearTimeout(scrollbarFadeTimer);
    scrollbarFadeTimer = undefined;
    if (mode === "hidden") {
      scrollbarLit.value = false;
      wheelRemainderPx = 0;
      cancelScrollbarDrag();
      return;
    }
    await nextTick();
    updateScrollbar();
    // Turning `auto` on shows the thumb once, so the answer to "did that do anything" is visible,
    // and from there the same fade as a scroll gets it out of the way again.
    wakeScrollbar();
  },
);

/**
 * The palette, repainted onto a terminal that is already open.
 *
 * The stylesheet repaints itself the moment the theme changes; xterm.js does not, because its
 * colors are an object rather than declarations. Assigning the theme is enough and the buffer stays
 * where it is, so the switch costs a repaint and not the session behind it.
 */
watch(theme, () => {
  terminal.options.theme = musterTerminalTheme();
});

/**
 * The size and the two faces of the terminal's own type, watched together because they all end in
 * the same fit.
 *
 * xterm.js re-measures the cell, clears and repaints on the fontSize assignment, so the only thing
 * left is the fit: the PTY is told its new column count through it, and nobody is told anything by
 * the repaint. The ligature joiner and the cursor are options on the same object, and both take
 * effect on the next paint, so neither needs one.
 */
watch(
  () => [props.fontSize, props.ligatures, props.cursorBlink, props.cursorStyle, props.zoom] as const,
  async ([fontSize, ligatures, cursorBlink, cursorStyle, zoom]) => {
    if (!terminalReady) return;
    terminal.options.fontSize = terminalFontSize(fontSize, zoom);
    terminal.options.cursorBlink = cursorBlink;
    terminal.options.cursorStyle = cursorStyle;
    setTerminalLigatures(terminal, ligatures);
    await nextTick();
    fitActiveView();
  },
);

watch(
  () => [props.active, props.visible, props.focused] as const,
  async ([active, visible, focused], [wasActive, wasVisible]) => {
    // Whether this pane is the one on screen is what the poll pace is read from, so it is re-read
    // here rather than left at the pace the session started at.
    syncStatusPolling();
    if (active && visible && sessionUnavailable.value && (!wasActive || !wasVisible)) void pollStatus();
    if (!active && !props.launch) return;
    await nextTick();
    fitActiveView();
    if (visible && focused) terminal.focus();
    void startSession();
  },
);

onMounted(async () => {
  if (!terminalElement.value) return;
  // Let xterm measure and rasterize only after both faces it can draw have loaded. Clearing a
  // WebGL atlas after the fallback was already painted made the first selected cells differ from
  // their neighbours; the bundled faces make waiting here local and deterministic. The load is
  // kicked off when the app starts rather than here, so on any terminal but the first this is
  // already settled and the await costs a microtask.
  await preloadTerminalFonts();
  if (disposed || !terminalElement.value) return;
  terminal.open(terminalElement.value);
  terminalReady = true;
  // Both of these need the terminal on the page, and the fit that follows has to measure the
  // renderer that will actually draw.
  setTerminalLigatures(terminal, props.ligatures);
  // The renderer that just went on, watched so a context lost to sleep or memory pressure can be
  // put back. Without it a terminal that lost one stays on the fallback for the rest of its life.
  rendererRecovery = watchTerminalRendererRecovery(terminal, attachTerminalRenderer(terminal));
  selectionCopyDisposer = enableTerminalSelectionCopy(terminal, (text) => {
    // The setting is read here, on the click, rather than at registration: that is what lets it
    // reach a terminal that was opened before the change without re-registering anything.
    if (!props.selectionCopy) return;
    void writeText(text).catch((cause) => {
      pushCause(cause);
    });
  });
  // After the copy, because both own the mouse: a path that turns out to be a file is also a
  // path a person might want to select, and the click that opens it is already narrowed to
  // ctrl+click, so the two do not contend for the same gesture.
  registerFileLinks();
  // The last registration that owns the mouse, and only once there is a terminal to put it on: it
  // answers the clicks the other two leave alone, which are the ones on the command being typed.
  clickCursor = enableClickToMoveCursor(terminal, queueInput);
  fitActiveView();
  resizeObserver = new ResizeObserver(() => fitActiveView());
  resizeObserver.observe(terminalElement.value);
  void startSession();
});

onUnmounted(() => {
  disposed = true;
  if (statusTimer !== undefined) window.clearInterval(statusTimer);
  if (scrollbarFadeTimer !== undefined) window.clearTimeout(scrollbarFadeTimer);
  scrollbarDrag = null;
  resizeObserver?.disconnect();
  selectionCopyDisposer?.dispose();
  clickCursor?.dispose();
  fileLinks?.dispose();
  rendererRecovery?.dispose();
  outputWriter?.dispose();
  shellIntegration?.dispose();
  if (sessionId) {
    // Nothing is going to parse this session's output any more, so the reader behind it is told
    // so: it lets go of the gate rather than holding the PTY until its own timeout, and it stops
    // waiting for answers that are never coming.
    void emitAppEvent(OUTPUT_FLOW_EVENT, { sessionId }).catch(() => {});
  }
  if (channel) channel.onmessage = () => {};
  terminal.dispose();
});
</script>

<template>
  <section class="terminal-surface flex h-full min-h-0 flex-col overflow-hidden">
    <div class="terminal-region relative min-h-0 flex-1">
      <!-- The host cancels out the root's scale rather than inheriting it: the grid is measured
           and rasterized in the screen's own pixels, and the cell is drawn at the scaled size. -->
      <div
        ref="terminalElement"
        class="terminal-host h-full min-h-0"
        :style="{ zoom: String(1 / props.zoom) }"
        aria-label="Shell terminal"
      />
      <!-- Overlaying the terminal preserves the grid width that xterm's native bar would reserve. -->
      <div
        v-if="scrollbarEnabled"
        ref="scrollbarTrack"
        class="terminal-scrollbar"
        :class="{ 'terminal-scrollbar-idle': !scrollbarVisible }"
        role="scrollbar"
        aria-label="Terminal scrollback"
        aria-orientation="vertical"
        aria-valuemin="0"
        :aria-valuemax="scrollbarMaximum"
        :aria-valuenow="scrollbarPosition"
        tabindex="0"
        @keydown="onScrollbarKeydown"
        @pointerdown="onScrollbarPointerDown"
        @pointermove="onScrollbarPointerMove"
        @pointerup="onScrollbarPointerUp"
        @pointercancel="onScrollbarPointerUp"
        @lostpointercapture="onScrollbarLostPointerCapture"
        @wheel="onScrollbarWheel"
      >
        <div
          class="terminal-scrollbar-thumb"
          :style="{ top: `${scrollbarThumb.top}px`, height: `${scrollbarThumb.height}px` }"
        />
      </div>
    </div>
    <p
      v-if="error || sessionUnavailable"
      role="alert"
      class="m-0 border-t border-(--muster-border) px-3 py-2 text-xs text-(--muster-danger-fg)"
    >
      <span v-if="sessionUnavailable">Terminal session is unavailable. </span>{{ error }}
      <button v-if="sessionUnavailable" type="button" class="ml-2 underline" @click="pollStatus">Retry</button>
    </p>

    <!-- Teleported, because a terminal that is not the one on screen is still mounted and still
         answers: `SessionPane` keeps every view alive with `v-show`, so a pane that is not the active
         one is `display: none` rather than gone. A dialog drawn inside it therefore existed in the
         DOM and was invisible — closing a background terminal with a build in it asked a question
         nobody could see or answer. Drawing it at the body also takes it out of the pane's stacking
         and out of the terminal's zoom, which are the other two ways an overlay ends up underneath
         what it is meant to cover. -->
    <Teleport to="body">
      <ConfirmDialog
        :open="pendingClose !== null"
        :title="closeQuestion.title"
        :message="closeQuestion.message"
        confirm-label="Stop and close"
        destructive
        @confirm="answerClose(true)"
        @close="answerClose(false)"
      />
    </Teleport>
  </section>
</template>
