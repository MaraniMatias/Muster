<script setup lang="ts">
/* eslint-disable vue/html-closing-bracket-newline, vue/html-indent */
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { homeDir } from "@tauri-apps/api/path";
import { computed, nextTick, onMounted, onUnmounted, provide, ref, watch } from "vue";
import { SplitterGroup, SplitterPanel, SplitterResizeHandle } from "reka-ui";
import {
  Columns2 as Columns2Icon,
  Columns3 as Columns3Icon,
  Copy as CopyIcon,
  Minus as MinusIcon,
  Settings as SettingsIcon,
  Square as SquareIcon,
  X as XIcon,
} from "@lucide/vue";
import type { Checkout, Repo } from "./domain/workspace";
import { availableReviewTargets as getAvailableReviewTargets } from "./domain/compatibility";
import type { CompatibilitySnapshot } from "./domain/compatibility";
import type { CheckoutFileActivity } from "./domain/git";
import type { ReviewTarget } from "./domain/review";
import {
  agentSessionTitle,
  displayCheckoutPath,
  resolveActiveSession,
  sessionTitle,
  terminalHasProcess,
} from "./domain/workspace";
import { mainViewFromState, mainViewLabel, mainViewToState, resolveMainView } from "./domain/main-document";
import type { DocumentMode, MainView } from "./domain/main-document";
import type { TitlebarMenuItem, TitlebarMenuSection } from "./domain/titlebar-menu";
import InspectorPane from "./components/InspectorPane.vue";
import FileIcon from "./components/FileIcon.vue";
import MainPane from "./components/MainPane.vue";
import Sidebar from "./components/Sidebar.vue";
import TitlebarMenu from "./components/TitlebarMenu.vue";
import ToastStack from "./components/ToastStack.vue";
import ConfirmDialog from "./components/ConfirmDialog.vue";
import WorktreeDialog from "./components/WorktreeDialog.vue";
import SettingsDialog from "./components/SettingsDialog.vue";
import type { TerminalSessionStatus, WorkspaceState } from "./domain/workspace";
import { workdirTitle } from "./domain/workspace";
import { carriesAppModifier } from "./domain/shortcuts";
import {
  clearReviewFolder,
  closeCheckout as persistCheckoutClose,
  closeMissingCheckout as persistMissingCheckoutClose,
  exportReviewMarkdown,
  getReviewFolder,
  getCompatibilitySnapshot,
  getTerminalStatus,
  loadReviewTarget,
  openExternalFile as requestExternalFile,
  openExternalUrl as requestExternalUrl,
  renameTerminal,
  restoreArchivedWorktrees as persistArchivedRestore,
  selectCheckout as persistCheckoutSelection,
  saveReviewTarget,
} from "./lib/ipc";

import { useWorkspaceState } from "./presentation/workspace";
import { useRecentPaths } from "./presentation/recent-paths";
import { useActiveGitSnapshot } from "./presentation/active-git-snapshot";
import { useGitWatchers } from "./presentation/git-watchers";
import { useWorktreeSync } from "./presentation/worktree-sync";
import { REVIEW_SENDER, useReviewNotes } from "./presentation/review-notes";
import type { ReviewSender } from "./presentation/review-notes";
import { useAgentRelocations, useAgentSessions, useTerminalAgentRows } from "./presentation/agent-sessions";
import { useToasts } from "./presentation/toasts";
import { useTerminalMovement } from "./presentation/terminal-movement-controller";
import { WORKDIR_ICONS } from "./presentation/workdir-icons";
import { theme } from "./presentation/theme";
import type { Theme } from "./presentation/theme";
import { AGENT_APP, defaultOpenCodeSession, matchAgentSessionTitle } from "./domain/agent";
import { DEFAULT_ZOOM, zoomKeyFor, zoomLabel, zoomStep } from "./domain/zoom";
import type { Zoom, ZoomModifier } from "./domain/zoom";
import { buildReviewMarkdown, localReviewTimestamp } from "./domain/review";
import { useLayoutPersistence } from "./presentation/layout-persistence";
import { isMarkdownPath } from "./presentation/markdown-preview";
import { mediaKind } from "./domain/media";
import {
  DEFAULT_APP_LAYOUT,
  DEFAULT_CHECKOUT_UI_STATE,
  INSPECTOR_WIDTH_LIMITS,
  SIDEBAR_WIDTH_LIMITS,
  needsInspectorDrawer,
  normalizeAppLayout,
  normalizeCheckoutUiState,
  resizeLayoutPanel,
} from "./domain/ui-state";
import type { AppLayoutState, CheckoutUiState } from "./domain/ui-state";
import { DEFAULT_SETTINGS, cloneSettings, normalizeSettings, uiFontScale } from "./domain/settings";
import type { AppSettings, ReviewStorageMode } from "./domain/settings";
import type { ReviewFolder } from "./domain/files";
import { formatBytes } from "./domain/files";
import { loadAppLayout, loadCheckoutUiState, loadSettings, prepareAppExit, saveSettings } from "./lib/ipc";
import type { TerminalLaunch } from "./lib/ipc";
import { reportFrontendDiagnostic } from "./lib/diagnostics";
import type { DiagnosticCategory } from "./lib/diagnostics";

const {
  workspace,
  activeCheckout,
  launchCheckoutId,
  isOpening,
  chooseFolder,
  openPath,
  selectCheckout,
  selectSession: selectWorkspaceSession,
  updateWorkspace,
  promptForDefaultBranchIfNeeded,
} = useWorkspaceState();
const { recentPaths, refresh: refreshRecentPaths } = useRecentPaths();
/** The one crumb menu that is open: opening any of them closes the other two. */
const openCrumb = ref<string | null>(null);
const { push: pushToast, pushCause: reportCause, dismiss: dismissToast } = useToasts();
const appLayout = ref<AppLayoutState>({ ...DEFAULT_APP_LAYOUT });
const appLayoutReady = ref(false);
const appShell = ref<HTMLElement | null>(null);
const checkoutUiStates = ref<Record<string, CheckoutUiState>>({});
const checkoutUiReady = ref(false);
/**
 * What the window looked like when it was last closed: written on its own schedule, in one
 * order, and flushed in full before the window is allowed to go.
 */
const { scheduleCheckoutUiSave, flushUiStateWrites } = useLayoutPersistence({
  appLayout,
  appLayoutReady,
  checkoutUiStates,
  reportCause,
});
const mainPane = ref<InstanceType<typeof MainPane> | null>(null);
/** ⌘/'s state: the navigation and the files and changes panel are one thing to the eye, so they go
 *  together. A window with the main view alone in it is the point of the shortcut. */
const sidePanelsVisible = ref(true);
type PanelRef = {
  collapse(): void;
  expand(): void;
  resize(size: number): void;
  $el: HTMLElement;
};
const sidebarPanel = ref<PanelRef | null>(null);
const inspectorPanel = ref<PanelRef | null>(null);
const viewportWidth = ref(window.innerWidth / DEFAULT_ZOOM);
/** The preferences in `~/.muster/config.yml`, in the shape the file has them in. */
const settings = ref<AppSettings>(cloneSettings(DEFAULT_SETTINGS));
const settingsButton = ref<HTMLButtonElement | null>(null);
const settingsOpen = ref(false);
const settingsSaving = ref(false);
/**
 * The version of the build this window is, read from the binary rather than from a file beside it.
 *
 * It is read once at startup and stays null if it cannot be: the About section draws a version only
 * when there is one, and a version guessed from a manifest that a release could have written and not
 * shipped is worse than no version at all.
 */
const appVersion = ref<string | null>(null);
/** The scale the whole window is drawn at, which every measurement below has to agree with. */
const appZoom = computed(() => settings.value.ui.zoom);
const isNarrow = computed(
  () => appLayout.value.mode === "focus" && needsInspectorDrawer(appLayout.value, viewportWidth.value),
);
const isSplitLayout = computed(() => appLayout.value.mode === "split");
const inspectorInDrawer = computed(() => isNarrow.value || isSplitLayout.value);
const splitInspectorOpen = ref(false);
const pointerOverSplitStrip = ref(false);
const pointerOverSplitDrawer = ref(false);
const activeRepo = computed(
  () =>
    workspace.value.repos.find((repo) => repo.checkouts.some((checkout) => checkout.id === activeCheckout.value?.id)) ??
    null,
);
// The sidebar names every checkout at once, so every one of them needs a change signal and not
// only the one on screen.
const unwatchedRepos = useGitWatchers(() => workspace.value.repos);
const gitSnapshot = useActiveGitSnapshot(
  activeCheckout,
  activeRepo,
  () => void promptForDefaultBranchIfNeeded(true),
  unwatchedRepos,
);
const review = useReviewNotes(activeCheckout, activeRepo);
const agent = useAgentSessions(activeCheckout, activeRepo);
/**
 * The OpenCode sessions of every checkout that has a terminal, for the sidebar rows.
 *
 * Separate from `agent`, which follows the active checkout for the review surface: a row is about
 * one terminal in one worktree, and it must be able to answer for a worktree nobody has selected.
 * It is the whole session list rather than one summary per worktree, because a row identifies its
 * own session by its terminal title and needs every candidate in that workdir to match against.
 */
const terminalAgents = useTerminalAgentRows(
  computed(() => [
    ...new Set(
      workspace.value.repos.flatMap((repo) =>
        repo.checkouts.filter((checkout) => checkout.sessions.length > 0).map((checkout) => checkout.id),
      ),
    ),
  ]),
);
const terminalMovement = useTerminalMovement({
  getWorkspace: () => workspace.value,
  getFollowMode: () => settings.value.terminal.followWorktree,
  terminalShowsSession,
  shouldFollowWindow: (sessionId, fromCheckoutId) =>
    !pendingShellNavigation &&
    workspace.value.activeCheckoutId === fromCheckoutId &&
    workspace.value.activeSessionId === sessionId &&
    terminalIsOnScreen(sessionId),
  isPiTerminal: (terminalId) => {
    const status = sessionRuntimeStatuses.value[terminalId];
    return Boolean(status?.foregroundProcess && status.foregroundApp === "pi");
  },
  moveSession: async (sessionId, checkoutId, index, changeDirectory, selectTarget) =>
    await mainPane.value?.moveSession(sessionId, checkoutId, index, changeDirectory, selectTarget),
  followMovedTerminal: async (sessionId, checkoutId, isCurrent) => {
    if (!isCurrent()) return;
    showView(checkoutId, { kind: "terminal", sessionId }, { navigation: false });
    await nextTick();
    if (isCurrent()) mainPane.value?.focusActiveTerminal();
  },
  onError: reportCause,
});
/**
 * Where OpenCode sessions are working, so a row can follow one into the worktree it moved to.
 *
 * Relocations arrive asynchronously after the window has been set up.
 */
useAgentRelocations(
  terminalMovement.onAgentRelocation,
  computed(
    () => settings.value.terminal.followWorktree === "agent" || settings.value.terminal.followWorktree === "both",
  ),
);
const compatibilitySnapshot = ref<CompatibilitySnapshot | null>(null);
const compatibilityLoading = ref(false);
const compatibilityError = ref("");
let compatibilityRequest = 0;
const availableReviewDestinations = computed(() => getAvailableReviewTargets(compatibilitySnapshot.value));

async function refreshCompatibilitySnapshot(): Promise<boolean> {
  const request = ++compatibilityRequest;
  compatibilityLoading.value = true;
  compatibilityError.value = "";
  try {
    const snapshot = await getCompatibilitySnapshot();
    if (request !== compatibilityRequest) return false;
    compatibilitySnapshot.value = snapshot;
    return true;
  } catch (cause) {
    if (request === compatibilityRequest) {
      compatibilityError.value = cause instanceof Error ? cause.message : String(cause);
    }
    return false;
  } finally {
    if (request === compatibilityRequest) compatibilityLoading.value = false;
  }
}

const reviewTarget = ref<ReviewTarget>("markdown");
let reviewTargetLoadGeneration = 0;
watch(
  () => [activeCheckout.value?.id ?? null, compatibilitySnapshot.value] as const,
  async ([checkoutId, snapshot], previous) => {
    const [previousCheckoutId, previousSnapshot] = previous ?? [null, null];
    const checkoutChanged = checkoutId !== previousCheckoutId;
    if (checkoutId && snapshot && !checkoutChanged && previousSnapshot) {
      if (getAvailableReviewTargets(snapshot).includes(reviewTarget.value)) return;
      ++reviewTargetLoadGeneration;
      reviewTarget.value = "markdown";
      try {
        await saveReviewTarget(checkoutId, "markdown");
      } catch (cause) {
        reportCause(cause);
      }
      return;
    }

    const request = ++reviewTargetLoadGeneration;
    reviewTarget.value = "markdown";
    if (!checkoutId || !snapshot) return;
    try {
      const target = await loadReviewTarget(checkoutId);
      if (request !== reviewTargetLoadGeneration) return;
      const selected = getAvailableReviewTargets(compatibilitySnapshot.value).includes(target) ? target : "markdown";
      reviewTarget.value = selected;
      if (selected !== target) await saveReviewTarget(checkoutId, "markdown");
    } catch (cause) {
      if (request === reviewTargetLoadGeneration) reportCause(cause);
    }
  },
  { immediate: true },
);
const allCheckouts = computed(() => workspace.value.repos.flatMap((repo) => repo.checkouts));
const registeredSessionIds = computed(() =>
  workspace.value.repos.flatMap((repo) =>
    repo.checkouts.flatMap((checkout) => checkout.sessions.map((session) => session.id)),
  ),
);
const lifecycle = ref<{ mode: "create" | "remove"; checkoutId: string } | null>(null);

// A session that reappears after a crash may carry messages this client never saw, so any
// round left unconfirmed is settled as soon as the checkout's sessions can be read.
watch(
  () => agent.sessions.map((session) => `${session.id}:${session.updatedAt}`).join(","),
  (sessionsKey) => {
    if (sessionsKey) void review.reconcileRounds();
  },
);

// v2.0.18 has no turn-completed event, so OpenCode completions are observed by session.
let observedCompletedTurns = 0;
watch(
  () => agent.turnsCompleted,
  async (completed) => {
    const sessionIds = agent.completedSessionIds.slice(observedCompletedTurns, completed);
    observedCompletedTurns = completed;
    if (!sessionIds.length) return;
    await review.flushQueuedRounds();
    for (const sessionId of sessionIds) await review.ackFinishedTurn(sessionId);
  },
);

const shellRequest = ref<{ checkoutId: string; token: number; launch?: TerminalLaunch } | null>(null);
let shellRequestToken = 0;
let pendingShellNavigation: { checkoutId: string; token: number; revision: number } | null = null;
watch(launchCheckoutId, (checkoutId) => {
  if (checkoutId) void requestShell(checkoutId);
});

// The recents are read when the workdir menu is about to be read, so a folder opened a moment
// ago is already in the list. A failure is a toast, and the menu opens without the group.
watch(openCrumb, (name) => {
  if (name === "workdir") void refreshRecentPaths();
});
const sendingReview = ref(false);
const mainViews = ref<Record<string, MainView>>({});
const sessionRuntimeStatuses = ref<Record<string, TerminalSessionStatus>>({});
/**
 * The order each checkout's terminals are listed in, as the pane saved it.
 *
 * The pane owns that order and the sidebar draws it, so it travels through here rather than being
 * decided twice: a sidebar that ordered the sessions itself would show a reorder as nothing at all,
 * because the order it computed is not the one that was saved.
 */
const sessionOrder = ref<Record<string, string[]>>({});
/**
 * What the last file activity said about each checkout: how many times, and which paths.
 *
 * The two travel together because the paths are what make the count actionable. A count alone
 * says that something was written in the checkout, and a document whose own bytes come back
 * unchanged cannot answer from that whether one of the figures it draws is the file that moved.
 */
const documentRefreshActivity = ref<Record<string, { revision: number; paths: string[] }>>({});
/** Hoisted so the pane is handed the same array when nothing has moved, rather than a new one on
 *  every repaint of the shell. */
const NO_REFRESH_PATHS: string[] = [];
let unlistenFileActivity: (() => void) | undefined;
let activityListenerDisposed = false;
let unlistenCloseRequested: (() => void) | undefined;
let unlistenWindowResized: (() => void) | undefined;
/** Linux draws no frame of its own, so the title bar carries the window's controls there. */
const windowDecorated = ref(true);
const windowMaximized = ref(false);
let allowWindowClose = false;
let windowClosePromise: Promise<void> | null = null;
let inspectorCloseTimer: number | undefined;
const loadedCheckoutUiIds = new Set<string>();
const pendingCheckoutUiPatches = new Map<string, Partial<CheckoutUiState>>();
let checkoutUiLoadGeneration = 0;
const lifecycleCheckout = computed<Checkout | null>(
  () =>
    workspace.value.repos
      .flatMap((repo) => repo.checkouts)
      .find((checkout) => checkout.id === lifecycle.value?.checkoutId) ?? null,
);
const lifecycleRepo = computed(
  () =>
    workspace.value.repos.find((repo) =>
      repo.checkouts.some((checkout) => checkout.id === lifecycle.value?.checkoutId),
    ) ?? null,
);
const activeMainView = computed(() => resolveMainView(mainViews.value, activeCheckout.value?.id ?? null));
/**
 * The terminal the window is on, and the one answer every reader of the selection shares.
 *
 * The sidebar row, the main panel and the titlebar menu are the same terminal drawn three times, so
 * they are handed `resolveActiveSession` rather than `activeSessionId` on its own: what is stored is
 * cleared whenever the selection stops naming anything (a close, another workdir), and a reader that
 * answered from the raw id showed nothing while the panel was showing a terminal. `activeSessionId`
 * is one input to that rule, not the rule.
 */
const activeSession = computed(() => resolveActiveSession(activeCheckout.value, workspace.value.activeSessionId));
/** What the sidebar row and the main pane are handed, so both draw the terminal this resolves to. */
const activeSessionId = computed(() => activeSession.value?.id ?? null);
/**
 * The open session under the name the sidebar row gives it.
 *
 * The crumb and the row list the same terminals, so both ask `sessionTitle` for the name: what the
 * PTY last set, the program in front of the shell, or the name it was opened with. A crumb reading
 * the stored name alone said `zsh` over a terminal running something else.
 */
const activeSessionTitle = computed(() => {
  const session = activeSession.value;
  return session ? sessionTitle(session, sessionRuntimeStatuses.value[session.id]) : null;
});
/** What the last crumb names. A file or a change set is not a session, so it is named as itself. */
const activeViewLabel = computed(() => mainViewLabel(activeMainView.value, activeSessionTitle.value));
/**
 * Whether there is a last crumb at all.
 *
 * It names what the panel is showing, so a workdir with no terminal has nothing to name there:
 * the crumb would read "Terminal" over a panel that says it has none. The file and change set
 * views always have a name, so only the empty terminal leaves the line without its last step.
 */
const hasItemCrumb = computed(() => activeMainView.value.kind !== "terminal" || activeSession.value !== null);
/**
 * The last crumb of a file, in the steps it is made of.
 *
 * A path is already a line of crumbs, so it is drawn as one: the directories and the file name
 * with the same separator the rest of the line uses, instead of one run of text with slashes
 * buried in it. A label that is not a path (a session, the whole change set) is one step.
 */
const pathSteps = computed(() => activeViewLabel.value.split("/").filter(Boolean));

const lineEl = ref<HTMLElement | null>(null);
const pathCrumbEl = ref<HTMLElement | null>(null);
const pathProbeEl = ref<HTMLElement | null>(null);
const pathElided = ref(false);
/**
 * The path as the crumb draws it: every step, or the two ends with a rule where the rest was.
 *
 * One list either way, so the separators, their colour and their space are the same code for
 * the whole shape: the elided crumb is not a different crumb, it is the same crumb with less
 * in it, and the two ends are what the reader came for.
 */
const drawnSteps = computed(() => {
  if (!pathElided.value) return pathSteps.value;
  const first = pathSteps.value[0]!;
  const last = pathSteps.value.at(-1)!;
  return first === last ? [first] : [first, "…", last];
});

/**
 * Whether the whole path fits in the room the line still has for it.
 *
 * The line is bounded by a share of the window, so the room is that share minus everything the
 * path does not get to keep: the workdir, the branch, their glyphs and the separators. The probe
 * is the path at its natural width and it is always mounted, so this weighs two numbers that do
 * not move when the shape on screen does. Otherwise an elided path would measure itself as the
 * one that fitted and the line could never come back.
 *
 * Only a path with something in the middle can lose it, and the two ends are what stay: the
 * first directory says where you are, the file name is what you came to see.
 */
function measurePath() {
  const line = lineEl.value;
  const crumb = pathCrumbEl.value;
  const probe = pathProbeEl.value;
  if (!line || !crumb || !probe) return;
  const style = window.getComputedStyle(line);
  const header = line.parentElement?.clientWidth ?? 0;
  const share = style.maxWidth.endsWith("%")
    ? (parseFloat(style.maxWidth) / 100) * header
    : parseFloat(style.maxWidth) || header;
  const others = [...line.children].reduce(
    (width, child) =>
      child === crumb || child === probe ? width : width + child.getBoundingClientRect().width / appZoom.value,
    0,
  );
  const gaps = (line.childElementCount - 1) * (parseFloat(style.columnGap) || 0);
  pathElided.value = pathSteps.value.length > 2 && probe.scrollWidth > share - others - gaps;
}

// The path is weighed after the DOM settles, and again whenever the window changes, because the
// room it has is what the window and the rest of the line leave it.
watch(
  () => [activeViewLabel.value, activeCheckout.value?.id],
  () => void nextTick(measurePath),
);

/**
 * The three crumb menus: the workdir line of the titlebar is the sidebar read sideways.
 *
 * Each crumb names one level of it (the workdir, its branch or worktree, and the item open
 * inside it) and each opens the list of the level it names. What is not in this window is a
 * matter of record, not of guessing, so a workdir whose directory is gone is listed dimmed
 * and cannot be picked.
 */
const workdirMenu = computed<TitlebarMenuSection[]>(() => {
  const open = allCheckouts.value;
  const openPaths = new Set(open.map((checkout) => checkout.canonicalPath));
  // A folder already open in this window belongs to This Window, not to the recents that
  // remember it: the same workdir listed twice would be two ways into one place.
  const recents: TitlebarMenuItem[] = recentPaths.value
    .filter((recent) => !openPaths.has(recent.canonicalPath))
    .map((recent) => ({
      id: `recent:${recent.canonicalPath}`,
      label: recent.canonicalPath.split(/[\\/]/).at(-1) || recent.canonicalPath,
      hint: recent.canonicalPath,
      title: recent.canonicalPath,
      run: () => void openPath(recent.canonicalPath),
    }));
  return [
    {
      kind: "group",
      label: "This Window",
      items: workspace.value.repos.flatMap((repo) =>
        repo.checkouts.map((checkout) => workdirItem(repo, checkout, `workdir:${checkout.id}`, repo.name)),
      ),
    },
    ...(recents.length ? [{ kind: "group" as const, label: "Recent Projects", items: recents }] : []),
    { kind: "separator" },
    { kind: "list", items: [{ id: "open-directory", label: "Open directory", pinned: true, run: chooseFolder }] },
  ];
});

/** The worktrees of the repo the active workdir belongs to, and the one that makes another. */
const worktreeMenu = computed<TitlebarMenuSection[]>(() => {
  const repo = activeRepo.value;
  if (repo?.kind !== "git") return [];
  // The worktree is added to the repo's root, wherever the active workdir is, so the action
  // names the root and the dialog resolves its defaults from it.
  const root = repo.checkouts.find((checkout) => checkout.isPrimary);
  return [
    {
      kind: "group",
      label: "Worktrees",
      // A branch names one worktree, so nothing has to tell two rows apart here: the path is
      // in the row's tooltip, where it does not cost the name its width.
      items: repo.checkouts.map((checkout) => workdirItem(repo, checkout, `worktree:${checkout.id}`)),
    },
    { kind: "separator" },
    {
      kind: "list",
      items: root
        ? [
            {
              id: "new-worktree",
              label: "New worktree",
              pinned: true,
              run: () => openWorktreeDialog("create", root.id),
            },
          ]
        : [],
    },
  ];
});

/** The subitems of the active workdir: the terminals it has open, and how to start another. */
const terminalMenu = computed<TitlebarMenuSection[]>(() => {
  const checkout = activeCheckout.value;
  if (!checkout) return [];
  const sessions: TitlebarMenuItem[] = checkout.sessions.map((session) => {
    // The rows are named the way the sidebar names them, so the menu of the open terminal and the
    // row that opened it cannot say two different things about the same session.
    const name = sessionTitle(session, sessionRuntimeStatuses.value[session.id]);
    return {
      id: session.id,
      label: name,
      title: name,
      checked: session.id === activeSessionId.value,
      run: () => void activateTerminalSession(session.id),
    };
  });
  return [
    { kind: "group", label: "Terminals", items: sessions },
    { kind: "separator" },
    {
      kind: "list",
      items: [{ id: "new-terminal", label: "New terminal", pinned: true, run: () => requestShell(checkout.id) }],
    },
  ];
});

/** What the workdir crumb says: the repo, or the folder when there is no repo to name. */
const workdirLabel = computed(() => activeRepo.value?.name ?? activeCheckout.value?.path.split(/[\\/]/).at(-1) ?? "");
const homePath = ref("");
void Promise.resolve(homeDir())
  .then((path) => {
    if (typeof path === "string") homePath.value = path;
  })
  .catch(() => undefined);

/**
 * The file the last crumb names, when what is open is a file or a change.
 *
 * Its glyph is the one the inspector's file rows give it, so the crumb and the row that opened it
 * cannot say two different things about the same file, and it sits on the step that names the
 * file rather than at the head of the path, which names directories. The whole change set names
 * no file, and a terminal names none either, so the crumb wears no icon in either case.
 */
const itemCrumbFile = computed(() => {
  const view = activeMainView.value;
  if (view.kind === "terminal") return null;
  const name = view.path?.split(/[\\/]/).at(-1);
  return name ? { name, kind: "file" as const } : null;
});

/** One crumb menu at a time: the name it reports becomes the one that is open. */
function setCrumbOpen(name: string, open: boolean) {
  openCrumb.value = open ? name : null;
}

function workdirItem(repo: Repo, checkout: Checkout, id: string, hint?: string): TitlebarMenuItem {
  return {
    id,
    label: workdirTitle(repo, checkout),
    ...(hint !== undefined && { hint }),
    title: displayCheckoutPath(checkout.canonicalPath, homePath.value),
    checked: checkout.id === workspace.value.activeCheckoutId,
    disabled: checkout.isMissing,
    run: () => void activateCheckoutTerminal(checkout.id),
  };
}
const activeCheckoutUiState = computed(() => {
  const checkoutId = activeCheckout.value?.id;
  return checkoutId ? checkoutUiStates.value[checkoutId] : undefined;
});

/**
 * Shows one view in the main panel and saves it as this checkout's restored view.
 *
 * `giveBackToTerminal` is the close button's own claim on that first refusal. A split layout keeps
 * the terminal on screen beside the preview, so a row in the sidebar asking for the terminal is
 * answered by leaving the preview alone; the close button is the reader saying the preview is done
 * with, and it takes the whole panel back to the terminal whether or not one was already showing.
 */
function showView(checkoutId: string, view: MainView, { giveBackToTerminal = false, navigation = true } = {}) {
  const previous = mainViews.value[checkoutId];
  if (
    !giveBackToTerminal &&
    isSplitLayout.value &&
    view.kind === "terminal" &&
    previous &&
    previous.kind !== "terminal"
  )
    return;
  if (navigation) noteTerminalNavigation();
  mainViews.value = { ...mainViews.value, [checkoutId]: view };
  updateCheckoutUiState(checkoutId, {
    ...mainViewToState(view, checkoutId),
    // A different file starts at the top: the offsets belong to what was read.
    ...(viewPath(previous) !== viewPath(view) && { documentScrollTop: 0, documentScrollLeft: 0, diffScrollTop: 0 }),
  });
}

function viewPath(view: MainView | undefined): string | null {
  if (!view || view.kind === "terminal") return null;
  return view.kind === "document" ? `${view.origin}\0${view.path}` : view.path;
}

/**
 * The close button on a file or a change set: the main panel goes back to the terminal.
 *
 * The session it returns to is the one the panel already had, which `activeSession` resolves the
 * same way the rest of the window does — the selected session, or the newest one this checkout
 * has. Nothing is started here: a checkout that has no terminal lands on the pane's own empty
 * state, which is the honest answer for a workdir nobody has opened a terminal in yet.
 */
function closePreview(checkoutId: string) {
  if (mainViews.value[checkoutId]?.kind === "terminal") return;
  showView(checkoutId, { kind: "terminal", sessionId: activeSessionId.value }, { giveBackToTerminal: true });
}

function openFileDocument(selection: { checkoutId: string; path: string }) {
  showView(selection.checkoutId, {
    kind: "document",
    path: selection.path,
    mode: isMarkdownPath(selection.path) || mediaKind(selection.path) !== null ? "view" : "code",
    origin: "checkout",
  });
}

function openChangedDocument(selection: { checkoutId: string; path: string }) {
  showView(selection.checkoutId, { kind: "diff", path: selection.path });
}

function openAllChanges(selection: { checkoutId: string }) {
  showView(selection.checkoutId, { kind: "diff", path: null });
}

function setDocumentMode(mode: DocumentMode) {
  const checkoutId = activeCheckout.value?.id;
  const view = activeMainView.value;
  if (!checkoutId || view.kind !== "document") return;
  showView(checkoutId, { ...view, mode });
}

async function activateCheckoutTerminal(checkoutId: string, hasChanges = false) {
  noteTerminalNavigation({ activeCheckoutId: checkoutId, activeSessionId: null });
  // A row that shows `+N -N` is advertising a diff, so picking it opens the whole change set
  // for that workdir rather than whatever view it had saved. A row with nothing changed keeps
  // restoring its saved view, and no terminal is started either way.
  if (!hasChanges) {
    void selectCheckout(checkoutId);
    return;
  }
  await selectCheckout(checkoutId);
  showView(checkoutId, { kind: "diff", path: null });
}

async function activateTerminalSession(sessionId: string) {
  const checkout = allCheckouts.value.find((item) => item.sessions.some((session) => session.id === sessionId));
  if (!checkout) return;
  noteTerminalNavigation({ activeCheckoutId: checkout.id, activeSessionId: sessionId });
  await selectWorkspaceSession(sessionId);
  if (!isSplitLayout.value) showView(checkout.id, { kind: "terminal", sessionId });
  else if (loadedCheckoutUiIds.has(checkout.id) && mainViews.value[checkout.id]?.kind === "terminal") {
    showView(checkout.id, { kind: "terminal", sessionId });
  }
  await nextTick();
  mainPane.value?.focusActiveTerminal();
}

/**
 * A web link in the document goes to the browser, which is the only place one can be opened.
 *
 * The preview's own links stay in the preview: a relative path names a file in this checkout and
 * goes to `openFileDocument`. A refusal from the other end is reported rather than swallowed,
 * because a link that was not opened and a link that was is not something a person can tell from
 * looking at the screen.
 */
function openExternalUrl(url: string) {
  void requestExternalUrl(url).catch(reportCause);
}

/**
 * ctrl+click in the Files tree goes to whatever application this machine has for the file.
 *
 * A refusal is reported rather than swallowed, for the reason the URL above gives: a row that did
 * not open and a row that opened something look identical from here, and the person who pressed
 * the key is the only one who can tell.
 */
function openExternalFile(selection: { checkoutId: string; path: string }) {
  void requestExternalFile(selection.checkoutId, selection.path).catch(reportCause);
}

function toggleLayoutMode() {
  appLayout.value = { ...appLayout.value, mode: appLayout.value.mode === "split" ? "focus" : "split" };
}

function cancelSplitInspectorClose() {
  if (inspectorCloseTimer !== undefined) window.clearTimeout(inspectorCloseTimer);
  inspectorCloseTimer = undefined;
}

function openSplitInspector() {
  if (!isSplitLayout.value) return;
  cancelSplitInspectorClose();
  splitInspectorOpen.value = true;
}

/**
 * The drawer closes only once the pointer is over neither the strip nor the drawer.
 *
 * The two live in different subtrees, so moving between them fires a leave on one and an
 * enter on the other. A timer alone would depend on the enter landing after the leave;
 * tracking where the pointer is makes the close a fact about the pointer instead.
 */
function scheduleSplitInspectorClose() {
  if (!isSplitLayout.value) return;
  if (pointerOverSplitStrip.value || pointerOverSplitDrawer.value) return;
  cancelSplitInspectorClose();
  inspectorCloseTimer = window.setTimeout(() => {
    inspectorCloseTimer = undefined;
    splitInspectorOpen.value = false;
  }, 300);
}

function enterSplitStrip() {
  pointerOverSplitStrip.value = true;
  openSplitInspector();
}

function leaveSplitStrip() {
  pointerOverSplitStrip.value = false;
  scheduleSplitInspectorClose();
}

function enterSplitDrawer() {
  // The narrow focus drawer is this same element without the split layout, and a pointer over it
  // there says nothing about a strip that does not exist. A flag left set is a drawer that cannot
  // be closed: the close is refused while either is set, and a hidden drawer fires no leave to
  // clear it. So the drawer opened for one arrangement stays up over the next.
  if (!isSplitLayout.value) return;
  pointerOverSplitDrawer.value = true;
  openSplitInspector();
}

function leaveSplitDrawer() {
  pointerOverSplitDrawer.value = false;
  scheduleSplitInspectorClose();
}

function closeSplitInspector() {
  cancelSplitInspectorClose();
  pointerOverSplitStrip.value = false;
  pointerOverSplitDrawer.value = false;
  splitInspectorOpen.value = false;
}

/**
 * ⌘ on macOS and Ctrl everywhere else, which is the modifier every app shortcut here is pressed
 * with — and the only one it answers, because on a Mac `Ctrl` is a key the terminal already has.
 */
function isAppShortcut(event: KeyboardEvent) {
  return carriesAppModifier(event) && !event.altKey;
}

/**
 * Whether a key is the one that spells `/`, wherever the layout puts it.
 *
 * `key` is not enough on its own, and the reason is specific to the modifier these shortcuts are
 * pressed with: with ⌘ held down macOS hands the webview the character of the key *without* shift,
 * so a layout whose `/` lives on the number row arrives as a plain `7`. With Ctrl the same
 * keystroke arrives as `/`, which is what makes it a property of the modifier and not of the key.
 *
 * So the position is the shortcut, and the character is only how one layout spells it.
 *
 * Matching on position is deliberately blind to the layout, so it is wider than the shortcut: on a
 * US keyboard ⌘⇧7 types `&`, and this answers for that too. The cost is one chord that means `&`
 * where it means `/` elsewhere, and it is paid knowingly — demanding the character narrows the
 * shortcut to the layouts that need no narrowing and loses the one that does. `App.test.ts` pins
 * both halves: ⌘⇧7 opens the panels, and a bare number 7 without the shift is not captured.
 */
function isSlashKey(event: KeyboardEvent) {
  return event.key === "/" || event.code === "Slash" || (event.code === "Digit7" && event.shiftKey);
}

/**
 * The app shortcut a key asks for, or nothing at all for a key that asks for none.
 *
 * The panels are on `⌘/`, and what makes that chord answerable is only that nothing in the menu
 * bar takes it: `menus.rs` builds the app menu and an Edit submenu of undo, redo, cut, copy,
 * paste and select all, and not one of those items carries `⌘/`, so it arrives at the webview
 * whole. A chord a menu *does* carry never arrives — `⌘H` is the app menu's Hide item and `⌘Esc`
 * is the system's own cancel, both of which resolve before the webview is told about the key.
 */
function appShortcutFor(event: KeyboardEvent): "panels" | "terminal" | undefined {
  if (isSlashKey(event)) return "panels";
  if (event.key.toLowerCase() === "n") return "terminal";
  return undefined;
}

/**
 * Every shortcut the window answers itself: the zoom, the side panels and a new terminal.
 *
 * This one listens on capture, which is the whole reason it works: by the time a keydown reaches
 * the window on its way up, xterm has already turned it into input, and a shell reading ⌘N as
 * downcase-word is exactly what a sidebar shortcut must not do.
 * Stopping the key here means neither the terminal nor the editor ever sees it, whatever had
 * the focus.
 */
function onAppKeydown(event: KeyboardEvent) {
  if (!isAppShortcut(event)) return;
  if (!appLayoutReady.value || settingsOpen.value) return;
  const direction = zoomKeyFor(event);
  const shortcut = appShortcutFor(event);
  if (direction === undefined && shortcut === undefined) return;
  // The webview is told the scale, and it does nothing with a `0` on its own, but the key is
  // stopped here anyway: a shortcut that also reaches the terminal as input is a shortcut that
  // types into whatever had the focus.
  event.preventDefault();
  event.stopPropagation();
  if (shortcut === "panels") {
    toggleSidePanels();
    return;
  }
  if (shortcut === "terminal") {
    requestTerminalInActiveWorkdir();
    return;
  }
  // Neither the panels key nor N is a zoom key, so a shortcut that got this far is a zoom request.
  if (direction !== undefined) setZoom(zoomStep(appZoom.value, direction), event.metaKey ? "Cmd" : "Ctrl");
}

/** Takes both side panels away, or gives them back. */
function toggleSidePanels() {
  sidePanelsVisible.value = !sidePanelsVisible.value;
  const show = sidePanelsVisible.value;
  sidebarPanel.value?.[show ? "expand" : "collapse"]();
  // The drawer is the one thing the shortcut can put away without any pointer being over it, and
  // a hidden drawer fires no leave to correct the flags that say one is. Those flags refuse every
  // later close, so the drawer would come back unable to close. Restoring the panels should not
  // find it already open either, which is why this runs in both directions.
  closeSplitInspector();
  // The inspector panel is not sized here: the watcher below answers the flag this just set, so
  // the two cannot disagree about a panel holding the width of a drawer that is not drawn.
}

/**
 * A terminal in the workdir the window is on, and in Home when there is none.
 *
 * The Home checkout is the one the app records for the user's own directory at startup, so a
 * window with no workdir open still answers the key with a shell rather than ignoring it. Which
 * directory it lands in is the checkout's own, so nothing here has to know what Home is.
 */
function requestTerminalInActiveWorkdir() {
  const checkoutId = activeCheckout.value?.id ?? workspace.value.homeCheckoutId;
  if (!checkoutId) return;
  void requestShell(checkoutId);
}

let zoomToastId: number | undefined;

/**
 * The one line that says where the window is scaled to, replaced rather than stacked.
 *
 * `push` folds away a message identical to the one before it and every step of a held key is a
 * different number, so without taking the last one down first, holding the key leaves the last
 * three percentages on screen as a history of the gesture instead of the current one.
 */
function announceZoom(zoom: Zoom, modifier: ZoomModifier) {
  if (zoomToastId !== undefined) dismissToast(zoomToastId);
  zoomToastId = pushToast(zoomLabel(zoom, modifier), "info");
}

function setZoom(zoom: Zoom, modifier: ZoomModifier) {
  // Already there: the key did something, so there is nothing to say about it.
  if (zoom === appZoom.value) return;
  // The scale responds immediately, but only a saved step is announced as the new preference.
  void applySettings({ ...settings.value, ui: { ...settings.value.ui, zoom } }).then((saved) => {
    if (saved) announceZoom(zoom, modifier);
  });
}

/** The last set the file is known to carry, which is what a rejected write falls back to. */
const savedSettings = ref<AppSettings>(cloneSettings(DEFAULT_SETTINGS));
let settingsWrite: Promise<boolean> = Promise.resolve(true);
let settingsRevision = 0;

/**
 * A preference takes effect at once and is written behind it, with the file as the arbiter.
 *
 * It cannot wait for the write before drawing: the window's scale is a gesture, and a gesture
 * whose next step is computed from a value that has not landed yet is a gesture that only moves
 * once per release. So the setting is adopted immediately and the write follows, queued rather
 * than raced. Two presses in a row have to reach the file in the order they were made, or the
 * older set lands last and the next launch opens at the wrong size.
 *
 * A rejected write puts the window back to what the file does carry and says why. That is what
 * makes Cancel in the dialog honest: there is never a state the window is showing and the file
 * does not have, so there is nothing to roll back.
 */
function applySettings(next: AppSettings): Promise<boolean> {
  const normalized = normalizeSettings(next);
  const revision = ++settingsRevision;
  settings.value = normalized;
  settingsWrite = settingsWrite
    .catch(() => false)
    .then(async () => {
      try {
        await saveSettings(normalized);
      } catch (cause) {
        reportCause(cause);
        // A newer preference may already be queued. Only the newest failed write may roll back
        // the optimistic value; an older failure must not erase a later intent.
        if (revision === settingsRevision) settings.value = cloneSettings(savedSettings.value);
        return false;
      }
      savedSettings.value = cloneSettings(normalized);
      return true;
    });
  return settingsWrite;
}

function restoreSettingsFocus() {
  void nextTick(() => settingsButton.value?.focus());
}

function closeSettings() {
  if (settingsSaving.value) return;
  settingsOpen.value = false;
  restoreSettingsFocus();
}

/**
 * Where exported reviews go for the mode the row is showing, and what is in there.
 *
 * Asked of the backend every time rather than held: the folder is on disk and a round exported
 * from another window lands in it, so a number held across the session is the one thing on that
 * row that could be wrong.
 */
const reviewFolder = ref<ReviewFolder | null>(null);
let reviewFolderRequest = 0;
const reviewFolderStorage = ref<ReviewStorageMode>("default");

async function loadReviewFolder(storage: ReviewStorageMode) {
  const request = ++reviewFolderRequest;
  reviewFolderStorage.value = storage;
  const checkoutId = activeCheckout.value?.id;
  reviewFolder.value = null;
  if (!checkoutId) return;
  try {
    const folder = await getReviewFolder(checkoutId, storage);
    if (request === reviewFolderRequest && settingsOpen.value && activeCheckout.value?.id === checkoutId) {
      reviewFolder.value = folder;
    }
  } catch {
    // A folder that cannot be read is not a reason to refuse the dialog. Ignore stale failures too.
    if (request === reviewFolderRequest) reviewFolder.value = null;
  }
}

watch(
  settingsOpen,
  (open) => {
    if (open) return;
    reviewFolderRequest += 1;
    reviewFolder.value = null;
  },
  { flush: "sync" },
);

watch(
  () => activeCheckout.value?.id,
  () => {
    reviewFolderRequest += 1;
    reviewFolder.value = null;
    if (settingsOpen.value) void loadReviewFolder(reviewFolderStorage.value);
  },
  { flush: "sync" },
);

function openSettings() {
  settingsOpen.value = true;
  void loadReviewFolder(settings.value.reviews.storage);
}

/**
 * Deletes the exported reviews in the app's own folder.
 *
 * It is asked about rather than done: the folder is in the person's home and a clear cannot be
 * taken back, so the question names the folder and says what it leaves alone.
 */
function requestClearReviewFolder(storage: ReviewStorageMode) {
  const folder = reviewFolder.value;
  const checkoutId = activeCheckout.value?.id;
  if (storage !== "default" || folder?.storage !== storage || !checkoutId) return;
  askConfirm({
    title: "Delete exported review notes",
    message: `${folder.files} exported ${folder.files === 1 ? "note" : "notes"} (${formatBytes(folder.bytes)}) in ${folder.path} are deleted. Anything else in that folder is left alone.`,
    confirmLabel: "Delete",
    destructive: true,
    run: async () => {
      const cleared = await clearReviewFolder(checkoutId, folder.path);
      pushToast(
        `Deleted ${cleared.files} exported ${cleared.files === 1 ? "note" : "notes"} (${formatBytes(cleared.bytes)}).`,
        "info",
      );
      await loadReviewFolder(reviewFolderStorage.value);
    },
  });
}

/** The dialog's own Apply: the one place that decides whether the dialog closes. */
async function applyFromDialog(next: AppSettings) {
  settingsSaving.value = true;
  try {
    if (await applySettings(next)) {
      settingsOpen.value = false;
      restoreSettingsFocus();
    }
  } finally {
    settingsSaving.value = false;
  }
}

/**
 * The interface's own sizes are a ratio of one number rather than of the root's font size, because
 * everything here is drawn in pixels: a base font size would move the padding and the icons with
 * the text, and what a person changes in Settings is the text.
 */
const fontScale = computed(() => uiFontScale(settings.value.ui.fontSize));

watch(
  fontScale,
  (scale) => {
    document.documentElement.style.setProperty("--muster-ui-font-scale", String(scale));
  },
  { immediate: true },
);

/**
 * The palette the window is drawn in, which is the preference resolved: `system` asks the operating
 * system, and the answer changes while the window is open, so this follows it rather than asking
 * once. The attribute is the whole of it for everything a stylesheet paints (the two palettes in
 * `src/muster.css` are keyed off it) and `theme` carries the answer to the two things CSS cannot
 * reach, the terminal and the diff view.
 *
 * It is asked for and applied while `settings` still holds the defaults, because a first paint in
 * the wrong palette is a frame of a theme the person did not choose.
 */
function applyTheme() {
  const preference = settings.value.ui.theme;
  const resolved: Theme = preference === "system" ? (systemPrefersDark.matches ? "dark" : "light") : preference;
  theme.value = resolved;
  document.documentElement.dataset.theme = resolved;
  if (resolved === "dark") {
    document.documentElement.style.setProperty("--muster-content-bg-0", settings.value.ui.contentBackground);
  } else {
    document.documentElement.style.removeProperty("--muster-content-bg-0");
  }
}

const systemPrefersDark = window.matchMedia("(prefers-color-scheme: dark)");

watch(() => [settings.value.ui.theme, settings.value.ui.contentBackground], applyTheme, { immediate: true });
systemPrefersDark.addEventListener("change", applyTheme);

/**
 * The window is scaled from the root element, so everything inside it is laid out in the space a
 * window of this size has before the scale and painted at the scale after it. Two things follow
 * from that, and both of them are read from here: the viewport a breakpoint is measured against
 * is `zoom` times wider than the space the app is arranging, and a pointer reports where it is in
 * the same widened units.
 */
function applyZoom(zoom: number) {
  document.documentElement.style.setProperty("zoom", String(zoom));
  viewportWidth.value = window.innerWidth / zoom;
}

watch(appZoom, applyZoom, { immediate: true });

/**
 * reka-ui measures its panels with `getBoundingClientRect()` and reports them in the same units,
 * which are the units of the window rather than the units the layout is written in. The two are
 * the same number at 100% and at nothing else, so a width is scaled on its way into the splitter
 * and unscaled on its way back out, and the number in the saved layout keeps meaning what it says.
 */
const toScreen = (px: number) => px * appZoom.value;
const toLayout = (px: number) => px / appZoom.value;

type CheckoutUiScrollField =
  "filesScrollTop" | "changesScrollTop" | "documentScrollTop" | "documentScrollLeft" | "diffScrollTop";
const CHECKOUT_UI_SCROLL_FIELDS = new Set<CheckoutUiScrollField>([
  "filesScrollTop",
  "changesScrollTop",
  "documentScrollTop",
  "documentScrollLeft",
  "diffScrollTop",
]);

function updateCheckoutUiState(checkoutId: string, patch: Partial<CheckoutUiState>) {
  const previous = checkoutUiStates.value[checkoutId] ?? { ...DEFAULT_CHECKOUT_UI_STATE };
  const patchKeys = Object.keys(patch);
  const scrollOnly =
    patchKeys.length > 0 && patchKeys.every((key) => CHECKOUT_UI_SCROLL_FIELDS.has(key as CheckoutUiScrollField));
  let state: CheckoutUiState;
  if (scrollOnly) {
    // The map only holds the default, normalized IPC state, or a normalized result from this function.
    // Validate new scroll values with the same rules without rebuilding its accepted directory paths.
    const validated = normalizeCheckoutUiState(patch);
    state = { ...previous };
    for (const key of patchKeys as CheckoutUiScrollField[]) state[key] = validated[key];
  } else {
    state = normalizeCheckoutUiState({ ...previous, ...patch });
  }
  checkoutUiStates.value = { ...checkoutUiStates.value, [checkoutId]: state };
  if (!loadedCheckoutUiIds.has(checkoutId)) {
    pendingCheckoutUiPatches.set(checkoutId, { ...pendingCheckoutUiPatches.get(checkoutId), ...patch });
  }
  if (checkoutUiReady.value && loadedCheckoutUiIds.has(checkoutId)) scheduleCheckoutUiSave(checkoutId);
}

function updateDocumentReadingPosition(checkoutId: string, position: { top: number; left: number }) {
  updateCheckoutUiState(checkoutId, {
    documentScrollTop: Math.round(position.top),
    documentScrollLeft: Math.round(position.left),
  });
}

function updateDiffReadingPosition(checkoutId: string, top: number) {
  updateCheckoutUiState(checkoutId, { diffScrollTop: Math.round(top) });
}

function updateInspectorUiState(
  checkoutId: string,
  patch: Pick<
    CheckoutUiState,
    | "inspectorTab"
    | "selectedFilePath"
    | "selectedChangePath"
    | "expandedDirectories"
    | "filesScrollTop"
    | "changesScrollTop"
  >,
) {
  updateCheckoutUiState(checkoutId, patch);
}

/**
 * Waits for the settings writes already asked for, and for any that arrived while waiting.
 *
 * The window stays alive for this, so a settings action made during the wait belongs to this close
 * rather than to a window that is about to be gone.
 */
async function settleSettingsWrites() {
  let revision: number;
  do {
    revision = settingsRevision;
    await settingsWrite;
  } while (revision !== settingsRevision);
}

function onSplitterLayout(sizes: number[]) {
  if (!appLayoutReady.value || sizes.length < 3) return;
  let next = appLayout.value;
  if (sizes[0] > 0) next = resizeLayoutPanel(next, "sidebar", toLayout(sizes[0]));
  if (!inspectorInDrawer.value && sizes[2] > 0) next = resizeLayoutPanel(next, "inspector", toLayout(sizes[2]));
  if (next.sidebarWidth !== appLayout.value.sidebarWidth || next.inspectorWidth !== appLayout.value.inspectorWidth) {
    appLayout.value = next;
  }
}

function resetPanelWidth(panel: "sidebar" | "inspector") {
  const width = panel === "sidebar" ? DEFAULT_APP_LAYOUT.sidebarWidth : DEFAULT_APP_LAYOUT.inspectorWidth;
  (panel === "sidebar" ? sidebarPanel.value : inspectorPanel.value)?.resize(toScreen(width));
  appLayout.value = resizeLayoutPanel(appLayout.value, panel, width);
}

function resizeAppPreview(width: number) {
  appLayout.value = resizeLayoutPanel(appLayout.value, "preview", width);
}

/**
 * The width the sidebar is drawn at, read while the arrangement is still the one on screen.
 *
 * reka-ui rebuilds the whole row from the widths the panels were mounted with as soon as any panel's
 * limits change, and it asks each panel for the width it was mounted with rather than the one being
 * drawn. A window too narrow for that width draws a narrower sidebar, so switching to the split
 * layout — which changes what the main panel needs — grows the sidebar into the space the main view
 * had. The width is read before the row changes and given back after, so the sidebar stays where
 * the reader left it and the main view keeps what it was given.
 */
let sidebarWidthOnScreen = 0;
watch(
  [isSplitLayout, () => (isSplitLayout.value ? activeMainView.value.kind : null), appZoom],
  () => {
    sidebarWidthOnScreen = toLayout(sidebarPanel.value?.$el.getBoundingClientRect().width ?? 0);
  },
  { flush: "pre" },
);

// A narrow window cannot hold the main panel and the inspector side by side, so the inspector
// floats over it as a drawer. Its width is left alone, to be restored when space returns. A panel
// ⌘/ took away is collapsed as well, so a window that grows back does not reserve room for a panel
// nobody can see.
//
// The layout mode is watched in its own right because `inspectorInDrawer` is already true on both
// sides of a switch between the two modes: a window narrow enough to need the drawer can reach the
// split layout without that flag ever changing, which leaves this never run and the panel holding
// the width of a drawer drawn over the main view. ⌘/ only sets `sidePanelsVisible` and lets this
// answer.
watch(
  [
    inspectorInDrawer,
    sidePanelsVisible,
    isSplitLayout,
    () => (isSplitLayout.value ? activeMainView.value.kind : null),
    appZoom,
    appLayoutReady,
  ],
  async () => {
    if (!appLayoutReady.value) return;
    await nextTick();
    if (inspectorInDrawer.value || !sidePanelsVisible.value) inspectorPanel.value?.collapse();
    else {
      inspectorPanel.value?.expand();
      inspectorPanel.value?.resize(toScreen(appLayout.value.inspectorWidth));
    }
    // The width the sidebar was drawn at before this change, given back to the row that has just
    // been rebuilt without it. It is read ahead of the change because afterwards the panel is
    // already the width the rebuild asked for.
    if (sidebarWidthOnScreen) {
      sidebarPanel.value?.resize(toScreen(sidebarWidthOnScreen));
      sidebarWidthOnScreen = 0;
    }
  },
  { immediate: true, flush: "post" },
);

// Either way the pointer is answering a question about an arrangement that no longer exists, and
// the flags that recorded it cannot be corrected by a leave: the strip and the drawer are different
// elements afterwards, so both directions have to start from the drawer closed.
watch(isSplitLayout, () => closeSplitInspector());

watch(
  () => activeCheckout.value?.id,
  async (checkoutId) => {
    const request = ++checkoutUiLoadGeneration;
    checkoutUiReady.value = false;
    if (!checkoutId) {
      checkoutUiReady.value = true;
      return;
    }
    if (loadedCheckoutUiIds.has(checkoutId)) {
      checkoutUiReady.value = true;
      return;
    }
    let state = { ...DEFAULT_CHECKOUT_UI_STATE };
    try {
      state = normalizeCheckoutUiState(await loadCheckoutUiState(checkoutId));
    } catch (cause) {
      if (request === checkoutUiLoadGeneration) {
        showWindowError(cause);
      }
    } finally {
      if (request === checkoutUiLoadGeneration) {
        state = normalizeCheckoutUiState({ ...state, ...pendingCheckoutUiPatches.get(checkoutId) });
        pendingCheckoutUiPatches.delete(checkoutId);
        checkoutUiStates.value = { ...checkoutUiStates.value, [checkoutId]: state };
        mainViews.value = { ...mainViews.value, [checkoutId]: mainViewFromState(state) };
        loadedCheckoutUiIds.add(checkoutId);
        checkoutUiReady.value = true;
      }
    }
  },
  { immediate: true, flush: "sync" },
);

onMounted(async () => {
  window.addEventListener("resize", onViewportResize);
  window.addEventListener("focus", onWindowFocus);
  window.addEventListener("keydown", onAppKeydown, true);
  void refreshCompatibilitySnapshot();
  const currentWindow = getCurrentWindow();
  try {
    unlistenCloseRequested = await currentWindow.onCloseRequested(async (event) => {
      if (allowWindowClose) return;
      event.preventDefault();
      await requestWindowClose(currentWindow);
    });
  } catch (cause) {
    showWindowError(cause);
  }
  try {
    // Whether the window has its own frame decides where the controls live, and it is the
    // only honest answer: a platform that draws one and a platform that does not are told
    // apart by the window rather than by a guess about the user agent.
    windowDecorated.value = await currentWindow.isDecorated();
    if (!windowDecorated.value) {
      windowMaximized.value = await currentWindow.isMaximized();
      unlistenWindowResized = await currentWindow.onResized(() => void readWindowMaximized());
    }
  } catch (cause) {
    showWindowError(cause);
  }
  try {
    appLayout.value = normalizeAppLayout(await loadAppLayout());
  } catch (cause) {
    reportCause(cause);
    appLayout.value = { ...DEFAULT_APP_LAYOUT };
  }
  try {
    settings.value = normalizeSettings(await loadSettings());
    savedSettings.value = cloneSettings(settings.value);
  } catch (cause) {
    // The file is either not there yet or is not something this build can read. Either way the
    // defaults are what the rest of the app draws with, and a broken file stays broken on disk
    // rather than being replaced by them the next time anything is applied.
    reportCause(cause);
  } finally {
    // Do not expose Settings or accept zoom shortcuts until the saved set is known.
    appLayoutReady.value = true;
  }
  try {
    appVersion.value = await getVersion();
  } catch (cause) {
    // One section of one dialog shows one line without it. The window knows nothing else that is
    // wrong, so this is reported and the About section is drawn without a version.
    reportCause(cause);
  }
  try {
    const dispose = await listen<CheckoutFileActivity[]>("checkout-file-activity", (event) => {
      // One watcher covers a whole repository, so a write in one worktree arrives naming the
      // worktree it landed in rather than the one that happened to be on screen.
      for (const activity of event.payload) {
        const current = documentRefreshActivity.value[activity.checkoutId]?.revision ?? 0;
        documentRefreshActivity.value = {
          ...documentRefreshActivity.value,
          [activity.checkoutId]: { revision: current + 1, paths: activity.paths },
        };
      }
    });
    if (activityListenerDisposed) dispose();
    else unlistenFileActivity = dispose;
  } catch {
    // File-write activity is optional; PTY foreground-process activity remains observable.
  }
});

onUnmounted(() => {
  activityListenerDisposed = true;
  unlistenCloseRequested?.();
  unlistenWindowResized?.();
  window.removeEventListener("resize", onViewportResize);
  window.removeEventListener("focus", onWindowFocus);
  // The capture flag is part of the registration: a listener removed without it is not the one
  // that was added, and this one would outlive the window it belongs to.
  window.removeEventListener("keydown", onAppKeydown, true);
  systemPrefersDark.removeEventListener("change", applyTheme);
  unlistenFileActivity?.();
  if (inspectorCloseTimer !== undefined) window.clearTimeout(inspectorCloseTimer);
});

function onWindowFocus() {
  void refreshCompatibilitySnapshot();
}

function onViewportResize() {
  viewportWidth.value = window.innerWidth / appZoom.value;
  measurePath();
}

function showWindowError(cause: unknown) {
  reportCause(cause);
}

function requestWindowClose(currentWindow: ReturnType<typeof getCurrentWindow>): Promise<void> {
  if (allowWindowClose) return Promise.resolve();
  if (windowClosePromise) return windowClosePromise;
  windowClosePromise = (async () => {
    try {
      // The question comes before any of the close work, because everything below it writes and
      // stops things: asking afterwards would be asking about a build that is already gone.
      if (!(await askAboutRunningProcesses())) return;
      // The window waits for the queued writes, but only for as long as that can reasonably
      // take. A window that cannot be closed is worse than a layout that is one launch stale, and
      // the write is on the other side of the bridge: it goes on after this stops waiting for it.
      // A close request leaves the webview alive until this finishes, so a settings action made
      // during the wait is included before the window is allowed to close.
      await withinDeadline(
        flushUiStateWrites().then(settleSettingsWrites),
        CLOSE_BUDGET,
        `The window closed before its queued writes finished (${CLOSE_BUDGET}ms).`,
        "ui_writes_deadline",
      );
      // The sweep runs while the window is still here to ask for it: an exit event the app could
      // clean up from is never delivered on this path, and a server left running holds its port.
      // Failing to sweep is not a reason to keep the window open, so it is not one.
      await sweepBeforeExit();
      allowWindowClose = true;
      await currentWindow.close();
    } catch (cause) {
      // Whatever the close did is undone together, so a failed one leaves the window open and
      // the next request tries again. The only failure here with no way back is one that keeps
      // the window from ever being asked again.
      allowWindowClose = false;
      showWindowError(cause);
    } finally {
      windowClosePromise = null;
    }
  })();
  return windowClosePromise;
}

/**
 * The question a close asks while one is open, and the answer it is waiting for.
 *
 * Its own dialog rather than `pendingConfirm`, because this one has to be awaited: the close below
 * continues on the answer rather than finishing on its own, so a second close request arriving
 * while the reader is still looking at it has to find this one rather than ask another.
 */
const closeQuestion = ref<{ title: string; message: string } | null>(null);
let closeAnswer: ((confirmed: boolean) => void) | null = null;

function answerCloseQuestion(confirmed: boolean) {
  const answer = closeAnswer;
  closeAnswer = null;
  closeQuestion.value = null;
  answer?.(confirmed);
}

/**
 * The terminals a close would stop, read fresh rather than believed.
 *
 * Every terminal this window created is asked about, not the ones on screen: a pane the user last
 * looked at is not the pane that has been building for twenty minutes, and only the created ones
 * exist to be asked. The status is read from the backend rather than taken from the poll, because
 * the poll is up to three quarters of a second old in both directions — warning about a build that
 * already finished is wrong, and staying quiet about one that started is worse.
 */
async function runningProcessTerminals(): Promise<{ name: string; program: string; where: string }[]> {
  const live = workspace.value.repos.flatMap((repo) =>
    repo.checkouts.flatMap((checkout) =>
      checkout.sessions
        .filter((session) => session.id in sessionRuntimeStatuses.value)
        .map((session) => ({ session, checkout, repo })),
    ),
  );
  const found = await Promise.all(
    live.map(async ({ session, checkout, repo }) => {
      try {
        const status = await getTerminalStatus(checkout.id, session.id);
        if (!terminalHasProcess(status)) return null;
        return {
          // The name the sidebar row gives this terminal, so the question and the row agree.
          name: sessionTitle(session, status),
          program: status.foregroundApp ?? "a process",
          where: workdirTitle(repo, checkout),
        };
      } catch (cause) {
        // A terminal whose status cannot be read is reported as still running rather than as safe.
        // The cost of the mistake is a question that was not needed; the cost of the other way
        // round is a build stopped without anyone being asked.
        showWindowError(cause);
        return { name: session.name, program: "a process", where: workdirTitle(repo, checkout) };
      }
    }),
  );
  return found.filter((entry): entry is { name: string; program: string; where: string } => entry !== null);
}

/**
 * What the question lists: as many as a reader can act on, and a count for the rest.
 *
 * Each entry is the terminal the way the sidebar names it, with the program that would stop beside
 * it — and without the program when it is the same word, which is what an unrenamed terminal is
 * called by the app itself. Two terminals can share a name, and a question naming the same word
 * twice tells the reader nothing about which is which, so a repeated name carries the worktree.
 */
function describeRunningProcesses(entries: { name: string; program: string; where: string }[]): string {
  const NAMED = 4;
  const named = entries
    .slice(0, NAMED)
    .map((entry) => {
      const where = entries.filter((other) => other.name === entry.name).length > 1 ? ` · ${entry.where}` : "";
      return `“${entry.name}${where}”${entry.name === entry.program ? "" : ` (${entry.program})`}`;
    })
    .join(", ");
  const rest = entries.length - NAMED;
  const one = entries.length === 1;
  return `${named}${rest > 0 ? ` and ${rest} more` : ""}. Closing the window stops ${one ? "it" : "them"}, and whatever ${one ? "it has" : "they have"} not written yet is lost.`;
}

/**
 * Asks before stopping anything, and answers yes on its own when there is nothing to stop.
 *
 * A refusal and a failure land the same way on purpose: both leave the window open, and neither is
 * silent. A window that cannot be closed is a bug, but a build that was killed without a word is
 * worse than both, so the question comes first and the close follows the answer rather than a
 * timer.
 */
async function askAboutRunningProcesses(): Promise<boolean> {
  if (closeQuestion.value) return false;
  let running: { name: string; program: string; where: string }[];
  try {
    running = await runningProcessTerminals();
  } catch (cause) {
    // The question could not be asked, which is not the same as there being nothing to lose. The
    // window stays open and says why, because a close nobody could confirm is not a close anybody
    // agreed to.
    showWindowError(cause);
    return false;
  }
  if (!running.length) return true;
  const one = running.length === 1;
  return new Promise<boolean>((resolve) => {
    closeAnswer = resolve;
    closeQuestion.value = {
      title: one ? "A process is still running" : `${running.length} processes are still running`,
      message: `${describeRunningProcesses(running)} The window stays open if you cancel.`,
    };
  });
}

/** How long a close waits for each thing it waits for before it stops waiting for it. */
const CLOSE_BUDGET = 5000;

/** Asks the backend to end the processes it started, and says so when it cannot. */
async function sweepBeforeExit(): Promise<void> {
  try {
    // Bounded for the same reason the writes are. A sweep that never answers is not a reason to keep
    // the window open: what it did not manage is swept on the next launch.
    await withinDeadline(
      prepareAppExit(),
      CLOSE_BUDGET,
      `The window closed before the app's own processes were ended (${CLOSE_BUDGET}ms).`,
      "exit_sweep_deadline",
    );
  } catch (cause) {
    reportCause(cause);
  }
}

const CLOSE_DIAGNOSTIC_BUDGET = 250;

/** Waits briefly for the categorized Rust log command, without holding close on a stalled bridge. */
function reportDiagnosticBeforeClose(category: DiagnosticCategory): Promise<void> {
  return new Promise((resolve) => {
    let deadline: number;
    const finish = () => {
      window.clearTimeout(deadline);
      resolve();
    };
    deadline = window.setTimeout(finish, CLOSE_DIAGNOSTIC_BUDGET);
    void reportFrontendDiagnostic(category).then(finish, finish);
  });
}

/**
 * Waits for `work`, or for the deadline, whichever lands first.
 *
 * The deadline resolves rather than rejects: the caller asked to close the window, and a write
 * that arrives late still arrives, so all that is left to say is that it was late.
 */
function withinDeadline(
  work: Promise<void>,
  milliseconds: number,
  late: string,
  category: DiagnosticCategory,
): Promise<void> {
  return new Promise((resolve, reject) => {
    let timedOut = false;
    const deadline = window.setTimeout(() => {
      timedOut = true;
      reportCause(new Error(late));
      void reportDiagnosticBeforeClose(category).then(resolve);
    }, milliseconds);
    work.then(
      () => {
        if (timedOut) return;
        window.clearTimeout(deadline);
        resolve();
      },
      (cause: unknown) => {
        if (timedOut) return;
        window.clearTimeout(deadline);
        reject(cause);
      },
    );
  });
}

/**
 * Reads back whether the window is maximized, so the control says what it will do.
 *
 * It is read rather than tracked because the window is not only maximized from here: the
 * desktop maximizes it on its own, and a control that still offered to maximize a maximized
 * window is worse than one that never changed.
 */
async function readWindowMaximized() {
  try {
    windowMaximized.value = await getCurrentWindow().isMaximized();
  } catch (cause) {
    showWindowError(cause);
  }
}

function minimizeWindow() {
  void getCurrentWindow().minimize().catch(showWindowError);
}

function toggleWindowMaximized() {
  void getCurrentWindow().toggleMaximize().then(readWindowMaximized).catch(showWindowError);
}

function closeWindow() {
  void requestWindowClose(getCurrentWindow());
}

function openWorktreeDialog(mode: "create" | "remove", checkoutId: string) {
  lifecycle.value = { mode, checkoutId };
}

/**
 * Takes a checkout whose directory is gone off the list, and nothing else.
 *
 * A repo root (the primary checkout of a Git repository, and the only checkout of a plain
 * folder) is registered as the head of a list, so closing one takes that list with it; a
 * worktree is an entry of its own. Muster deletes no files either way, so the confirmation
 * only has to name the scope.
 */
async function closeMissingCheckout(checkoutId: string) {
  const checkout = allCheckouts.value.find((item) => item.id === checkoutId);
  if (!checkout) return;
  const scope = checkout.isPrimary ? " and its checkout list" : "";
  if (!window.confirm(`Close “${checkout.path}”${scope} in Muster? No files will be deleted.`)) return;
  try {
    applyWorkspace(await persistMissingCheckoutClose(checkoutId));
  } catch (cause) {
    reportCause(cause);
  }
}

/**
 * Takes a workdir off the panel, and leaves the disk alone.
 *
 * This is the row's own action, so the confirmation has to say what the row cannot: nothing is
 * deleted, and the directory is still there to open. A repo root (the primary checkout of a
 * Git repository, and the only checkout of a plain folder) is registered as the head of a
 * list, so closing one takes that list with it, exactly as closing a missing checkout does.
 */
async function closeWorkdir(checkoutId: string) {
  const checkout = allCheckouts.value.find((item) => item.id === checkoutId);
  if (!checkout) return;
  const scope = checkout.isPrimary ? " and its checkout list" : "";
  const question = `Remove “${checkout.path}”${scope} from Muster? No files will be deleted, and opening the folder again brings it back.`;
  if (!window.confirm(question)) return;
  try {
    applyWorkspace(await persistCheckoutClose(checkoutId));
  } catch (cause) {
    reportCause(cause);
  }
}

/**
 * The one question the app has open, and what answering "yes" does.
 *
 * The action is held rather than run, because the question is the point: archiving a
 * worktree and putting a set of them back are both things a misclick would undo a
 * stranger's afternoon over, and neither is destructive enough to deserve a dialog of
 * its own the way removing a worktree from disk does.
 */
const pendingConfirm = ref<{
  title: string;
  message: string;
  confirmLabel: string;
  /** The questions whose answer cannot be taken back are drawn in the failure colour. */
  destructive?: boolean;
  run(): Promise<void>;
} | null>(null);

function askConfirm(question: NonNullable<typeof pendingConfirm.value>) {
  pendingConfirm.value = question;
}

async function answerConfirm(confirmed: boolean) {
  const question = pendingConfirm.value;
  if (!question) return;
  if (!confirmed) {
    pendingConfirm.value = null;
    return;
  }
  try {
    await question.run();
    pendingConfirm.value = null;
  } catch (cause) {
    // The question stays open on a failure, so the answer is not lost to a toast the user
    // has to read while the dialog is still covering the panel.
    reportCause(cause);
  }
}

/**
 * Puts every worktree this repository archived back on the panel, in one action.
 *
 * It is one action because they were archived one at a time and the panel has room for
 * all of them; walking a set of worktrees back individually is the work the archive was
 * meant to end.
 */
function restoreArchivedWorktrees(repoId: string) {
  const repo = workspace.value.repos.find((item) => item.id === repoId);
  const archived = (workspace.value.archivedWorktrees ?? []).filter((item) => item.repoId === repoId);
  if (!repo || !archived.length) return;
  const plural = archived.length === 1 ? "worktree" : "worktrees";
  askConfirm({
    title: `Restore archived ${plural}`,
    message: `${archived.length} archived ${plural} return to the sidebar. Nothing on disk changes.`,
    confirmLabel: "Restore",
    run: async () => {
      applyWorkspace(await persistArchivedRestore(repoId));
    },
  });
}

async function requestShell(checkoutId: string, launch?: TerminalLaunch) {
  const selection = { activeCheckoutId: checkoutId, activeSessionId: null };
  noteTerminalNavigation(selection);
  showView(checkoutId, { kind: "terminal", sessionId: null }, { navigation: false });
  const token = ++shellRequestToken;
  const revision = terminalMovement.navigationRevision;
  pendingShellNavigation = { checkoutId, token, revision };
  if (launch) {
    // Launch ownership is the requested checkout, not whichever navigation finishes last. The
    // terminal request must survive a later navigation, while its stale selection must not.
    shellRequest.value = { checkoutId, token, launch };
    void persistCheckoutSelection(checkoutId)
      .then(() => {
        if (token === shellRequestToken && revision === terminalMovement.navigationRevision) {
          workspace.value = { ...workspace.value, ...selection };
        }
      })
      .catch(reportCause);
    return;
  }
  try {
    await persistCheckoutSelection(checkoutId);
    if (token !== shellRequestToken || revision !== terminalMovement.navigationRevision) return;
    workspace.value = { ...workspace.value, ...selection };
    shellRequest.value = { checkoutId, token, launch };
  } catch (cause) {
    if (token === shellRequestToken) {
      if (pendingShellNavigation?.token === token) pendingShellNavigation = null;
      reportCause(cause);
    }
  }
}

/**
 * Sends the selected notes to Markdown, an existing OpenCode session, or a new interactive Pi terminal.
 *
 * Pi launch success means only that its terminal opened; OpenCode retains recorded, queued review rounds.
 */
async function sendReviewToAgent(ids: string[], queue = false) {
  const checkout = activeCheckout.value;
  // Read from the payload rather than the list: the sender decides what is included.
  const chosen = new Set(ids);
  const notes = review.notes.filter((note) => chosen.has(note.id));
  if (!checkout || checkout.isMissing || activeRepo.value?.kind !== "git" || notes.length === 0) return;
  if (sendingReview.value) return;
  const destination = reviewTarget.value;
  const status = gitSnapshot.status;
  const localTime = localReviewTimestamp();
  const markdown = buildReviewMarkdown(notes, {
    date: localTime.date,
    branch: status?.branch,
    defaultBranch: status?.defaultBranch,
  });
  sendingReview.value = true;
  try {
    if (destination !== "markdown") {
      const checked = await refreshCompatibilitySnapshot();
      if (!checked) {
        const message = compatibilityError.value || "Could not verify the installed review destination.";
        pushToast(message);
        return;
      }
      if (!availableReviewDestinations.value.includes(destination)) {
        reviewTarget.value = "markdown";
        try {
          await saveReviewTarget(checkout.id, "markdown");
        } catch (cause) {
          reportCause(cause);
        }
        const message = `${destination === "pi" ? "Pi" : "OpenCode"} is no longer detected. Review destination reset to Markdown.`;
        pushToast(message);
        return;
      }
    }
    if (destination === "markdown") {
      const exported = await exportReviewMarkdown(checkout.id, localTime.date, localTime.timestamp, markdown);
      showView(checkout.id, {
        kind: "document",
        path: exported.path,
        mode: "view",
        origin: exported.origin,
      });
      return;
    }
    // No queue and no target to resolve: every Pi send opens its own terminal in this checkout.
    if (destination === "pi") {
      await requestShell(checkout.id, {
        kind: "pi-review",
        request: { markdown },
      });
      return;
    }
    const target = await resolveAgentTarget();
    if (!target) {
      pushToast("No connected OpenCode session. Start OpenCode and open a session before sending.");
      return;
    }
    // `queue` records the round and leaves the agent alone: the message it was accepted
    // with is stored with it, so the flush that runs at the end of the turn sends exactly
    // those bytes, even after a restart.
    await review.dispatchRound(
      target,
      notes.map((note) => note.id),
      markdown,
      queue,
    );
  } catch (cause) {
    reportCause(cause);
  } finally {
    sendingReview.value = false;
  }
}

async function selectReviewTarget(target: ReviewTarget) {
  const checkoutId = activeCheckout.value?.id;
  if (!checkoutId || !availableReviewDestinations.value.includes(target)) return;
  reviewTargetLoadGeneration += 1;
  reviewTarget.value = target;
  try {
    await saveReviewTarget(checkoutId, target);
  } catch (cause) {
    reportCause(cause);
  }
}

/** The session a review goes to: the selected one, otherwise the newest live session. */
async function resolveAgentTarget(): Promise<string | null> {
  const target =
    agent.sessions.find((session) => session.provider === "opencode" && session.id === agent.targetId)?.id ??
    defaultOpenCodeSession(agent.sessions)?.id ??
    null;
  return target;
}

/**
 * The send, as the diff sees it.
 *
 * `acked` is the only round whose turn is known to be over, so anything else is what the
 * "not finished" count is counting. The target keeps following the newest live session until
 * the user picks one, which `selectTarget` does.
 */
const reviewSender: ReviewSender = {
  get target() {
    return reviewTarget.value;
  },
  get availableTargets() {
    return availableReviewDestinations.value;
  },
  get sessions() {
    return agent.sessions;
  },
  get targetId() {
    return agent.targetId;
  },
  get unfinishedRounds() {
    return review.rounds.filter((round) => round.status !== "acked" && round.status !== "relocated").length;
  },
  get error() {
    return review.error;
  },
  selectTarget: (sessionId) => agent.selectTarget(sessionId),
  selectReviewTarget,
  send: sendReviewToAgent,
};
provide(REVIEW_SENDER, reviewSender);

function updateSessionStatus(sessionId: string, status: TerminalSessionStatus | null) {
  if (status) sessionRuntimeStatuses.value[sessionId] = status;
  else delete sessionRuntimeStatuses.value[sessionId];
  terminalMovement.onStatus(sessionId, status);
}

function updateSessionOrder(checkoutId: string, order: string[]) {
  sessionOrder.value = { ...sessionOrder.value, [checkoutId]: order };
}

async function closeTerminalSession(sessionId: string) {
  await mainPane.value?.requestClose(sessionId);
}

/**
 * Whether the terminal is the one on screen.
 *
 * Being the selected session is not the same thing as being visible: opening a document takes the
 * whole panel in a single layout, and the terminal behind it is still selected while nothing of it
 * is on screen. A `null` session in a terminal view means the pane is on terminals and is drawing
 * the selected one, so that is the same answer as naming this terminal outright.
 */
function terminalIsOnScreen(sessionId: string): boolean {
  if (isSplitLayout.value) return true;
  const view = activeMainView.value;
  return view.kind === "terminal" && (view.sessionId === null || view.sessionId === sessionId);
}

function noteTerminalNavigation(
  selection: Pick<WorkspaceState, "activeCheckoutId" | "activeSessionId"> = {
    activeCheckoutId: workspace.value.activeCheckoutId,
    activeSessionId: workspace.value.activeSessionId,
  },
) {
  terminalMovement.noteNavigation(selection);
  pendingShellNavigation = null;
}
/**
 * Whether that terminal has exactly that session open, as far as anything here can tell.
 *
 * The foreground program confirms the terminal is an agent's at all, and the title the TUI wrote
 * names one of the sessions the service lists: `one`, because anything else names nothing or names
 * several, and the id, because the same title can be on that list more than once.
 */
function terminalShowsSession(checkoutId: string, terminalId: string, sessionId: string): boolean {
  const status = sessionRuntimeStatuses.value[terminalId];
  if (!status?.foregroundProcess || status.foregroundApp !== AGENT_APP) return false;
  const match = matchAgentSessionTitle(terminalAgents.row(checkoutId).sessions, agentSessionTitle(status));
  return match.kind === "one" && match.session.id === sessionId;
}

/**
 * Names a terminal session.
 *
 * The database decides the name, not this call: it trims, refuses an empty or over-long one
 * and only then writes, so a rejected rename leaves the row showing the name it already had
 * rather than one the backend would not keep.
 */
async function renameTerminalSession(sessionId: string, name: string) {
  const checkoutId = workspace.value.activeCheckoutId;
  if (!checkoutId) return;
  try {
    applyWorkspace(await renameTerminal(checkoutId, sessionId, name));
  } catch (cause) {
    reportCause(cause);
  }
}

function applyWorkspace(next: WorkspaceState) {
  updateWorkspace(next);
}

/** Reports the launch result, not whether Pi read or completed the review. */
function onReviewLaunchOpened() {
  pushToast("Review opened in Pi", "info");
}

function onReviewLaunchFailed(message: string) {
  pushToast(message);
}

async function onShellCreated(token: number, checkoutId: string, sessionId: string) {
  const pending = pendingShellNavigation;
  if (
    !pending ||
    pending.token !== token ||
    pending.checkoutId !== checkoutId ||
    pending.revision !== terminalMovement.navigationRevision
  )
    return;
  const selection = { activeCheckoutId: checkoutId, activeSessionId: sessionId };
  noteTerminalNavigation(selection);
  workspace.value = { ...workspace.value, ...selection };
}

// An automatic move returns its old selection snapshot; keep any newer navigation over it.
function updatePaneWorkspace(next: WorkspaceState, shellToken?: number) {
  const selection = terminalMovement.workspaceUpdateSelection(pendingShellNavigation, shellToken);
  let updated = selection ? { ...next, ...selection } : next;
  if (selection?.activeSessionId) {
    const selectedSession = workspace.value.repos
      .flatMap((repo) => repo.checkouts)
      .flatMap((checkout) => checkout.sessions)
      .find((session) => session.id === selection.activeSessionId);
    const returnedSession = updated.repos
      .flatMap((repo) => repo.checkouts)
      .flatMap((checkout) => checkout.sessions)
      .some((session) => session.id === selection.activeSessionId);
    if (selectedSession && !returnedSession) {
      updated = {
        ...updated,
        repos: updated.repos.map((repo) => ({
          ...repo,
          checkouts: repo.checkouts.map((checkout) =>
            checkout.id === selection.activeCheckoutId
              ? { ...checkout, sessions: [...checkout.sessions, selectedSession] }
              : checkout,
          ),
        })),
      };
    }
  }
  updateWorkspace(updated);
}

// A worktree added by something other than this app (an agent running `git worktree add`, a
// script) belongs in the panel the same way one added here does, and the only reading that can
// put it there is the one a launch takes.
useWorktreeSync(() => workspace.value, applyWorkspace, reportCause);

function reportWarning(message: string) {
  pushToast(message, "info");
}
</script>

<template>
  <div
    v-if="appLayoutReady"
    ref="appShell"
    class="app-shell relative flex h-full min-w-[900px] flex-col"
    :style="{ '--inspector-width': `${appLayout.inspectorWidth}px` }"
  >
    <!-- The padding on the left is where macOS puts its traffic lights. A window that draws no
         frame of its own has none, so the space goes to the crumbs instead. -->
    <header
      class="window-header flex h-8 shrink-0 items-center gap-4 border-b pr-4"
      :class="windowDecorated ? 'pl-[78px]' : 'pl-3'"
    >
      <!-- The mockup packs the crumbs against the left, with the empty space and the gear at the
           far end. Dragging lives on that empty space, so the crumbs keep their place instead of
           being pushed to the opposite edge. The nav grows into the room the mockup gives it and
           only truncates when it runs out, rather than capping each crumb. -->
      <!-- The crumb line is chrome that names where you are, not text to copy: a drag across it
           was painting a selection over the whole header. -->
      <nav
        ref="lineEl"
        aria-label="Repository location"
        class="window-breadcrumb flex h-full min-w-0 max-w-[70%] shrink select-none items-center gap-1.5"
      >
        <template v-if="activeCheckout">
          <!-- Each crumb is text that opens the list of the level it names, and nothing looks
               like a button until it is pointed at: the workdir, its branch or worktree, and
               the item open inside it. They share one open name, so opening one closes the
               others instead of stacking them. The workdir is the one that wears nothing: it is
               the end of the line that is always there, and a name that never changes is one a
               glyph would only repeat. -->
          <TitlebarMenu
            testid="repo-crumb"
            crumb="workdir"
            :label="workdirLabel"
            :sections="workdirMenu"
            :open="openCrumb === 'workdir'"
            search-placeholder="Search workdirs…"
            @update:open="setCrumbOpen('workdir', $event)"
          />
          <template v-if="activeRepo?.kind === 'git'">
            <span aria-hidden="true" class="text-(--muster-text-faint)">/</span>
            <TitlebarMenu
              testid="worktree-crumb"
              crumb="branch"
              :label="activeCheckout.branch || 'Detached'"
              :icon="WORKDIR_ICONS.worktree"
              :sections="worktreeMenu"
              :open="openCrumb === 'worktree'"
              search-placeholder="Search worktrees…"
              @update:open="setCrumbOpen('worktree', $event)"
            />
          </template>
          <!-- The last crumb names the view that is open, and a workdir with no terminal has
               none to name. A file or a change set is picked from the details panel, so it is
               plain text there: a session menu over a diff would name what is not on screen. -->
          <template v-if="hasItemCrumb">
            <span aria-hidden="true" class="text-(--muster-text-faint)">/</span>
            <TitlebarMenu
              v-if="activeMainView.kind === 'terminal'"
              testid="item-crumb"
              crumb="item"
              align="end"
              :label="activeViewLabel"
              :icon="WORKDIR_ICONS.terminal"
              :sections="terminalMenu"
              :open="openCrumb === 'terminal'"
              @update:open="setCrumbOpen('terminal', $event)"
            />
            <span
              v-else
              ref="pathCrumbEl"
              data-testid="item-crumb"
              class="crumb-item min-w-0 truncate"
              :title="activeViewLabel"
            >
              <template v-for="(step, index) in drawnSteps" :key="`${index}-${step}`">
                <span v-if="index > 0" aria-hidden="true" class="crumb-sep">/</span>
                <!-- The last step is the file, so that is the one wearing its icon: the rest of
                     the path is directories, and an icon at the head of the line would name the
                     first of them rather than the thing that is open.

                     The glyph is a box of its own beside the name rather than inside it, because
                     a box holding both would be a flex row, and a flex row takes its baseline
                     from its first item: the step would sit a line below the steps around it. -->
                <span v-if="index === drawnSteps.length - 1 && itemCrumbFile" class="crumb-icon-slot"
                  ><FileIcon class="icon-xs crumb-icon" :name="itemCrumbFile.name" kind="file" /></span
                >{{ step }}
              </template>
            </span>
            <!-- The path at the width it wants, glyph and all: the crumb weighs this probe against
                 the room the line has left, and a path that does not count its own icon would
                 overflow the line before it was told to give anything up. It sits beside the crumb
                 rather than inside it so it never joins the text the crumb reads as, and it is
                 always mounted so the crumb can be weighed against the room the line has left. -->
            <span
              v-if="activeMainView.kind !== 'terminal' && pathSteps.length > 2"
              ref="pathProbeEl"
              data-testid="path-probe"
              aria-hidden="true"
              class="path-probe"
              >{{ activeViewLabel
              }}<span v-if="itemCrumbFile" class="crumb-icon-slot"
                ><FileIcon class="icon-xs crumb-icon" :name="itemCrumbFile.name" kind="file" /></span
            ></span>
          </template>
        </template>
      </nav>
      <!-- Native dragging lives on the empty space the mockup leaves at the far end, so it
           cannot swallow a click on a crumb or the gear. Zooming on a double click is left to
           the window: it reads the double click off the drag region itself, and answering it
           here as well maximized and restored the window in the same gesture. -->
      <div data-tauri-drag-region aria-hidden="true" class="h-full min-w-8 flex-1" />
      <button
        type="button"
        :aria-label="appLayout.mode === 'split' ? 'Switch to focus layout' : 'Switch to split layout'"
        :aria-pressed="appLayout.mode === 'split'"
        data-testid="layout-toggle"
        class="icon-button shrink-0 text-(--muster-text-secondary) hover:text-(--muster-text)"
        @click="toggleLayoutMode"
      >
        <Columns2Icon v-if="appLayout.mode === 'split'" class="icon-xs" aria-hidden="true" />
        <Columns3Icon v-else class="icon-xs" aria-hidden="true" />
      </button>
      <button
        ref="settingsButton"
        type="button"
        aria-label="Settings"
        data-testid="settings-button"
        class="icon-button shrink-0 text-(--muster-text-secondary) hover:text-(--muster-text)"
        @click="openSettings"
      >
        <SettingsIcon class="icon-xs" aria-hidden="true" />
      </button>
      <!-- The window's own controls, for the platform whose window has no frame to draw them.
           They are past the drag region, so a click on one is a click on the button. -->
      <div v-if="!windowDecorated" class="window-controls -mr-4 flex h-full shrink-0 self-stretch">
        <button
          type="button"
          aria-label="Minimize window"
          data-testid="window-minimize"
          class="window-control"
          @click="minimizeWindow"
        >
          <MinusIcon class="icon-xs" aria-hidden="true" />
        </button>
        <button
          type="button"
          :aria-label="windowMaximized ? 'Restore window' : 'Maximize window'"
          data-testid="window-maximize"
          class="window-control"
          @click="toggleWindowMaximized"
        >
          <!-- Two squares for restore and one for maximize: the same pair every other window
               on the platform uses, which is what makes it readable without a label. -->
          <CopyIcon v-if="windowMaximized" class="icon-xs" aria-hidden="true" />
          <SquareIcon v-else class="icon-xs" aria-hidden="true" />
        </button>
        <button
          type="button"
          aria-label="Close window"
          data-testid="window-close"
          class="window-control window-control-close"
          @click="closeWindow"
        >
          <XIcon class="icon-xs" aria-hidden="true" />
        </button>
      </div>
    </header>
    <SplitterGroup direction="horizontal" class="app-splitter flex min-h-0 flex-1" @layout="onSplitterLayout">
      <SplitterPanel
        id="navigation-panel"
        ref="sidebarPanel"
        :default-size="toScreen(appLayout.sidebarWidth)"
        :min-size="toScreen(SIDEBAR_WIDTH_LIMITS.min)"
        :max-size="toScreen(SIDEBAR_WIDTH_LIMITS.max)"
        :collapsed-size="0"
        collapsible
        size-unit="px"
        class="min-h-0 shrink-0"
      >
        <Sidebar
          :repos="workspace.repos"
          :font-scale="fontScale"
          :home-checkout-id="workspace.homeCheckoutId"
          :active-checkout-id="workspace.activeCheckoutId"
          :active-session-id="activeSessionId"
          :main-view-kind="activeMainView.kind"
          :session-runtime-statuses="sessionRuntimeStatuses"
          :session-order="sessionOrder"
          :agent-rows="terminalAgents.byCheckout"
          :is-opening="isOpening"
          :archived-worktrees="workspace.archivedWorktrees"
          @open-folder="chooseFolder"
          @select-checkout="activateCheckoutTerminal"
          @select-session="activateTerminalSession"
          @create-worktree="openWorktreeDialog('create', $event)"
          @new-terminal="requestShell"
          @remove-worktree="openWorktreeDialog('remove', $event)"
          @close-workdir="closeWorkdir"
          @close-missing="closeMissingCheckout"
          @restore-archived="restoreArchivedWorktrees"
          @close-session="closeTerminalSession"
          @rename-session="renameTerminalSession"
          @move-session="terminalMovement.moveManually"
        />
      </SplitterPanel>
      <SplitterResizeHandle
        v-show="sidePanelsVisible"
        id="navigation-resize-handle"
        aria-label="Resize navigation sidebar"
        class="splitter-handle splitter-handle-edge-left"
        @dblclick.stop="resetPanelWidth('sidebar')"
      />
      <SplitterPanel
        id="main-panel"
        :min-size="toScreen(isSplitLayout ? (activeMainView.kind === 'terminal' ? 320 : 585) : 420)"
        size-unit="px"
        class="main-column min-h-0 min-w-0 flex-1"
      >
        <MainPane
          ref="mainPane"
          :checkout="activeCheckout"
          :checkouts="allCheckouts"
          :view="activeMainView"
          :split="isSplitLayout"
          :preview-width="appLayout.previewWidth"
          :zoom="appZoom"
          :terminal-settings="settings.terminal"
          :editor-settings="settings.editor"
          :ready="checkoutUiReady"
          :git-snapshot="gitSnapshot"
          :review="review"
          :active-session-id="activeSessionId"
          :is-opening="isOpening"
          :shell-request="shellRequest"
          :registered-session-ids="registeredSessionIds"
          :refresh-revision="documentRefreshActivity[activeCheckout?.id ?? '']?.revision ?? 0"
          :refresh-paths="documentRefreshActivity[activeCheckout?.id ?? '']?.paths ?? NO_REFRESH_PATHS"
          :reading-position="{
            top: activeCheckoutUiState?.documentScrollTop ?? 0,
            left: activeCheckoutUiState?.documentScrollLeft ?? 0,
          }"
          :diff-scroll-top="activeCheckoutUiState?.diffScrollTop ?? 0"
          @open-folder="chooseFolder"
          @workspace-updated="updatePaneWorkspace"
          @shell-created="onShellCreated"
          @review-launch-opened="onReviewLaunchOpened"
          @review-launch-failed="onReviewLaunchFailed"
          @session-status-changed="updateSessionStatus"
          @session-order="updateSessionOrder"
          @update-document-mode="setDocumentMode"
          @reading-position-changed="activeCheckout && updateDocumentReadingPosition(activeCheckout.id, $event)"
          @diff-position-changed="activeCheckout && updateDiffReadingPosition(activeCheckout.id, $event)"
          @resize-preview="resizeAppPreview"
          @close-preview="activeCheckout && closePreview(activeCheckout.id)"
          @open-markdown-link="activeCheckout && openFileDocument({ checkoutId: activeCheckout.id, path: $event })"
          @open-external-url="openExternalUrl"
          @open-file="activeCheckout && openFileDocument({ checkoutId: activeCheckout.id, path: $event })"
        />
      </SplitterPanel>
      <SplitterResizeHandle
        v-show="sidePanelsVisible && !inspectorInDrawer"
        id="inspector-resize-handle"
        aria-label="Resize files and changes inspector"
        class="splitter-handle splitter-handle-edge-right"
        @dblclick.stop="resetPanelWidth('inspector')"
      />
      <SplitterPanel
        id="inspector-panel"
        ref="inspectorPanel"
        :default-size="inspectorInDrawer ? 0 : toScreen(appLayout.inspectorWidth)"
        :min-size="toScreen(INSPECTOR_WIDTH_LIMITS.min)"
        :max-size="toScreen(INSPECTOR_WIDTH_LIMITS.max)"
        :collapsed-size="0"
        collapsible
        size-unit="px"
        class="inspector-splitter-panel relative min-h-0 shrink-0 overflow-visible"
      >
        <!-- The review, the agent list and the send itself belong to the diff (F.5): this panel
             is the file tree and the change set, and nothing more. -->
        <Teleport :to="appShell" :disabled="!isSplitLayout || !appShell">
          <InspectorPane
            v-show="sidePanelsVisible"
            :class="{
              'right-inspector-drawer': inspectorInDrawer,
              'split-inspector-drawer': isSplitLayout,
              'split-inspector-closed': isSplitLayout && !splitInspectorOpen,
            }"
            :checkout="checkoutUiReady ? activeCheckout : null"
            :home-path="homePath"
            :repo="checkoutUiReady ? activeRepo : null"
            :git-snapshot="gitSnapshot"
            :saved-state="activeCheckout ? checkoutUiStates[activeCheckout.id] : null"
            :font-scale="fontScale"
            :tree-sticky-scroll="settings.ui.treeStickyScroll"
            @open-file="openFileDocument"
            @open-external-file="openExternalFile"
            @open-change="openChangedDocument"
            @open-all-changes="openAllChanges"
            @update-ui-state="activeCheckout && updateInspectorUiState(activeCheckout.id, $event)"
            @pointerenter="enterSplitDrawer"
            @pointerleave="leaveSplitDrawer"
          />
        </Teleport>
      </SplitterPanel>
    </SplitterGroup>
    <div
      v-if="isSplitLayout && sidePanelsVisible"
      aria-hidden="true"
      class="inspector-hover-strip"
      :class="{ 'inspector-hover-strip-open': splitInspectorOpen }"
      @pointerenter="enterSplitStrip"
      @pointerleave="leaveSplitStrip"
    />
    <!-- The one the window close asks, which is asked before anything is written or stopped. -->
    <ConfirmDialog
      :open="closeQuestion !== null"
      :title="closeQuestion?.title ?? ''"
      :message="closeQuestion?.message ?? ''"
      confirm-label="Stop and close"
      destructive
      @confirm="answerCloseQuestion(true)"
      @close="answerCloseQuestion(false)"
    />
    <WorktreeDialog
      :open="!!lifecycle"
      :mode="lifecycle?.mode ?? 'create'"
      :repo="lifecycleRepo"
      :checkout="lifecycleCheckout"
      @close="lifecycle = null"
      @workspace-updated="applyWorkspace"
      @request-shell="requestShell"
      @warning="reportWarning"
    />
    <SettingsDialog
      :open="settingsOpen"
      :settings="settings"
      :saving="settingsSaving"
      :version="appVersion"
      :review-folder="reviewFolder"
      :compatibility="compatibilitySnapshot"
      :compatibility-loading="compatibilityLoading"
      :compatibility-error="compatibilityError"
      @close="closeSettings"
      @apply="applyFromDialog"
      @open-external-url="openExternalUrl"
      @review-storage-changed="loadReviewFolder"
      @clear-review-folder="requestClearReviewFolder"
      @refresh-compatibility="refreshCompatibilitySnapshot"
    />
    <!-- After the settings dialog, because it is the only question asked from inside one, and two
         surfaces at the same height are painted in the order they are written: the answer has to
         land on top of the question that asked it. -->
    <ConfirmDialog
      :open="!!pendingConfirm"
      :title="pendingConfirm?.title ?? ''"
      :message="pendingConfirm?.message ?? ''"
      :confirm-label="pendingConfirm?.confirmLabel ?? 'Confirm'"
      :destructive="pendingConfirm?.destructive ?? false"
      @confirm="answerConfirm(true)"
      @close="answerConfirm(false)"
    />
    <ToastStack />
  </div>
  <div v-else class="h-full bg-transparent p-5 text-sm text-zinc-400" role="status">Restoring workspace layout…</div>
</template>
