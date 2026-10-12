<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { isIpcError } from "../domain/ipc";
import { Plus as PlusIcon } from "@lucide/vue";
import type { Checkout, Session, TerminalSessionStatus, WorkspaceState } from "../domain/workspace";
import type { TerminalSettings } from "../domain/settings";
import {
  addSessionToLayout,
  createTerminalLayout,
  normalizeTerminalLayout,
  moveSessionId,
  orderedSessionIds,
  removeSessionFromLayout,
} from "../domain/terminal-layout";
import type { CheckoutTerminalLayout } from "../domain/terminal-layout";
import { loadTerminalLayout, moveTerminal, saveTerminalLayout } from "../lib/ipc";
import { useToasts } from "../presentation/toasts";
import Button from "./ui/button/Button.vue";
import TerminalSession from "./TerminalSession.vue";

// Not a `withDefaults` block, and that is deliberate: `shellRequest` and `registeredSessionIds`
// are optional because a caller that has neither says so by leaving them out, and a default of
// `null` or `[]` would be a second way of saying the same thing that the watchers below cannot
// tell apart from the real one. `visible` is a default for the same reason TerminalSession has it:
// the pane is visible unless told otherwise.
const props = defineProps<{
  checkout: Checkout | null;
  /**
   * Every checkout on the panel, and the reason it is here at all: a move names a destination the
   * pane cannot select, so it needs to know the destination exists, is not missing, and where it
   * is on disk. A caller with no moves to offer passes nothing, and then there is nowhere to
   * move to.
   */
  checkouts?: Checkout[];
  activeSessionId: string | null;
  isOpening: boolean;
  visible?: boolean;
  shellRequest?: { checkoutId: string; token: number } | null;
  registeredSessionIds?: string[];
  terminalSettings?: TerminalSettings;
  zoom?: number;
}>();
const isVisible = computed(() => props.visible ?? true);

const emit = defineEmits<{
  openFolder: [];
  workspaceUpdated: [workspace: WorkspaceState, shellRequestToken?: number];
  sessionStatusChanged: [sessionId: string, status: TerminalSessionStatus | null];
  shellCreated: [token: number, checkoutId: string, sessionId: string];
  /**
   * The order this checkout's terminals are now listed in, published on every save.
   *
   * The sidebar row and this layout are one list drawn twice, and the panel is the only place the
   * order is decided — so it is announced rather than recomputed there. Without it the sidebar draws
   * whatever order the database hands out and a reorder is persisted, invisible and useless.
   */
  sessionOrder: [checkoutId: string, order: string[]];
  openFile: [path: string];
  openExternalUrl: [url: string];
}>();

interface TerminalView {
  /** Stable for the entire lifetime of this view; never replace it with the backend session ID. */
  key: string;
  checkoutId: string;
  session: Session | null;
  shellRequestToken?: number;
}
type SessionMoveResult = {
  moved: boolean;
  directoryChange: "not-requested" | "written" | "not-written";
};
interface TerminalSessionHandle {
  requestClose(): Promise<boolean>;
  changeDirectory(path: string): Promise<boolean>;
  focus(): void;
}

const layouts = ref<Record<string, CheckoutTerminalLayout>>({});
const loadedCheckoutIds = new Set<string>();
const layoutLoads = new Map<string, Promise<void>>();
const layoutSaveQueues = new Map<string, Promise<void>>();
const views = ref<TerminalView[]>([]);
const pendingViewKey = ref<string | null>(null);
const startingCheckoutIds = ref(new Set<string>());
const terminalRefs = new Map<string, TerminalSessionHandle>();
const nextViewId = ref(1);
const { push: pushToast } = useToasts();
const checkout = computed(() => props.checkout);
/**
 * The view this workdir is showing.
 *
 * The id arrives already resolved by `App.vue` (`resolveActiveSession`: the selected session, or the
 * newest this workdir has), so the row in the sidebar and the pane here are the same terminal and
 * cannot name two. The last fallback is the safety net under that, not a rule of its own: a session
 * with no view here cannot exist — no PTY outlives the process that spawned it, so the backend clears
 * every stored session when the window reopens and each one in the panel has a live view — and if it
 * ever did, the pane shows its newest terminal rather than an empty panel where a terminal is open.
 */
const activeView = computed(() => {
  const checkoutViews = views.value.filter((view) => view.checkoutId === checkout.value?.id);
  return (
    checkoutViews.find((view) => view.key === pendingViewKey.value) ??
    checkoutViews.find((view) => view.session?.id === props.activeSessionId) ??
    checkoutViews.at(-1)
  );
});
const isStarting = computed(
  () =>
    Boolean(checkout.value && startingCheckoutIds.value.has(checkout.value.id)) ||
    Boolean(activeView.value && !activeView.value.session),
);

/**
 * A failure to read or write the layout is not shown at all: a stale layout is healed into a
 * working one, so reporting it would name a problem the user never has. Everything else is
 * announced (A.6) rather than left in a bar under the terminal, which pushed the last lines of
 * output out of the panel for a message that is gone six seconds later.
 */
function reportTerminalError(cause: unknown) {
  const message = isIpcError(cause) ? cause.message : cause instanceof Error ? cause.message : String(cause);
  if (/^(?:saved terminal layout is invalid|terminal layout\b)/.test(message)) return;
  pushToast(message);
}

function saveLayout(checkoutId: string, layout: CheckoutTerminalLayout) {
  layouts.value[checkoutId] = layout;
  emit("sessionOrder", checkoutId, [...layout.sessionOrder]);
  const snapshot = JSON.parse(JSON.stringify(layout)) as CheckoutTerminalLayout;
  const previous = layoutSaveQueues.get(checkoutId) ?? Promise.resolve();
  const pending = previous
    .catch(() => {})
    .then(() => saveTerminalLayout(checkoutId, snapshot))
    .catch(reportTerminalError);
  layoutSaveQueues.set(checkoutId, pending);
  return pending;
}

/**
 * The order a checkout's terminals are drawn in: what its saved layout says, then the rest.
 *
 * Written in full rather than as a delta, because a layout that only knows the terminals created in
 * this run knows nothing about the ones that were here when it was loaded — and a reorder aimed at
 * one of those is a reorder of a list it does not contain.
 */
function currentOrder(checkoutId: string): string[] {
  const checkout = (props.checkouts ?? []).find((item) => item.id === checkoutId);
  return orderedSessionIds(checkout?.sessions ?? [], layouts.value[checkoutId]);
}

function initializeLayout(target: Checkout) {
  if (loadedCheckoutIds.has(target.id)) return Promise.resolve();
  const loading = layoutLoads.get(target.id);
  if (loading) return loading;
  const pending = loadTerminalLayout(target.id)
    .then((saved) => {
      const normalized = normalizeTerminalLayout(saved, target.sessions, props.activeSessionId);
      layouts.value[target.id] = normalized;
      if (saved && JSON.stringify(saved) !== JSON.stringify(normalized)) {
        return saveLayout(target.id, normalized);
      }
    })
    .catch((cause: unknown) => {
      reportTerminalError(cause);
      layouts.value[target.id] = createTerminalLayout([]);
    })
    .then(() => {
      loadedCheckoutIds.add(target.id);
    })
    .finally(() => layoutLoads.delete(target.id));
  layoutLoads.set(target.id, pending);
  return pending;
}

async function createTerminalSession(additional = false, shellRequestToken?: number) {
  const target = props.checkout;
  if (!target || target.isMissing || (!additional && views.value.some((view) => view.checkoutId === target.id))) return;
  if (startingCheckoutIds.value.has(target.id)) return;
  startingCheckoutIds.value = new Set(startingCheckoutIds.value).add(target.id);
  try {
    await initializeLayout(target);
    if (props.checkout?.id !== target.id || target.isMissing) return;
    const key = `pending-${nextViewId.value++}`;
    pendingViewKey.value = key;
    views.value.push({ key, checkoutId: target.id, session: null, shellRequestToken });
  } finally {
    const pending = new Set(startingCheckoutIds.value);
    pending.delete(target.id);
    startingCheckoutIds.value = pending;
  }
}

function setTerminalRef(key: string, instance: unknown) {
  if (instance) terminalRefs.set(key, instance as TerminalSessionHandle);
  else terminalRefs.delete(key);
}

function focusActiveTerminal() {
  const view = activeView.value;
  if (view) terminalRefs.get(view.key)?.focus();
}

async function requestClose(sessionId: string) {
  const view = views.value.find((item) => item.session?.id === sessionId);
  if (!view) return false;
  return (await terminalRefs.get(view.key)?.requestClose()) ?? false;
}

/**
 * Hands a live terminal to another worktree of the same repository.
 *
 * The process is not restarted and its output is not lost: what moves is which checkout's layout
 * the pane belongs to, and the session row that says so. Both layouts are rewritten (the session
 * leaves the one it was in and opens as a tab in the one it moved to) because a layout that kept
 * a pane for a session its checkout no longer holds is the layout the backend refuses to save.
 *
 * A manual move always tells a shell sitting at a prompt to `cd`. A
 * shell with something running in front of it is left where it is, and says so: typing `cd` into a
 * build would feed the build. A caller that has already moved the program itself passes false:
 * there is no shell at a prompt to tell, and announcing a `cd` that was never attempted would
 * report a failure the user did not have.
 *
 * `selectTarget` is the same distinction one level up: a move someone asked for takes the window
 * with it, and a move that happened on its own does not get to change what is on screen.
 *
 * Whether the row moved and whether the requested `cd` was written are returned separately: the
 * PTY write only confirms bytes were accepted, not that the shell executed the command.
 */
async function moveSession(
  sessionId: string,
  targetCheckoutId: string,
  index: number,
  changeDirectory = true,
  selectTarget = true,
): Promise<SessionMoveResult> {
  const view = views.value.find((item) => item.session?.id === sessionId);
  const target = (props.checkouts ?? []).find((checkout) => checkout.id === targetCheckoutId);
  // A drop in the terminal's own worktree is a reorder, which is not a move at all: nothing about the
  // terminal changes, and asking the backend about it would be refused. Checked before the target,
  // because the target is what a reorder names and the reorder is what is really happening.
  if (view && view.session && view.checkoutId === targetCheckoutId) {
    const layout = layouts.value[view.checkoutId] ?? createTerminalLayout([]);
    void saveLayout(view.checkoutId, {
      ...layout,
      sessionOrder: moveSessionId(currentOrder(view.checkoutId), sessionId, index),
    });
    return { moved: true, directoryChange: "not-requested" };
  }
  if (!view || !view.session || !target || target.isMissing) {
    return { moved: false, directoryChange: "not-requested" };
  }
  const source = view.checkoutId;
  let workspace: WorkspaceState;
  try {
    workspace = await moveTerminal(source, sessionId, targetCheckoutId, selectTarget);
  } catch (cause) {
    reportTerminalError(cause);
    return { moved: false, directoryChange: "not-requested" };
  }
  view.checkoutId = targetCheckoutId;
  const moved = { ...view.session, checkoutId: targetCheckoutId };
  view.session = moved;
  void saveLayout(source, removeSessionFromLayout(layouts.value[source] ?? createTerminalLayout([]), sessionId));
  const targetLayout = layouts.value[targetCheckoutId] ?? createTerminalLayout([]);
  // Inserted where the drop aimed rather than at the end: a terminal dragged onto the second row of
  // another worktree belongs second there, and a move that quietly reordered its list by appending
  // would answer a question nobody asked. The incoming id is filtered out of the order first
  // because `moveSessionId` removes it anyway, and a layout still naming the session's old worktree
  // would have it twice.
  void saveLayout(targetCheckoutId, {
    ...addSessionToLayout(targetLayout, moved),
    sessionOrder: moveSessionId(orderedSessionIds(target.sessions, targetLayout), moved.id, index),
  });
  emit("workspaceUpdated", workspace);
  if (!changeDirectory) return { moved: true, directoryChange: "not-requested" };
  // The pane's own checkout prop is what the session is written under, and the new one only
  // reaches the terminal on the next tick. Writing before that would address the `cd` to the
  // worktree the session just left, which the backend refuses.
  await nextTick();
  const changed = await terminalRefs.get(view.key)?.changeDirectory(target.path);
  if (!changed) {
    // False also covers a rejected PTY write, whose actual error remains in the terminal alert.
    pushToast(`${target.path}: the terminal moved, but its directory was not changed.`);
  }
  return { moved: true, directoryChange: changed ? "written" : "not-written" };
}

defineExpose({ focusActiveTerminal, requestClose, moveSession });

function onCreated(key: string, result: { session: Session; workspace: WorkspaceState }) {
  const view = views.value.find((item) => item.key === key);
  if (!view || view.session) return;
  view.session = result.session;
  const layout = layouts.value[view.checkoutId] ?? createTerminalLayout([]);
  void saveLayout(view.checkoutId, addSessionToLayout(layout, result.session));
  emit("workspaceUpdated", result.workspace, view.shellRequestToken);
  if (view.shellRequestToken !== undefined) {
    emit("shellCreated", view.shellRequestToken, view.checkoutId, result.session.id);
  }
}

function onStatusChanged(sessionId: string, status: TerminalSessionStatus) {
  emit("sessionStatusChanged", sessionId, status);
}

function onClosed(key: string, workspace: WorkspaceState) {
  const view = views.value.find((item) => item.key === key);
  if (view?.session) {
    terminalRefs.delete(key);
    emit("sessionStatusChanged", view.session.id, null);
    if (pendingViewKey.value === key) pendingViewKey.value = null;
    void saveLayout(
      view.checkoutId,
      removeSessionFromLayout(layouts.value[view.checkoutId] ?? createTerminalLayout([]), view.session.id),
    );
  }
  views.value = views.value.filter((item) => item.key !== key);
  emit("workspaceUpdated", workspace);
}

function onFailed(key: string, message: string) {
  const view = views.value.find((item) => item.key === key);
  terminalRefs.delete(key);
  if (view?.session) {
    emit("sessionStatusChanged", view.session.id, null);
    void saveLayout(
      view.checkoutId,
      removeSessionFromLayout(layouts.value[view.checkoutId] ?? createTerminalLayout([]), view.session.id),
    );
  }
  if (pendingViewKey.value === key) pendingViewKey.value = null;
  views.value = views.value.filter((item) => item.key !== key);
  pushToast(message);
}

watch(
  () => props.activeSessionId,
  (sessionId) => {
    if (sessionId && views.value.some((view) => view.session?.id === sessionId)) pendingViewKey.value = null;
  },
);

/** The shell request this pane has already answered, so one request is one terminal. */
let answeredShellToken = 0;

/**
 * A shell request names the workdir it is for, and the pane only has that workdir once its saved
 * UI state has been read, which for a workdir picked for the first time is after the request was
 * made. Keyed on the pair, so a request that arrives before its workdir is answered as soon as
 * the workdir lands instead of being dropped and needing a second click.
 */
watch(
  () => [props.shellRequest?.token, props.checkout?.id] as const,
  ([token, checkoutId]) => {
    if (!token || token === answeredShellToken || props.shellRequest?.checkoutId !== checkoutId) return;
    answeredShellToken = token;
    void createTerminalSession(true, token);
  },
);

watch(
  () => props.registeredSessionIds?.join("\0"),
  (ids) => {
    if (ids === undefined) return;
    const registered = new Set(ids ? ids.split("\0") : []);
    views.value = views.value.filter((view) => {
      if (!view.session || registered.has(view.session.id)) return true;
      terminalRefs.delete(view.key);
      return false;
    });
  },
);
</script>

<template>
  <main class="session-pane flex min-w-0 flex-1 flex-col">
    <section class="relative min-h-0 flex-1" aria-label="Terminal session view">
      <div
        v-for="view in views"
        v-show="view.checkoutId === checkout?.id && view.key === activeView?.key"
        :key="view.key"
        class="absolute inset-0"
      >
        <TerminalSession
          :ref="(instance) => setTerminalRef(view.key, instance)"
          :key="view.key"
          :checkout-id="view.checkoutId"
          :name="view.session?.name"
          :active="view.checkoutId === checkout?.id && view.key === activeView?.key"
          :visible="isVisible"
          :focused="isVisible && view.session?.id === activeSessionId"
          :scrollbar="terminalSettings?.scrollbar"
          :font-size="terminalSettings?.fontSize ?? 16"
          :ligatures="terminalSettings?.ligatures ?? true"
          :selection-copy="terminalSettings?.selectionCopy ?? true"
          :cursor-blink="terminalSettings?.cursorBlink ?? true"
          :cursor-style="terminalSettings?.cursorStyle ?? 'block'"
          :zoom="zoom"
          @created="onCreated(view.key, $event)"
          @closed="onClosed(view.key, $event)"
          @status-changed="onStatusChanged(view.session?.id ?? view.key, $event)"
          @failed="onFailed(view.key, $event)"
          @open-file="emit('openFile', $event)"
          @open-external-url="emit('openExternalUrl', $event)"
        />
      </div>

      <div v-if="!activeView" class="session-empty grid h-full place-items-center p-6 text-center">
        <div class="max-w-md">
          <p class="text-sm text-(--muster-text-dim)">
            {{
              checkout?.isMissing
                ? `Directory missing: ${checkout.path}`
                : checkout
                  ? isStarting
                    ? "Starting terminal…"
                    : "No terminal session."
                  : "Open a folder to start a terminal."
            }}
          </p>
          <Button
            v-if="checkout && !checkout.isMissing && !isStarting"
            variant="tinted"
            class="mt-4"
            @click="createTerminalSession()"
          >
            <PlusIcon class="size-3.5 shrink-0" aria-hidden="true" />
            New terminal
          </Button>
          <Button
            v-else-if="!checkout"
            variant="tinted"
            class="mt-4"
            :disabled="isOpening"
            @click="$emit('openFolder')"
          >
            <PlusIcon class="size-3.5 shrink-0" aria-hidden="true" />
            {{ isOpening ? "Opening…" : "Open directory" }}
          </Button>
          <span v-else-if="isStarting" class="mt-3 block text-xs text-(--muster-text-faint)" role="status">
            Starting session…
          </span>
        </div>
      </div>
    </section>
  </main>
</template>
