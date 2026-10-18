<script setup lang="ts">
/* The rename field is a void element with enough attributes that it cannot fit on one line, and
   prettier and this rule disagree about how a void element closes: the rule would have it end in
   `>`, the formatter rewrites that to `/>`. The formatter owns it, as in the other panes that
   hold a field. */
/* eslint-disable vue/html-self-closing */
import type { Component, Ref } from "vue";
import { computed, nextTick, onMounted, onUnmounted, ref, shallowRef, toRef, watch } from "vue";
import {
  ArchiveRestore as ArchiveRestoreIcon,
  ChevronDown as ChevronDownIcon,
  Ellipsis as EllipsisIcon,
  FolderMinus as FolderMinusIcon,
  FolderPlus as FolderPlusIcon,
  GitBranchPlus as GitBranchPlusIcon,
  Plus as PlusIcon,
  SquareTerminal as SquareTerminalIcon,
  X as XIcon,
} from "@lucide/vue";
import {
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuPortal,
  DropdownMenuRoot,
  DropdownMenuTrigger,
} from "reka-ui";
import type { ArchivedCheckout, Checkout, Repo, Session, TerminalSessionStatus } from "../domain/workspace";
import { AGENT_APP, agentSessionTitle, sessionRowTitle, workdirIconKind, workdirTitle } from "../domain/workspace";
import type { AgentAttention } from "../domain/agent";
import { matchAgentSessionTitle } from "../domain/agent";
import type { MainView } from "../domain/main-document";
import type { TerminalAgentRow } from "../presentation/agent-sessions";
import { useDiffStats } from "../presentation/diff-stats";
import { WORKDIR_ICONS } from "../presentation/workdir-icons";
import { nameSteps, pathSteps, widestThatFits } from "../lib/fit-text";
import OverlayScrollbar from "./OverlayScrollbar.vue";

defineOptions({ name: "FolderSidebar" });

const props = withDefaults(
  defineProps<{
    repos: Repo[];
    activeCheckoutId: string | null;
    activeSessionId: string | null;
    /**
     * What the main panel is showing for the active checkout, which is what the accent edge in
     * this panel is worn for.
     *
     * `activeSessionId` answers which terminal the checkout has open and is therefore the row that
     * wears the selection while the panel shows that terminal. It is not enough on its own: opening
     * a file leaves the checkout's session selected in the persistence while the panel is showing
     * the document, and the sidebar went on claiming that the terminal was what was being read.
     *
     * So the panel's own view comes in as well, and the two together are the rule. It is one value
     * for the whole panel rather than one per checkout because only the active checkout can have a
     * view open: the main panel shows one checkout at a time, and every other row in here is
     * context.
     */
    mainViewKind?: MainView["kind"];
    homeCheckoutId?: string | null;
    isOpening: boolean;
    sessionRuntimeStatuses?: Record<string, TerminalSessionStatus>;
    /**
     * The order each checkout's terminals are listed in, keyed by checkout id, as the pane saved it.
     *
     * Absent for a checkout whose layout has not been read yet, and the panel falls back to the
     * order it was given rather than inventing one.
     */
    sessionOrder?: Record<string, string[]>;
    /**
     * The OpenCode sessions of each checkout that has terminals, keyed by checkout id.
     *
     * Keyed by checkout rather than handed over as one summary because a row is about one terminal
     * in one worktree: two terminals in different worktrees are different rows, and each reads only
     * its own. OpenCode 2.0.22 cannot report which session a given terminal has open, so this is
     * every session the service lists — not just this checkout's, because a terminal filed here
     * routinely has its session open in a worktree beside it — and the row matches them by the
     * title its own terminal carried (`matchAgentSessionTitle`). A row that matches exactly one draws
     * that session's state and nothing else draws any, which is what keeps a namesake session off a
     * terminal that is not showing it.
     */
    agentRows?: Record<string, TerminalAgentRow>;
    /**
     * The worktrees that were archived, so a repo root can offer its own back.
     *
     * The sidebar draws the list of what is on the panel; this is what is behind it.
     */
    archivedWorktrees?: ArchivedCheckout[];
    /**
     * The ratio the window's type is drawn at, which the panel needs only to know when to weigh its
     * labels again: a font scale moves every word in the panel without moving the panel, so nothing
     * about the panel's own box changes and nothing else would say so.
     */
    fontScale?: number;
  }>(),
  {
    mainViewKind: "terminal",
    homeCheckoutId: null,
    sessionRuntimeStatuses: () => ({}),
    sessionOrder: () => ({}),
    agentRows: () => ({}),
    archivedWorktrees: () => [],
    fontScale: 1,
  },
);

const emit = defineEmits<{
  openFolder: [];
  selectCheckout: [checkoutId: string, hasChanges: boolean];
  selectSession: [sessionId: string];
  createWorktree: [checkoutId: string];
  newTerminal: [checkoutId: string];
  removeWorktree: [checkoutId: string];
  closeWorkdir: [checkoutId: string];
  closeMissing: [checkoutId: string];
  restoreArchived: [repoId: string];
  closeSession: [sessionId: string];
  renameSession: [sessionId: string, name: string];
  /** Hands a live terminal to another registered checkout. */
  moveSession: [sessionId: string, targetCheckoutId: string, index: number];
}>();

/** Whether the terminal under the pointer came out of this checkout's list. */
function isDraggedFrom(checkout: Checkout): boolean {
  const drag = pointerDrag.value;
  return Boolean(drag?.started && drag.session.checkoutId === checkout.id);
}

/**
 * The slot this row sits in, counted the way `dropIndexAtPoint` counts.
 *
 * The drop line is drawn before a row, so its condition needs the same number the measurement
 * produced — and the measurement leaves the dragged terminal out. Comparing it against the drawn
 * index instead put the line one row too high, which is worse than an offset: inserting a row there
 * moved every row below it down, so the next measurement of the same still pointer landed on a
 * different row and the line vanished.
 */
function dropSlotFor(checkout: Checkout, itemIndex: number): number {
  if (!isDraggedFrom(checkout)) return itemIndex;
  const draggedIndex = orderedSessions(checkout).findIndex((session) => session.id === pointerDrag.value?.session.id);
  return draggedIndex >= 0 && itemIndex > draggedIndex ? itemIndex - 1 : itemIndex;
}

/**
 * How many slots this list has while the dragged terminal is out of the way.
 *
 * The slot past the last row is drawn separately, and it needs the same count: with the dragged row
 * still counted, "past the end" is one slot too far and the line for the real end never appears.
 */
function dropSlotCount(checkout: Checkout): number {
  return orderedSessions(checkout).length - (isDraggedFrom(checkout) ? 1 : 0);
}

/**
 * Whether the drop line belongs immediately above this row.
 *
 * The whole condition lives here rather than in the template because it says the same thing in two
 * places, and a template that spells it out twice is one that will spell it out slightly differently
 * twice.
 */
function dropLineBefore(checkout: Checkout, itemIndex: number): boolean {
  return (
    Boolean(pointerDrag.value?.started) &&
    dropCheckoutId.value === checkout.id &&
    dropIndex.value === dropSlotFor(checkout, itemIndex)
  );
}

/** And whether it belongs past the last row instead, which is a slot of its own. */
function dropLineAfter(checkout: Checkout): boolean {
  return (
    Boolean(pointerDrag.value?.started) &&
    dropCheckoutId.value === checkout.id &&
    dropIndex.value >= dropSlotCount(checkout)
  );
}

/** The one session whose name is being typed, and the text as typed so far. */
const editingId = ref<string | null>(null);
const draftName = ref("");
const renameField = shallowRef<HTMLInputElement | null>(null);

/**
 * A function ref, because the field sits inside a `v-for`: a plain `ref` there would collect
 * every row's element into an array, and there would be nothing to focus.
 */
function captureRenameField(element: unknown) {
  renameField.value = (element as HTMLInputElement | null) ?? null;
}

function startRename(session: Session) {
  editingId.value = session.id;
  draftName.value = session.name;
  // The text the user is replacing is preselected, so typing overwrites the name rather than
  // appending to it, which is what a rename is for.
  void nextTick(() => {
    renameField.value?.focus();
    renameField.value?.select();
  });
}

/** Nothing typed is not a rename: the row keeps the name it had. */
function commitRename(session: Session) {
  const name = draftName.value.trim();
  editingId.value = null;
  if (name && name !== session.name) emit("renameSession", session.id, name);
}

function cancelRename() {
  editingId.value = null;
}

/** The one session whose destination list is open, which is also what builds that list. */
const moveMenuFor = ref<string | null>(null);

/**
 * The one group whose actions menu is open.
 *
 * The menu is reka's, like the titlebar's: it brings the outside press, the Escape, the arrows,
 * the typeahead and the focus back to the button, none of which the panel was giving it. What the
 * panel keeps is which of the groups is open, because that is the one thing reka does not know:
 * there is a menu per group and only one of them is allowed to be.
 */
const groupMenuFor = ref<string | null>(null);

/** A right click anywhere on the header opens the menu the button opens. */
function openGroupMenu(groupId: string) {
  moveMenuFor.value = null;
  groupMenuFor.value = groupId;
}

/** Closing, from the menu itself or from a row that was chosen. reka says when. */
function closeGroupMenu() {
  groupMenuFor.value = null;
}

interface PointerDrag {
  session: Session;
  pointerId: number;
  originX: number;
  originY: number;
  x: number;
  y: number;
  started: boolean;
  source: HTMLElement;
}

/** A press selects as usual; only a deliberate 5px move turns it into a tree drag. */
const DRAG_THRESHOLD = 5;
const AUTO_SCROLL_EDGE = 36;
const pointerDrag = ref<PointerDrag | null>(null);
const dropCheckoutId = ref<string | null>(null);
/** The slot in that checkout's list the dragged terminal would land in. */
const dropIndex = ref(0);
/** The pointer's own y, so a drop above or below a row can be told without re-reading the event. */
const dropPointerY = ref(0);
const sidebarScroll = ref<HTMLElement | null>(null);
let autoScrollFrame: number | undefined;
let suppressedClickSessionId: string | null = null;
let suppressedClickTimer: number | undefined;

/** Any registered, live checkout can take a terminal. */
function moveDestination(session: Session, target: Checkout, index: number): string | null {
  const sourceExists = props.repos.some((repo) =>
    repo.checkouts.some((checkout) => checkout.id === session.checkoutId),
  );
  const targetRepo = props.repos.find((repo) => repo.checkouts.some((checkout) => checkout.id === target.id));
  const registeredTarget = targetRepo?.checkouts.find((checkout) => checkout.id === target.id);
  if (!sourceExists || !registeredTarget || registeredTarget.isMissing) return null;
  // Its own worktree is a destination only when the drop changes its slot.
  if (target.id === session.checkoutId) {
    const current = orderedSessions(registeredTarget).findIndex((item) => item.id === session.id);
    return current < 0 || current === index ? null : target.id;
  }
  return target.id;
}

interface Destination {
  id: string;
  label: string;
  title: string;
}

/**
 * Every registered, live checkout, in the order the panel lists them.
 *
 * The list does not depend on which terminal is asking, so it is built once per pass rather than
 * walked once per terminal: that walk was the whole checkout list again for every candidate of
 * every row, on every pass, to draw a list that was almost always closed.
 */
const moveCandidates = computed(() =>
  props.repos.flatMap((repo) =>
    repo.checkouts
      .filter((checkout) => !checkout.isMissing)
      .map((checkout) => ({ checkout, label: workdirTitle(repo, checkout) })),
  ),
);

/**
 * Whether a terminal has anywhere to go, which is what the row's menu is for.
 *
 * The same question `moveDestination` answers for each candidate, asked once: a terminal's own
 * worktree is not one of them, so with more than one candidate there is always a checkout that is
 * not this row's own, and with a single one the answer is whether that checkout is its own.
 */
function canMoveTo(session: Session): boolean {
  const candidates = moveCandidates.value;
  if (!candidates.length) return false;
  return candidates.length > 1 || candidates[0]!.checkout.id !== session.checkoutId;
}

/** Destinations use the current row session, not a stale snapshot. */
const moveMenuItems = computed<Destination[]>(() => {
  const sessionId = moveMenuFor.value;
  if (!sessionId) return [];
  const session = groups.value
    .flatMap((group) => group.workdirs.flatMap((workdir) => workdir.items))
    .find((item) => item.session.id === sessionId)?.session;
  if (!session) return [];
  return moveCandidates.value.flatMap(({ checkout, label }) =>
    checkout.id !== session.checkoutId && moveDestination(session, checkout, 0)
      ? [{ id: checkout.id, label, title: checkout.path }]
      : [],
  );
});

function selectSession(sessionId: string, event: MouseEvent) {
  if (event.detail > 0 && suppressedClickSessionId === sessionId) {
    suppressedClickSessionId = null;
    if (suppressedClickTimer !== undefined) window.clearTimeout(suppressedClickTimer);
    suppressedClickTimer = undefined;
    return;
  }
  emit("selectSession", sessionId);
}

function startPointerDrag(session: Session, event: PointerEvent) {
  // Mouse only: a touch pointer must retain the normal vertical scroll gesture of the tree.
  if (event.pointerType !== "mouse" || !event.isPrimary || event.button !== 0) return;
  const home = props.repos.flatMap((repo) => repo.checkouts).find((checkout) => checkout.id === session.checkoutId);
  // A drag is worth starting when another worktree can take it, or when its own worktree holds more
  // than one terminal and it can therefore be reordered.
  const elsewhere = props.repos
    .flatMap((repo) => repo.checkouts)
    .some((checkout) => moveDestination(session, checkout, 0));
  if (!home || (!elsewhere && orderedSessions(home).length < 2)) return;
  const source = event.currentTarget as HTMLElement;
  pointerDrag.value = {
    session,
    pointerId: event.pointerId,
    originX: event.clientX,
    originY: event.clientY,
    x: event.clientX,
    y: event.clientY,
    started: false,
    source,
  };
  moveMenuFor.value = null;
  if (suppressedClickTimer !== undefined) window.clearTimeout(suppressedClickTimer);
  suppressedClickSessionId = null;
  try {
    source.setPointerCapture(event.pointerId);
  } catch {
    // Global listeners below still track the press if a webview declines pointer capture.
  }
  window.addEventListener("pointermove", onPointerMove);
  window.addEventListener("pointerup", onPointerUp);
  window.addEventListener("pointercancel", onPointerCancel);
  window.addEventListener("keydown", onDragKeydown);
  window.addEventListener("blur", cancelPointerDrag);
}

/**
 * Where in the target's list the dragged terminal would land: which slot, counted from the top.
 *
 * **The dragged row is not one of the rows counted.** It is drawn at its old position until it is
 * dropped, so a slot counted with it in place would mean one thing for a terminal travelling up and
 * another for the same terminal travelling down — and the difference is a row, which is where the
 * terminal ends up. Measured without it, one number is the same before the drop line is drawn, in the
 * line itself and in the layout that gets saved.
 *
 * It is also measured against the pointer's own position rather than the one the last move carried,
 * which is a frame late and therefore a slot wrong at every edge of a row.
 */
function dropIndexAtPoint(target: HTMLElement, dragging: Session | null): number {
  const rows = [...target.querySelectorAll<HTMLElement>(".workdir-child[data-session-id]")].filter(
    (row) => !row.classList.contains("new-item") && row.dataset.sessionId !== dragging?.id,
  );
  const index = rows.findIndex((row) => {
    const box = row.getBoundingClientRect();
    return dropPointerY.value < box.top + box.height / 2;
  });
  return index < 0 ? rows.length : index;
}

function checkoutAtPoint(x: number, y: number, dragging: Session | null): { checkout: Checkout; index: number } | null {
  const target = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-workdir-checkout]");
  const checkoutId = target?.dataset.workdirCheckout;
  const checkout = checkoutId
    ? props.repos.flatMap((repo) => repo.checkouts).find((item) => item.id === checkoutId)
    : null;
  return checkout && target ? { checkout, index: dropIndexAtPoint(target, dragging) } : null;
}

function updateDropTarget(x: number, y: number) {
  const drag = pointerDrag.value;
  // The pointer's own y is set before it is measured against anything: measuring first would answer
  // with the previous move's position.
  dropPointerY.value = y;
  const at = checkoutAtPoint(x, y, drag?.started ? drag.session : null);
  dropCheckoutId.value = drag?.started && at ? moveDestination(drag.session, at.checkout, at.index) : null;
  dropIndex.value = dropCheckoutId.value ? (at?.index ?? 0) : 0;
}

function onPointerMove(event: PointerEvent) {
  const drag = pointerDrag.value;
  if (!drag || event.pointerId !== drag.pointerId) return;
  drag.x = event.clientX;
  drag.y = event.clientY;
  if (!drag.started && Math.hypot(event.clientX - drag.originX, event.clientY - drag.originY) >= DRAG_THRESHOLD) {
    drag.started = true;
    moveMenuFor.value = null;
  }
  if (!drag.started) return;
  event.preventDefault();
  updateDropTarget(drag.x, drag.y);
  scheduleAutoScroll();
}

function suppressNextClick(sessionId: string) {
  if (suppressedClickTimer !== undefined) window.clearTimeout(suppressedClickTimer);
  suppressedClickSessionId = sessionId;
  suppressedClickTimer = window.setTimeout(() => {
    suppressedClickSessionId = null;
    suppressedClickTimer = undefined;
  }, 0);
}

function onPointerUp(event: PointerEvent) {
  const drag = pointerDrag.value;
  if (!drag || event.pointerId !== drag.pointerId) return;
  if (drag.started) updateDropTarget(event.clientX, event.clientY);
  const destination = dropCheckoutId.value;
  const sessionId = drag.session.id;
  const shouldMove = drag.started && destination !== null;
  finishPointerDrag();
  const index = dropIndex.value;
  if (drag.started) suppressNextClick(sessionId);
  if (shouldMove && destination) emit("moveSession", sessionId, destination, index);
}

function onDragKeydown(event: KeyboardEvent) {
  if (event.key !== "Escape") return;
  event.preventDefault();
  cancelPointerDrag();
}

function onPointerCancel(event: PointerEvent) {
  if (pointerDrag.value?.pointerId === event.pointerId) cancelPointerDrag();
}

function cancelPointerDrag() {
  if (pointerDrag.value?.started) suppressNextClick(pointerDrag.value.session.id);
  finishPointerDrag();
}

function finishPointerDrag() {
  const drag = pointerDrag.value;
  pointerDrag.value = null;
  dropCheckoutId.value = null;
  if (autoScrollFrame !== undefined) window.cancelAnimationFrame(autoScrollFrame);
  autoScrollFrame = undefined;
  window.removeEventListener("pointermove", onPointerMove);
  window.removeEventListener("pointerup", onPointerUp);
  window.removeEventListener("pointercancel", onPointerCancel);
  window.removeEventListener("keydown", onDragKeydown);
  window.removeEventListener("blur", cancelPointerDrag);
  try {
    if (drag && drag.source.hasPointerCapture(drag.pointerId)) drag.source.releasePointerCapture(drag.pointerId);
  } catch {
    // A pointer can lose capture between its last event and cleanup.
  }
}

function onLostPointerCapture(event: PointerEvent) {
  if (pointerDrag.value?.pointerId === event.pointerId) cancelPointerDrag();
}

function scheduleAutoScroll() {
  if (autoScrollFrame === undefined) autoScrollFrame = window.requestAnimationFrame(autoScroll);
}

function autoScroll() {
  autoScrollFrame = undefined;
  const drag = pointerDrag.value;
  const element = sidebarScroll.value;
  if (!drag?.started || !element) return;
  const bounds = element.getBoundingClientRect();
  const distance =
    drag.y < bounds.top + AUTO_SCROLL_EDGE
      ? drag.y - (bounds.top + AUTO_SCROLL_EDGE)
      : drag.y > bounds.bottom - AUTO_SCROLL_EDGE
        ? drag.y - (bounds.bottom - AUTO_SCROLL_EDGE)
        : 0;
  if (!distance) return;
  const delta = Math.sign(distance) * Math.min(14, Math.max(2, Math.abs(distance) / 2));
  const previous = element.scrollTop;
  element.scrollTop += delta;
  if (element.scrollTop === previous) return;
  updateDropTarget(drag.x, drag.y);
  scheduleAutoScroll();
}

onUnmounted(() => {
  finishPointerDrag();
  if (suppressedClickTimer !== undefined) window.clearTimeout(suppressedClickTimer);
  document.fonts?.removeEventListener("loadingdone", onFontsLoaded);
  detachLabelObserver();
});

/** Opens the list without a button: the context-menu key, or the same gesture with a pointer. */
function openMoveMenu(session: Session) {
  moveMenuFor.value = moveMenuFor.value === session.id ? null : session.id;
}

/**
 * The list closes on the move itself, so a rejected move can be aimed again without a second click.
 *
 * The menu has no position to give: it names a worktree, and a worktree takes the terminal at the end
 * of its list, which is where the row is appended when nothing else says otherwise.
 */
function chooseDestination(sessionId: string, targetCheckoutId: string) {
  moveMenuFor.value = null;
  const target = props.repos.flatMap((repo) => repo.checkouts).find((checkout) => checkout.id === targetCheckoutId);
  // A menu names a worktree and cannot name a row inside it, so it takes the end of the list: that is
  // the slot below every terminal the worktree already has, and no row of the destination is offered
  // as a position because none was chosen.
  emit("moveSession", sessionId, targetCheckoutId, target ? orderedSessions(target).length : 0);
}

/**
 * The one thing a row's icon can say, and the colour it says it in.
 *
 * The icon *is* the state in this design: there is no corner badge, no chip and no second line, so
 * what the row is doing is drawn by which glyph it wears and which colour that glyph has. Only
 * `working` moves; the rest are states, and a state is noticed without moving.
 */
type RowState = "working" | "waiting" | "failed" | "running" | "idle";
/**
 * What each agent state is called, beside the glyph and the colour it wears.
 *
 * `blocked` is the service reporting a turn stuck on a permission this server version cannot
 * answer, which is the only honest reading of "waiting for you" available here: nothing in the
 * protocol says who is supposed to answer, only that the turn has not moved.
 */
const AGENT_STATE: Record<AgentAttention, { state: RowState; text: string }> = {
  busy: { state: "working", text: "Working" },
  blocked: { state: "waiting", text: "Waiting for your reply" },
  failed: { state: "failed", text: "Last turn failed" },
  none: { state: "idle", text: "Idle" },
};

/**
 * What a row with the agent in front of it says when nothing said which session it has open.
 *
 * OpenCode 2.0.22 offers no route that maps a TUI process to a session, so an agent terminal whose
 * own title named nothing has no session to draw a state from — and "idle" is the one reading the
 * row must not assert: nothing observed a turn to be still, and nothing observed one to be running.
 * So the glyph stays the agent's and goes grey, exactly as an idle agent's does, and the honest
 * difference is spelled out in words beside the name: `sin sesión`, the reference's own sentence,
 * which says a session is missing rather than inventing one.
 */
const NO_AGENT_SESSION = "sin sesión";

/**
 * What the same row says when the title named several sessions, which is a different fact.
 *
 * The candidates are the whole service's list, because a terminal's session is not necessarily in
 * the worktree the terminal is filed under, and two sessions may share one title — measured against
 * the real service: `Humanizer` twice, `Read-only worktree probe` twice. Neither may claim the
 * other, so the row draws no state either way; what changes is the words, because "no session" would
 * then be a claim about a session that exists and is merely not distinguishable from its namesake.
 */
const AMBIGUOUS_AGENT_SESSION = "varias con este nombre";

interface WorkdirItem {
  session: Session;
  /**
   * Whether this terminal has anywhere to move to, which is what the row's menu is for.
   *
   * The menu's own list is built when the menu opens (`moveMenuItems`): a panel with a hundred
   * terminals would otherwise walk every registered checkout for every one of them on every pass,
   * to draw a list that is closed.
   */
  canMove: boolean;
  /**
   * The terminal this checkout has open, which is context rather than selection.
   *
   * It is what the row reports about itself, and it does not say that the terminal is what the
   * main panel is showing: a file opened over it leaves this session the checkout's active one.
   */
  active: boolean;
  /**
   * The terminal the panel has moved off, while it is still the checkout's own.
   *
   * It is drawn grey and without the accent, and this is what says why: the file the panel went to
   * is selected in the file tree beside this panel, and a row still wearing the accent would be a
   * second claim about the same selection in a list that has one. The row keeps its shape rather
   * than losing it, because the terminal is still there and still belongs to this checkout — it is
   * simply not what is being read.
   */
  dimmed: boolean;
  /**
   * The row's own name: the agent session's own title where this terminal's title names one, and
   * the program in front of the shell or the session's own name otherwise. See `toWorkdir`, which
   * is where the attribution is decided.
   */
  title: string;
  /**
   * The secondary detail, inline and muted, and the first thing to give way when the row is short.
   *
   * Three answers, all of them a fact rather than a filler:
   *
   * - An identified agent session's mode: `plan`, `coder`. It is the one word the service named for
   *   this terminal's own session, and it sits beside the session's title rather than under it.
   * - `sin sesión`, when OpenCode is in front but no title named a session, and `varias con este
   *   nombre` when one named several. The row's name is already `opencode`, so these say what is
   *   missing rather than repeating what is present, and they are kept apart because they are
   *   different findings: nothing was found, or more than one thing was.
   *
   * A plain terminal has none, and that is the user's call after seeing it: the worktree name beside
   * `zsh` repeated the group row directly above it, three rows under it, spelling the same branch
   * four times in one block. Indentation already says which group a row is in, so repeating it says
   * nothing new and costs the name the width. Two idle shells in one worktree therefore read as two
   * identical rows — which is true, because nothing observed them apart.
   */
  detail?: string;
  /** The glyph the row wears: what it is. */
  icon: Component;
  /** The row's state; an identified OpenCode agent may also supply the glyph's tint. */
  state: RowState;
  /** OpenCode's colour for this session's agent, when the state leaves room for it. */
  tint: string | null;
  /**
   * How long ago the session this terminal has open was last updated, drawn compactly.
   *
   * Real or nothing: the only clock this app has for a session is the service's own `updatedAt`,
   * so a row with no identified session has no time to draw and draws none. Never a duration
   * measured from when the panel happened to open or from when a row was created.
   */
  elapsed: string | null;
  /**
   * The state in words, for the row's tooltip and its accessible name. Empty on a row that is only
   * a shell, because a shell at a prompt has nothing to report.
   */
  note: string;
}

interface Workdir {
  checkout: Checkout;
  title: string;
  /** The checkout's own line counts, absent when Git has none to show. */
  additions?: number;
  deletions?: number;
  /** What is wrong with this workdir, if anything (E.4). */
  error?: string;
  /** The glyph the row wears: the same one the titlebar's crumb for this checkout wears. */
  icon: Component;
  /**
   * Worktrees can be removed; repo roots cannot. Everything that acts on the repo as a whole
   * (add a worktree, restore archived ones, take it off the panel) lives in the group header,
   * so a row only ever carries the action that belongs to itself.
   */
  worktree: boolean;
  /**
   * The directory is gone, so the row keeps only its reason for existing and its one way out
   * (closing it). Nothing behind it can be selected, run or created.
   */
  missing: boolean;
  home: boolean;
  /**
   * The current checkout, which is context and not selection.
   *
   * It is what the app is pointed at, and it is said by neutral ink — see the group glyph rule — never
   * by the accent, because the accent means one thing only and that thing is `selected`. A checkout
   * holding the terminal being read is a place you are in, not a row you have chosen.
   */
  active: boolean;
  /**
   * The one row in the whole panel that is selected: this checkout row, when this checkout is the
   * current one *and* has no terminal of its own selected.
   *
   * That conjunction is what makes the invariant hold without a rule that has to be defended. A
   * terminal row is selected by its own session, so while one of this checkout's terminals is
   * selected the branch row gives the selection up and the terminal wears it alone; when none is, the
   * branch is what is selected and takes it. Either way at most one row in the panel carries the
   * accent edge, which is the only thing the edge is allowed to mean.
   *
   * "At most" rather than "exactly one" because the panel is also showing something that is not a
   * terminal: a file or a diff takes the selection with it, and it wears it on its own row in the
   * inspector beside this one. A checkout with a document open selects nothing here rather than
   * selecting the nearest row it can find, because a row that claims a selection nothing in the panel
   * agrees with is worse than a panel that says nothing.
   */
  selected: boolean;
  items: WorkdirItem[];
}

interface Group {
  id: string;
  label: string;
  /** Where the repo lives, at full length: it is the header's tooltip and nothing else. */
  path: string;
  /** Where the repo lives in the forms the header can draw it, widest first; the last is nothing. */
  pathSteps: string[];
  /** The repo root, which is the checkout the header's actions are addressed to. */
  root: Checkout | null;
  git: boolean;
  home: boolean;
  missing: boolean;
  /** How many worktrees this repo archived. */
  archived: number;
  /** What the header menu offers. A group with none of these has no menu at all. */
  canAdd: boolean;
  canRestore: boolean;
  canClose: boolean;
  hasMenu: boolean;
  workdirs: Workdir[];
}

// The sidebar names every checkout at once, so it asks for the totals of all of them in one
// call rather than per row. The active checkout is named too, so the Changes tab of the
// inspector shares the same refresh instead of running a second listener.
const diffStats = useDiffStats(toRef(props, "repos"), toRef(props, "activeCheckoutId"));

/**
 * Whether the terminal is the thing the main panel is showing, which is what the accent edge in
 * this panel is for.
 *
 * The selection is a claim about what is on screen in the middle, and the middle is the only place
 * that knows. `activeSessionId` cannot answer it on its own because it is a fact about the checkout
 * rather than about the panel: a terminal stays the active session while a document is open over it,
 * which is what left a terminal row wearing the accent next to a file being read.
 *
 * A split layout does not get its own answer. The terminal is on screen there too, but it is on
 * screen *beside* the document rather than instead of it, and the row is one row: a panel where the
 * edge meant "this is what you are reading" cannot wear it for one of two things and stay honest.
 */
const showingTerminal = computed(() => props.mainViewKind === "terminal");

/** How many worktrees each repo archived, counted once so no row walks the list itself. */
const archivedByRepo = computed(() => {
  const counts = new Map<string, number>();
  for (const archived of props.archivedWorktrees) {
    counts.set(archived.repoId, (counts.get(archived.repoId) ?? 0) + 1);
  }
  return counts;
});

const groups = computed<Group[]>(() =>
  props.repos.map((repo) => {
    const root = repo.checkouts.find((checkout) => checkout.isPrimary) ?? repo.checkouts[0] ?? null;
    const git = repo.kind === "git";
    const missing = root?.isMissing ?? false;
    const home = root !== null && root.id === props.homeCheckoutId;
    const archived = archivedByRepo.value.get(repo.id) ?? 0;
    const canAdd = git && !missing && root !== null;
    const canRestore = git && !missing && root !== null && archived > 0;
    const canClose = !home && root !== null;
    return {
      id: repo.id,
      label: repo.name,
      path: root?.path ?? "",
      pathSteps: pathSteps(root?.path ?? ""),
      root,
      git,
      home,
      missing,
      archived,
      canAdd,
      canRestore,
      canClose,
      hasMenu: canAdd || canRestore || canClose,
      workdirs: repo.checkouts.map((checkout) => toWorkdir(repo, checkout)),
    };
  }),
);

/** The four type sizes a label in this panel is drawn in, one specimen each. */
type FitStyle = "groupName" | "groupPath" | "rowName" | "rowDetail";

const groupNameSpecimen = ref<HTMLElement | null>(null);
const groupPathSpecimen = ref<HTMLElement | null>(null);
const rowNameSpecimen = ref<HTMLElement | null>(null);
const rowDetailSpecimen = ref<HTMLElement | null>(null);
const probes: Record<FitStyle, Readonly<Ref<HTMLElement | null>>> = {
  groupName: groupNameSpecimen,
  groupPath: groupPathSpecimen,
  rowName: rowNameSpecimen,
  rowDetail: rowDetailSpecimen,
};

/** What each word is worth, kept per specimen because the type is not the same in every one. */
const fitWidths = new Map<string, number>();
/** How many words that book holds before it is emptied, which bounds what a long session keeps. */
const MAX_MEASURED_WORDS = 4_000;
/** Every row of one kind, keyed by the id the row carries, for the pass that weighs them. */
const blockCache = new Map<string, Map<string, HTMLElement>>();

/**
 * The step each label is drawn at, keyed by the row it belongs to.
 *
 * Empty until the panel has been weighed, and a row with no entry is drawn at its widest: a panel
 * that has not been measured says nothing about how much room it has, so the shape that fits the
 * most room is the one nothing has been proved against.
 */
const fitSteps = shallowRef<Record<string, number>>({});

/**
 * Weighs the panel's labels and records the step each is drawn at.
 *
 * A row has a room and a set of rungs, and the widest rung that fits is the one drawn; the ellipsis
 * in CSS runs after the last rung, which is why nothing here cuts a word. The room is read off the
 * row's own control rather than off the label beside it: a label is itself shrinkable, so once it
 * overflows it reports the width it was squeezed to and the row would give up a little more room on
 * every pass and never come back.
 */
function measureLabels() {
  const root = sidebarScroll.value;
  if (!root) return;
  blockCache.clear();
  const steps: Record<string, number> = {};
  /** The room a row's label has: its control's content box less the glyph and the gap beside it. */
  const roomOf = (select: HTMLElement | null | undefined): number | null => {
    // The row being renamed is a field and not a label, so there is nothing in it to weigh.
    if (!select?.querySelector<HTMLElement>(".lbl")) return null;
    const glyph = select.querySelector<HTMLElement>(".workdir-icon");
    return contentWidth(select) - (glyph?.offsetWidth ?? 0) - gapOf(select);
  };

  for (const group of groups.value) {
    const block = blocksById(root, "group", ".workdir-group", "repoId").get(group.id);
    const control = block?.querySelector<HTMLElement>(".group-heading-text");
    if (!control) continue;
    const room = contentWidth(control) - gapOf(control);
    if (room <= 0) continue;
    const gap = gapOf(control);
    const name = widthOf("groupName", group.label);
    // Rung for rung: the name with the whole path, the name with a shorter one, and the name alone,
    // which is what a header says once the panel cannot say where the repo lives.
    steps[`header:${group.id}`] = widestThatFits(
      group.pathSteps.map((path) => name + (path ? gap + widthOf("groupPath", path) : 0)),
      room,
    );
  }

  const worktrees = groups.value.flatMap((group) => group.workdirs);

  for (const workdir of worktrees) {
    const block = blocksById(root, "workdir", ".workdir-checkouts", "workdirCheckout").get(workdir.checkout.id);
    const room = roomOf(block?.querySelector<HTMLElement>(".workdir-select"));
    if (room === null || room <= 0) continue;
    steps[`workdir:${workdir.checkout.id}`] = widestThatFits(
      nameSteps(workdir.title).map((name) => widthOf("rowName", name)),
      room,
    );
  }

  // A terminal is weighed in a pass of its own rather than inside the loop above, because a worktree
  // row with nothing to say about its room is not a reason to leave its terminals unweighed: the two
  // rows have rooms of their own and one of them being unknown says nothing about the other.
  for (const workdir of worktrees) {
    for (const item of workdir.items) {
      const row = blocksById(root, "session", ".workdir-child[data-session-id]", "sessionId").get(item.session.id);
      const select = row?.querySelector<HTMLElement>(".workdir-select");
      const room = roomOf(select);
      if (room === null || room <= 0) continue;
      // The detail goes before the name does, and it goes whole: a mode word is drawn or it is not
      // there, and a name cut to keep half of one is the worse of the two rungs.
      const gap = gapOf(select!.querySelector<HTMLElement>(".lbl")!);
      const detail = item.detail ?? "";
      const detailWidth = detail ? gap + widthOf("rowDetail", detail) : 0;
      const both = widthOf("rowName", item.title) + detailWidth;
      const withDetail = Boolean(detail) && room >= both;
      steps[`detail:${item.session.id}`] = withDetail ? 0 : 1;
      steps[`name:${item.session.id}`] = widestThatFits(
        nameSteps(item.title).map((name) => widthOf("rowName", name)),
        room - (withDetail ? detailWidth : 0),
      );
    }
  }

  applyFitSteps(steps);
}

/** Every row of one kind, keyed by the id its own row carries, read from the panel that drew them. */
function blocksById(
  root: HTMLElement,
  cacheKey: string,
  selector: string,
  datasetKey: string,
): Map<string, HTMLElement> {
  const cached = blockCache.get(cacheKey);
  if (cached) return cached;
  const blocks = new Map<string, HTMLElement>();
  for (const block of root.querySelectorAll<HTMLElement>(selector)) {
    blocks.set(block.dataset[datasetKey] ?? "", block);
  }
  blockCache.set(cacheKey, blocks);
  return blocks;
}

/** The width a box has for its own contents: `clientWidth` counts the padding the box is drawn with. */
function contentWidth(element: HTMLElement): number {
  const style = window.getComputedStyle(element);
  return element.clientWidth - (parseFloat(style.paddingLeft) || 0) - (parseFloat(style.paddingRight) || 0);
}

/** The space two flex children are put apart by, which is what separates the halves of a label. */
function gapOf(element: HTMLElement): number {
  return parseFloat(window.getComputedStyle(element).columnGap) || 0;
}

/**
 * How wide a piece of text is in one of the panel's type sizes.
 *
 * Both numbers are read in the same units on purpose: the room is a `clientWidth` and the text is a
 * `scrollWidth`, so a window zoomed or scaled scales both or neither. The widths are kept, because a
 * resize re-weighs every row and most of them are asking about the same words.
 */
function widthOf(style: FitStyle, text: string): number {
  const specimen = probes[style].value;
  if (!specimen || !text) return 0;
  const key = `${style} ${text}`;
  const known = fitWidths.get(key);
  if (known !== undefined) return known;
  specimen.textContent = text;
  const width = specimen.scrollWidth;
  // A panel left open through a session's worth of renames would otherwise keep every word it ever
  // saw, so past this many the pass starts the book again rather than growing it.
  if (fitWidths.size >= MAX_MEASURED_WORDS) fitWidths.clear();
  fitWidths.set(key, width);
  return width;
}

/**
 * The drawn steps, kept only when they are not the steps already drawn.
 *
 * The pass runs on every resize and on every change to what the panel lists, and a panel whose rows
 * all fit re-weighs to exactly what it had: a whole list re-rendering to say nothing would make
 * dragging its own edge expensive for nothing.
 */
function applyFitSteps(steps: Record<string, number>) {
  const drawn = fitSteps.value;
  const keys = Object.keys(steps);
  if (keys.length === Object.keys(drawn).length && keys.every((key) => drawn[key] === steps[key])) return;
  fitSteps.value = steps;
}

/** What a repo's header says about where it lives, which at the last step is nothing at all. */
function groupPathText(group: Group): string {
  return group.pathSteps[fitSteps.value[`header:${group.id}`] ?? 0] ?? "";
}

/** The worktree's name, given up a segment at a time rather than cut from either end. */
function workdirNameText(workdir: Workdir): string {
  const steps = nameSteps(workdir.title);
  return steps[fitSteps.value[`workdir:${workdir.checkout.id}`] ?? 0] ?? workdir.title;
}

/** The terminal's name, given up a segment at a time rather than cut from either end. */
function sessionNameText(item: WorkdirItem): string {
  const steps = nameSteps(item.title);
  return steps[fitSteps.value[`name:${item.session.id}`] ?? 0] ?? item.title;
}

/** The mode beside the terminal's name, which is drawn whole or not drawn at all. */
function sessionDetailText(item: WorkdirItem): string {
  return (fitSteps.value[`detail:${item.session.id}`] ?? 0) === 0 ? (item.detail ?? "") : "";
}

/**
 * The panel's own box is what every room in it comes from, so that box is what is watched.
 *
 * Its width is the splitter's and not the labels': nothing a row draws can make the panel wider
 * (every label is `min-width: 0` inside a scroller), so watching it cannot feed back into itself.
 * The specimens are deliberately not watched — writing a word into one changes that box, and a
 * callback that changed the boxes it watches is a loop — and the one change they cannot see coming
 * is the type, which arrives as `fontScale` instead.
 */
let labelObserver: ResizeObserver | undefined;
let labelFrame: number | undefined;

function attachLabelObserver() {
  detachLabelObserver();
  const panel = sidebarScroll.value;
  if (!panel) return;
  labelObserver = new ResizeObserver(() => scheduleMeasureLabels());
  labelObserver.observe(panel);
  scheduleMeasureLabels();
}

function detachLabelObserver() {
  labelObserver?.disconnect();
  labelObserver = undefined;
  if (labelFrame !== undefined) window.cancelAnimationFrame(labelFrame);
  labelFrame = undefined;
}

/** One pass per frame however many things asked for one: a resize says this once. */
function scheduleMeasureLabels() {
  if (labelFrame !== undefined) return;
  labelFrame = window.requestAnimationFrame(() => {
    labelFrame = undefined;
    measureLabels();
  });
}

/** Text and row identities that affect fitted label geometry, excluding runtime-only status. */
const weighedWords = computed(() => {
  const words: string[] = [];
  for (const group of groups.value) {
    words.push(group.id, group.label, ...group.pathSteps, String(group.hasMenu));
    for (const workdir of group.workdirs) {
      words.push(
        workdir.checkout.id,
        workdir.title,
        workdir.error ?? "",
        String(workdir.worktree),
        String(workdir.home),
        String(workdir.missing),
      );
      for (const item of workdir.items) words.push(item.session.id, item.title, item.detail ?? "");
    }
  }
  return JSON.stringify(words);
});

function onFontsLoaded() {
  fitWidths.clear();
  scheduleMeasureLabels();
}

onMounted(() => {
  attachLabelObserver();
  document.fonts?.addEventListener("loadingdone", onFontsLoaded);
});

// What the panel says, and the type it is drawn in: either one changes a row's words or a word's
// width without the panel moving a pixel, so nothing else would ask for the pass they need.
watch([weighedWords, () => props.fontScale], scheduleMeasureLabels, { flush: "post" });
watch(
  () => props.fontScale,
  () => fitWidths.clear(),
);

/** Every header action closes the menu first, so the list never outlives the thing it chose. */
function addWorktree(group: Group) {
  closeGroupMenu();
  if (group.root) emit("createWorktree", group.root.id);
}

function restoreArchived(group: Group) {
  closeGroupMenu();
  if (group.root) emit("restoreArchived", group.root.repoId);
}

/** A missing root is closed through its own event, since there is nothing on disk to close. */
function closeGroup(group: Group) {
  closeGroupMenu();
  if (!group.root) return;
  // Two literal calls rather than one with a computed event name: `emit` is overloaded per
  // event, and a union of names matches none of the overloads.
  if (group.missing) emit("closeMissing", group.root.id);
  else emit("closeWorkdir", group.root.id);
}

/**
 * Whether the row is advertising changes, which is what makes clicking it open the full diff.
 * The counts are the same ones the row paints, so the click and the numbers cannot disagree.
 */
function hasChanges(workdir: Workdir): boolean {
  return Boolean(workdir.additions || workdir.deletions);
}

/**
 * Whether the row carries a cross of its own.
 *
 * The template's `v-if` and the stylesheet's rule that opens the cross's strip both ask this, so
 * they are the same question asked twice rather than two conditions kept in step by hand: a row
 * that draws no cross must not reserve the strip a cross would stand in, or its counts hover into
 * a gap nothing is ever drawn in.
 */
function hasClose(workdir: Workdir): boolean {
  return workdir.worktree && !workdir.home;
}

/** The row's name at full length, which the row cannot fit, and where the checkout lives. */
function workdirTooltip(checkout: Checkout): string {
  return checkout.branch ? `${checkout.branch} — ${checkout.path}` : checkout.path;
}

/**
 * How long ago a session was last touched, drawn the way a person writes it: `12s`, `2m`, `1h`.
 *
 * The service's own `updatedAt` is the only clock here, so a row whose session was never
 * identified has no number to draw and this is never called for it. Anything a minute old or more
 * is rounded rather than floored, because `59s` and `60s` say the same thing and the shorter one
 * would be a lie about how long.
 */
function elapsedSince(updatedAt: number): string {
  const seconds = Math.max(0, Math.round((Date.now() - updatedAt) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  return `${Math.round(minutes / 60)}h`;
}

/**
 * The terminals of a checkout in the order they are listed, which is not the order the database
 * hands them out.
 *
 * The pane owns that order — it is what gets saved, and it is where a drag puts a terminal — so it
 * publishes it and this reads it. A list drawn in the database's order would show a reorder as
 * nothing at all: persisted, correct, and invisible on the only screen that draws it.
 */
function orderedSessions(checkout: Checkout): Session[] {
  const order = props.sessionOrder?.[checkout.id];
  if (!order) return checkout.sessions;
  const byId = new Map(checkout.sessions.map((session) => [session.id, session] as const));
  const ordered = order.flatMap((id) => (byId.get(id) ? [byId.get(id)!] : []));
  const seen = new Set(ordered.map((session) => session.id));
  return [...ordered, ...checkout.sessions.filter((session) => !seen.has(session.id))];
}

function toWorkdir(repo: Repo, checkout: Checkout): Workdir {
  const isGit = repo.kind === "git";
  const counts = isGit ? diffStats.checkoutTotals[checkout.id] : undefined;
  const title = workdirTitle(repo, checkout);
  return {
    checkout,
    // Git roots use their branch (or "Base" when detached); plain workdirs use the repo name.
    title,
    additions: counts?.additions || undefined,
    deletions: counts?.deletions || undefined,
    // The one failure that belongs to a single workdir (E.4): it names the checkout whose
    // directory is gone, so it belongs in that row and not in a toast about the window.
    error: checkout.isMissing ? "Directory missing" : undefined,
    icon: WORKDIR_ICONS[workdirIconKind(repo, checkout, props.homeCheckoutId)],
    worktree: isGit && !checkout.isPrimary,
    missing: checkout.isMissing,
    home: checkout.id === props.homeCheckoutId,
    active: checkout.id === props.activeCheckoutId,
    // At most one row in the panel wears the accent edge, and only while a terminal is what the
    // main panel is showing. A branch that holds the selected terminal gives the selection up; a
    // branch with no selected terminal of its own takes it, and that is the branch being what is
    // open. With a document or a diff on screen there is no selection to give here: the file that is
    // open wears it in the inspector, and this branch stays context, which `active` already says.
    selected:
      showingTerminal.value &&
      checkout.id === props.activeCheckoutId &&
      !checkout.sessions.some((session) => session.id === props.activeSessionId),
    items: orderedSessions(checkout).map((session) => {
      const status = props.sessionRuntimeStatuses[session.id];
      const app = status?.foregroundApp;
      const candidates = props.agentRows[checkout.id]?.sessions ?? [];
      const foregroundProcess = status?.foregroundProcess === true;
      const openCodeTerminal = foregroundProcess && app === AGENT_APP;
      const piTerminal = foregroundProcess && app === "pi";
      const match = openCodeTerminal ? matchAgentSessionTitle(candidates, agentSessionTitle(status)) : null;
      const identified = match?.kind === "one" ? match.session : null;
      const agentTerminal = Boolean(identified) || openCodeTerminal || piTerminal;
      const unidentified = match?.kind === "ambiguous" ? AMBIGUOUS_AGENT_SESSION : NO_AGENT_SESSION;
      const attention: AgentAttention = identified?.attention ?? "none";
      const pendingUnknown = identified?.awaitingReply === null;
      const piTaskState = piTerminal ? status?.piTaskState : undefined;
      const piState =
        piTaskState === "working"
          ? { state: "working" as const, text: "Working" }
          : piTaskState === "error"
            ? { state: "failed" as const, text: "Last task failed" }
            : piTaskState === "done"
              ? { state: "idle" as const, text: "Task complete" }
              : undefined;
      const agent = identified && !pendingUnknown ? AGENT_STATE[attention] : piState;
      /**
       * OpenCode's colour for this session's agent, on every state but a failed turn.
       *
       * A blocked session is the one that asks, and the question belongs to the agent: painting it in
       * the panel's warning said something about this row's health rather than about who is waiting,
       * and it cost the agent its colour on the one state where the agent is doing its job. The glyph
       * says what happened and the colour says who. A failed turn is the exception, because that one
       * is the row's own news and stays in the danger colour.
       */
      const tint =
        !pendingUnknown && identified?.agent?.color && attention !== "failed" ? identified.agent.color : null;
      return {
        session,
        canMove: canMoveTo(session),
        active: session.id === props.activeSessionId,
        dimmed: session.id === props.activeSessionId && !showingTerminal.value,
        title: sessionRowTitle(session, status),
        detail: identified
          ? (identified.agent?.label ?? NO_AGENT_SESSION)
          : openCodeTerminal
            ? unidentified
            : undefined,
        /** Pi task state is read only while Pi is the foreground process. */
        icon: agentTerminal
          ? agent?.state === "working"
            ? WORKDIR_ICONS.working
            : agent?.state === "waiting"
              ? WORKDIR_ICONS.asking
              : WORKDIR_ICONS.agent
          : WORKDIR_ICONS.terminal,
        // An OpenCode whose session nobody identified has no state to draw, so it is idle rather
        // than green: `running` on a row with an agent glyph in front would claim a process that
        // the panel cannot identify, and the word beside it already says what is missing.
        state:
          agent?.state ??
          (agentTerminal
            ? "idle"
            : status?.lastCommandExit
              ? "failed"
              : status?.foregroundProcess
                ? "running"
                : "idle"),
        tint,
        // Only a row that identified a session has a clock to read, and that row is the only one
        // that draws a time.
        elapsed: identified ? elapsedSince(identified.updatedAt) : null,
        // The words say the same thing the glyph says, and a session that was identified and is
        // simply quiet says it is idle rather than claiming that nothing could be found.
        note: pendingUnknown
          ? "Session state unavailable"
          : agent
            ? agent.text
            : openCodeTerminal
              ? match?.kind === "ambiguous"
                ? "Several sessions share this name"
                : "Session not identified"
              : "",
      };
    }),
  };
}

/**
 * The row the ghost under the pointer is a copy of, so the name under the cursor is the name the
 * row it came from wears. Read from the same model rather than named a second time.
 */
const draggedItem = computed(
  () =>
    groups.value
      .flatMap((group) => group.workdirs)
      .flatMap((workdir) => workdir.items)
      .find((item) => item.session.id === pointerDrag.value?.session.id) ?? null,
);

/**
 * The groups that are folded away, by checkout id.
 *
 * Local state and nothing else, on purpose. A fold is one person's view of a list they are reading
 * right now, not a preference they set once and expect to still be there tomorrow — and a preference
 * is a field in `config.yml` and a line in the Settings dialog, with its own key on both sides. If it
 * should be remembered, that is a separate decision; it is not smuggled in here as a field nobody
 * declared.
 */
const collapsedGroups = ref(new Set<string>());

function isCollapsed(workdir: Workdir): boolean {
  return collapsedGroups.value.has(workdir.checkout.id);
}

function toggleGroup(workdir: Workdir) {
  const next = new Set(collapsedGroups.value);
  if (!next.delete(workdir.checkout.id)) next.add(workdir.checkout.id);
  collapsedGroups.value = next;
}

/**
 * The repositories folded away, by repo id.
 *
 * Separate from the checkouts' own fold rather than the same set: a repo folded shut has its
 * checkouts hidden, so their chevrons are not on screen to say anything, and a set that held both
 * would let one control answer for the other.
 */
const collapsedRepos = ref(new Set<string>());

function isRepoCollapsed(group: Group): boolean {
  return collapsedRepos.value.has(group.id);
}

function toggleRepo(group: Group) {
  const next = new Set(collapsedRepos.value);
  if (!next.delete(group.id)) next.add(group.id);
  collapsedRepos.value = next;
}

watch([collapsedGroups, collapsedRepos, editingId], scheduleMeasureLabels, { flush: "post" });

/**
 * What the row announces: its name, its detail, and the state in words.
 *
 * The state is drawn as a colour on a glyph, and a colour is not something a screen reader reads, so
 * it is spelled out here. Both answers come from the same `item.state`, so the drawn colour and the
 * announced word can never disagree.
 */
function rowLabel(item: WorkdirItem): string {
  const said = [`${item.title}${item.detail ? `, ${item.detail}` : ""}`];
  if (item.elapsed) said.push(item.elapsed);
  if (item.note) said.push(item.note);
  return said.join(" — ");
}
</script>

<template>
  <aside class="app-sidebar flex h-full min-h-0 flex-col" :class="{ 'is-terminal-dragging': pointerDrag?.started }">
    <!-- The list's scrollbar is drawn over the list's right edge rather than sitting in it, so a row's
         name and counts run to the panel's edge instead of stopping short of a scrollbar's worth of
         padding to keep them clear of macOS's own overlay. That overlay is off (in `style.css`). -->
    <div ref="sidebarScroll" class="sidebar-scroll">
      <div
        v-for="group in groups"
        :key="group.id"
        class="workdir-group"
        :class="{ 'is-collapsed': isRepoCollapsed(group) }"
        :data-repo-id="group.id"
      >
        <!-- The header is where the repo as a whole is acted on, and all of it lives in one menu:
             the three dots, or a right click anywhere on the header. Adding a worktree is the only
             frequent one, but a button that sat two pixels from "remove" made the two easy to
             confuse, so the rare and destructive actions are one click further away.

             The name folds the repository, which is why it is a button and not a `div` with a click
             on it: folding a list has to be reachable by keyboard, and `aria-expanded` is what says
             which way it goes — the header carries no glyph for it, because a folded repository is
             already legible as one: its row and nothing under it. The menu button is a sibling
             rather than a child, because a button inside a button is not a control a person can
             operate. -->
        <div
          class="group-heading"
          :class="{ 'menu-open': groupMenuFor === group.id }"
          @contextmenu.prevent="group.hasMenu && openGroupMenu(group.id)"
        >
          <button
            type="button"
            class="group-heading-text"
            :title="group.path || group.label"
            :aria-expanded="!isRepoCollapsed(group)"
            @click="toggleRepo(group)"
          >
            <span class="group-name">{{ group.label }}</span>
            <!-- Where the repo lives, in the form the panel had room for. The name is what gives way
                 last, and the path is what gives way first, because the name is the only half of
                 this row that says which repository it is. -->
            <span v-if="groupPathText(group)" class="group-path">{{ groupPathText(group) }}</span>
            <!-- A path the panel cannot draw is still part of what the header names, so it is said
                 here rather than dropped: what is not drawn must still be readable, and this row's
                 own title carries the whole path as well. -->
            <span v-else-if="group.pathSteps[0]" class="sr-only">{{ group.pathSteps[0] }}</span>
          </button>

          <!-- reka's menu, the same one the titlebar crumbs open, so the outside press, the
               Escape, the arrows and the focus back to the button are the library's to get right.
               The panel only says which group is open: there is a menu per group and `groupMenuFor`
               is the one that may be. A right click on the header sets it directly, which is the
               only way in that the trigger is not part of. -->
          <DropdownMenuRoot
            v-if="group.hasMenu"
            :open="groupMenuFor === group.id"
            @update:open="(isOpen) => (isOpen ? openGroupMenu(group.id) : closeGroupMenu())"
          >
            <DropdownMenuTrigger
              class="workdir-action group-more"
              :aria-label="`Actions for ${group.label}`"
              title="More actions"
            >
              <EllipsisIcon class="icon-xs" aria-hidden="true" />
            </DropdownMenuTrigger>
            <!-- Portalled and placed by reka, like every other menu in the app: the rows are
                 absolutely positioned, so the pane under them paints over anything left in place. -->
            <DropdownMenuPortal>
              <DropdownMenuContent
                class="surface-popover muster-menu group-menu"
                side="bottom"
                align="end"
                :side-offset="4"
                :aria-label="`Actions for ${group.label}`"
              >
                <DropdownMenuItem
                  v-if="group.canAdd"
                  class="menu-item select-none text-left"
                  @select="addWorktree(group)"
                >
                  <GitBranchPlusIcon class="icon-xs" aria-hidden="true" />
                  <span class="menu-item-label">New worktree</span>
                </DropdownMenuItem>
                <!-- Only while there is something to bring back, and the count says how much. -->
                <DropdownMenuItem
                  v-if="group.canRestore"
                  class="menu-item select-none text-left"
                  :aria-label="`Restore ${group.archived} archived worktree${group.archived === 1 ? '' : 's'}`"
                  @select="restoreArchived(group)"
                >
                  <ArchiveRestoreIcon class="icon-xs" aria-hidden="true" />
                  <span class="menu-item-label">Restore archived worktrees</span>
                  <span class="group-menu-count">{{ group.archived }}</span>
                </DropdownMenuItem>
                <!-- Taking the repo off the panel takes its whole list with it. Nothing here deletes
                     a file, and the home directory is the one thing that cannot be closed. -->
                <DropdownMenuItem
                  v-if="group.canClose"
                  class="menu-item select-none text-left group-menu-danger"
                  @select="closeGroup(group)"
                >
                  <FolderMinusIcon class="icon-xs" aria-hidden="true" />
                  <span class="menu-item-label">Remove from panel</span>
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenuPortal>
          </DropdownMenuRoot>
        </div>

        <!-- One group per checkout, and the fold is the group's own: `col` is what hides its
             children and turns its chevron, so the two can never disagree about whether it is
             open. The chevron is the control rather than the whole row because the row itself
             selects the checkout, and folding is not selecting. -->
        <div
          v-for="workdir in group.workdirs"
          :key="workdir.checkout.id"
          class="workdir-checkouts grp"
          :class="{
            col: isCollapsed(workdir),
            'has-active': workdir.items.some((item) => item.active),
            'is-drop-target': dropCheckoutId === workdir.checkout.id,
          }"
          :data-workdir-checkout="workdir.checkout.id"
        >
          <!-- The row is the click, all of it: the surface a hover paints is the whole row, so the
               empty stretch, the chevron's own margin and the trailing slot are the same click as
               the name. Only the two controls inside it are not — the chevron folds and the cross
               closes, and both stop the click rather than also selecting what they sit on. The
               keyboard still arrives through the button inside, whose Enter bubbles up here. -->
          <div
            class="workdir-row workdir-parent"
            :class="{
              active: workdir.active,
              selected: workdir.selected,
              missing: workdir.missing,
              'has-close': hasClose(workdir),
            }"
            @click="!workdir.missing && emit('selectCheckout', workdir.checkout.id, hasChanges(workdir))"
          >
            <button
              type="button"
              class="workdir-fold"
              :aria-expanded="!isCollapsed(workdir)"
              :aria-label="`${isCollapsed(workdir) ? 'Expand' : 'Collapse'} ${workdir.title}`"
              @click.stop="toggleGroup(workdir)"
            >
              <ChevronDownIcon class="chv" aria-hidden="true" />
            </button>

            <button
              type="button"
              class="workdir-select"
              :aria-current="workdir.active ? 'page' : undefined"
              :aria-disabled="workdir.missing || undefined"
              :title="workdirTooltip(workdir.checkout)"
            >
              <component :is="workdir.icon" class="workdir-icon" aria-hidden="true" />
              <span class="lbl">
                <span class="nm">{{ workdirNameText(workdir) }}</span>
              </span>
            </button>

            <!-- The row's own trailing slot. The counts live here and nowhere else, in the
                 monospace the reference draws them in and right against the row's edge. At rest it
                 holds a bare `+`, `−`, or both, coloured as the figures it stands in for and
                 carrying only the signs this row actually has, and on hover it grows into them,
                 with the cross standing beside them in the strip it opened. -->
            <span class="workdir-end" :class="{ 'workdir-end-error': !!workdir.error }">
              <span v-if="workdir.error" class="workdir-end-note">{{ workdir.error }}</span>
              <span v-else-if="workdir.additions || workdir.deletions" class="workdir-diff">
                <span class="workdir-diff-mark" aria-hidden="true">
                  <span v-if="workdir.additions" class="ad">+</span>
                  <span v-if="workdir.deletions" class="rm">−</span>
                </span>
                <span class="workdir-diff-nums">
                  <span v-if="workdir.additions" class="ad">+{{ workdir.additions }}</span>
                  <span v-if="workdir.deletions" class="rm">−{{ workdir.deletions }}</span>
                </span>
              </span>
            </span>

            <!-- A worktree is the only row with an action of its own, and it is out of flow so it
                 cannot reflow the name beside it: the counts give up the slot's width instead. A
                 repo root's own actions are in the group header above. -->
            <button
              v-if="hasClose(workdir)"
              type="button"
              class="workdir-action workdir-close"
              :aria-label="
                workdir.missing
                  ? `Close missing checkout: ${workdir.title}`
                  : `Remove or archive worktree ${workdir.title}`
              "
              :title="workdir.missing ? 'Remove from list' : 'Remove or archive worktree'"
              @click.stop="
                workdir.missing
                  ? emit('closeMissing', workdir.checkout.id)
                  : emit('removeWorktree', workdir.checkout.id)
              "
            >
              <XIcon class="icon-xs" aria-hidden="true" />
            </button>
          </div>

          <!-- The group's terminals, one level in and behind a guide that only appears while the
               pointer is over the group itself, so the line never reads as a selection. The whole
               list is also the rest of the workdir's drop zone: a terminal is dropped on the
               worktree it should belong to, not on whichever of its rows is under it.

               "New terminal" is deliberately not gated on having terminals open: picking a workdir
               no longer opens a terminal by itself, so a workdir with nothing running would
               otherwise offer no way to start one from here. A directory that is gone has no live
               sessions and nothing to run one in. -->
          <div v-if="!workdir.missing" class="kids workdir-kids">
            <template v-for="(item, itemIndex) in workdir.items" :key="item.session.id">
              <!-- The line that says where the terminal lands. It is drawn in the slot itself rather
                 than once at the end of the list, because a line that does not move while the rows
                 above it do says nothing about the position being offered. It is a `workdir-row`
                 because it IS the row at that slot: it takes its box, its indent and its glyph axis
                 from the same rules, and a line with a geometry of its own sat a few pixels left of
                 the terminals above and below it. -->
              <div
                v-if="dropLineBefore(workdir.checkout, itemIndex)"
                class="workdir-row workdir-child terminal-drop-insertion"
                aria-hidden="true"
              >
                <SquareTerminalIcon class="workdir-icon" />
                <span>Drop terminal here</span>
              </div>

              <div
                class="workdir-row workdir-child"
                :data-session-id="item.session.id"
                :class="[
                  `state-${item.state}`,
                  {
                    active: item.active,
                    selected: item.active && showingTerminal,
                    dimmed: item.dimmed,
                    'agent-tinted': item.tint !== null,
                    'is-being-dragged': pointerDrag?.started && pointerDrag.session.id === item.session.id,
                  },
                ]"
                :style="item.tint ? { '--agent-color': item.tint } : undefined"
              >
                <!-- Editing swaps the button for the field, rather than nesting an input inside
                   one: a control inside a control cannot be focused or read on its own. The row
                   keeps its shape because both are laid out the same way. -->
                <div v-if="editingId === item.session.id" class="workdir-select">
                  <component :is="item.icon" class="workdir-icon" aria-hidden="true" />
                  <input
                    :ref="captureRenameField"
                    v-model="draftName"
                    class="workdir-rename"
                    type="text"
                    maxlength="60"
                    aria-label="Terminal session name"
                    @keydown.enter.prevent="commitRename(item.session)"
                    @keydown.esc.prevent="cancelRename"
                    @blur="commitRename(item.session)"
                  />
                </div>
                <!-- A terminal moves by mouse after a small pointer threshold; an ordinary click
                   still selects it. The menu remains the keyboard equivalent. -->
                <button
                  v-else
                  type="button"
                  class="workdir-select"
                  :aria-current="item.active && showingTerminal ? 'page' : undefined"
                  :aria-label="`Terminal session: ${rowLabel(item)}`"
                  :aria-haspopup="item.canMove ? 'menu' : undefined"
                  :aria-expanded="item.canMove ? moveMenuFor === item.session.id : undefined"
                  :title="rowLabel(item)"
                  @pointerdown="startPointerDrag(item.session, $event)"
                  @lostpointercapture="onLostPointerCapture"
                  @click="selectSession(item.session.id, $event)"
                  @dblclick="startRename(item.session)"
                  @contextmenu.prevent="item.canMove && openMoveMenu(item.session)"
                >
                  <!-- The glyph carries the state, in colour and in motion: a spinner while a turn
                     runs, the sparkles while it waits for a reply or after a turn failed, a
                     terminal's square while a plain process is up, and the same glyph in grey when
                     nothing is. There is no badge, no chip and no second line, so the icon is the
                     only place the state can live and it has to be right. -->
                  <component :is="item.icon" class="workdir-icon" aria-hidden="true" />
                  <!-- The mode goes before the name does, and it goes whole: a mode word is drawn or
                       it is not there, and a name cut in half to keep one is the worse row. The
                       row's accessible name and its tooltip carry both either way. -->
                  <span class="lbl">
                    <span class="nm">{{ sessionNameText(item) }}</span>
                    <span v-if="sessionDetailText(item)" class="dm">{{ sessionDetailText(item) }}</span>
                  </span>
                </button>

                <!-- The time and the cross share this slot, and the slot is exactly as wide as the
                   cross: the time is `display: none` on hover rather than moved, so nothing in the
                   row can shift when the pointer arrives. A row with no session has no time and
                   only ever had the empty slot. -->
                <span class="workdir-end">
                  <span v-if="item.elapsed" class="workdir-end-time">{{ item.elapsed }}</span>
                </span>

                <button
                  type="button"
                  class="workdir-action workdir-close"
                  :aria-label="`Close terminal session: ${item.title}`"
                  :title="`Close ${item.title}`"
                  @click="emit('closeSession', item.session.id)"
                >
                  <XIcon class="icon-xs" aria-hidden="true" />
                </button>

                <!-- The destinations as a menu, for the move a drag cannot make. It hangs below the
                   row rather than pushing it, so a list of worktrees never changes the panel it is
                   read from. -->
                <ul
                  v-if="item.canMove && moveMenuFor === item.session.id"
                  class="muster-menu move-menu"
                  role="menu"
                  :aria-label="`Move ${item.title} to`"
                  @keydown.esc.prevent="moveMenuFor = null"
                >
                  <li v-for="destination in moveMenuItems" :key="destination.id" role="none">
                    <button
                      type="button"
                      role="menuitem"
                      class="menu-item select-none text-left"
                      :title="destination.title"
                      @click="chooseDestination(item.session.id, destination.id)"
                    >
                      <span class="menu-item-label">{{ destination.label }}</span>
                    </button>
                  </li>
                </ul>
              </div>
            </template>

            <div
              v-if="dropLineAfter(workdir.checkout)"
              class="workdir-row workdir-child terminal-drop-insertion"
              aria-hidden="true"
            >
              <SquareTerminalIcon class="workdir-icon" />
              <span>Drop terminal here</span>
            </div>

            <div class="workdir-row workdir-child new-item">
              <button
                type="button"
                class="workdir-select"
                :aria-label="`New terminal for ${workdir.title}`"
                @click="emit('newTerminal', workdir.checkout.id)"
              >
                <!-- The same glyph slot a terminal wears, so this row's label starts on the axis the
                     terminals' labels start on rather than on the branch row's. -->
                <PlusIcon class="workdir-icon" aria-hidden="true" />
                <span class="lbl">
                  <span class="nm">New terminal</span>
                </span>
              </button>
            </div>
          </div>
        </div>
      </div>

      <p v-if="!groups.length" class="pane-state" role="status">
        Your opened folders will appear here. Use Open directory below to add one.
      </p>
    </div>
    <OverlayScrollbar :target="sidebarScroll" label="Repos and worktrees" />

    <div class="sidebar-footer">
      <div class="sep" />
      <button
        type="button"
        class="workdir-row add-item"
        aria-label="Open directory"
        title="Open directory"
        :disabled="isOpening"
        @click="emit('openFolder')"
      >
        <FolderPlusIcon class="workdir-icon" aria-hidden="true" />
        <span class="lbl">
          <span class="nm">Open directory</span>
        </span>
      </button>
    </div>

    <!-- The four type sizes a label in this panel is drawn in, one specimen each, so the panel can
         weigh a label against the room its row has. They carry the class of the thing they weigh,
         so a specimen is the type that row is really drawn in and cannot drift from it, and they
         are held out of sight and out of the flow rather than given a box to sit in. -->
    <span ref="groupNameSpecimen" class="fit-probe group-name" aria-hidden="true" />
    <span ref="groupPathSpecimen" class="fit-probe group-path" aria-hidden="true" />
    <span ref="rowNameSpecimen" class="fit-probe nm" aria-hidden="true" />
    <span ref="rowDetailSpecimen" class="fit-probe dm" aria-hidden="true" />

    <Teleport to="body">
      <div
        v-if="pointerDrag?.started"
        class="terminal-drag-ghost"
        aria-hidden="true"
        :style="{ left: pointerDrag.x + 14 + 'px', top: pointerDrag.y + 14 + 'px' }"
      >
        <SquareTerminalIcon class="workdir-icon" />
        <span>{{ draggedItem?.title }}</span>
      </div>
    </Teleport>
  </aside>
</template>

<style scoped>
/* ---------------------------------------------------------------------------------------------
   The panel, from the reference: one line per row, dense and flat, the state in the colour of the
   icon. Every rule below is a decision about pixels or about what a row is allowed to claim, and
   the comments say which. Nothing here draws a chip, a badge or a second line.
   --------------------------------------------------------------------------------------------- */

/* Air between two repos, and only between two repos: the gap above each header is what says this is
   a new list. Two checkouts of the same repo have none between them \u2014 they are one list, and a gap
   there would read as a division the repo does not have. */
.workdir-group {
  padding: 2px 0 8px;
}

/* Group header: the repo's name at a size and weight that read as a section, its location
   underneath in the muted 11px the reference uses, and one menu button on the right that appears
   on hover.

   It is set apart by weight and size rather than by a surface: nothing here is a row, so nothing
   here is painted like one. The name is medium rather than bold because a repository is a heading,
   not an announcement. */
.group-heading {
  position: relative;
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 8px 8px 4px 10px;
}

/* The name is the control that folds the repository, so it is a button wearing the heading's own
   type: it takes the row the pointer is over, it says which way it goes with `aria-expanded`, and it
   carries a chevron rather than leaving a reader to infer the state from what happens to be
   underneath it. Reset here rather than inherited, because what it would inherit is the button
   treatment every other control in the app has — a surface and a border a heading must not have. */
.group-heading-text {
  min-width: 0;
  flex: 1;
  display: flex;
  align-items: baseline;
  gap: 8px;
  /* The button's gutter is always there: a header whose text moved as the pointer arrived would
     move the name of everything under it. */
  padding: 0 30px 0 0;
  background: transparent;
  border: none;
  color: inherit;
  text-align: left;
  cursor: pointer;
  font-family: inherit;
}

/* A repository folded shut is its header and nothing else, the way a folded worktree is its row.
   Nothing marks it: the empty space under the header says it, and a glyph beside the name would put
   an icon on a row that is a heading rather than one of the list's own rows. */
.workdir-group.is-collapsed .workdir-checkouts {
  display: none;
}

/* The two halves of a header, and what each of them is allowed to do about a narrow panel.

   Neither one is cut here. `measureLabels` weighs the row and the panel drops a whole segment off
   the front of the path, then the path itself, before either word loses a character — which is why
   these two are the last thing that happens rather than the first, and why the name keeps the room
   it needs while the path is still there. The ellipsis on both is the rung after the last whole
   shape: a repo whose own name is longer than the panel cannot be drawn whole, and the tooltip
   carries the rest. */

.group-name {
  overflow: hidden;
  flex-shrink: 1;
  color: var(--muster-text);
  font-size: 0.875rem;
  font-weight: 500;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.group-path {
  overflow: hidden;
  flex: 0 1 auto;
  color: var(--muster-text-faint);
  font-size: 0.6875rem;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* The specimens `widthOf` measures against: one per type size a label is drawn in, held out of the
   flow and out of sight so they take no room and are never read. They carry the class of the thing
   they weigh, which is what keeps a specimen the type that row is really drawn in.

   The `min-width` is the one declaration they undo. `.dm`'s floor is there so a mode word is cut
   rather than deleted, and a floor on a specimen would report a word wider than the row draws —
   which is the one measurement here that would lie. */
.fit-probe {
  position: absolute;
  visibility: hidden;
  white-space: nowrap;
  pointer-events: none;
}

.fit-probe.group-name,
.fit-probe.group-path,
.fit-probe.nm,
.fit-probe.dm {
  min-width: 0;
}

.group-more {
  position: absolute;
  top: 50%;
  right: 8px;
  transform: translateY(-50%);
  opacity: 0;
  transition: opacity 0.12s ease;
}

.group-heading:hover .group-more,
.group-heading:focus-within .group-more,
.group-heading.menu-open .group-more {
  opacity: 1;
}

/* The button stays lit while its menu is open, so the menu reads as belonging to it. */
.group-heading.menu-open .group-more {
  background: var(--muster-control-hover);
  color: var(--muster-text);
}

/* Nothing here positions the menu: reka hangs it below the button and portals it to the body, the
   way every other menu in the app is placed. What is left is what this one says that the others do
   not: a width for whole phrases rather than branch names, the gap its leading icon asks for, the
   icon itself, and the count at the end of a row. The surface comes from `surface-popover` and the
   rows from `.menu-item`, so the two kinds of menu cannot drift apart. */
.group-menu {
  min-width: 224px;
}

.group-menu .menu-item {
  gap: 8px;
}

.group-menu .menu-item .icon-xs {
  flex-shrink: 0;
  color: var(--muster-text-secondary);
}

.group-menu-count {
  color: var(--muster-text-faint);
  font-size: 0.6875rem;
}

/* The destructive action is set apart by a rule, and is the only place red appears. The colour
   follows the row the way the shared rule does: `data-highlighted` is what reka puts on the row the
   arrows are on, which a CSS `:focus-visible` cannot see, because the menu moves that focus itself. */
.group-menu .group-menu-danger {
  margin-top: 6px;
  border-top: 1px solid var(--muster-control-hover);
}

.group-menu .group-menu-danger[data-highlighted]:not([data-disabled]),
.group-menu .group-menu-danger:hover {
  color: var(--muster-danger-fg);
}

/* ---------------------------------------------------------------------------------------------
   The row. One line, 26px, and square like everything else in Muster: a rounded row carrying a
   two-pixel accent edge down its left is a shape saying "this one" twice, and the edge says it once.
   Every row here — a branch, a terminal, an action — is the same square box, so which of them it is
   is said by the glyph and the name rather than by the outline.
   --------------------------------------------------------------------------------------------- */
.workdir-row {
  position: relative;
  display: flex;
  align-items: center;
  gap: 7px;
  width: 100%;
  height: 26px;
  padding: 0 8px;
}

/* A group that holds the terminal being read is said by its own row's glyph, never by a surface:
   a second tinted row above the selected one would put two claims about "where you are" in the same
   list. It comes *before* the selection rules below, because on a row that is both the current
   checkout and the selected one the selection has to win: this is context, and context never
   outranks a choice. */
.workdir-checkouts.has-active .workdir-parent .workdir-icon {
  color: var(--muster-text-secondary);
}

/* Hover and selection are told apart, and this is the whole of the panel's vocabulary for them.
   A hover is the subtle surface and nothing else: no edge and no accent, because the row under the
   pointer is a row being acted on, not a row being chosen. Selection is a different surface again,
   plus a two-pixel accent edge down its own left, so it can be picked out from across the panel
   without reading the ink. The edge is an inset shadow, so it is drawn inside the row's box and
   cannot spill onto the group guide beside it. */
.workdir-row:hover {
  background: var(--muster-el-hover);
}

/* Selection is at most one row in the whole panel, and it is whichever row carries `selected`: the
   terminal being read, or the branch when no terminal of it is selected. A checkout that merely
   HOLDS the selected terminal is context, and it used to wear the same tint and the same blue edge,
   so the panel showed two rows claiming the selection at once. */
.workdir-row.selected {
  background: var(--muster-el-selected);
  color: var(--muster-text);
  box-shadow: inset 2px 0 0 var(--muster-accent);
}

/* A terminal the panel has read something else in front of is greyed rather than deselected: same
   row, same box, the selection drawn in neutral ink. The edge is the accent's own shape with the
   accent taken out of it, because that is what changed — the row is still the terminal this checkout
   has, and it is still there; it is just not what the middle of the window is showing. The surface
   is the neutral one rather than the selected one for the same reason: blue is what a selection
   looks like, and nothing in this panel is selected while a file is. */
.workdir-row.dimmed {
  background: var(--muster-el-active);
  box-shadow: inset 2px 0 0 var(--muster-border-strong);
}

/* The name gives way on a dimmed row, and only the name. `.nm` carries its own colour, so a colour
   on the row alone would leave the label at full strength over a grey surface, which reads as a
   hover rather than as a row the panel has moved off. The glyph is left alone on purpose, exactly as
   it is on a selected row below: it says what is running in that terminal, and nothing about what
   the main panel happens to be showing changes that. */
.workdir-row.dimmed .nm {
  color: var(--muster-text-secondary);
}

/* There is deliberately NO rule here for the selected row's glyph. A rule would outrank every
   `.state-*` rule below — `.workdir-row.selected .workdir-icon` is three classes against their two —
   so selecting a terminal with a process in it repainted its green glyph in primary ink, and the row
   said, for as long as it was selected, that nothing was running in it. Selection is not a state: it is
   already said by the tint and the accent edge, and the glyph keeps saying what it means. The grey of
   an idle row is lifted below instead, so it reads on the selected tint without repainting anything. */

/* The repo header is a row too, and hovers like one: the same surface, the same padding, the same
   square corners. The surface belongs to the heading itself rather than to the button inside it, so
   the empty stretch past the name is part of the click that folds the repository. It has no selected
   state, because what is selected is a checkout and that is the row underneath it. */
.group-heading:hover {
  background: var(--muster-el-hover);
}

/* The panel's own type: the reference's 13px, which is a step under the interface's 14px because
   every row in here is one line and the rows are read in columns rather than one at a time.

   It is also what the scrollbar is positioned against, which is the only reason the panel is a
   containing block rather than a static box in a layout that never asked for one. */
.app-sidebar {
  position: relative;
  font-size: 13px;
}

.sidebar-scroll {
  min-height: 0;
  flex: 1;
  overflow-y: auto;
}

/* The fold, which is a button of its own rather than the whole row: the row selects the checkout,
   and folding a group is not selecting it. */
.workdir-fold {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 16px;
  height: 16px;
  margin-left: -4px;
  flex-shrink: 0;
  padding: 0;
  background: transparent;
  border: none;
  color: var(--muster-text-faint);
  cursor: pointer;
}

.workdir-fold:hover {
  color: var(--muster-text);
}

/* One turn, and the only motion in the panel. */
.chv {
  transition: transform 0.12s ease;
}

.col .chv {
  transform: rotate(-90deg);
}

/* A folded group is its row and nothing else. The chevron is what says it, and it says it by
   pointing at the list that is not there. */
.col > .kids {
  display: none;
}

/* ---------------------------------------------------------------------------------------------
   The group's children, and the guide that says they belong to it.
   --------------------------------------------------------------------------------------------- */
.kids {
  display: flex;
  flex-direction: column;
  margin-left: 14px;
  /* Transparent at rest and taken only while the pointer is over the group. A guide that is always
     there is a second vertical line in the panel, and a reader cannot tell an indent from a
     selection; this one is only ever drawn while it is being pointed at, which is also the only
     moment its meaning is being used. */
  border-left: 1px solid transparent;
  transition: border-color 0.15s ease;
}

.grp:hover > .kids {
  border-left-color: var(--muster-border);
}

.kids .workdir-row {
  padding-left: 12px;
}

/* ---------------------------------------------------------------------------------------------
   The label: a name that gives way last and a detail that gives way first.
   --------------------------------------------------------------------------------------------- */
.workdir-select {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 7px;
  height: 100%;
  padding: 0;
  background: transparent;
  border: none;
  color: inherit;
  text-align: left;
  cursor: pointer;
  font-family: inherit;
}

.lbl {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: baseline;
  gap: 6px;
  overflow: hidden;
  white-space: nowrap;
}

/* The name gives way, but last: `flex: 0 1 auto` lets it shrink, and `min-width: 0` is what lets it
   be narrower than its text so the ellipsis has a width to work in at all. */
.nm {
  flex: 0 1 auto;
  min-width: 0;
  overflow: hidden;
  color: var(--muster-text);
  text-overflow: ellipsis;
}

/* The detail goes first, and this is the whole rule for it.
   `flex: 1 1 0` looked like it did that and did not: a zero basis means the detail's own share of the
   shrink is zero, so a long name absorbed every pixel taken and the detail collapsed to nothing rather
   than being cut — `Review the duplicated rows in the sidebar` with no mode beside it at all, which is
   the one thing a two-part label cannot do. So the detail is sized by its own contents, like the name.

   Shrink 2 against the name's 1 is what makes it go first: both lose room in proportion to how much
   each brought, and the detail gives up twice as fast, so its tail is cut before the name is touched.
   There is deliberately NO `max-width` here. A cap was tried — `50%` of `.lbl` — and it cut a branch
   name to `feat/feedbac…` on a 203px label that had 176px spare, purely because a cap is a ceiling
   rather than a share: it trims a row with nothing competing for the space. Without one, the detail
   shows in full whenever the row can hold it, and is cut only when something is actually in the way.

   `min-width` is the other half and it is a floor, not a ceiling: the detail is never trimmed to
   nothing (measured at 2px on a 240px panel, which is not a truncation, it is a deletion), and once
   it reaches the floor the NAME is what gives way — `min-width: 0` on `.nm` is what lets it be
   narrower than its text so the ellipsis has a width to work in. Six characters is enough for a mode
   like `plan` or `coder` to read whole, and a longer one still gets a cut. */
.dm {
  flex: 0 2 auto;
  min-width: 6ch;
  overflow: hidden;
  color: var(--muster-text-faint);
  font-size: 0.75rem;
  text-overflow: ellipsis;
}

/* The glyph, and the whole of the row's state. 14px is one line of this type, so it sits on the
   name's own line box with no nudge. */
.workdir-icon {
  width: 14px;
  height: 14px;
  flex-shrink: 0;
  color: var(--muster-text-faint);
}

/* The whole branch row is one click, so the whole of it says so: the pointer belongs to the row
   and not only to the name in it, which is what the hover surface already claimed. */
.workdir-parent {
  cursor: pointer;
}

.workdir-parent.missing {
  cursor: not-allowed;
}

/* A row whose directory is gone stays listed to say so and to be closed. Its label reads as
   unavailable: nothing behind it can be selected, and the single action beside it is the only
   thing the row still does. */
.workdir-parent.missing .workdir-select {
  color: var(--muster-text-disabled);
  cursor: not-allowed;
}

.workdir-parent.missing .workdir-icon,
.workdir-parent.missing .nm {
  color: var(--muster-text-disabled);
}

.workdir-select[aria-disabled="true"] {
  color: var(--muster-text-disabled);
  cursor: not-allowed;
}

/* ---------------------------------------------------------------------------------------------
   The trailing slot: a row's time, or a branch's counts, and the cross that replaces it on hover.
   --------------------------------------------------------------------------------------------- */
.workdir-end {
  position: relative;
  flex: none;
  display: flex;
  align-items: center;
  justify-content: flex-end;
  min-width: 20px;
  color: var(--muster-text-faint);
  font-size: 0.6875rem;
  font-variant-numeric: tabular-nums;
}

.workdir-end-note {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.workdir-end-error {
  color: var(--muster-danger-fg);
}

/* The counts: monospaced, because they are read down a column of rows rather than one at a time,
   and a proportional digit moves them sideways. Green for additions and red for deletions, which
   are the two colours the reference uses and the two the palette already has.

   At rest the slot holds a bare `+−`, which says the row has changes in the one width the trailing
   edge of a row in this panel has ever been reserved for. The figures are one pointer away, and
   the two swap places by WIDTH rather than by display: two grid tracks, one open and one closed,
   so the figures grow out from under the mark instead of the mark being replaced by them.

   The closed track is `0px` and NOT `0fr`, and that is the whole trick. This container has no
   width of its own — it is a `flex: none` item sized by its contents — and a flexible track in a
   container of indefinite size is sized by its MAX-CONTENT contribution whatever its flex factor
   is, so `0fr` is not zero here: it measured the figures in full and left the mark stranded at the
   left of a slot twice as wide as it needed, squeezing the branch name for nothing. A length is
   definite whatever the container is doing, so `0px` is zero. Both values interpolate — `1fr` to
   `0fr` and `0px` to `1fr` — so the swap still animates, and on an engine that cannot interpolate
   a flex track against a length it snaps instead, which is only a missing animation.

   The mark is `aria-hidden` and the figures are not: the figures are the row's real content, and a
   reader who never lands a pointer on the row still has to be told what changed. */
.workdir-diff {
  display: grid;
  grid-template-columns: 1fr 0px;
  font-family: var(--muster-font);
  /* The cross's strip, held at zero here and opened to 28px by the hover rule below on a row that
     draws a cross. It is a margin on the figures rather than padding on the slot because the slot
     also carries a missing-directory note and a terminal's elapsed time, and neither of those is a
     count with anything to reveal. */
  margin-right: 0;
  transition:
    grid-template-columns 0.18s ease,
    margin-right 0.18s ease;
}

/* Both tracks clip: the closed one has to be closed by something, and `min-width: 0` is what stops
   the figures inside it from holding the track open on their own. */
.workdir-diff > * {
  min-width: 0;
  overflow: hidden;
  white-space: nowrap;
}

.workdir-diff-nums {
  display: flex;
  gap: 5px;
}

/* The mark wears the figures' own two colours, because it is them with the digits left off, and
   the same 5px they are separated by: the `+` therefore sits at the same offset in the mark as in
   the figures, so the swap reads as digits being added rather than as one thing becoming another. */
.workdir-diff-mark {
  display: flex;
  gap: 5px;
}

/* Hover, and the one other way a keyboard arrives at the same place — the same pairing the cross
   answers to, so a row reveals itself the same way whether the pointer or the Tab key found it. The
   mark closes on a length for the same reason it did at rest: `0fr` here would leave it measuring
   itself and push the figures right by the width of a mark nobody can see.

   The strip itself is NOT here. Every row opens into its figures, but only a row that draws a
   cross has anything standing in the space the figures give up, and on the rest — a repo root, the
   home checkout, a checkout that is missing — those 28px were an empty gap trailing the numbers for
   no reason a reader could see. The rule that gives them is the one below. */
.workdir-row:hover .workdir-diff,
.workdir-row:focus-within .workdir-diff {
  grid-template-columns: 0px 1fr;
}

/* The cross's strip, and only on a row that has one: 8px of row padding plus the cross's own 20px,
   spent out of the figures' box so the name does not have to move for it to have somewhere to
   stand. `has-close` is the same question the cross's own `v-if` asks, so a row cannot end up
   holding the space without drawing the thing that fills it, or drawing it without the space. */
.workdir-row.has-close:hover .workdir-diff,
.workdir-row.has-close:focus-within .workdir-diff {
  margin-right: 28px;
}

.ad {
  color: var(--muster-content-added);
}

.rm {
  color: var(--muster-content-removed);
}

/* The cross, out of flow at the trailing slot's own width so the row never reflows and the name
   never moves. It paints the surface of the row it belongs to — the panel at rest, the hover surface
   under the pointer, the selected tint on a selected row — so whatever it does land over, it lands
   over the row rather than letting text show through it.

   At rest it is `opacity: 0`: invisible, and nothing around it either, because the reference draws
   no cross on a resting row and a grey cell at the end of every branch is an object that list does
   not have. It is faded rather than `display: none`, which is the one deliberate departure from the
   reference and the reason is the tab order: a removed button cannot be reached, so a close a
   keyboard cannot find is a close that does not exist for half the people who use this. Invisible and
   focusable is the pair that works, and the rule below is what makes it visible the moment it is
   either pointed at or focused. */
.workdir-close {
  position: absolute;
  top: 50%;
  /* The row's right edge, so it lands exactly on top of whatever sits there — the diff figures or the
     elapsed time — the way an actions menu opens over a row rather than pushing it along. */
  right: 8px;
  width: 20px;
  height: 20px;
  transform: translateY(-50%);
  /* Above the trailing slot, which is itself positioned: two positioned siblings would be painted in
     DOM order anyway, and the cross is last, but stacking is stated rather than inferred so moving a
     child in the template cannot silently put the figures on top of the glyph. */
  z-index: 1;
  background: var(--muster-bg-1);
  opacity: 0;
  /* The same 0.18s the figures take to arrive, so the strip opens as one gesture rather than as a
     cross fading in over figures that are still growing. */
  transition: opacity 0.18s ease;
}

/* The surface the cross paints is the row's own, and it follows the row: the panel at rest, the hover
   surface under the pointer, the selected tint on a selected row. That opaque fill is what makes it
   read as a menu opening *on* the row rather than as content moving underneath it — nothing shows
   through it, and the glyph is never left under the figures it is covering. The two rules are one
   selector apart and the selected one comes second, so a hovered selected row wears its own tint. */
.workdir-row:hover > .workdir-close {
  background: var(--muster-el-hover);
}

.workdir-row.selected > .workdir-close {
  background: var(--muster-el-selected);
}

/* The cross on a dimmed row follows that row's surface, for the same reason the selected one does:
   an opaque fill is what makes it read as a menu opening *on* the row. */
.workdir-row.dimmed > .workdir-close {
  background: var(--muster-el-active);
}

/* Visible whenever it is pointed at or focused — three ways, and no fourth. */
.workdir-row:hover > .workdir-close,
.workdir-row:focus-within > .workdir-close,
.workdir-close:focus-visible {
  opacity: 1;
}

/* **Nothing on a row moves but the trailing slot.** The figures are one pointer away, so the slot
   has to be able to grow into them, and it grows out of its own box towards the left: the name
   truncates by the width of the figures for as long as the pointer is on the row, and is whole
   again the moment it leaves. The row's height, its chevron, its icon and the rhythm of every
   other row do not move — only the text between the name and the row's edge, which is where the
   numbers go. The cross is still out of flow, so it opens a strip rather than pushing the row. */

/* ---------------------------------------------------------------------------------------------
   The state, in the icon's colour. Five answers, and the words are in the row's accessible name.
   --------------------------------------------------------------------------------------------- */
.state-working .workdir-icon {
  color: var(--muster-accent);
  animation: row-spin 1.1s linear infinite;
}

.state-waiting .workdir-icon {
  color: var(--muster-warning);
}

.state-failed .workdir-icon {
  color: var(--muster-danger-fg);
}

.state-running .workdir-icon {
  color: var(--muster-success);
}

/* An idle row without an OpenCode agent colour uses the panel's muted foreground — lifted one step
   from the faintest token, because this glyph also has to read on the selected row's tint. */
.state-idle .workdir-icon {
  color: var(--muster-text-muted);
}

/* The agent's own colour, on every state that is the agent's: idle, working, and asking. Asking is
   here rather than left in the warning colour above because a question is not this row being unwell
   — it is the agent at work, and the warning took away the one colour that says which agent it is.
   An agent with no colour of its own never reaches this rule and keeps the warning, which is all
   that is left to say anything with. Three classes against two, so this outranks the state rules
   without depending on where it sits among them. */
.state-idle.agent-tinted .workdir-icon,
.state-working.agent-tinted .workdir-icon,
.state-waiting.agent-tinted .workdir-icon {
  color: color-mix(in srgb, var(--agent-color) 75%, var(--muster-text-muted));
}

@keyframes row-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .state-working .workdir-icon {
    animation: none;
  }

  .chv {
    transition: none;
  }

  .workdir-diff {
    transition: none;
  }
}

/* ---------------------------------------------------------------------------------------------
   The two rows that add something rather than name what exists.
   --------------------------------------------------------------------------------------------- */
.add-item,
.new-item {
  color: var(--muster-text-faint);
}

/* The panel's own last action is a button, and it is the one row that does not lead with a text
   left edge: the name is the whole of it, so the glyph and the name sit centred as one block.
   The label stops growing (`.lbl` fills the row everywhere else) so the block has a width to centre. */
.add-item {
  justify-content: center;
  cursor: pointer;
}

.add-item .lbl {
  flex: 0 1 auto;
}

.add-item .workdir-icon,
.new-item .workdir-icon,
.add-item .nm,
.new-item .nm {
  color: inherit;
}

.add-item:hover,
.new-item:hover,
.add-item:focus-visible,
.new-item:focus-visible {
  color: var(--muster-text);
}

/* The hairline the reference draws between the list and the panel's own last action. It runs the
   panel's whole width and takes no margin at all: an inset on either side reads as a line that
   stops short of the edges, and a margin under it reads as a gap between the line and the row. */
.sep {
  height: 1px;
  margin: 0;
  background: var(--muster-border);
}

.sidebar-footer {
  flex-shrink: 0;
}

/* The footer row is not a label for anything above it the way the list rows are, so it sits on the
   panel's centre line instead of on the axis the rest of the rows are read from. The label stops
   growing first: `.lbl` is `flex: 1` everywhere else, and a label that fills the row leaves the
   centring nothing to move. The row's own padding is left alone — it is even on both sides, so it
   moves nothing the centring did not already move, and it is what the row's height is measured in. */
.sidebar-footer .workdir-row {
  justify-content: center;
  cursor: pointer;
}

.sidebar-footer .lbl {
  flex: none;
}

/* ---------------------------------------------------------------------------------------------
   The rename field takes the row's own type so the text does not jump when it appears.
   --------------------------------------------------------------------------------------------- */
.workdir-rename {
  min-width: 0;
  flex: 1;
  padding: 0;
  background: transparent;
  border: none;
  color: var(--muster-text);
  font: inherit;
}

.workdir-rename:focus-visible {
  outline: 1px solid var(--muster-control-focus);
  outline-offset: -1px;
}

/* ---------------------------------------------------------------------------------------------
   Row actions, and the button every one of them wears. The focus case is not a courtesy: a
   keyboard reaches these with Tab, and a button that only appears on hover is a button a keyboard
   user cannot see.
   --------------------------------------------------------------------------------------------- */
.workdir-action {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  padding: 0;
  background: transparent;
  border: none;
  color: var(--muster-text-secondary);
  cursor: pointer;
}

.workdir-action:hover {
  background: var(--muster-control-hover);
  color: var(--muster-text);
}

/* The drop target, and the row under a drag that is being held over the panel. */
.workdir-checkouts.is-drop-target > .workdir-parent {
  box-shadow: inset 2px 0 0 var(--muster-accent);
}

.app-sidebar.is-terminal-dragging,
.app-sidebar.is-terminal-dragging * {
  user-select: none !important;
  cursor: grabbing !important;
}

.workdir-child.is-being-dragged {
  opacity: 0.42;
}

/* The destination list, hanging below the row it belongs to. It is out of flow so a list of
   worktrees does not push the rows after it, and it does not fade with a hover: a menu that
   disappears when the pointer leaves the row cannot be read. */
.move-menu {
  position: absolute;
  top: 26px;
  left: 12px;
  z-index: 20;
  min-width: 200px;
}

/* The drop line is a row of the list, and says nothing but what it is. Its box, its indent and its
   glyph axis come from `.workdir-row` and `.kids .workdir-row` — which is the whole point: it used
   to be a box with a margin of its own, four pixels left of the terminals above and below it, so the
   glyph it drew on the cursor's way in was not on the axis of the row it was about to become. */
.terminal-drop-insertion {
  /* An outline rather than a border, for one measurable reason: a border takes a pixel of the row's
     own content box, so the line's glyph would sit one pixel right of every terminal's — the exact
     misalignment the row classes were brought in to remove. Drawn inside the box, it costs nothing. */
  outline: 1px dashed var(--muster-accent);
  outline-offset: -1px;
  color: var(--muster-accent);
  font-size: 0.6875rem;
}

.terminal-drag-ghost {
  position: fixed;
  z-index: 1000;
  display: flex;
  align-items: center;
  /* The same row again, this time under the pointer rather than in the list, so it carries the same
   * indent the row it came from wears. The pointer sits inside it at an offset, as it does in the
   * reference; the glyph does not move away from the column it will land in.
   *
   * The type is named here rather than inherited, because this box is teleported to the body and
   * therefore sits outside `.app-sidebar` — which is where the panel's 13px lives. Inheriting the
   * body's size made the dragged row's name larger than the row it was a copy of, which is the same
   * misalignment as an icon off its column and just as wrong. */
  gap: 7px;
  height: 26px;
  padding: 0 12px;
  font-family: inherit;
  font-size: 13px;
  /* Never wider than the panel's own minimum, so the ghost stays a chip over the workspace rather
     than a banner drawn across it — and so it is never wider than the row it is a copy of. */
  max-width: 240px;
  overflow: hidden;
  border: 1px solid var(--muster-accent);
  background: var(--muster-control-bg);
  color: var(--muster-text);
  box-shadow: 0 4px 14px rgb(0 0 0 / 30%);
  pointer-events: none;
  white-space: nowrap;
}

.terminal-drag-ghost span {
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
