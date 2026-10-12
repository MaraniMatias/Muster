// @vitest-environment happy-dom
// Test doubles intentionally colocate small component shells and omit production prop defaults.
/* eslint-disable vue/one-component-per-file, vue/require-default-prop */
import { flushPromises, mount } from "@vue/test-utils";
import { listen } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h, inject, onMounted } from "vue";
import type { InjectionKey, Ref } from "vue";
import { DEFAULT_APP_LAYOUT, DEFAULT_CHECKOUT_UI_STATE } from "./domain/ui-state";
import * as uiStateDomain from "./domain/ui-state";
import type { AppLayoutState } from "./domain/ui-state";
import { ACKNOWLEDGEMENT, CREDITS, CREDITS_TITLE, REPOSITORY } from "./domain/credits";
import { SHORTCUT_GROUPS, shortcutChord } from "./domain/shortcuts";
import { DEFAULT_SETTINGS, SETTINGS_SECTIONS, cloneSettings } from "./domain/settings";
import type { AppSettings } from "./domain/settings";
import type { ReviewNote } from "./domain/review";
import { REVIEW_SENDER } from "./presentation/review-notes";
import { useToasts } from "./presentation/toasts";
import { WORKDIR_ICONS } from "./presentation/workdir-icons";
import type { ReviewSender } from "./presentation/review-notes";
import type { Checkout, Repo, Session, WorkspaceState } from "./domain/workspace";
import FileIcon from "./components/FileIcon.vue";
import SelectControl from "./components/ui/select/SelectControl.vue";

const mocks = vi.hoisted(() => ({
  initialWorkspace: null as WorkspaceState | null,
  workspaceRef: null as { value: WorkspaceState } | null,
  launchCheckoutId: null as { value: string | null } | null,
  sessionPaneMounts: 0,
  saveAppLayout: vi.fn(),
  loadAppLayout: vi.fn(),
  saveSettings: vi.fn(),
  loadSettings: vi.fn(),
  loadCheckoutUiState: vi.fn(),
  saveCheckoutUiState: vi.fn(),
  prepareAppExit: vi.fn(),
  reportFrontendDiagnostic: vi.fn(),
  getVersion: vi.fn().mockResolvedValue("9.9.9"),
  getCompatibilitySnapshot: vi.fn(),
  getTerminalStatus: vi.fn(),
  loadReviewTarget: vi.fn(),
  saveReviewTarget: vi.fn(),
  exportReviewMarkdown: vi.fn(),
  getReviewFolder: vi.fn(),
  clearReviewFolder: vi.fn(),
  closeCheckout: vi.fn(),
  closeMissingCheckout: vi.fn(),
  archiveCheckout: vi.fn(),
  restoreArchivedWorktrees: vi.fn(),
  renameTerminal: vi.fn(),
  listRecentPaths: vi.fn(),
  openPath: vi.fn(),
  openExternalUrl: vi.fn(),
  openExternalFile: vi.fn(),
  selectCheckout: vi.fn(),
  restoreWorkspace: vi.fn(),
  toggleMaximize: vi.fn(),
  minimize: vi.fn(),
  isDecorated: vi.fn(),
  isMaximized: vi.fn(),
  onWindowResized: null as (() => void) | null,
  followAgentRelocation: null as ((relocation: unknown) => void) | null,
  /**
   * The sessions each worktree's OpenCode lists, as `useTerminalAgentRows` would report them.
   *
   * `provider` is OpenCode for all of them, which is the whole of what these tests need: the panel
   * matches an OpenCode terminal by the title its TUI wrote, and it matches only OpenCode sessions,
   * so a stub without the field would be read as belonging to no provider and named by nobody.
   */
  agentSessionsByCheckout: {} as Record<string, { id: string; title: string; provider: string }[]>,
  moveSession: vi.fn(),
  focusActiveTerminal: vi.fn(),
  onCloseRequested: null as ((event: { preventDefault(): void }) => Promise<void>) | null,
  currentWindow: null as {
    onCloseRequested: (handler: (event: { preventDefault(): void }) => Promise<void>) => Promise<() => void>;
    close: () => Promise<void>;
  } | null,
  onProgrammaticPanelResize: null as ((panelId: string, size: number) => void) | null,
  gitStatus: null as { branch: string; defaultBranch: string } | null,
  reviewNotes: [] as ReviewNote[],
  agentSessions: [] as Array<{
    id: string;
    checkoutId: string;
    provider: "opencode";
    title: string;
    busy: boolean;
    awaitingReply: boolean;
    createdAt: number;
    updatedAt: number;
  }>,
  agentTargetId: null as string | null,
  createAgentSession: vi.fn(),
  dispatchReviewRound: vi.fn(),
  refreshReviewNotes: vi.fn(async () => undefined),
  reconcileRounds: vi.fn(),
  flushQueuedRounds: vi.fn(),
  ackFinishedTurn: vi.fn(),
  // Turn completions are observed, so the watcher needs a real reactive source.
  turns: null as { value: number } | null,
  completedSessionIds: [] as string[],
}));

vi.mock("reka-ui", async () => {
  const { computed, defineComponent, h, inject, provide } = await import("vue");
  const SplitterGroup = defineComponent({
    name: "SplitterGroup",
    emits: ["layout"],
    setup(_, { attrs, slots }) {
      return () => h("div", attrs, slots.default?.());
    },
  });
  const SplitterPanel = defineComponent({
    name: "SplitterPanel",
    props: { id: String },
    emits: ["collapse", "expand"],
    setup(props, { emit, slots, expose }) {
      expose({
        collapse: () => emit("collapse"),
        expand: () => emit("expand"),
        resize: vi.fn((size: number) => mocks.onProgrammaticPanelResize?.(props.id ?? "", size)),
      });
      return () => h("div", { id: props.id, "data-collapsed": String(emit.length > 0) }, slots.default?.());
    },
  });
  const SplitterResizeHandle = defineComponent({
    name: "SplitterResizeHandle",
    emits: ["dragging"],
    setup(_, { attrs, slots }) {
      return () => h("div", { ...attrs, tabindex: 0 }, slots.default?.());
    },
  });
  // The titlebar's crumbs open menus; the stub always renders their content so the rows can be
  // reached without driving the open state. A row stands in for a menu item: it takes the attrs
  // it is given and reports a selection, which is what a click on it does.
  const passThrough = (name: string) =>
    defineComponent({
      name,
      setup(_, { attrs, slots }) {
        return () => h("div", attrs, slots.default?.());
      },
    });
  const menuRow = defineComponent({
    name: "DropdownMenuItem",
    inheritAttrs: false,
    props: { disabled: Boolean },
    setup(_, { attrs, emit, slots }) {
      return () => h("button", { ...attrs, onClick: () => emit("select") }, slots.default?.());
    },
  });
  // The root takes the open name as a prop, the way reka's does, so "only one menu at a time"
  // can be asserted on what the shell decided rather than on what a click did.
  const menuRoot = defineComponent({
    name: "DropdownMenuRoot",
    props: { open: Boolean },
    emits: ["update:open"],
    setup(props, { slots }) {
      return () => h("div", { "data-open": String(props.open) }, slots.default?.());
    },
  });
  const menuFilter = defineComponent({
    name: "DropdownMenuFilter",
    props: { modelValue: String, placeholder: String },
    emits: ["update:modelValue"],
    setup(props, { emit }) {
      return () =>
        h("input", {
          "data-testid": "menu-search",
          placeholder: props.placeholder,
          value: props.modelValue,
          onInput: (event: Event) => emit("update:modelValue", (event.target as HTMLInputElement).value),
        });
    },
  });
  // The Settings dialog's pickers are the only selects in the shell, and they are stubbed as a
  // list that is always open: the trigger carries the testid and the current value, and a row
  // reports a choice, which is what clicking one does.
  // Typed on the symbol rather than at each `inject`, so the trigger and the rows cannot disagree
  // about the shape of what the root provides.
  const selected = Symbol("selected") as InjectionKey<{
    value: Readonly<Ref<string | undefined>>;
    choose: (value: string) => void;
  }>;
  const SelectRoot = defineComponent({
    name: "SelectRoot",
    props: { modelValue: String },
    emits: ["update:modelValue"],
    setup(props, { emit, slots }) {
      provide(selected, {
        value: computed(() => props.modelValue),
        choose: (value: string) => emit("update:modelValue", value),
      });
      return () => slots.default?.();
    },
  });
  const SelectTrigger = defineComponent({
    name: "SelectTrigger",
    inheritAttrs: false,
    setup(_, { attrs, slots }) {
      return () => h("button", attrs, slots.default?.());
    },
  });
  const SelectValue = defineComponent({
    name: "SelectValue",
    setup() {
      const current = inject(selected)!;
      return () => current.value.value;
    },
  });
  const SelectItem = defineComponent({
    name: "SelectItem",
    props: { value: String },
    setup(props, { slots }) {
      const current = inject(selected)!;
      return () =>
        h(
          "button",
          {
            type: "button",
            "data-value": props.value,
            onClick: () => current.choose(props.value ?? ""),
          },
          slots.default?.(),
        );
    },
  });
  const selectPassThrough = (name: string) =>
    defineComponent({
      name,
      inheritAttrs: false,
      setup(_, { attrs, slots }) {
        return () => h("div", attrs, slots.default?.());
      },
    });

  return {
    SplitterGroup,
    SplitterPanel,
    SplitterResizeHandle,
    SelectRoot,
    SelectTrigger,
    SelectValue,
    SelectContent: selectPassThrough("SelectContent"),
    SelectPortal: selectPassThrough("SelectPortal"),
    SelectViewport: selectPassThrough("SelectViewport"),
    SelectItem,
    SelectItemIndicator: selectPassThrough("SelectItemIndicator"),
    SelectItemText: selectPassThrough("SelectItemText"),
    DropdownMenuRoot: menuRoot,
    DropdownMenuTrigger: passThrough("DropdownMenuTrigger"),
    DropdownMenuContent: passThrough("DropdownMenuContent"),
    DropdownMenuPortal: passThrough("DropdownMenuPortal"),
    DropdownMenuSeparator: passThrough("DropdownMenuSeparator"),
    DropdownMenuFilter: menuFilter,
    DropdownMenuItem: menuRow,
    // A group and its label only wrap the rows: the stub always renders their content, which is
    // what lets a grouped menu be reached without driving the open state, the same as the rest.
    DropdownMenuGroup: passThrough("DropdownMenuGroup"),
    DropdownMenuLabel: passThrough("DropdownMenuLabel"),
  };
});

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    ...mocks.currentWindow,
    toggleMaximize: mocks.toggleMaximize,
    minimize: mocks.minimize,
    isDecorated: mocks.isDecorated,
    isMaximized: mocks.isMaximized,
    onResized: vi.fn(async (handler: () => void) => {
      mocks.onWindowResized = handler;
      return vi.fn();
    }),
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(vi.fn()) }));
// The version the About section prints. Resolved rather than rejected because the read happens at
// startup and the default case is the one a build normally is in; the refusal is a build that cannot
// read its own version, which the About test below draws as a missing line.
vi.mock("@tauri-apps/api/app", () => ({ getVersion: mocks.getVersion }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./lib/diagnostics", () => ({ reportFrontendDiagnostic: mocks.reportFrontendDiagnostic }));
vi.mock("./lib/ipc", () => ({
  archiveCheckout: mocks.archiveCheckout,
  closeCheckout: mocks.closeCheckout,
  closeMissingCheckout: mocks.closeMissingCheckout,
  restoreArchivedWorktrees: mocks.restoreArchivedWorktrees,
  listRecentPaths: mocks.listRecentPaths,
  exportReviewMarkdown: mocks.exportReviewMarkdown,
  getReviewFolder: mocks.getReviewFolder,
  getCompatibilitySnapshot: mocks.getCompatibilitySnapshot,
  clearReviewFolder: mocks.clearReviewFolder,
  loadReviewTarget: mocks.loadReviewTarget,
  loadAppLayout: mocks.loadAppLayout,
  loadCheckoutUiState: mocks.loadCheckoutUiState,
  renameTerminal: mocks.renameTerminal,
  saveAppLayout: mocks.saveAppLayout,
  loadSettings: mocks.loadSettings,
  saveSettings: mocks.saveSettings,
  saveCheckoutUiState: mocks.saveCheckoutUiState,
  prepareAppExit: mocks.prepareAppExit,
  getTerminalStatus: mocks.getTerminalStatus,
  saveReviewTarget: mocks.saveReviewTarget,
  // The real command returns the refreshed workspace; App assigns it straight back,
  // so returning undefined here crashed the next render. Only the shell and Neovim requests
  // reach it from the titlebar, so it also reports that a terminal was asked for.
  selectCheckout: mocks.selectCheckout,
  restoreWorkspace: mocks.restoreWorkspace,
  openExternalUrl: mocks.openExternalUrl,
  openExternalFile: mocks.openExternalFile,
}));
vi.mock("./presentation/workspace", async () => {
  const { computed, ref } = await import("vue");
  return {
    useWorkspaceState: () => {
      const workspace = ref(mocks.initialWorkspace!);
      mocks.workspaceRef = workspace;
      const activeCheckout = computed(
        () =>
          workspace.value.repos
            .flatMap((repo) => repo.checkouts)
            .find((checkout) => checkout.id === workspace.value.activeCheckoutId) ?? null,
      );
      const isOpening = ref(false);
      const launchCheckoutId = ref<string | null>(null);
      mocks.launchCheckoutId = launchCheckoutId;
      const error = ref<string | null>(null);
      const setActiveCheckout = (checkoutId: string | null, sessionId: string | null = null) => {
        workspace.value = { ...workspace.value, activeCheckoutId: checkoutId, activeSessionId: sessionId };
      };
      return {
        workspace,
        activeCheckout,
        launchCheckoutId,
        isOpening,
        error,
        chooseFolder: vi.fn(),
        openPath: mocks.openPath,
        selectCheckout: async (checkoutId: string | null) => {
          setActiveCheckout(checkoutId);
          return workspace.value;
        },
        selectSession: async (sessionId: string) => {
          const checkout = workspace.value.repos
            .flatMap((repo) => repo.checkouts)
            .find((item) => item.sessions.some((session) => session.id === sessionId));
          if (checkout) setActiveCheckout(checkout.id, sessionId);
        },
        updateWorkspace: (next: WorkspaceState) => (workspace.value = next),
        promptForDefaultBranchIfNeeded: vi.fn(),
      };
    },
  };
});
vi.mock("./presentation/active-git-snapshot", () => ({
  useActiveGitSnapshot: () => ({
    checkoutId: null,
    status: mocks.gitStatus ? { aheadCount: 1, files: [], ...mocks.gitStatus } : null,
    loading: false,
    statusState: "idle",
    statusError: "",
    changesStatusError: "",
    changesWatchError: "",
    statusRevision: 0,
    statusEventRevision: 0,
    statusEventCheckoutId: null,
  }),
}));
vi.mock("./presentation/git-watchers", () => ({
  // The watchers are the backend's business; this suite is about what the app does with the
  // workspace, not about when a repository starts being observed.
  useGitWatchers: vi.fn(),
}));
vi.mock("./presentation/review-notes", () => ({
  // The diff reaches the send through this key, so the real one has to be here.
  REVIEW_SENDER: Symbol("muster:review-sender"),
  useReviewNotes: () => ({
    checkoutId: "checkout:one",
    notes: mocks.reviewNotes,
    rounds: [],
    state: "ready",
    error: "",
    addNote: vi.fn(),
    updateNote: vi.fn(),
    deleteNote: vi.fn(),
    verifyAnchors: vi.fn(),
    clearOutdated: vi.fn(),
    resolveNote: vi.fn(),
    dispatchRound: mocks.dispatchReviewRound,
    refreshNotes: mocks.refreshReviewNotes,
    reconcileRounds: mocks.reconcileRounds,
    flushQueuedRounds: mocks.flushQueuedRounds,
    ackFinishedTurn: mocks.ackFinishedTurn,
  }),
}));
vi.mock("./presentation/agent-sessions", async () => {
  const { reactive } = await import("vue");
  const turns = reactive({ value: 0 });
  mocks.turns = turns;
  return {
    useAgentSessions: () => ({
      checkoutId: "checkout:one",
      sessions: mocks.agentSessions,
      targetId: mocks.agentTargetId,
      state: "ready",
      error: "",
      reload: vi.fn(),
      createSession: mocks.createAgentSession,
      selectTarget: vi.fn(),
      stop: vi.fn(),
      // A getter, so the watcher that observes turn completions tracks this source.
      get turnsCompleted() {
        return turns.value;
      },
      get completedSessionIds() {
        return mocks.completedSessionIds;
      },
    }),
    // The sidebar rows read this, keyed by checkout. Empty here: these tests are about the rest
    // of the app, and a row with no agent behind it is drawn without a chip.
    useTerminalAgentRows: () => ({
      byCheckout: {},
      row: (checkoutId: string) => ({
        agent: null,
        running: false,
        sessions: mocks.agentSessionsByCheckout[checkoutId] ?? [],
      }),
      reload: vi.fn(),
    }),
    useAgentRelocations: (onRelocated: (relocation: unknown) => void) => {
      mocks.followAgentRelocation = onRelocated;
    },
  };
});

import { SplitterGroup, SplitterResizeHandle } from "reka-ui";
import App from "./App.vue";

const SidebarStub = defineComponent({
  name: "SidebarStub",
  props: { activeSessionId: String, sessionOrder: Object as () => Record<string, string[]> },
  emits: [
    "selectCheckout",
    "selectSession",
    "closeWorkdir",
    "closeMissing",
    "removeWorktree",
    "restoreArchived",
    "renameSession",
    "newTerminal",
    "moveSession",
  ],
  setup(props, { emit }) {
    return () =>
      h("div", [
        h("span", { "data-testid": "sidebar-active-session" }, props.activeSessionId ?? "none"),
        h("span", { "data-testid": "sidebar-session-order" }, JSON.stringify(props.sessionOrder ?? {})),
        h("button", { "data-testid": "select-checkout-two", onClick: () => emit("selectCheckout", "checkout:two") }),
        h("button", { "data-testid": "select-session-one", onClick: () => emit("selectSession", "session:one") }),
        h("button", { "data-testid": "select-session-two", onClick: () => emit("selectSession", "session:two") }),
        h("button", { "data-testid": "new-terminal-one", onClick: () => emit("newTerminal", "checkout:one") }),
        h("button", { "data-testid": "close-workdir-base", onClick: () => emit("closeWorkdir", "checkout:one") }),
        h("button", { "data-testid": "close-workdir-worktree", onClick: () => emit("closeWorkdir", "checkout:two") }),
        h("button", { "data-testid": "close-missing-base", onClick: () => emit("closeMissing", "checkout:one") }),
        h("button", { "data-testid": "close-missing-worktree", onClick: () => emit("closeMissing", "checkout:two") }),
        // The row's own way off a worktree: one cross, and it opens the dialog that holds both
        // answers rather than picking one for the user.
        h("button", {
          "data-testid": "remove-worktree-two",
          onClick: () => emit("removeWorktree", "checkout:two"),
        }),
        h("button", {
          "data-testid": "restore-archived-shared",
          onClick: () => emit("restoreArchived", "repo:shared"),
        }),
        h("button", {
          "data-testid": "rename-session-one",
          onClick: () => emit("renameSession", "session:one", "build logs"),
        }),
      ]);
  },
});

const SessionPaneStub = defineComponent({
  name: "SessionPane",
  props: { activeSessionId: String },
  emits: [
    "sessionStatusChanged",
    "workspaceUpdated",
    "shellCreated",
    "sessionOrder",
    "reviewLaunchOpened",
    "reviewLaunchFailed",
  ],
  setup(props, { expose }) {
    onMounted(() => (mocks.sessionPaneMounts += 1));
    expose({ focusActiveTerminal: mocks.focusActiveTerminal, moveSession: mocks.moveSession });
    return () =>
      h("div", { "data-testid": "session-pane" }, [
        h("span", { "data-testid": "pane-active-session" }, props.activeSessionId ?? "none"),
        h("div", { class: "xterm" }, [h("textarea", { "data-testid": "terminal-input" })]),
      ]);
  },
});

const InspectorPaneStub = defineComponent({
  name: "InspectorPane",
  props: { checkout: Object },
  emits: ["openFile", "openExternalFile", "openAllChanges", "updateUiState"],
  setup(props, { emit }) {
    return () =>
      h("div", [
        h("button", {
          "data-testid": "open-file",
          disabled: !props.checkout,
          onClick: () =>
            props.checkout && emit("openFile", { checkoutId: (props.checkout as Checkout).id, path: "README.md" }),
        }),
        h("button", {
          "data-testid": "open-nested-file",
          disabled: !props.checkout,
          onClick: () =>
            props.checkout && emit("openFile", { checkoutId: (props.checkout as Checkout).id, path: "src/lib/one.ts" }),
        }),
        h("button", {
          "data-testid": "open-all-changes",
          disabled: !props.checkout,
          onClick: () => props.checkout && emit("openAllChanges", { checkoutId: (props.checkout as Checkout).id }),
        }),
        h("button", {
          "data-testid": "changes-scroll",
          disabled: !props.checkout,
          onClick: () =>
            emit("updateUiState", {
              inspectorTab: "changes",
              selectedFilePath: null,
              selectedChangePath: null,
              expandedDirectories: [],
              filesScrollTop: 0,
              changesScrollTop: 84,
            }),
        }),
      ]);
  },
});

const DocumentPaneStub = defineComponent({
  name: "DocumentPane",
  props: { path: String },
  emits: ["readingPositionChanged", "close"],
  setup(props, { emit }) {
    return () =>
      h("div", [
        h("span", { "data-testid": "document-pane" }, props.path ?? ""),
        h("button", {
          "data-testid": "document-scroll",
          onClick: () => emit("readingPositionChanged", { top: 240, left: 12 }),
        }),
        // The real pane's toolbar close button. What is under test here is what the shell does with
        // it, so the button stands in for the one the pane draws.
        h("button", { "data-testid": "close-preview", onClick: () => emit("close") }),
      ]);
  },
});

// The diff is where the send lives, so the stub takes the shell's sender the same way the real
// component does and asks it to hand over the notes, which is the wiring under test here.
const FileDiffStub = defineComponent({
  name: "FileDiff",
  props: { path: String },
  emits: ["scrollPositionChanged", "close"],
  setup(props, { emit }) {
    const sender = inject<ReviewSender | null>(REVIEW_SENDER, null);
    return () =>
      h("div", [
        h("span", { "data-testid": "file-diff" }, props.path ?? "all"),
        h("span", { "data-testid": "review-target-state" }, sender?.target ?? ""),
        h("span", { "data-testid": "review-available-targets" }, sender?.availableTargets.join(",") ?? ""),
        h("button", { "data-testid": "select-review-pi", onClick: () => sender?.selectReviewTarget("pi") }),
        h("button", { "data-testid": "diff-scroll", onClick: () => emit("scrollPositionChanged", 132) }),
        // The diff's own close button, for the same reason the document stub carries one.
        h("button", { "data-testid": "close-preview", onClick: () => emit("close") }),
        h("button", {
          "data-testid": "send-review",
          onClick: () =>
            void sender?.send(
              (mocks.reviewNotes as ReviewNote[]).map((note) => note.id),
              false,
            ),
        }),
      ]);
  },
});

// The dialog itself is not what the titlebar is judged on; what matters there is which
// lifecycle it was asked for and against which checkout, so the stub reports both.
const WorktreeDialogStub = defineComponent({
  name: "WorktreeDialog",
  props: { open: Boolean, mode: String, checkout: Object },
  setup(props) {
    return () =>
      h("div", {
        "data-testid": "worktree-dialog",
        "data-open": String(props.open),
        "data-mode": props.mode,
        "data-checkout": (props.checkout as Checkout | null)?.id ?? "",
      });
  },
});

function session(id: string, name: string, checkoutId = "checkout:one"): Session {
  return { id, type: "shell", checkoutId, name, createdAt: "now", status: "active" };
}

function checkout(id: string, sessions: Session[] = []): Checkout {
  return {
    id,
    repoId: "repo:shared",
    path: `/${id}`,
    canonicalPath: `/${id}`,
    branch: "main",
    isPrimary: id === "checkout:one",
    changedFiles: 0,
    isMissing: false,
    sessions,
  };
}

function workspaceWith(...checkouts: Checkout[]): WorkspaceState {
  const repo: Repo = {
    id: "repo:shared",
    kind: "git",
    name: "shared",
    root: "/",
    defaultBranch: "main",
    checkouts,
    createdAt: "now",
    lastOpenedAt: "now",
  };
  return { repos: [repo], activeCheckoutId: checkouts[0]?.id ?? null, activeSessionId: null };
}

/**
 * The platform this window reports, for the tests about the platform's own modifier key.
 *
 * ⌘ is the shortcut key on a Mac and Ctrl everywhere else, and it is one key or the other rather
 * than both at once, so a test that presses Ctrl has to say which machine it is on. What it says
 * is put back after the test whatever the test did, because a leaked platform answers every key
 * that follows it as if it were a Mac.
 */
const reportedPlatform = Object.getOwnPropertyDescriptor(window.navigator, "platform");

function reportsPlatform(value: string) {
  Object.defineProperty(window.navigator, "platform", { configurable: true, value });
}

afterEach(() => {
  if (reportedPlatform) Object.defineProperty(window.navigator, "platform", reportedPlatform);
  else delete (window.navigator as unknown as Record<string, unknown>).platform;
});

async function mountApp(
  workspace: WorkspaceState,
  layout: AppLayoutState = { ...DEFAULT_APP_LAYOUT },
  options: { attachTo?: HTMLElement; settings?: AppSettings; settingsLoad?: Promise<AppSettings> } = {},
) {
  mocks.initialWorkspace = workspace;
  mocks.loadAppLayout.mockResolvedValue(layout);
  if (options.settingsLoad) mocks.loadSettings.mockReturnValue(options.settingsLoad);
  else mocks.loadSettings.mockResolvedValue(options.settings ?? cloneSettings(DEFAULT_SETTINGS));
  const wrapper = mount(App, {
    attachTo: options.attachTo,
    global: {
      stubs: {
        Sidebar: SidebarStub,
        SessionPane: SessionPaneStub,
        InspectorPane: InspectorPaneStub,
        DocumentPane: DocumentPaneStub,
        FileDiff: FileDiffStub,
        WorktreeDialog: WorktreeDialogStub,
      },
    },
  });
  await flushPromises();
  await vi.advanceTimersByTimeAsync(300);
  await flushPromises();
  mocks.saveAppLayout.mockClear();
  mocks.saveSettings.mockClear();
  mocks.saveCheckoutUiState.mockClear();
  return wrapper;
}

describe("App UI integration", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.resetAllMocks();
    // Muster is a macOS app, so ⌘ is the modifier these tests press, and saying it here is what
    // keeps the machine running them from deciding it: `navigator.platform` comes from the host's
    // own OS, which on a Linux runner reports Linux, and then every ⌘ in this file answers as Ctrl
    // and no shortcut fires. A test about the other platform says so itself.
    reportsPlatform("MacIntel");
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1400 });
    mocks.initialWorkspace = null;
    mocks.workspaceRef = null;
    mocks.sessionPaneMounts = 0;
    mocks.onCloseRequested = null;
    mocks.onProgrammaticPanelResize = null;
    mocks.gitStatus = null;
    mocks.reviewNotes = [];
    mocks.agentSessions = [];
    mocks.agentTargetId = null;
    mocks.completedSessionIds = [];
    mocks.followAgentRelocation = null;
    mocks.agentSessionsByCheckout = {};
    mocks.createAgentSession.mockReset();
    // The pane reports row movement separately from whether a manual `cd` was written.
    mocks.moveSession.mockReset();
    mocks.moveSession.mockResolvedValue({ moved: true, directoryChange: "not-requested" });
    mocks.dispatchReviewRound.mockReset();
    mocks.reconcileRounds.mockReset();
    mocks.flushQueuedRounds.mockReset();
    mocks.ackFinishedTurn.mockReset();
    if (mocks.turns) mocks.turns.value = 0;
    mocks.loadAppLayout.mockResolvedValue({ ...DEFAULT_APP_LAYOUT });
    mocks.loadSettings.mockResolvedValue(cloneSettings(DEFAULT_SETTINGS));
    mocks.listRecentPaths.mockResolvedValue([]);
    mocks.selectCheckout.mockImplementation(async (checkoutId: string | null) => {
      const workspace = mocks.workspaceRef;
      if (workspace) workspace.value = { ...workspace.value, activeCheckoutId: checkoutId };
      return workspace?.value;
    });
    mocks.loadCheckoutUiState.mockResolvedValue({ ...DEFAULT_CHECKOUT_UI_STATE });
    mocks.saveAppLayout.mockResolvedValue(undefined);
    mocks.saveSettings.mockResolvedValue(undefined);
    mocks.openExternalUrl.mockResolvedValue(undefined);
    mocks.saveCheckoutUiState.mockResolvedValue(undefined);
    mocks.prepareAppExit.mockResolvedValue(undefined);
    mocks.reportFrontendDiagnostic.mockResolvedValue(undefined);
    mocks.getTerminalStatus.mockResolvedValue({ state: "running", foregroundProcess: false });
    mocks.getCompatibilitySnapshot.mockResolvedValue({
      piInstalled: true,
      openCodeInstalled: true,
    });
    mocks.loadReviewTarget.mockResolvedValue("markdown");
    mocks.saveReviewTarget.mockResolvedValue(undefined);
    mocks.exportReviewMarkdown.mockResolvedValue({ path: "2026-03-14-1532.md", origin: "review-default" });
    mocks.clearReviewFolder.mockResolvedValue({ files: 0, bytes: 0 });
    mocks.getVersion.mockResolvedValue("9.9.9");
    mocks.toggleMaximize.mockResolvedValue(undefined);
    mocks.minimize.mockResolvedValue(undefined);
    // A window with a frame of its own is the case the app was written against, so it is what
    // every test starts from; the ones that care about the frameless window say so.
    mocks.isDecorated.mockResolvedValue(true);
    mocks.isMaximized.mockResolvedValue(false);
    mocks.onWindowResized = null;
    mocks.currentWindow = {
      onCloseRequested: vi.fn(async (handler) => {
        mocks.onCloseRequested = handler;
        return vi.fn();
      }),
      close: vi.fn(async () => undefined),
    };
  });

  afterEach(() => {
    // The toast stack is a module singleton on purpose (A.6), so a message one test raises is
    // still up for the next one. Each test starts with an empty stack.
    const { toasts } = useToasts();
    toasts.value = [];
    // The scale is written to the document rather than to the app, so it outlives the component
    // that set it and would be the next test's starting point.
    document.documentElement.style.removeProperty("zoom");
    document.documentElement.style.removeProperty("--muster-ui-font-scale");
    delete document.documentElement.dataset.theme;
    vi.useRealTimers();
  });

  function reviewNoteFixture(): ReviewNote {
    return {
      id: "note:1",
      checkoutId: "checkout:one",
      path: "src/foo.js",
      side: "new",
      lineStart: 10,
      lineEnd: null,
      content: "revisit this calculation",
      code: "const result = a + b;",
      codeHash: "0000000000000001",
      outdated: false,
      roundId: null,
      status: "draft",
      createdAt: "1",
      updatedAt: "1",
    };
  }

  function agentSessionFixture(id: string, updatedAt: number) {
    return {
      id,
      checkoutId: "checkout:one",
      provider: "opencode" as const,
      title: "review",
      busy: false,
      idleAt: 1,
      awaitingReply: false,
      createdAt: 1,
      updatedAt,
    };
  }

  describe("titlebar", () => {
    it("leaves an empty spacer for the window to drag, and the double click to the window", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      const spacer = wrapper.get("[data-tauri-drag-region]");
      expect(spacer.text()).toBe("");
      expect(spacer.attributes("aria-hidden")).toBe("true");
      // The window reads the double click off the drag region itself. Answering it here as
      // well toggled twice, so the window grew and came straight back.
      await spacer.trigger("dblclick");

      expect(mocks.toggleMaximize).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("leaves the window's controls to the frame when the window has one", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      expect(wrapper.find('[data-testid="window-close"]').exists()).toBe(false);
      // The room macOS's traffic lights need is left alone.
      expect(wrapper.get("header").classes()).toContain("pl-[78px]");
      wrapper.unmount();
    });

    it("draws minimize, maximize and close on a window that has no frame of its own", async () => {
      mocks.isDecorated.mockResolvedValue(false);
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      // No frame means no traffic lights, so the space they took goes to the crumbs.
      expect(wrapper.get("header").classes()).toContain("pl-3");
      await wrapper.get('[data-testid="window-minimize"]').trigger("click");
      expect(mocks.minimize).toHaveBeenCalledOnce();
      await wrapper.get('[data-testid="window-maximize"]').trigger("click");
      expect(mocks.toggleMaximize).toHaveBeenCalledOnce();
      await flushPromises();

      await wrapper.get('[data-testid="window-close"]').trigger("click");
      await flushPromises();
      expect(mocks.currentWindow!.close).toHaveBeenCalledOnce();
      wrapper.unmount();
    });

    it("names the maximize control after what the window is, and follows the desktop", async () => {
      mocks.isDecorated.mockResolvedValue(false);
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      expect(wrapper.get('[data-testid="window-maximize"]').attributes("aria-label")).toBe("Maximize window");

      // The desktop maximizes the window on its own, on a double click over the drag region or
      // on its own shortcut, and the control has to offer to restore rather than to maximize.
      mocks.isMaximized.mockResolvedValue(true);
      mocks.onWindowResized!();
      await flushPromises();

      expect(wrapper.get('[data-testid="window-maximize"]').attributes("aria-label")).toBe("Restore window");
      wrapper.unmount();
    });

    it("waits for the queued persistence before closing from its own control", async () => {
      mocks.isDecorated.mockResolvedValue(false);
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      let resolveWrite!: () => void;
      mocks.saveAppLayout.mockImplementation(() => new Promise<void>((resolve) => (resolveWrite = resolve)));
      wrapper.getComponent(SplitterGroup).vm.$emit("layout", [320, 700, 300]);
      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();

      await wrapper.get('[data-testid="window-close"]').trigger("click");
      await flushPromises();
      expect(mocks.currentWindow!.close).not.toHaveBeenCalled();

      resolveWrite();
      await flushPromises();
      expect(mocks.currentWindow!.close).toHaveBeenCalledOnce();
      wrapper.unmount();
    });

    it("ends the processes the app started before the window goes", async () => {
      mocks.isDecorated.mockResolvedValue(false);
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      const order: string[] = [];
      mocks.prepareAppExit.mockImplementation(async () => {
        order.push("sweep");
      });
      mocks.currentWindow!.close = vi.fn(async () => {
        order.push("close");
      });

      await wrapper.get('[data-testid="window-close"]').trigger("click");
      await flushPromises();

      // The sweep has to happen while the window is still up to ask for it: no exit event reaches
      // the app on this path, so nothing else ends the servers the app started.
      expect(order).toEqual(["sweep", "close"]);
      wrapper.unmount();
    });

    it("closes even when the sweep before it fails", async () => {
      mocks.isDecorated.mockResolvedValue(false);
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      mocks.prepareAppExit.mockRejectedValue(new Error("the sweep did not finish"));

      await wrapper.get('[data-testid="window-close"]').trigger("click");
      await flushPromises();

      // A sweep that failed is a server left running, not a window the user cannot close.
      expect(mocks.currentWindow!.close).toHaveBeenCalledOnce();
      wrapper.unmount();
    });

    it("closes anyway when a queued write never finishes", async () => {
      mocks.isDecorated.mockResolvedValue(false);
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      // A write that answers nothing is what a stuck bridge looks like from here, and a window
      // that waits on it forever is a window the user cannot close.
      mocks.saveAppLayout.mockImplementation(() => new Promise<void>(() => {}));
      wrapper.getComponent(SplitterGroup).vm.$emit("layout", [320, 700, 300]);
      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();

      await wrapper.get('[data-testid="window-close"]').trigger("click");
      await flushPromises();
      expect(mocks.currentWindow!.close).not.toHaveBeenCalled();

      await vi.advanceTimersByTimeAsync(5000);
      await flushPromises();
      expect(mocks.currentWindow!.close).toHaveBeenCalledOnce();
      wrapper.unmount();
    });

    it("asks again when the last close did not go through", async () => {
      mocks.isDecorated.mockResolvedValue(false);
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      const refused = vi.fn(async () => {
        throw new Error("the window would not close");
      });
      mocks.currentWindow!.close = refused;

      await wrapper.get('[data-testid="window-close"]').trigger("click");
      await flushPromises();
      expect(refused).toHaveBeenCalledOnce();

      // A close that failed cannot keep the window from being asked to close again.
      const accepted = vi.fn(async () => undefined);
      mocks.currentWindow!.close = accepted;
      await wrapper.get('[data-testid="window-close"]').trigger("click");
      await flushPromises();
      expect(accepted).toHaveBeenCalledOnce();
      wrapper.unmount();
    });

    it("derives the crumbs from the active checkout, branch and session", async () => {
      const wrapper = await mountApp(
        workspaceWith(
          checkout("checkout:one", [session("session:one", "Terminal 1"), session("session:two", "Neovim")]),
        ),
      );

      expect(wrapper.get('[data-testid="repo-crumb"]').text()).toBe("shared");
      expect(wrapper.get("nav").text()).toContain("main");
      // No session is selected yet, so the titlebar names the last one of the workdir.
      expect(wrapper.get('[data-testid="item-crumb"]').text()).toBe("Neovim");
      wrapper.unmount();
    });

    it("wears the icon the sidebar gives the row each crumb names", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "Terminal 1")])));

      // The line is the sidebar read sideways, so a worktree and a terminal wear the two glyphs
      // their rows wear over there. The workdir wears nothing: it is the one name that is always
      // there, and a glyph on it would only repeat a word that never changes.
      expect(wrapper.get('[data-testid="repo-crumb"]').find(".crumb-icon").exists()).toBe(false);
      expect(wrapper.get('[data-testid="worktree-crumb"]').findComponent(WORKDIR_ICONS.worktree).exists()).toBe(true);
      expect(wrapper.get('[data-testid="item-crumb"]').findComponent(WORKDIR_ICONS.terminal).exists()).toBe(true);
      wrapper.unmount();
    });

    it("wears the file's own icon on the step that names it, and on the probe that weighs the path", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      await wrapper.get('[data-testid="open-nested-file"]').trigger("click");
      await flushPromises();

      // The one icon in the crumb, and it is the last thing before the file it names: an icon at
      // the head of the path would name the first directory rather than what is open.
      const crumb = wrapper.get('[data-testid="item-crumb"]');
      expect(crumb.findAll(".crumb-icon")).toHaveLength(1);
      // The glyph stands immediately before the name it belongs to, and after the separator.
      expect(crumb.element.querySelector(".crumb-icon-slot")?.nextSibling?.textContent).toBe("one.ts");
      expect(crumb.text()).toBe("src/lib/one.ts");
      // The probe is the crumb at the width it wants, glyph included: a path measured without it
      // would run past the line before it was told to give anything up.
      expect(wrapper.get('[data-testid="path-probe"]').findComponent(FileIcon).exists()).toBe(true);
      wrapper.unmount();
    });

    it("leaves the last crumb bare when what it names is not a file", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "zsh")])));
      await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
      await flushPromises();

      // The whole change set is not one file, so there is no file's icon to wear.
      const crumb = wrapper.get('[data-testid="item-crumb"]');
      expect(crumb.text()).toBe("All changes");
      expect(crumb.find("svg").exists()).toBe(false);
      wrapper.unmount();
    });

    it("opens every open workdir from the first crumb, and takes the one that is picked", async () => {
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one", [session("session:one", "Terminal 1")]), {
          ...checkout("checkout:two"),
          branch: "feature",
        }),
      );

      const rows = wrapper.findAll('[data-testid^="menu-item-workdir:"]');
      // Both checkouts of the repo, named the way the sidebar names them.
      expect(rows.map((row) => row.text())).toEqual(["mainshared", "featureshared"]);

      await wrapper.get('[data-testid="menu-item-workdir:checkout:two"]').trigger("click");
      await flushPromises();

      expect(mocks.workspaceRef?.value.activeCheckoutId).toBe("checkout:two");
      wrapper.unmount();
    });

    it("lists a folder opened before under the workdirs, and opens it again", async () => {
      mocks.listRecentPaths.mockResolvedValue([
        { canonicalPath: "/Trabajo/skills", lastOpenedAt: "1" },
        // Already open in this window, so it belongs to This Window and not here.
        { canonicalPath: "/checkout:one", lastOpenedAt: "2" },
      ]);
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      await flushPromises();

      expect(wrapper.text()).toContain("Recent Projects");
      const recent = wrapper.get('[data-testid="menu-item-recent:/Trabajo/skills"]');
      expect(recent.text()).toContain("skills");

      await recent.trigger("click");
      await flushPromises();

      expect(mocks.openPath).toHaveBeenCalledWith("/Trabajo/skills");
      wrapper.unmount();
    });

    it("offers no recents group when nothing has been opened before", async () => {
      mocks.listRecentPaths.mockResolvedValue([]);
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      await flushPromises();

      expect(wrapper.text()).not.toContain("Recent Projects");
      wrapper.unmount();
    });

    it("opens the worktrees of the repo from the branch crumb, and creates one from the root", async () => {
      const wrapper = await mountApp(
        workspaceWith(
          { ...checkout("checkout:one"), canonicalPath: "/Users/test/.docker" },
          { ...checkout("checkout:two"), canonicalPath: "/Users/test/worktrees/feature", branch: "feature" },
        ),
      );

      const rows = wrapper.findAll('[data-testid^="menu-item-worktree:"]');
      // Branch names stay visible while canonical checkout paths remain available in the tooltip.
      expect(rows.map((row) => row.text())).toEqual(["main", "feature"]);
      expect(rows.map((row) => row.attributes("title"))).toEqual([
        "/Users/test/.docker",
        "/Users/test/worktrees/feature",
      ]);

      await wrapper.get('[data-testid="menu-item-new-worktree"]').trigger("click");
      await flushPromises();

      const dialog = wrapper.get('[data-testid="worktree-dialog"]');
      expect(dialog.attributes()).toMatchObject({ "data-open": "true", "data-mode": "create" });
      // The worktree is added to the repo's root, which is not the worktree that was picked.
      expect(dialog.attributes("data-checkout")).toBe("checkout:one");
      wrapper.unmount();
    });

    it("has no branch crumb to open when the workdir is a plain folder", async () => {
      const plain: WorkspaceState = {
        repos: [
          {
            id: "repo:notes",
            kind: "plain",
            name: "notes",
            root: "/notes",
            checkouts: [{ ...checkout("checkout:one"), repoId: "repo:notes", path: "/notes", canonicalPath: "/notes" }],
            createdAt: "now",
            lastOpenedAt: "now",
          },
        ],
        activeCheckoutId: "checkout:one",
        activeSessionId: null,
      };
      const wrapper = await mountApp(plain);

      // The workdir crumb is still there to open, and the line has nothing to say a branch.
      expect(wrapper.find('[data-testid="repo-crumb"]').exists()).toBe(true);
      expect(wrapper.find('[data-testid="worktree-crumb"]').exists()).toBe(false);
      expect(wrapper.findAll('[data-testid^="menu-item-worktree:"]')).toHaveLength(0);
      wrapper.unmount();
    });

    it("opens the terminals of the workdir from the last crumb, and starts a new one", async () => {
      const wrapper = await mountApp(
        workspaceWith(
          checkout("checkout:one", [session("session:one", "Terminal 1"), session("session:two", "Neovim")]),
          checkout("checkout:two", [session("session:three", "Other", "checkout:two")]),
        ),
      );

      // Only the workdir's own terminals, not the ones of the checkout next to it.
      const rows = wrapper.findAll('[data-testid^="menu-item-session:"]');
      expect(rows.map((row) => row.text())).toEqual(["Terminal 1", "Neovim"]);

      await rows[0]!.trigger("click");
      await flushPromises();

      expect(mocks.workspaceRef?.value.activeSessionId).toBe("session:one");
      expect(wrapper.get('[data-testid="item-crumb"]').text()).toBe("Terminal 1");

      // The action at the foot asks the backend for a shell in this same workdir. The crumb
      // keeps naming the session that is selected, which is the one the new shell joins.
      await wrapper.get('[data-testid="menu-item-new-terminal"]').trigger("click");
      await flushPromises();
      expect(mocks.selectCheckout).toHaveBeenCalledWith("checkout:one");
      wrapper.unmount();
    });

    it("names the open terminal the way the sidebar row names it", async () => {
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one", [session("session:one", "zsh"), session("session:two", "Neovim")])),
      );

      await wrapper.get('[data-testid="menu-item-session:one"]').trigger("click");
      await flushPromises();

      // Nothing is in front of the shell yet, so the crumb is on the name the database holds.
      expect(wrapper.get('[data-testid="item-crumb"]').text()).toBe("zsh");

      wrapper.getComponent({ name: "SessionPane" }).vm.$emit("sessionStatusChanged", "session:one", {
        state: "running",
        foregroundProcess: true,
        foregroundApp: "opencode",
        terminalTitle: "OpenCode: review task",
      });
      await flushPromises();

      // The crumb and the row that opens it are one list read twice, so the title the program set
      // is what the header says as well — and the menu offers the names the sidebar is showing.
      expect(wrapper.get('[data-testid="item-crumb"]').text()).toBe("OpenCode: review task");
      expect(wrapper.get('[data-testid="item-crumb"]').attributes("title")).toBe("OpenCode: review task");
      expect(wrapper.findAll('[data-testid^="menu-item-session:"]').map((row) => row.text())).toEqual([
        "OpenCode: review task",
        "Neovim",
      ]);
      wrapper.unmount();
    });

    it("keeps the sidebar row and the main panel on the same terminal after one is closed", async () => {
      // The case this exists for. Closing the selected terminal clears `active_session_id` in the
      // database, so the row and the pane were each asked on their own: the row answered "nothing is
      // selected" and the pane fell through to the terminal it opened last. Same panel, two answers.
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one", [session("session:one", "zsh"), session("session:two", "Neovim")])),
      );
      mocks.workspaceRef!.value = { ...mocks.workspaceRef!.value, activeSessionId: "session:two" };
      await flushPromises();

      const bothRead = () => [
        wrapper.get('[data-testid="sidebar-active-session"]').text(),
        wrapper.get('[data-testid="pane-active-session"]').text(),
      ];

      expect(bothRead()).toEqual(["session:two", "session:two"]);

      // The close: the backend clears the selection and reports the workdir without it.
      wrapper.getComponent({ name: "SessionPane" }).vm.$emit("workspaceUpdated", {
        ...mocks.workspaceRef!.value,
        repos: [
          {
            ...mocks.workspaceRef!.value.repos[0]!,
            checkouts: [
              {
                ...checkout("checkout:one"),
                sessions: [session("session:one", "zsh")],
              },
            ],
          },
        ],
        activeSessionId: null,
      });
      await flushPromises();

      // What is left is what both of them name now: the row is marked and the pane shows it, rather
      // than a row saying nothing beside a panel still showing a terminal.
      expect(mocks.workspaceRef!.value.activeSessionId).toBeNull();
      expect(bothRead()).toEqual(["session:one", "session:one"]);
      wrapper.unmount();
    });

    it("requests a shell for the launch checkout", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      mocks.launchCheckoutId!.value = "checkout:one";
      await flushPromises();

      expect(mocks.selectCheckout).toHaveBeenCalledTimes(1);
      expect(mocks.selectCheckout).toHaveBeenCalledWith("checkout:one");
      wrapper.unmount();
    });

    it("launches Home after restore when it has no terminal session", async () => {
      const homeCheckout = checkout("checkout:home");
      const homeRepo: Repo = {
        ...workspaceWith(homeCheckout).repos[0]!,
        kind: "plain",
        name: "Home",
      };
      mocks.restoreWorkspace.mockResolvedValue({
        repos: [homeRepo],
        activeCheckoutId: homeCheckout.id,
        activeSessionId: null,
        homeCheckoutId: homeCheckout.id,
      });
      const { useWorkspaceState } =
        await vi.importActual<typeof import("./presentation/workspace")>("./presentation/workspace");
      let launchCheckoutId: Ref<string | null> | undefined;
      const host = defineComponent({
        setup() {
          launchCheckoutId = useWorkspaceState().launchCheckoutId;
          return () => h("div");
        },
      });

      const wrapper = mount(host);
      await flushPromises();

      expect(mocks.restoreWorkspace).toHaveBeenCalledTimes(1);
      expect(launchCheckoutId?.value).toBe(homeCheckout.id);
      wrapper.unmount();
    });

    it("draws a file path as the steps it is made of", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "zsh")])));

      await wrapper.get('[data-testid="open-nested-file"]').trigger("click");
      await flushPromises();

      // A path is a line of crumbs already, so it reads as one: the steps with the separator
      // the rest of the line uses, and still nothing to click.
      const crumb = wrapper.get('[data-testid="item-crumb"]');
      expect(crumb.text()).toBe("src/lib/one.ts");
      expect(crumb.attributes("title")).toBe("src/lib/one.ts");
      expect(crumb.findAll("span[aria-hidden='true']").map((el) => el.text())).toEqual(["/", "/"]);
      expect(crumb.classes()).not.toContain("text-menu-control");
      wrapper.unmount();
    });

    it("writes a renamed session to the database and repaints from its answer", async () => {
      mocks.renameTerminal.mockResolvedValue(
        workspaceWith(checkout("checkout:one", [{ ...session("session:one", "build logs"), name: "build logs" }])),
      );
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "zsh")])));

      await wrapper.get('[data-testid="rename-session-one"]').trigger("click");
      await flushPromises();

      // The row is renamed against the checkout it is listed under, never against a name the
      // caller made up: the id and the name are the two things the command needs.
      expect(mocks.renameTerminal).toHaveBeenCalledWith("checkout:one", "session:one", "build logs");
      // The workspace comes back from the rename, so the sidebar paints the name the database
      // now holds rather than the one that was asked for.
      expect(mocks.workspaceRef?.value.activeCheckoutId).toBe("checkout:one");
      expect(mocks.workspaceRef?.value.repos[0]!.checkouts[0]!.sessions[0]!.name).toBe("build logs");
      wrapper.unmount();
    });

    it("reports a rename the backend refuses instead of painting it anyway", async () => {
      mocks.renameTerminal.mockRejectedValue(new Error("a session needs a name"));
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "zsh")])));

      await wrapper.get('[data-testid="rename-session-one"]').trigger("click");
      await flushPromises();

      // The row keeps the name it had, and the refusal is said out loud rather than swallowed.
      const { toasts } = useToasts();
      expect(toasts.value.at(-1)?.message).toContain("a session needs a name");
      wrapper.unmount();
    });

    it("drops the middle of the path when the line has no room for it", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "zsh")])));

      await wrapper.get('[data-testid="open-nested-file"]').trigger("click");
      await flushPromises();
      const crumb = () => wrapper.get('[data-testid="item-crumb"]');
      expect(crumb().text()).toBe("src/lib/one.ts");

      // The path at the width it wants, wider than any room the line could give it. The probe
      // is what is weighed, so this is the whole decision in one number.
      const probe = wrapper.get('[data-testid="path-probe"]').element;
      Object.defineProperty(probe, "scrollWidth", { configurable: true, value: 400 });
      window.dispatchEvent(new Event("resize"));
      await wrapper.vm.$nextTick();

      // What goes is the middle: the first directory says where you are, the file name is what
      // you came to see, and the tooltip still has all of it.
      expect(crumb().text()).toBe("src/…/one.ts");
      expect(
        crumb()
          .findAll("span[aria-hidden='true']")
          .map((el) => el.text()),
      ).toEqual(["/", "/"]);
      expect(crumb().attributes("title")).toBe("src/lib/one.ts");
      wrapper.unmount();
    });

    it("never elides a path with nothing in the middle to drop", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "zsh")])));

      await wrapper.get('[data-testid="open-file"]').trigger("click");
      await flushPromises();
      // One step has no middle, so there is nothing to shorten it by.
      expect(wrapper.find('[data-testid="path-probe"]').exists()).toBe(false);
      expect(wrapper.get('[data-testid="item-crumb"]').text()).toBe("README.md");
      wrapper.unmount();
    });

    it("leaves the last crumb out when there is no terminal to name", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      // A crumb that reads "Terminal" over a panel that says it has none is naming nothing, and
      // neither is the separator that would come before it.
      const separators = () => wrapper.findAll("nav > span[aria-hidden='true']");
      expect(wrapper.find('[data-testid="item-crumb"]').exists()).toBe(false);
      expect(separators()).toHaveLength(1);

      // A workdir that has one names it, and the sessions are what the menu then lists.
      const withSession = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "zsh")])));
      expect(withSession.get('[data-testid="item-crumb"]').text()).toBe("zsh");
      expect(withSession.findAll("nav > span[aria-hidden='true']")).toHaveLength(2);
      expect(withSession.find('[data-testid="menu-item-session:one"]').exists()).toBe(true);
      wrapper.unmount();
      withSession.unmount();
    });

    it("keeps one crumb menu open at a time", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "zsh")])));

      const roots = () => wrapper.findAllComponents({ name: "DropdownMenuRoot" });
      const openNames = () => roots().map((root) => root.attributes("data-open"));

      // Nothing opens on its own. Each root reports that it opened and the shell names the one
      // that is, so the three menus can never be stacked on the same header.
      expect(openNames()).toEqual(["false", "false", "false"]);
      await roots()[0]!.vm.$emit("update:open", true);
      await flushPromises();
      expect(openNames()).toEqual(["true", "false", "false"]);

      await roots()[1]!.vm.$emit("update:open", true);
      await flushPromises();
      expect(openNames()).toEqual(["false", "true", "false"]);
      wrapper.unmount();
    });

    it("offers a new terminal from the menu of a workdir that has one open", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "zsh")])));

      expect(wrapper.find('[data-testid="menu-item-session:one"]').exists()).toBe(true);
      expect(wrapper.find('[data-testid="menu-item-new-terminal"]').exists()).toBe(true);
      wrapper.unmount();
    });

    it("leaves the crumbs as text: no chip, no chevron", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "Terminal 1")])));

      for (const testid of ["repo-crumb", "worktree-crumb", "item-crumb"]) {
        const crumb = wrapper.get(`[data-testid="${testid}"]`);
        expect(crumb.classes()).toContain("text-menu-control");
        expect(crumb.classes()).not.toContain("muster-control");
      }
      // The workdir is the one that says where you are, so it is the only crumb set forward, and
      // each crumb carries its own place in the line, which is what says what it gives up.
      expect(wrapper.get('[data-testid="repo-crumb"]').classes()).toContain("crumb-workdir");
      expect(wrapper.get('[data-testid="worktree-crumb"]').classes()).toContain("crumb-branch");
      expect(wrapper.get('[data-testid="item-crumb"]').classes()).toContain("crumb-item");

      // The glyph each crumb wears is the one its row wears in the sidebar, and it is the only
      // thing on the crumb but the name: no chip, no chevron, and nothing drawn between two
      // crumbs that belongs to neither of them.
      expect(wrapper.get('[data-testid="worktree-crumb"]').findAll(".crumb-icon")).toHaveLength(1);
      expect(wrapper.get('[data-testid="item-crumb"]').findAll(".crumb-icon")).toHaveLength(1);
      expect(wrapper.get('[data-testid="repo-crumb"]').findAll(".crumb-icon")).toHaveLength(0);
      wrapper.unmount();
    });

    it("names the settings gear for a screen reader", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      const settings = wrapper.get('[data-testid="settings-button"]');
      expect(settings.attributes("aria-label")).toBe("Settings");
      expect(settings.find("svg").exists()).toBe(true);
      // The dialog itself is asserted in its own block; what belongs here is that the gear opens
      // one at all rather than sitting there.
      await settings.trigger("click");
      expect(wrapper.find('[role="dialog"]').exists()).toBe(true);
      wrapper.unmount();
    });

    it("renders no error banner", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      expect(wrapper.find('[role="alert"]').exists()).toBe(false);
      wrapper.unmount();
    });
  });

  describe("a checkout whose directory is gone", () => {
    it("confirms the scope each close reaches, and closes only what was confirmed", async () => {
      const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one"), checkout("checkout:two")));

      // The repo root is the head of a list, so closing it reaches the worktrees with it.
      await wrapper.get('[data-testid="close-missing-base"]').trigger("click");
      expect(confirm).toHaveBeenLastCalledWith(
        "Close “/checkout:one” and its checkout list in Muster? No files will be deleted.",
      );
      // A worktree is one entry, and nothing is closed while the question is unanswered.
      await wrapper.get('[data-testid="close-missing-worktree"]').trigger("click");
      expect(confirm).toHaveBeenLastCalledWith("Close “/checkout:two” in Muster? No files will be deleted.");
      expect(mocks.closeMissingCheckout).not.toHaveBeenCalled();

      confirm.mockReturnValue(true);
      mocks.closeMissingCheckout.mockResolvedValue({ repos: [], activeCheckoutId: null, activeSessionId: null });
      await wrapper.get('[data-testid="close-missing-worktree"]').trigger("click");
      await flushPromises();

      expect(mocks.closeMissingCheckout).toHaveBeenCalledWith("checkout:two");
      expect(mocks.workspaceRef?.value.repos).toEqual([]);
      wrapper.unmount();
      confirm.mockRestore();
    });

    it("names a plain folder as losing its checkout list as well", async () => {
      const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
      const plain: WorkspaceState = {
        repos: [
          {
            id: "repo:notes",
            kind: "plain",
            name: "notes",
            root: "/notes",
            checkouts: [
              {
                // A plain folder is one primary checkout, so the stub's base entry is it.
                ...checkout("checkout:one"),
                repoId: "repo:notes",
                path: "/notes",
                canonicalPath: "/notes",
                branch: undefined,
              },
            ],
            createdAt: "now",
            lastOpenedAt: "now",
          },
        ],
        activeCheckoutId: "checkout:one",
        activeSessionId: null,
      };
      mocks.closeMissingCheckout.mockResolvedValue({ repos: [], activeCheckoutId: null, activeSessionId: null });
      const wrapper = await mountApp(plain);

      await wrapper.get('[data-testid="close-missing-base"]').trigger("click");

      expect(confirm).toHaveBeenCalledWith("Close “/notes” and its checkout list in Muster? No files will be deleted.");
      expect(mocks.closeMissingCheckout).toHaveBeenCalledWith("checkout:one");
      wrapper.unmount();
      confirm.mockRestore();
    });
  });

  describe("an OpenCode session that moved to another worktree", () => {
    /**
     * The session a terminal is showing: the id a move names, the title its TUI wrote, and the
     * program it belongs to.
     *
     * The provider is what scopes the title inference to OpenCode, so a stub without it names no
     * session at all and every move in this block would be refused for the wrong reason.
     */
    const agentSession = (title: string) => ({ id: `ses_${title}`, title, provider: "opencode" });
    const move = (
      sessionId: string,
      fromCheckoutId = "checkout:one",
      toCheckoutId = "checkout:two",
      observedAt = Date.now(),
    ) => mocks.followAgentRelocation!({ sessionId, fromCheckoutId, toCheckoutId, observedAt });

    /**
     * One terminal per worktree, with `name` in front of the shell in each checkout.
     *
     * Every OpenCode terminal is given the next title `agentTitles` holds for its worktree, and the
     * row for that worktree lists the same session under that title: the title is the only thing
     * that says which session a terminal has open, so a test that moves a session this terminal is
     * not showing has to be able to say so.
     */
    async function mountWithOpenCodeTerminals(
      sessionsByCheckout: Record<string, string[]>,
      agentTitles: Record<string, string[]> = {},
      options: {
        activeSessionId?: string | null;
        settings?: AppSettings;
        separateRepos?: Record<string, Repo["kind"]>;
      } = {},
    ): Promise<ReturnType<typeof mountApp> extends Promise<infer T> ? T : never> {
      const checkouts = Object.entries(sessionsByCheckout).map(([id, names]) =>
        checkout(
          id,
          names.map((name) => session(`session:${name}`, name, id)),
        ),
      );
      mocks.agentSessionsByCheckout = Object.fromEntries(
        Object.entries(agentTitles).map(([checkoutId, titles]) => [checkoutId, titles.map(agentSession)]),
      );
      const workspace = workspaceWith(...checkouts);
      for (const [checkoutId, kind] of Object.entries(options.separateRepos ?? {})) {
        const shared = workspace.repos[0];
        const moved = shared.checkouts.find((candidate) => candidate.id === checkoutId)!;
        shared.checkouts = shared.checkouts.filter((candidate) => candidate.id !== checkoutId);
        moved.repoId = `repo:${checkoutId}`;
        workspace.repos.push({ ...shared, id: moved.repoId, kind, root: moved.path, checkouts: [moved] });
      }
      workspace.activeSessionId = options.activeSessionId ?? workspace.activeSessionId;
      const wrapper = await mountApp(workspace, undefined, {
        settings: options.settings,
      });
      for (const [checkoutId, names] of Object.entries(sessionsByCheckout)) {
        let next = 0;
        for (const name of names) {
          const isAgent = name !== "shell";
          wrapper.getComponent({ name: "SessionPane" }).vm.$emit("sessionStatusChanged", `session:${name}`, {
            state: "running",
            foregroundProcess: true,
            foregroundApp: isAgent ? "opencode" : "zsh",
            terminalTitle: isAgent ? `OC | ${agentTitles[checkoutId]?.[next++] ?? name}` : null,
          });
        }
      }
      await flushPromises();
      mocks.moveSession.mockClear();
      return wrapper;
    }

    it.each(["checkout:one", "checkout:two"] as const)(
      "keeps a new terminal in %s selected when an automatic move replies late",
      async (shellCheckoutId) => {
        const wrapper = await mountWithOpenCodeTerminals(
          { "checkout:one": ["agent"], "checkout:two": [], "checkout:three": [] },
          { "checkout:one": ["ship it"] },
          { activeSessionId: "session:agent" },
        );
        let finishMove: (result: {
          moved: boolean;
          directoryChange: "not-requested" | "written" | "not-written";
        }) => void = () => {};
        mocks.moveSession.mockImplementationOnce(() => new Promise((resolve) => (finishMove = resolve)));

        move("ses_ship it", "checkout:one", "checkout:three");
        await flushPromises();
        const oldWorkspace = mocks.workspaceRef!.value;
        const sidebar = wrapper.getComponent({ name: "SidebarStub" });
        sidebar.vm.$emit("newTerminal", shellCheckoutId);
        await flushPromises();
        const main = wrapper.getComponent({ name: "MainPane" });
        const pane = wrapper.getComponent({ name: "SessionPane" });
        const shellRequest = main.props("shellRequest") as { checkoutId: string; token: number };
        const createdSession = session("session:new", "new shell", shellCheckoutId);
        const createdWorkspace: WorkspaceState = {
          ...oldWorkspace,
          activeCheckoutId: shellCheckoutId,
          activeSessionId: createdSession.id,
          repos: oldWorkspace.repos.map((repo) => ({
            ...repo,
            checkouts: repo.checkouts.map((item) =>
              item.id === shellCheckoutId ? { ...item, sessions: [...item.sessions, createdSession] } : item,
            ),
          })),
        };
        pane.vm.$emit("workspaceUpdated", createdWorkspace, shellRequest.token);
        await flushPromises();
        pane.vm.$emit("shellCreated", shellRequest.token, shellCheckoutId, createdSession.id);
        await flushPromises();

        // The older move snapshot has neither the new selection nor its session row.
        main.vm.$emit("workspaceUpdated", {
          ...oldWorkspace,
          activeCheckoutId: "checkout:one",
          activeSessionId: "session:agent",
        });
        await flushPromises();
        finishMove({ moved: true, directoryChange: "not-requested" });
        await flushPromises();

        expect(mocks.workspaceRef!.value.activeCheckoutId).toBe(shellCheckoutId);
        expect(mocks.workspaceRef!.value.activeSessionId).toBe(createdSession.id);
        expect(main.props("checkout")).toMatchObject({ id: shellCheckoutId });
        expect(main.props("activeSessionId")).toBe(createdSession.id);
        expect(main.props("registeredSessionIds")).toContain(createdSession.id);
        expect(mocks.focusActiveTerminal).not.toHaveBeenCalled();
        wrapper.unmount();
      },
    );

    it("ignores older shell persistence and creation responses", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one"), checkout("checkout:two")));
      let finishFirst: (workspace: WorkspaceState) => void = () => {};
      mocks.selectCheckout.mockImplementationOnce(() => new Promise((resolve) => (finishFirst = resolve)));
      const sidebar = wrapper.getComponent({ name: "SidebarStub" });
      const main = wrapper.getComponent({ name: "MainPane" });
      const pane = wrapper.getComponent({ name: "SessionPane" });

      sidebar.vm.$emit("newTerminal", "checkout:one");
      await flushPromises();
      sidebar.vm.$emit("newTerminal", "checkout:two");
      await flushPromises();
      expect(main.props("shellRequest")).toEqual({ checkoutId: "checkout:two", token: 2 });

      const staleSession = session("session:old-shell", "old shell");
      const staleWorkspace = mocks.workspaceRef!.value;
      pane.vm.$emit(
        "workspaceUpdated",
        {
          ...staleWorkspace,
          activeCheckoutId: "checkout:one",
          activeSessionId: staleSession.id,
          repos: staleWorkspace.repos.map((repo) => ({
            ...repo,
            checkouts: repo.checkouts.map((item) =>
              item.id === "checkout:one" ? { ...item, sessions: [...item.sessions, staleSession] } : item,
            ),
          })),
        },
        1,
      );
      await flushPromises();
      pane.vm.$emit("shellCreated", 1, "checkout:one", staleSession.id);
      await flushPromises();
      expect(mocks.workspaceRef!.value.activeCheckoutId).toBe("checkout:two");
      expect(mocks.workspaceRef!.value.activeSessionId).toBeNull();

      finishFirst({ ...mocks.workspaceRef!.value, activeCheckoutId: "checkout:one", activeSessionId: null });
      await flushPromises();
      expect(main.props("shellRequest")).toEqual({ checkoutId: "checkout:two", token: 2 });
      expect(mocks.workspaceRef!.value.activeCheckoutId).toBe("checkout:two");

      const createdSession = session("session:new", "new shell", "checkout:two");
      const current = mocks.workspaceRef!.value;
      pane.vm.$emit(
        "workspaceUpdated",
        {
          ...current,
          activeSessionId: createdSession.id,
          repos: current.repos.map((repo) => ({
            ...repo,
            checkouts: repo.checkouts.map((item) =>
              item.id === "checkout:two" ? { ...item, sessions: [...item.sessions, createdSession] } : item,
            ),
          })),
        },
        2,
      );
      await flushPromises();
      pane.vm.$emit("shellCreated", 2, "checkout:two", createdSession.id);
      await flushPromises();
      pane.vm.$emit(
        "workspaceUpdated",
        {
          ...staleWorkspace,
          activeCheckoutId: "checkout:one",
          activeSessionId: staleSession.id,
          repos: staleWorkspace.repos.map((repo) => ({
            ...repo,
            checkouts: repo.checkouts.map((item) =>
              item.id === "checkout:one" ? { ...item, sessions: [...item.sessions, staleSession] } : item,
            ),
          })),
        },
        1,
      );
      await flushPromises();
      expect(mocks.workspaceRef!.value.activeCheckoutId).toBe("checkout:two");
      expect(mocks.workspaceRef!.value.activeSessionId).toBe(createdSession.id);
      expect(main.props("registeredSessionIds")).toContain(createdSession.id);
      wrapper.unmount();
    });

    it("moves the one terminal whose OpenCode went, and takes the window with it when it was on screen", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["shell", "agent"], "checkout:two": [] },
        { "checkout:one": ["ship it"] },
        { activeSessionId: "session:agent" },
      );

      // OpenCode moved its own session; the shell behind it is still sitting in the old worktree
      // and must not be told to `cd`, because there is no prompt there to read a `cd`. This
      // terminal was the one on screen, so the window follows it rather than leaving an empty pane.
      move("ses_ship it");
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledWith("session:agent", "checkout:two", 0, false, true);
      wrapper.unmount();
    });

    it("keeps a terminal selected while a visible terminal's move is in flight", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["agent"], "checkout:two": ["two"] },
        { "checkout:one": ["ship it"], "checkout:two": ["still working"] },
        { activeSessionId: "session:agent" },
      );
      let finishMove: (result: {
        moved: boolean;
        directoryChange: "not-requested" | "written" | "not-written";
      }) => void = () => {};
      mocks.moveSession.mockImplementationOnce(() => new Promise((resolve) => (finishMove = resolve)));

      move("ses_ship it");
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledWith("session:agent", "checkout:two", 0, false, true);

      await wrapper.get('[data-testid="select-session-two"]').trigger("click");
      await flushPromises();
      expect(wrapper.get('[data-testid="sidebar-active-session"]').text()).toBe("session:two");

      const beforeMoveResponse = mocks.workspaceRef!.value;
      const movedSession = beforeMoveResponse.repos
        .flatMap((repo) => repo.checkouts)
        .find((checkout) => checkout.id === "checkout:one")!
        .sessions.find((session) => session.id === "session:agent")!;
      const moveResponse: WorkspaceState = {
        ...beforeMoveResponse,
        activeCheckoutId: "checkout:two",
        activeSessionId: "session:agent",
        repos: beforeMoveResponse.repos.map((repo) => ({
          ...repo,
          checkouts: repo.checkouts.map((checkout) => {
            if (checkout.id === "checkout:one")
              return { ...checkout, sessions: checkout.sessions.filter((session) => session.id !== "session:agent") };
            if (checkout.id === "checkout:two")
              return { ...checkout, sessions: [...checkout.sessions, { ...movedSession, checkoutId: checkout.id }] };
            return checkout;
          }),
        })),
      };
      finishMove({ moved: true, directoryChange: "not-requested" });
      wrapper.getComponent({ name: "MainPane" }).vm.$emit("workspaceUpdated", moveResponse);
      await flushPromises();

      expect(wrapper.get('[data-testid="sidebar-active-session"]').text()).toBe("session:two");
      expect(wrapper.get('[data-testid="pane-active-session"]').text()).toBe("session:two");
      expect(wrapper.getComponent({ name: "MainPane" }).props("view")).toMatchObject({
        kind: "terminal",
        sessionId: "session:two",
      });
      expect(mocks.focusActiveTerminal).toHaveBeenCalledTimes(1);
      wrapper.unmount();
    });

    it("moves the row without taking the window when another terminal is on screen", async () => {
      // The person is working in the shell, not in the agent's terminal. That terminal moves in
      // the sidebar and the pane they are in stays exactly where it is.
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["shell", "agent"], "checkout:two": [] },
        { "checkout:one": ["ship it"] },
        { activeSessionId: "session:shell" },
      );

      move("ses_ship it");
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledWith("session:agent", "checkout:two", 0, false, false);
      wrapper.unmount();
    });

    it("moves the row without taking a window that is reading something else", async () => {
      // Being selected is not the same as being on screen: a document takes the whole panel in a
      // single layout, and a terminal that moves behind it has nobody waiting to be taken along.
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["shell", "agent"], "checkout:two": [] },
        { "checkout:one": ["ship it"] },
        { activeSessionId: "session:agent" },
      );
      await wrapper.get('[data-testid="open-file"]').trigger("click");

      move("ses_ship it");
      await flushPromises();

      // The row moves; the pane the reader is in does not.
      expect(mocks.moveSession).toHaveBeenCalledWith("session:agent", "checkout:two", 0, false, false);
      wrapper.unmount();
    });

    it("moves nothing for a move this terminal has already taken", async () => {
      // The service keeps a move on offer for half a minute, so the hop arrives again after the
      // terminal took it. Asking again is the backend refusing a session that is already there,
      // which is an error the person did nothing to earn.
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["shell"], "checkout:two": ["agent"] },
        { "checkout:two": ["ship it"] },
        { activeSessionId: "session:agent" },
      );

      move("ses_ship it", "checkout:one", "checkout:two");
      await flushPromises();

      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("moves the terminal when the session takes a second hop and the first is still on offer", async () => {
      // A session that moved twice while the first move was still owed for is reported as the hop
      // it last took. The terminal may have taken the first one or may never have got it, and the
      // worktree the report names is not the answer either way: the title the terminal is writing
      // is, so the terminal is found wherever it actually is.
      const wrapper = await mountWithOpenCodeTerminals(
        {
          "checkout:one": ["shell", "agent"],
          "checkout:two": [],
          "checkout:three": [],
        },
        { "checkout:one": ["ship it"] },
        { activeSessionId: "session:agent" },
      );

      move("ses_ship it", "checkout:one", "checkout:three");
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledWith("session:agent", "checkout:three", 0, false, true);
      wrapper.unmount();
    });

    it.each(["off", "cd", "agent", "both"] as const)("gates agent moves with %s", async (mode) => {
      const settings = cloneSettings(DEFAULT_SETTINGS);
      settings.terminal.followWorktree = mode;
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["shell", "agent"], "checkout:two": [] },
        { "checkout:one": ["ship it"] },
        { activeSessionId: "session:agent", settings },
      );

      move("ses_ship it");
      await flushPromises();

      if (mode === "agent" || mode === "both") {
        expect(mocks.moveSession).toHaveBeenCalledWith("session:agent", "checkout:two", 0, false, true);
      } else expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("moves nothing for a session this terminal is not showing", async () => {
      // The same service-wide list is offered to every worktree, so a session another client
      // opened in this directory is in it too. Moving this terminal on that session's move is the
      // mistake this cannot come back from: the terminal is still there, reading the wrong tree.
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["shell", "agent"], "checkout:two": [] },
        { "checkout:one": ["ship it", "theirs"] },
        { activeSessionId: "session:agent" },
      );

      move("ses_theirs");
      await flushPromises();

      expect(mocks.moveSession).not.toHaveBeenCalled();
      // The session it does show still moves it, which is what makes the refusal above a refusal
      // of that one move rather than of the feature.
      move("ses_ship it");
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledWith("session:agent", "checkout:two", 0, false, true);
      wrapper.unmount();
    });

    it("moves nothing when the terminal's own title names none of the sessions", async () => {
      // A title nothing matches is a terminal whose session this panel cannot name, which is a
      // different thing from a session nobody moved.
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["shell", "agent"], "checkout:two": [] },
        { "checkout:one": ["ship it"] },
        { activeSessionId: "session:agent" },
      );
      mocks.agentSessionsByCheckout["checkout:one"] = [agentSession("something else")];

      move("ses_ship it");
      await flushPromises();

      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("moves nothing when two sessions carry the one title this terminal shows", async () => {
      // Two terminals in one worktree can be showing one session between them, and nothing here can
      // tell that from two sessions that happen to share a name. An ambiguous title names neither.
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["shell", "agent"], "checkout:two": [] },
        { "checkout:one": ["ship it"] },
        { activeSessionId: "session:agent" },
      );
      mocks.agentSessionsByCheckout["checkout:one"] = [
        agentSession("ship it"),
        { id: "ses_duplicate", title: "ship it", provider: "opencode" },
      ];

      move("ses_ship it");
      await flushPromises();

      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("follows attributable tool work while both OpenCode sessions and their shells remain under main", async () => {
      const settings = cloneSettings(DEFAULT_SETTINGS);
      settings.terminal.followWorktree = "agent";
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["writer", "other"], "checkout:two": [] },
        { "checkout:one": ["writing in test", "still in main"] },
        { activeSessionId: "session:writer", settings },
      );
      // The backend reports the writer's tool target, not a session.location.directory change.
      // Both candidate sessions and the foreground TUIs still belong to the original directory.
      for (const terminalId of ["session:writer", "session:other"]) {
        wrapper.getComponent({ name: "SessionPane" }).vm.$emit("sessionStatusChanged", terminalId, {
          state: "running",
          foregroundProcess: true,
          foregroundApp: "opencode",
          terminalTitle: terminalId === "session:writer" ? "OC | writing in test" : "OC | still in main",
          workingDirectory: "/repo/main",
        });
      }
      move("ses_writing in test");
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledTimes(1);
      expect(mocks.moveSession).toHaveBeenCalledWith("session:writer", "checkout:two", 0, false, true);
      wrapper.unmount();
    });

    it.each([
      ["session:writer", true],
      ["session:other", false],
    ] as const)("follows the Home writer into Git with %s active", async (activeSessionId, follow) => {
      const settings = cloneSettings(DEFAULT_SETTINGS);
      settings.terminal.followWorktree = "agent";
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:home": ["writer", "other"], "checkout:one": [], "checkout:two": [] },
        { "checkout:home": ["writing test", "staying home"] },
        { activeSessionId, settings, separateRepos: { "checkout:home": "plain" } },
      );
      // OpenCode still lives in checkout:one; only its write.path names checkout:two.
      move("ses_writing test");
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledTimes(1);
      expect(mocks.moveSession).toHaveBeenCalledWith("session:writer", "checkout:two", 0, false, follow);
      expect(mocks.focusActiveTerminal).toHaveBeenCalledTimes(follow ? 1 : 0);
      wrapper.unmount();
    });

    it.each(["checkout:one", "checkout:plain"])("rejects a Home match also shown by %s", async (duplicate) => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:home": ["writer"], [duplicate]: ["duplicate"], "checkout:two": [] },
        { "checkout:home": ["writing test"], [duplicate]: ["writing test"] },
        {
          separateRepos: {
            "checkout:home": "plain",
            ...(duplicate === "checkout:plain" ? { [duplicate]: "plain" as const } : {}),
          },
        },
      );
      move("ses_writing test");
      await flushPromises();
      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("never follows an OpenCode session into a plain checkout", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["writer"], "checkout:two": [] },
        { "checkout:one": ["writing test"] },
        { separateRepos: { "checkout:two": "plain" } },
      );
      move("ses_writing test");
      await flushPromises();
      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("follows a uniquely identified TUI across Git repositories", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["writer"], "checkout:two": [] },
        { "checkout:one": ["writing test"] },
        { activeSessionId: "session:writer", separateRepos: { "checkout:two": "git" } },
      );

      move("ses_writing test");
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledWith("session:writer", "checkout:two", 0, false, true);
      wrapper.unmount();
    });

    it("acknowledges a moved relocation even when the window does not follow", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["agent"], "checkout:two": [] },
        { "checkout:one": ["ship it"] },
      );
      const observedAt = Date.now();

      move("ses_ship it", "checkout:one", "checkout:two", observedAt);
      await flushPromises();
      move("ses_ship it", "checkout:one", "checkout:two", observedAt);
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledTimes(1);
      wrapper.unmount();
    });

    it("acknowledges a relocation whose terminal is already in the destination", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": [], "checkout:two": ["agent"] },
        { "checkout:two": ["ship it"] },
      );
      const observedAt = Date.now();

      move("ses_ship it", "checkout:one", "checkout:two", observedAt);
      await flushPromises();
      move("ses_ship it", "checkout:one", "checkout:two", observedAt);
      await flushPromises();

      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("rejects the same OpenCode session shown by terminals in different repositories", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["writer"], "checkout:foreign": ["duplicate"], "checkout:two": [] },
        { "checkout:one": ["writing test"], "checkout:foreign": ["writing test"] },
        { separateRepos: { "checkout:foreign": "git", "checkout:two": "git" } },
      );

      move("ses_writing test");
      await flushPromises();

      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("moves only the one of two OpenCodes that is showing the session", async () => {
      // Two OpenCodes in one worktree are no longer a reason to give up: each wrote the title of the
      // session it has open, so the one that moved is named rather than guessed at, and the other
      // stays where it is.
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["agent", "other"], "checkout:two": [] },
        { "checkout:one": ["ship it", "second"] },
      );

      move("ses_ship it");
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledTimes(1);
      expect(mocks.moveSession).toHaveBeenCalledWith("session:agent", "checkout:two", 0, false, false);
      wrapper.unmount();
    });

    it("moves nothing when no OpenCode is running in the worktree the session left", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["shell"], "checkout:two": [] },
        { "checkout:one": ["ship it"] },
      );

      move("ses_ship it");
      await flushPromises();

      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("moves nothing when the destination is a worktree whose directory is gone", async () => {
      // A missing directory is not a tree a terminal can work in, so the backend would refuse
      // the move and the only honest outcome is not to ask.
      const missing = { ...checkout("checkout:two"), isMissing: true };
      mocks.agentSessionsByCheckout = {
        "checkout:one": [agentSession("ship it")],
      };
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one", [session("session:agent", "agent", "checkout:one")]), missing),
      );
      wrapper.getComponent({ name: "SessionPane" }).vm.$emit("sessionStatusChanged", "session:agent", {
        state: "running",
        foregroundProcess: true,
        foregroundApp: "opencode",
        terminalTitle: "OC | ship it",
      });
      await flushPromises();

      move("ses_ship it");
      await flushPromises();

      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("runs a manual move after an in-flight automatic move", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["agent"], "checkout:two": [], "checkout:three": [] },
        { "checkout:one": ["ship it"] },
        { activeSessionId: "session:agent" },
      );
      let finishAutomatic: (result: {
        moved: boolean;
        directoryChange: "not-requested" | "written" | "not-written";
      }) => void = () => {};
      mocks.moveSession.mockImplementationOnce(() => new Promise((resolve) => (finishAutomatic = resolve)));

      move("ses_ship it", "checkout:one", "checkout:two", Date.now());
      await flushPromises();
      wrapper.getComponent({ name: "SidebarStub" }).vm.$emit("moveSession", "session:agent", "checkout:three", 0);
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledTimes(1);
      expect(mocks.moveSession).toHaveBeenLastCalledWith("session:agent", "checkout:two", 0, false, true);
      finishAutomatic({ moved: true, directoryChange: "not-requested" });
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledTimes(2);
      expect(mocks.moveSession).toHaveBeenLastCalledWith("session:agent", "checkout:three", 0, true, true);
      wrapper.unmount();
    });

    it("ignores a replayed relocation after a manual move but follows a newer signal", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["agent"], "checkout:two": [], "checkout:three": [] },
        { "checkout:one": ["ship it"] },
      );
      mocks.moveSession.mockResolvedValueOnce({ moved: true, directoryChange: "written" });
      const staleObservedAt = Date.now() - 1_000;
      wrapper.getComponent({ name: "SidebarStub" }).vm.$emit("moveSession", "session:agent", "checkout:three", 0);
      await flushPromises();
      wrapper.getComponent({ name: "SessionPane" }).vm.$emit("sessionStatusChanged", "session:agent", {
        state: "running",
        foregroundProcess: true,
        foregroundApp: "opencode",
        terminalTitle: "OC | ship it",
        workingDirectory: "/checkout:three",
      });
      await flushPromises();
      mocks.moveSession.mockClear();

      move("ses_ship it", "checkout:one", "checkout:two", staleObservedAt);
      await flushPromises();
      expect(mocks.moveSession).not.toHaveBeenCalled();

      move("ses_ship it", "checkout:two", "checkout:two", Date.now() + 1_000);
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledWith("session:agent", "checkout:two", 0, false, false);
      wrapper.unmount();
    });

    it("keeps the latest new relocation received while a move is in flight", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["agent"], "checkout:two": [], "checkout:three": [] },
        { "checkout:one": ["ship it"] },
        { activeSessionId: "session:agent" },
      );
      let finishAutomatic: (result: {
        moved: boolean;
        directoryChange: "not-requested" | "written" | "not-written";
      }) => void = () => {};
      mocks.moveSession.mockImplementationOnce(() => new Promise((resolve) => (finishAutomatic = resolve)));

      move("ses_ship it", "checkout:one", "checkout:two", Date.now());
      await flushPromises();
      move("ses_ship it", "checkout:two", "checkout:three", Date.now() + 1_000);
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledTimes(1);

      finishAutomatic({ moved: true, directoryChange: "not-requested" });
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledTimes(2);
      expect(mocks.moveSession).toHaveBeenLastCalledWith("session:agent", "checkout:three", 0, false, true);
      wrapper.unmount();
    });

    it("retries the same relocation after an automatic move fails", async () => {
      const wrapper = await mountWithOpenCodeTerminals(
        { "checkout:one": ["agent"], "checkout:two": [], "checkout:three": [] },
        { "checkout:one": ["ship it"] },
      );
      const firstObservedAt = Date.now();
      mocks.moveSession.mockRejectedValueOnce(new Error("move failed"));

      move("ses_ship it", "checkout:one", "checkout:two", firstObservedAt);
      await flushPromises();
      move("ses_ship it", "checkout:one", "checkout:two", firstObservedAt);
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledTimes(2);
      expect(mocks.moveSession).toHaveBeenLastCalledWith("session:agent", "checkout:two", 0, false, false);

      move("ses_ship it", "checkout:one", "checkout:three", firstObservedAt + 1_000);
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledTimes(3);
      expect(mocks.moveSession).toHaveBeenLastCalledWith("session:agent", "checkout:three", 0, false, false);
      wrapper.unmount();
    });
  });

  describe("a shell that changed directory into a worktree", () => {
    /** The status the pane emits when the OS reports where a terminal's shell is. */
    function status(workingDirectory: string | undefined, foregroundApp = "zsh") {
      return {
        state: "running" as const,
        foregroundProcess: false,
        foregroundApp,
        workingDirectory,
      };
    }

    async function mountWithSettings(terminal: typeof DEFAULT_SETTINGS.terminal) {
      const settings = cloneSettings(DEFAULT_SETTINGS);
      settings.terminal = terminal;
      const workspace = workspaceWith(
        checkout("checkout:one", [session("session:one", "zsh", "checkout:one")]),
        checkout("checkout:two"),
      );
      workspace.activeSessionId = "session:one";
      const wrapper = await mountApp(workspace, undefined, { settings });
      mocks.moveSession.mockClear();
      return wrapper;
    }

    const following = () => ({ ...cloneSettings(DEFAULT_SETTINGS).terminal, followWorktree: "cd" as const });

    it.each([
      ["Git A to Git B", "git", "git", "/repo/b/src"],
      ["Git to Home", "git", "plain", "/home/src"],
      ["Home to Git", "plain", "git", "/repo/b/src"],
    ] as const)("follows shell cd from %s", async (_label, sourceKind, targetKind, directory) => {
      const sourceId = sourceKind === "plain" ? "checkout:home" : "checkout:source";
      const targetId = targetKind === "plain" ? "checkout:home-target" : "checkout:foreign";
      const sourcePath = sourceKind === "plain" ? "/home" : "/repo/a";
      const targetPath = targetKind === "plain" ? "/home" : "/repo/b";
      const source: Checkout = {
        ...checkout(sourceId, [session("session:follow", "zsh", sourceId)]),
        repoId: "repo:" + sourceId,
        path: sourcePath,
        canonicalPath: sourcePath,
      };
      const target: Checkout = {
        ...checkout(targetId),
        repoId: "repo:" + targetId,
        path: targetPath,
        canonicalPath: targetPath,
      };
      const makeRepo = (item: Checkout, kind: Repo["kind"]): Repo => ({
        id: item.repoId,
        kind,
        name: item.id,
        root: item.path,
        checkouts: [item],
        createdAt: "now",
        lastOpenedAt: "now",
      });
      const workspace: WorkspaceState = {
        repos: [makeRepo(source, sourceKind), makeRepo(target, targetKind)],
        activeCheckoutId: sourceId,
        activeSessionId: "session:follow",
        homeCheckoutId: sourceKind === "plain" ? sourceId : targetKind === "plain" ? targetId : undefined,
      };
      const settings = cloneSettings(DEFAULT_SETTINGS);
      settings.terminal = following();
      const wrapper = await mountApp(workspace, undefined, { settings });
      mocks.moveSession.mockClear();

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:follow", status(directory));
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledWith("session:follow", targetId, 0, false, true);
      wrapper.unmount();
    });

    it.each(["off", "cd", "agent", "both"] as const)(
      "gates Home cd into Git and subsequent hops with %s",
      async (mode) => {
        const home = {
          ...checkout("checkout:home", [session("session:home", "zsh", "checkout:home")]),
          repoId: "repo:home",
          path: "/work",
          canonicalPath: "/work",
        };
        const root = { ...checkout("checkout:one"), path: "/work/app", canonicalPath: "/work/app" };
        const worktree = {
          ...checkout("checkout:two"),
          path: "/work/app/.worktrees/feature",
          canonicalPath: "/work/app/.worktrees/feature",
        };
        const workspace = workspaceWith(root, worktree);
        workspace.repos.unshift({
          ...workspace.repos[0],
          id: home.repoId,
          kind: "plain",
          root: home.path,
          checkouts: [home],
        });
        workspace.homeCheckoutId = home.id;
        workspace.activeCheckoutId = home.id;
        workspace.activeSessionId = "session:home";
        const settings = cloneSettings(DEFAULT_SETTINGS);
        settings.terminal.followWorktree = mode;
        const wrapper = await mountApp(workspace, undefined, { settings });
        mocks.moveSession.mockClear();
        const report = async (directory: string) => {
          wrapper
            .getComponent({ name: "SessionPane" })
            .vm.$emit("sessionStatusChanged", "session:home", status(directory));
          await flushPromises();
        };
        await report(root.path + "/src");
        if (mode === "off" || mode === "agent") {
          expect(mocks.moveSession).not.toHaveBeenCalled();
        } else {
          expect(mocks.moveSession).toHaveBeenLastCalledWith("session:home", root.id, 0, false, true);
          // The move stub does not return the backend's updated ownership; apply that result here.
          const arrive = (target: Checkout) => {
            const state = mocks.workspaceRef!.value;
            const moved = state.repos
              .flatMap((repo) => repo.checkouts)
              .flatMap((checkout) => checkout.sessions)
              .find((session) => session.id === "session:home")!;
            mocks.workspaceRef!.value = {
              ...state,
              activeCheckoutId: target.id,
              repos: state.repos.map((repo) => ({
                ...repo,
                checkouts: repo.checkouts.map((checkout) => ({
                  ...checkout,
                  sessions:
                    checkout.id === target.id
                      ? [{ ...moved, checkoutId: target.id }]
                      : checkout.sessions.filter((session) => session.id !== moved.id),
                })),
              })),
            };
          };
          arrive(root);
          await report(worktree.path + "/src");
          expect(mocks.moveSession).toHaveBeenLastCalledWith("session:home", worktree.id, 0, false, true);
          arrive(worktree);
          mocks.moveSession.mockClear();
          await report(worktree.path + "/src");
          expect(mocks.moveSession).not.toHaveBeenCalled();
          await report(root.path);
          expect(mocks.moveSession).toHaveBeenLastCalledWith("session:home", root.id, 0, false, true);
        }
        wrapper.unmount();
      },
    );

    it("does not follow a stale cwd during a manual move, then follows later cd", async () => {
      const settings = cloneSettings(DEFAULT_SETTINGS);
      settings.terminal = following();
      const workspace = workspaceWith(
        checkout("checkout:one", [session("session:one", "zsh", "checkout:one")]),
        checkout("checkout:two"),
        checkout("checkout:three"),
      );
      workspace.activeSessionId = "session:one";
      const wrapper = await mountApp(workspace, undefined, { settings });
      let finishMove: (result: {
        moved: boolean;
        directoryChange: "not-requested" | "written" | "not-written";
      }) => void = () => {};
      mocks.moveSession.mockImplementationOnce(() => new Promise((resolve) => (finishMove = resolve)));

      wrapper.getComponent({ name: "SidebarStub" }).vm.$emit("moveSession", "session:one", "checkout:two", 0);
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledWith("session:one", "checkout:two", 0, true, true);

      // The pane has moved ownership, but its unresolved moveSession call still represents the cd.
      const beforeMove = mocks.workspaceRef!.value;
      const movedSession = beforeMove.repos[0].checkouts[0].sessions[0];
      const afterMove: WorkspaceState = {
        ...beforeMove,
        activeCheckoutId: "checkout:two",
        repos: beforeMove.repos.map((repo) => ({
          ...repo,
          checkouts: repo.checkouts.map((item) =>
            item.id === "checkout:one"
              ? { ...item, sessions: item.sessions.filter((itemSession) => itemSession.id !== movedSession.id) }
              : item.id === "checkout:two"
                ? { ...item, sessions: [...item.sessions, { ...movedSession, checkoutId: item.id }] }
                : item,
          ),
        })),
      };
      wrapper.getComponent({ name: "MainPane" }).vm.$emit("workspaceUpdated", afterMove);
      await flushPromises();

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:one/src"));
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledTimes(1);

      finishMove({ moved: true, directoryChange: "written" });
      await flushPromises();
      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:one/src"));
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledTimes(1);

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:two/src"));
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledTimes(1);

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:three/src"));
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenLastCalledWith("session:one", "checkout:three", 0, false, true);
      wrapper.unmount();
    });

    it("releases manual follow suppression when the shell could not accept cd", async () => {
      const settings = cloneSettings(DEFAULT_SETTINGS);
      settings.terminal = following();
      const workspace = workspaceWith(
        checkout("checkout:one", [session("session:one", "zsh", "checkout:one")]),
        checkout("checkout:two"),
      );
      workspace.activeSessionId = "session:one";
      const wrapper = await mountApp(workspace, undefined, { settings });
      mocks.moveSession.mockResolvedValueOnce({ moved: true, directoryChange: "not-written" });

      wrapper.getComponent({ name: "SidebarStub" }).vm.$emit("moveSession", "session:one", "checkout:two", 0);
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledWith("session:one", "checkout:two", 0, true, true);

      const before = mocks.workspaceRef!.value;
      const moved = before.repos[0]!.checkouts[0]!.sessions[0]!;
      const after: WorkspaceState = {
        ...before,
        activeCheckoutId: "checkout:two",
        repos: before.repos.map((repo) => ({
          ...repo,
          checkouts: repo.checkouts.map((item) =>
            item.id === "checkout:one"
              ? { ...item, sessions: item.sessions.filter((itemSession) => itemSession.id !== moved.id) }
              : item.id === "checkout:two"
                ? { ...item, sessions: [...item.sessions, { ...moved, checkoutId: item.id }] }
                : item,
          ),
        })),
      };
      wrapper.getComponent({ name: "MainPane" }).vm.$emit("workspaceUpdated", after);
      await flushPromises();
      mocks.moveSession.mockClear();

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:one/src"));
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledWith("session:one", "checkout:one", 0, false, true);
      wrapper.unmount();
    });

    it("bounds manual follow suppression when cwd never acknowledges the requested cd", async () => {
      const wrapper = await mountWithSettings(following());
      mocks.moveSession.mockResolvedValueOnce({ moved: true, directoryChange: "written" });

      wrapper.getComponent({ name: "SidebarStub" }).vm.$emit("moveSession", "session:one", "checkout:two", 0);
      await flushPromises();
      const before = mocks.workspaceRef!.value;
      const moved = before.repos[0]!.checkouts[0]!.sessions[0]!;
      const after: WorkspaceState = {
        ...before,
        activeCheckoutId: "checkout:two",
        repos: before.repos.map((repo) => ({
          ...repo,
          checkouts: repo.checkouts.map((item) =>
            item.id === "checkout:one"
              ? { ...item, sessions: item.sessions.filter((itemSession) => itemSession.id !== moved.id) }
              : item.id === "checkout:two"
                ? { ...item, sessions: [...item.sessions, { ...moved, checkoutId: item.id }] }
                : item,
          ),
        })),
      };
      wrapper.getComponent({ name: "MainPane" }).vm.$emit("workspaceUpdated", after);
      await flushPromises();
      mocks.moveSession.mockClear();

      await vi.advanceTimersByTimeAsync(15_000);
      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:one/src"));
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledWith("session:one", "checkout:one", 0, false, true);
      wrapper.unmount();
    });

    it("moves the terminal to the worktree its shell is now in", async () => {
      const wrapper = await mountWithSettings(following());

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:two"));
      await flushPromises();

      // The shell did the `cd` itself, so there is nothing to type at it.
      expect(mocks.moveSession).toHaveBeenCalledWith("session:one", "checkout:two", 0, false, true);
      wrapper.unmount();
    });

    it("moves it for a directory inside the worktree, not only its root", async () => {
      // Nobody `cd`s into a worktree's root exactly; they `cd` into a directory in it.
      const wrapper = await mountWithSettings(following());

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:two/src/deep"));
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledWith("session:one", "checkout:two", 0, false, true);
      wrapper.unmount();
    });

    it("moves nothing when the shell is where its row already says it is", async () => {
      // The invariant that stops a moved row from moving again lives here rather than in the poll:
      // once the row names the worktree the shell is in, there is no other checkout to find.
      const wrapper = await mountWithSettings(following());

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:one/src"));
      await flushPromises();

      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it.each(["off", "cd", "agent", "both"] as const)("gates shell moves with %s", async (mode) => {
      const wrapper = await mountWithSettings({ ...DEFAULT_SETTINGS.terminal, followWorktree: mode });

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:two"));
      await flushPromises();

      if (mode === "cd" || mode === "both") {
        expect(mocks.moveSession).toHaveBeenCalledWith("session:one", "checkout:two", 0, false, true);
      } else expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("moves nothing for a terminal with an agent in front of it", async () => {
      // An agent that moved itself reports where it is working; its shell never changed
      // directory, so a directory here is a leftover rather than a move.
      const wrapper = await mountWithSettings(following());

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:two", "opencode"));
      await flushPromises();

      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("moves nothing when the OS will not say where the shell is", async () => {
      const wrapper = await mountWithSettings(following());

      wrapper.getComponent({ name: "SessionPane" }).vm.$emit("sessionStatusChanged", "session:one", status(undefined));
      await flushPromises();

      expect(mocks.moveSession).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("asks once while a move is still on its way", async () => {
      // The status poll runs every 750ms and a relocation stays on offer for half a minute, so
      // the same move is announced over and over while the first one is in flight. Asking twice
      // is not harmless: the second lands on a session that has already arrived and answers
      // "already in that worktree", which reaches the user as an error toast.
      const wrapper = await mountWithSettings(following());
      let land: (moved: boolean) => void = () => {};
      mocks.moveSession.mockImplementationOnce(() => new Promise<boolean>((resolve) => (land = resolve)));

      const report = () =>
        wrapper
          .getComponent({ name: "SessionPane" })
          .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:two"));
      report();
      await flushPromises();
      report();
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledTimes(1);

      land(true);
      await flushPromises();

      // The guard is about a move in flight, not about the first one a terminal ever gets.
      mocks.moveSession.mockImplementationOnce(() => Promise.resolve(true));
      report();
      await flushPromises();
      expect(mocks.moveSession).toHaveBeenCalledTimes(2);
      wrapper.unmount();
    });

    it("leaves a split layout's preview alone when it follows", async () => {
      // Following is what clicking the row does, and clicking the row does not take the document
      // someone is reading in the destination away from them. Only the close button claims the
      // whole panel back.
      const settings = cloneSettings(DEFAULT_SETTINGS);
      settings.terminal = following();
      const workspace = workspaceWith(
        checkout("checkout:one", [session("session:one", "zsh", "checkout:one")]),
        checkout("checkout:two"),
      );
      workspace.activeSessionId = "session:one";
      const wrapper = await mountApp(workspace, { ...DEFAULT_APP_LAYOUT, mode: "split" }, { settings });

      // The destination is reading a file when the terminal lands in it.
      await wrapper.get('[data-testid="select-checkout-two"]').trigger("click");
      await wrapper.get('[data-testid="open-file"]').trigger("click");
      await wrapper.get('[data-testid="select-session-one"]').trigger("click");
      mocks.moveSession.mockImplementationOnce(() => Promise.resolve(true));

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionStatusChanged", "session:one", status("/checkout:two"));
      await flushPromises();

      expect(mocks.moveSession).toHaveBeenCalledWith("session:one", "checkout:two", 0, false, true);
      // Asking for it again is what shows whether the destination still remembers its document.
      await wrapper.get('[data-testid="select-checkout-two"]').trigger("click");
      expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).not.toBe("none");
      wrapper.unmount();
    });
  });

  describe("a terminal reordered inside its own worktree", () => {
    it("reaches the sidebar, because that list is drawn from this order and nothing else", async () => {
      // The pane is the only place a checkout's terminal order is decided, and it announces every
      // save. This was the half of the chain that was missing: the event was announced by the pane
      // and listened for by the app, and stopped in between. So a reorder was persisted to the
      // layout, correct and invisible — the sidebar kept drawing the order the database hands out,
      // which is also the order the drop measured its slot against, so a drag moved nothing on the
      // only screen that draws the result. The same was true in every worktree, the repo root
      // included, because the event never arrived anywhere.
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one", [session("session:one", "zsh", "checkout:one")])),
      );
      expect(wrapper.get('[data-testid="sidebar-session-order"]').text()).toBe("{}");

      wrapper
        .getComponent({ name: "SessionPane" })
        .vm.$emit("sessionOrder", "checkout:one", ["session:two", "session:one"]);
      await flushPromises();

      expect(JSON.parse(wrapper.get('[data-testid="sidebar-session-order"]').text())).toEqual({
        "checkout:one": ["session:two", "session:one"],
      });
      wrapper.unmount();
    });
  });

  describe("a workdir with its directory still there", () => {
    it("asks first, names the scope, and only then takes the row off the list", async () => {
      const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one"), checkout("checkout:two")));

      // The repo root is the head of a list, so its row's close reaches the worktrees with it.
      await wrapper.get('[data-testid="close-workdir-base"]').trigger("click");
      expect(confirm).toHaveBeenLastCalledWith(
        "Remove “/checkout:one” and its checkout list from Muster? No files will be deleted, and opening the folder again brings it back.",
      );
      // A worktree is one entry, and nothing is closed while the question is unanswered.
      await wrapper.get('[data-testid="close-workdir-worktree"]').trigger("click");
      expect(confirm).toHaveBeenLastCalledWith(
        "Remove “/checkout:two” from Muster? No files will be deleted, and opening the folder again brings it back.",
      );
      expect(mocks.closeCheckout).not.toHaveBeenCalled();

      confirm.mockReturnValue(true);
      mocks.closeCheckout.mockResolvedValue({ repos: [], activeCheckoutId: null, activeSessionId: null });
      await wrapper.get('[data-testid="close-workdir-worktree"]').trigger("click");
      await flushPromises();

      expect(mocks.closeCheckout).toHaveBeenCalledWith("checkout:two");
      expect(mocks.workspaceRef?.value.repos).toEqual([]);
      wrapper.unmount();
      confirm.mockRestore();
    });

    it("never reaches the missing-checkout command, which prunes what a live workdir still needs", async () => {
      const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
      mocks.closeCheckout.mockResolvedValue({ repos: [], activeCheckoutId: null, activeSessionId: null });
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one"), checkout("checkout:two")));

      await wrapper.get('[data-testid="close-workdir-base"]').trigger("click");
      await flushPromises();

      expect(mocks.closeMissingCheckout).not.toHaveBeenCalled();
      wrapper.unmount();
      confirm.mockRestore();
    });
  });

  describe("archiving a worktree and putting it back", () => {
    const archived = [
      { id: "checkout:three", repoId: "repo:shared", path: "/checkout:three", branch: "three" },
      { id: "checkout:four", repoId: "repo:shared", path: "/checkout:four", branch: "four" },
    ];

    it("opens one dialog for both answers when the row asks to take the worktree off the panel", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one"), checkout("checkout:two")));

      await wrapper.get('[data-testid="remove-worktree-two"]').trigger("click");

      // Archiving and deleting are one question, so the row asks it once and the dialog
      // answers with both, against the worktree that was clicked.
      const dialog = wrapper.get('[data-testid="worktree-dialog"]');
      expect(dialog.attributes("data-mode")).toBe("remove");
      expect(dialog.attributes("data-checkout")).toBe("checkout:two");
      wrapper.unmount();
    });

    it("restores a whole repository's archived worktrees in one confirmed action", async () => {
      mocks.restoreArchivedWorktrees.mockResolvedValue({
        ...workspaceWith(checkout("checkout:one"), checkout("checkout:two"), checkout("checkout:three")),
        archivedWorktrees: [],
      });
      const wrapper = await mountApp({ ...workspaceWith(checkout("checkout:one")), archivedWorktrees: archived });

      await wrapper.get('[data-testid="restore-archived-shared"]').trigger("click");

      // Two, said as two: the count is what the user is about to see change.
      const dialog = wrapper.get('[role="dialog"]');
      expect(dialog.text()).toContain("Restore archived worktrees");
      expect(dialog.text()).toContain("2 archived worktrees");
      expect(mocks.restoreArchivedWorktrees).not.toHaveBeenCalled();

      await dialog.findAll("footer button")[0]!.trigger("click");
      expect(mocks.restoreArchivedWorktrees).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("says one when there is only one, so the row never promises a set of none", async () => {
      const wrapper = await mountApp({
        ...workspaceWith(checkout("checkout:one")),
        archivedWorktrees: [archived[0]!],
      });

      await wrapper.get('[data-testid="restore-archived-shared"]').trigger("click");
      await flushPromises();

      expect(wrapper.get('[role="dialog"]').text()).toContain("Restore archived worktree");
      expect(wrapper.get('[role="dialog"]').text()).toContain("1 archived worktree");
      wrapper.unmount();
    });
  });

  describe("file activity", () => {
    it("hands the document the paths the batch moved, alongside the revision", async () => {
      const handlers = new Map<string, (event: { payload: unknown }) => void>();
      vi.mocked(listen).mockImplementation((async (name: string, handler: (event: { payload: unknown }) => void) => {
        handlers.set(name, handler);
        return () => {};
      }) as unknown as typeof listen);

      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      const pane = wrapper.findComponent({ name: "MainPane" });
      expect(pane.props("refreshRevision")).toBe(0);

      // The watcher names the paths as well as the checkout, which is what lets a document whose
      // own bytes came back unchanged tell whether one of the files it draws is the one that moved.
      handlers.get("checkout-file-activity")?.({
        payload: [{ checkoutId: "checkout:one", paths: ["docs/pic.png"] }],
      });
      await flushPromises();

      expect(pane.props("refreshRevision")).toBe(1);
      expect(pane.props("refreshPaths")).toEqual(["docs/pic.png"]);
      wrapper.unmount();
    });

    it("counts a batch that could not name its paths without inventing any", async () => {
      const handlers = new Map<string, (event: { payload: unknown }) => void>();
      vi.mocked(listen).mockImplementation((async (name: string, handler: (event: { payload: unknown }) => void) => {
        handlers.set(name, handler);
        return () => {};
      }) as unknown as typeof listen);

      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      const pane = wrapper.findComponent({ name: "MainPane" });

      handlers.get("checkout-file-activity")?.({ payload: [{ checkoutId: "checkout:one", paths: [] }] });
      await flushPromises();

      // An empty list is the watcher declining to say what it moved, which is not a checkout
      // nothing touched: the revision still moves, and no path is claimed on the way.
      expect(pane.props("refreshRevision")).toBe(1);
      expect(pane.props("refreshPaths")).toEqual([]);
      wrapper.unmount();
    });
  });

  describe("layout", () => {
    it("starts the panels at the muster default widths within their clamps", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      const panels = wrapper.findAll("#navigation-panel, #inspector-panel");

      expect(panels[0]!.attributes()).toMatchObject({ "default-size": "240", "min-size": "240" });
      expect(panels[1]!.attributes()).toMatchObject({
        "default-size": "280",
        "min-size": "200",
        "max-size": "480",
      });
      expect(wrapper.get("#main-panel").attributes("min-size")).toBe("420");
      wrapper.unmount();
    });

    it("persists the widths a drag reports, in pixels", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      wrapper.getComponent(SplitterGroup).vm.$emit("layout", [310, 750, 340]);
      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();

      expect(mocks.saveAppLayout).toHaveBeenLastCalledWith({
        mode: "focus",
        sidebarWidth: 310,
        inspectorWidth: 340,
        previewWidth: 360,
      });
      wrapper.unmount();
    });

    it("clamps a drag that goes past the supported range", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      wrapper.getComponent(SplitterGroup).vm.$emit("layout", [900, 400, 12]);
      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();

      expect(mocks.saveAppLayout).toHaveBeenLastCalledWith({
        mode: "focus",
        sidebarWidth: 500,
        inspectorWidth: 200,
        previewWidth: 360,
      });
      wrapper.unmount();
    });

    it("keeps the inspector width when the narrow drawer reports a collapsed panel", async () => {
      // The scrollbar mode is carried through the same save rather than checked on its own, so
      // this one is stored as `always`: a preference read back as the default would be invisible
      // everywhere else, because the default is what a layout with no preference in it gives.
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")), {
        mode: "focus",
        sidebarWidth: 345,
        inspectorWidth: 450,
        previewWidth: 360,
      });
      const group = wrapper.getComponent(SplitterGroup);
      const inspectorPanel = wrapper.findAllComponents({ name: "SplitterPanel" })[2]!;

      Object.defineProperty(window, "innerWidth", { configurable: true, value: 900 });
      window.dispatchEvent(new Event("resize"));
      await flushPromises();
      expect(inspectorPanel.emitted("collapse")).toBeTruthy();

      group.vm.$emit("layout", [400, 500, 0]);
      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();

      expect(mocks.saveAppLayout).toHaveBeenLastCalledWith({
        mode: "focus",
        sidebarWidth: 400,
        inspectorWidth: 450,
        previewWidth: 360,
      });
      wrapper.unmount();
    });

    it("lets double-click on a handle reset just that panel", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")), {
        mode: "focus",
        sidebarWidth: 345,
        inspectorWidth: 450,
        previewWidth: 360,
      });
      const resizeCalls = vi.fn();
      mocks.onProgrammaticPanelResize = resizeCalls;

      const handles = wrapper.findAllComponents(SplitterResizeHandle);
      // Nothing is drawn on a handle at rest: the edge appears when the pointer is on it, so there
      // is no grip to assert and a dash standing in the middle of the panel for every pane is gone.
      expect(handles[0]!.find(".h-6.w-1").exists()).toBe(false);
      await handles[0]!.trigger("dblclick");
      await flushPromises();

      expect(resizeCalls).toHaveBeenCalledWith("navigation-panel", DEFAULT_APP_LAYOUT.sidebarWidth);
      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();
      expect(mocks.saveAppLayout).toHaveBeenLastCalledWith({
        mode: "focus",
        sidebarWidth: DEFAULT_APP_LAYOUT.sidebarWidth,
        inspectorWidth: 450,
        previewWidth: 360,
      });
      wrapper.unmount();
    });

    it("places the handles over the edges they move, so the panels meet with nothing between them", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")), { ...DEFAULT_APP_LAYOUT });

      // A handle between two panels is a gutter of five pixels with the window's background behind
      // it, which is a line drawn twice: once by the border on each side of it and once by the gap
      // itself. Laid over the edge it moves, the same five pixels are still something a pointer finds
      // and cost nothing, so nothing is drawn until the pointer is there.
      const handles = wrapper.findAllComponents(SplitterResizeHandle);
      expect(handles[0]!.classes()).toContain("splitter-handle-edge-left");
      expect(handles[1]!.classes()).toContain("splitter-handle-edge-right");
      wrapper.unmount();
    });

    it("takes the inspector's width away when the split layout draws it as a drawer", async () => {
      // A window narrow enough for the drawer reaches the split layout already being a drawer, so
      // the flag that says the inspector is a drawer never changes and nothing re-collapses the
      // panel. A drawer drawn over the main view leaves an empty strip of reserved space down the
      // side of the window with nothing in it.
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")), {
        mode: "focus",
        sidebarWidth: 300,
        inspectorWidth: 300,
        previewWidth: 360,
      });
      const inspectorPanel = wrapper.findAllComponents({ name: "SplitterPanel" })[2]!;
      const resizeCalls = vi.fn();
      mocks.onProgrammaticPanelResize = resizeCalls;

      Object.defineProperty(window, "innerWidth", { configurable: true, value: 900 });
      window.dispatchEvent(new Event("resize"));
      await flushPromises();
      expect(inspectorPanel.emitted("collapse")).toBeTruthy();
      const collapsesBeforeTheSwitch = inspectorPanel.emitted("collapse")?.length ?? 0;
      resizeCalls.mockClear();

      await wrapper.get('[data-testid="layout-toggle"]').trigger("click");
      await flushPromises();
      expect(wrapper.get('[data-testid="layout-toggle"]').attributes("aria-pressed")).toBe("true");

      // A drawer is given no width of its own, so the switch adds a collapse and no resize.
      expect(inspectorPanel.emitted("collapse")?.length ?? 0).toBe(collapsesBeforeTheSwitch + 1);
      expect(resizeCalls).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("keeps the sidebar at the width it is drawn at when the split layout takes its room", async () => {
      // reka-ui rebuilds the whole row from the widths the panels were mounted with whenever a
      // panel's limits change, and the split layout changes what the main panel needs. A window too
      // narrow for the sidebar's saved width draws it narrower than that, and the rebuild grew it
      // back out of the main view, so the sidebar was given the width it was drawn at.
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "Terminal 1")])), {
        mode: "focus",
        sidebarWidth: 500,
        inspectorWidth: 200,
        previewWidth: 360,
      });
      const resizeCalls = vi.fn();
      mocks.onProgrammaticPanelResize = resizeCalls;
      vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
        const measured = this.id === "navigation-panel" ? 460 : 900;
        return {
          width: measured,
          height: 800,
          top: 0,
          left: 0,
          right: measured,
          bottom: 800,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      });

      await wrapper.get('[data-testid="layout-toggle"]').trigger("click");
      await flushPromises();

      expect(resizeCalls).toHaveBeenCalledWith("navigation-panel", 460);

      // In split mode the main panel's minimum changes again when a document replaces the terminal;
      // that constraint rebuild must preserve the same drawn sidebar width too.
      resizeCalls.mockClear();
      await wrapper.get('[data-testid="open-file"]').trigger("click");
      await flushPromises();
      expect(resizeCalls).toHaveBeenCalledExactlyOnceWith("navigation-panel", 460);
      wrapper.unmount();
    });

    it("gives the inspector its full width back when space returns", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")), {
        mode: "focus",
        sidebarWidth: 300,
        inspectorWidth: 300,
        previewWidth: 360,
      });
      const inspectorPanel = wrapper.findAllComponents({ name: "SplitterPanel" })[2]!;
      const resizeCalls = vi.fn();
      mocks.onProgrammaticPanelResize = resizeCalls;

      Object.defineProperty(window, "innerWidth", { configurable: true, value: 900 });
      window.dispatchEvent(new Event("resize"));
      await flushPromises();
      expect(inspectorPanel.emitted("collapse")).toBeTruthy();

      Object.defineProperty(window, "innerWidth", { configurable: true, value: 1400 });
      window.dispatchEvent(new Event("resize"));
      await flushPromises();

      expect(inspectorPanel.emitted("expand")).toBeTruthy();
      expect(resizeCalls).toHaveBeenCalledWith("inspector-panel", 300);
      wrapper.unmount();
    });

    it("keeps the terminal mounted with the latest preview in split mode", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "Terminal 1")])));

      await wrapper.get('[data-testid="open-file"]').trigger("click");
      await wrapper.get('[data-testid="layout-toggle"]').trigger("click");
      expect(wrapper.get('[data-testid="layout-toggle"]').attributes()).toMatchObject({
        "aria-label": "Switch to focus layout",
        "aria-pressed": "true",
      });
      expect((wrapper.get("#main-view-terminal").element as HTMLElement).style.display).not.toBe("none");
      expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).not.toBe("none");
      expect(mocks.sessionPaneMounts).toBe(1);

      const resizeHandle = wrapper.get('[aria-label="Resize preview panel"]');
      await resizeHandle.trigger("pointermove", { pointerId: 1, clientX: 400 });
      expect(wrapper.getComponent({ name: "MainPane" }).emitted("resizePreview")).toBeUndefined();
      await wrapper.get('[data-testid="select-session-one"]').trigger("click");
      await flushPromises();
      expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).not.toBe("none");
      expect(mocks.sessionPaneMounts).toBe(1);

      await wrapper.get('[data-testid="new-terminal-one"]').trigger("click");
      await flushPromises();
      expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).not.toBe("none");

      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();
      expect(mocks.saveAppLayout).toHaveBeenLastCalledWith({ ...DEFAULT_APP_LAYOUT, mode: "split" });
      expect(mocks.saveCheckoutUiState).toHaveBeenLastCalledWith(
        "checkout:one",
        expect.objectContaining({ mainView: "document", document: expect.objectContaining({ path: "README.md" }) }),
      );
      await wrapper.get('[data-testid="layout-toggle"]').trigger("click");
      expect((wrapper.get("#main-view-terminal").element as HTMLElement).style.display).toBe("none");
      expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).not.toBe("none");
      wrapper.unmount();
    });

    it("resizes the preview only while its handle is being dragged", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")), {
        ...DEFAULT_APP_LAYOUT,
        mode: "split",
      });
      await wrapper.get('[data-testid="open-file"]').trigger("click");
      const handle = wrapper.get('[aria-label="Resize preview panel"]');
      const pane = wrapper.getComponent({ name: "MainPane" });
      const element = handle.element as HTMLElement;
      element.setPointerCapture = vi.fn();
      element.hasPointerCapture = vi.fn().mockReturnValue(true);
      element.releasePointerCapture = vi.fn();
      vi.spyOn(wrapper.get("#main-panel > div").element, "getBoundingClientRect").mockReturnValue({
        right: 1000,
      } as DOMRect);

      await handle.trigger("pointermove", { pointerId: 1, clientX: 700 });
      expect(pane.emitted("resizePreview")).toBeUndefined();
      await handle.trigger("pointerdown", { pointerId: 1, clientX: 700 });
      await handle.trigger("pointermove", { pointerId: 2, clientX: 650 });
      expect(pane.emitted("resizePreview")).toBeUndefined();
      await handle.trigger("pointermove", { pointerId: 1, clientX: 650 });
      expect(pane.emitted("resizePreview")).toEqual([[345]]);
      await handle.trigger("pointerup", { pointerId: 1 });
      await handle.trigger("pointermove", { pointerId: 1, clientX: 600 });
      expect(pane.emitted("resizePreview")).toHaveLength(1);
      wrapper.unmount();
    });

    it("peeks the split inspector for 300ms, closes it when the pointer leaves for good, and leaves the narrow drawer shut", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")), {
        ...DEFAULT_APP_LAYOUT,
        mode: "split",
      });
      const inspector = wrapper.findComponent({ name: "InspectorPane" });

      expect(wrapper.find(".inspector-hover-strip").exists()).toBe(true);
      expect(inspector.element.parentElement).toBe(wrapper.get(".app-shell").element);
      expect(inspector.classes()).toContain("split-inspector-closed");
      await wrapper.get(".inspector-hover-strip").trigger("pointerenter");
      expect(inspector.classes()).not.toContain("split-inspector-closed");

      await wrapper.get(".inspector-hover-strip").trigger("pointerleave");
      await vi.advanceTimersByTimeAsync(299);
      expect(inspector.classes()).not.toContain("split-inspector-closed");
      await inspector.trigger("pointerenter");
      await vi.advanceTimersByTimeAsync(300);
      expect(inspector.classes()).not.toContain("split-inspector-closed");

      await inspector.trigger("pointerleave");
      await vi.advanceTimersByTimeAsync(299);
      expect(inspector.classes()).not.toContain("split-inspector-closed");
      await inspector.trigger("pointerenter");
      await vi.advanceTimersByTimeAsync(300);
      expect(inspector.classes()).not.toContain("split-inspector-closed");

      // The drawer appearing under the pointer fires a leave on the strip and an enter on the
      // drawer at once. In whichever order the browser delivers them the drawer stays open, and
      // it closes on the single leave that means the pointer is over neither.
      await wrapper.get(".inspector-hover-strip").trigger("pointerenter");
      await inspector.trigger("pointerenter");
      await wrapper.get(".inspector-hover-strip").trigger("pointerleave");
      await vi.advanceTimersByTimeAsync(300);
      expect(inspector.classes()).not.toContain("split-inspector-closed");

      await inspector.trigger("pointerleave");
      await vi.advanceTimersByTimeAsync(300);
      expect(inspector.classes()).toContain("split-inspector-closed");

      Object.defineProperty(window, "innerWidth", { configurable: true, value: 900 });
      window.dispatchEvent(new Event("resize"));
      await flushPromises();
      expect(wrapper.find(".inspector-hover-strip").exists()).toBe(true);
      expect(inspector.classes()).toContain("right-inspector-drawer");
      expect(inspector.classes()).toContain("split-inspector-closed");
      expect(wrapper.findComponent({ name: "MainPane" }).props("split")).toBe(true);
      wrapper.unmount();
    });

    it("forgets a drawer hovered under another layout, so it can still be closed", async () => {
      // The drawer is the same element in the narrow focus layout and in the split one, so a
      // pointerenter while focused says nothing about a strip that does not exist yet. Carried
      // over, the flag refuses every close: the drawer is hidden, a hidden drawer fires no leave,
      // and the panel stays up over the split layout with nothing on screen able to put it away.
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 900 });
      window.dispatchEvent(new Event("resize"));
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")), { ...DEFAULT_APP_LAYOUT });
      // Unmounted in a finally because a failed assertion in here used to leave a live tree behind,
      // and the drawers its watchers answer leaked into the layout tests that follow.
      try {
        const inspector = wrapper.findComponent({ name: "InspectorPane" });
        expect(inspector.classes()).toContain("right-inspector-drawer");

        await inspector.trigger("pointerenter");
        await wrapper.get('[data-testid="layout-toggle"]').trigger("click");
        await flushPromises();
        expect(wrapper.findComponent({ name: "MainPane" }).props("split")).toBe(true);

        await wrapper.get(".inspector-hover-strip").trigger("pointerenter");
        await wrapper.get(".inspector-hover-strip").trigger("pointerleave");
        await vi.advanceTimersByTimeAsync(300);
        expect(wrapper.findComponent({ name: "InspectorPane" }).classes()).toContain("split-inspector-closed");
      } finally {
        wrapper.unmount();
      }
    });

    it("does not bring a drawer back open when the shortcut hides it", async () => {
      // Cmd+/ is the only way to put the drawer away without a pointer over it, so it is also the
      // only way to strand the flag that refuses every close: a hidden drawer fires no leave.
      // Attached and dispatched from a panel, because that is the path the capture listener on the
      // window actually answers to.
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT, mode: "split" },
        { attachTo: document.body },
      );
      try {
        const press = () =>
          wrapper
            .get("#navigation-panel")
            .element.dispatchEvent(new KeyboardEvent("keydown", { key: "/", metaKey: true, bubbles: true }));
        const inspector = wrapper.findComponent({ name: "InspectorPane" });
        await wrapper.get(".inspector-hover-strip").trigger("pointerenter");
        expect(inspector.classes()).not.toContain("split-inspector-closed");

        expect(press()).toBe(true); // not cancelled: the event was handled
        await flushPromises();
        expect(inspector.isVisible()).toBe(false);

        press();
        await flushPromises();
        expect(inspector.isVisible()).toBe(true);
        expect(inspector.classes()).toContain("split-inspector-closed");
      } finally {
        wrapper.unmount();
      }
    });

    it("opens media from both the inspector and terminal in View mode", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      for (const ext of ["png", "jpeg", "gif", "webp", "avif", "ico", "bmp", "svg", "mp4", "webm", "mov", "ogv"]) {
        wrapper
          .getComponent({ name: "InspectorPane" })
          .vm.$emit("openFile", { checkoutId: "checkout:one", path: "asset." + ext });
        await flushPromises();
        expect(wrapper.getComponent({ name: "MainPane" }).props("view")).toMatchObject({
          kind: "document",
          path: "asset." + ext,
          mode: "view",
        });
      }
      wrapper.getComponent({ name: "MainPane" }).vm.$emit("openFile", "terminal.mov");
      await flushPromises();
      expect(wrapper.getComponent({ name: "MainPane" }).props("view")).toMatchObject({
        path: "terminal.mov",
        mode: "view",
      });
      wrapper.unmount();
    });

    it("gives the main panel back to the terminal when a preview is closed", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "Terminal 1")])));

      await wrapper.get('[data-testid="open-file"]').trigger("click");
      expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).not.toBe("none");
      expect((wrapper.get("#main-view-terminal").element as HTMLElement).style.display).toBe("none");

      await wrapper.get('[data-testid="close-preview"]').trigger("click");
      await flushPromises();

      // The focus layout has no terminal beside the preview, so closing one used to leave the only
      // way back inside the sidebar's row list.
      expect((wrapper.get("#main-view-terminal").element as HTMLElement).style.display).not.toBe("none");
      expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).toBe("none");
      wrapper.unmount();
    });

    it("closes a preview onto the session the panel already had, starting nothing", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "Terminal 1")])));
      await wrapper.get('[data-testid="open-file"]').trigger("click");

      await wrapper.get('[data-testid="close-preview"]').trigger("click");
      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();

      // The view goes back to a terminal rather than to nothing, and it is the one this checkout
      // already had: a close that started a session would spawn a shell nobody asked for, and the
      // pane that holds terminals is mounted once for the window and never again.
      expect(mocks.sessionPaneMounts).toBe(1);
      expect(mocks.saveCheckoutUiState).toHaveBeenLastCalledWith(
        "checkout:one",
        expect.objectContaining({ mainView: "terminal", document: null, diffAllFiles: false }),
      );
      wrapper.unmount();
    });

    it("gives the whole panel back to the terminal when a split preview is closed", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "Terminal 1")])), {
        ...DEFAULT_APP_LAYOUT,
        mode: "split",
      });
      await wrapper.get('[data-testid="open-file"]').trigger("click");
      await flushPromises();

      // A split layout already keeps the terminal on screen, and this is the one place that refuses
      // to give it the panel: the close button is the reader saying the preview is done with, which
      // a sidebar row asking for a terminal is not.
      expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).not.toBe("none");
      await wrapper.get('[data-testid="close-preview"]').trigger("click");
      await flushPromises();

      expect((wrapper.get("#main-view-terminal").element as HTMLElement).style.display).not.toBe("none");
      expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).toBe("none");
      wrapper.unmount();
    });

    it("does not re-save a view that is already the terminal", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "Terminal 1")])));
      await wrapper.get('[data-testid="open-file"]').trigger("click");
      await wrapper.get('[data-testid="close-preview"]').trigger("click");
      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();
      mocks.saveCheckoutUiState.mockClear();

      // Closing a panel that is already the terminal would write the same view back to disk. The
      // button is still in the DOM — the pane is hidden rather than unmounted, so a terminal can
      // come back without a fresh editor — but it answers to nothing.
      await wrapper.get('[data-testid="close-preview"]').trigger("click");
      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();

      expect(mocks.saveCheckoutUiState).not.toHaveBeenCalled();
      wrapper.unmount();
    });
  });

  describe("the window scale", () => {
    const press = (init: KeyboardEventInit) => {
      const event = new KeyboardEvent("keydown", { cancelable: true, ...init });
      window.dispatchEvent(event);
      return event;
    };
    const scale = () => document.documentElement.style.getPropertyValue("zoom");
    const { toasts } = useToasts();
    const settingsAtZoom = (zoom: number): AppSettings => ({
      ...cloneSettings(DEFAULT_SETTINGS),
      ui: { ...DEFAULT_SETTINGS.ui, zoom: zoom as AppSettings["ui"]["zoom"] },
    });

    it("scales the window one step and says where it landed", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      expect(press({ key: "+", metaKey: true }).defaultPrevented).toBe(true);
      await flushPromises();

      expect(scale()).toBe("1.1");
      expect(toasts.value.map((toast) => toast.message)).toEqual(["Zoom 110% (Cmd 0 to reset)"]);
      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();
      expect(mocks.saveSettings).toHaveBeenLastCalledWith(settingsAtZoom(1.1));
      wrapper.unmount();
    });

    it("does not let an older failed write roll back a newer zoom step", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      let rejectFirst!: (error: Error) => void;
      let resolveSecond!: () => void;
      mocks.saveSettings
        .mockImplementationOnce(() => new Promise<void>((_, reject) => (rejectFirst = reject)))
        .mockImplementationOnce(() => new Promise<void>((resolve) => (resolveSecond = resolve)));

      press({ key: "+", metaKey: true });
      press({ key: "+", metaKey: true });
      await flushPromises();
      expect(scale()).toBe("1.2");
      expect(mocks.saveSettings).toHaveBeenCalledTimes(1);

      rejectFirst(new Error("older write failed"));
      await flushPromises();
      expect(mocks.saveSettings).toHaveBeenCalledTimes(2);
      expect(scale()).toBe("1.2");

      resolveSecond();
      await flushPromises();
      expect(scale()).toBe("1.2");
      expect(mocks.saveSettings).toHaveBeenLastCalledWith(settingsAtZoom(1.2));
      expect(toasts.value.at(-1)?.message).toBe("Zoom 120% (Cmd 0 to reset)");
      wrapper.unmount();
    });

    it("names Ctrl when Ctrl is the key that worked", async () => {
      // The label names the key rather than the platform, so it can only be read on the platform
      // whose shortcut key is Ctrl — where Ctrl and the window's own chords are one key.
      reportsPlatform("Linux x86_64");
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      press({ key: "=", ctrlKey: true });
      await flushPromises();

      expect(scale()).toBe("1.1");
      expect(toasts.value.at(-1)?.message).toBe("Zoom 110% (Ctrl 0 to reset)");
      wrapper.unmount();
    });

    it("goes back to 100% on 0, from wherever it was", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      press({ key: "+", metaKey: true });
      press({ key: "+", metaKey: true });
      press({ key: "-", metaKey: true });
      press({ key: "0", metaKey: true });
      await flushPromises();

      expect(scale()).toBe("1");
      expect(toasts.value.at(-1)?.message).toBe("Zoom 100% (Cmd 0 to reset)");
      wrapper.unmount();
    });

    it("keeps one line of feedback while the key is held down", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      for (let step = 0; step < 3; step += 1) {
        press({ key: "+", metaKey: true });
        await flushPromises();
      }

      // Three different numbers, and `push` only folds away a message identical to the one before
      // it, so without taking the last one down the stack would be a history of the gesture.
      expect(toasts.value.map((toast) => toast.message)).toEqual(["Zoom 130% (Cmd 0 to reset)"]);
      wrapper.unmount();
    });

    it("says nothing when the key cannot move the scale any further", async () => {
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        { settings: { ...structuredClone(DEFAULT_SETTINGS), ui: { ...DEFAULT_SETTINGS.ui, zoom: 1.5 } } },
      );
      expect(scale()).toBe("1.5");

      press({ key: "+", metaKey: true });
      await flushPromises();

      expect(scale()).toBe("1.5");
      expect(toasts.value).toEqual([]);
      wrapper.unmount();
    });

    it("says nothing when a 0 arrives at 100%", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      press({ key: "0", metaKey: true });
      await flushPromises();

      expect(scale()).toBe("1");
      expect(toasts.value).toEqual([]);
      wrapper.unmount();
    });

    it("leaves a 0 and a + that are being typed alone", async () => {
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

      press({ key: "0" });
      press({ key: "+" });
      press({ key: "a", metaKey: true });
      await flushPromises();

      expect(scale()).toBe("1");
      expect(toasts.value).toEqual([]);
      expect(mocks.saveAppLayout).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("comes back at the scale it was left at, and lays the window out in that space", async () => {
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        { settings: { ...structuredClone(DEFAULT_SETTINGS), ui: { ...DEFAULT_SETTINGS.ui, zoom: 1.5 } } },
      );

      expect(scale()).toBe("1.5");
      expect(wrapper.findComponent({ name: "MainPane" }).props("zoom")).toBe(1.5);
      // The app arranges itself in the space the window has before the scale, and a window 1400
      // wide at 150% is 933 of that, which is not enough for the panels beside the main one.
      expect(wrapper.get("#inspector-panel").classes()).toContain("min-h-0");
      expect(wrapper.findComponent({ name: "InspectorPane" }).classes()).toContain("right-inspector-drawer");

      press({ key: "0", metaKey: true });
      await flushPromises();
      expect(wrapper.findComponent({ name: "InspectorPane" }).classes()).not.toContain("right-inspector-drawer");
      wrapper.unmount();
    });

    it("stores the panel widths in the app's pixels rather than the window's", async () => {
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        { settings: { ...structuredClone(DEFAULT_SETTINGS), ui: { ...DEFAULT_SETTINGS.ui, zoom: 1.2 } } },
      );
      const resizeCalls = vi.fn();
      mocks.onProgrammaticPanelResize = resizeCalls;

      // The splitter measures with `getBoundingClientRect()`, which is a number after the scale.
      wrapper.getComponent(SplitterGroup).vm.$emit("layout", [300, 700, 300]);
      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();
      expect(mocks.saveAppLayout).toHaveBeenLastCalledWith({
        ...DEFAULT_APP_LAYOUT,
        sidebarWidth: 250,
        inspectorWidth: 250,
      });

      // And a reset is a width in the same units, so it goes into the splitter scaled.
      const handles = wrapper.findAllComponents(SplitterResizeHandle);
      await handles[0]!.trigger("dblclick");
      await flushPromises();
      expect(resizeCalls).toHaveBeenCalledWith("navigation-panel", DEFAULT_APP_LAYOUT.sidebarWidth * 1.2);
      wrapper.unmount();
    });
  });

  it.each([
    ["default", "review-workdir"],
    ["workdir", "review-default"],
  ] as const)(
    "pins the backend-reported origin when frontend storage is %s",
    async (frontendStorage, effectiveOrigin) => {
      mocks.gitStatus = { branch: "feature", defaultBranch: "main" };
      mocks.reviewNotes = [reviewNoteFixture()];
      mocks.exportReviewMarkdown.mockResolvedValue({ path: "2026-03-14-1532.md", origin: effectiveOrigin });
      const settings = cloneSettings(DEFAULT_SETTINGS);
      settings.reviews.storage = frontendStorage;
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")), { ...DEFAULT_APP_LAYOUT }, { settings });

      await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
      await flushPromises();
      await wrapper.get('[data-testid="send-review"]').trigger("click");
      await flushPromises();

      expect(mocks.exportReviewMarkdown).toHaveBeenCalledOnce();
      // The checkout travels with it: the folder an export lands in is per checkout, not one
      // answer for the whole app.
      const [checkoutId, date, timestamp, markdown] = mocks.exportReviewMarkdown.mock.calls[0];
      expect(checkoutId).toBe("checkout:one");
      expect(date).toMatch(/^\d{4}-\d{2}-\d{2}$/);
      expect(timestamp).toMatch(new RegExp(`^${date}-\\d{4}$`));
      expect(markdown).toContain(`# Code Review ${date}`);
      expect(mocks.dispatchReviewRound).not.toHaveBeenCalled();
      expect(mocks.createAgentSession).not.toHaveBeenCalled();
      expect(mocks.reviewNotes[0]?.status).toBe("draft");
      expect(wrapper.get('[data-testid="document-pane"]').text()).toBe("2026-03-14-1532.md");

      await vi.advanceTimersByTimeAsync(300);
      await flushPromises();
      expect(mocks.saveCheckoutUiState).toHaveBeenLastCalledWith(
        "checkout:one",
        expect.objectContaining({ document: expect.objectContaining({ origin: effectiveOrigin }) }),
      );
      wrapper.unmount();
    },
  );

  it("dispatches the review to the selected OpenCode session", async () => {
    mocks.loadReviewTarget.mockResolvedValue("opencode");
    mocks.gitStatus = { branch: "feature", defaultBranch: "main" };
    mocks.reviewNotes = [reviewNoteFixture()];
    mocks.agentSessions = [agentSessionFixture("ses_one", 10)];
    mocks.agentTargetId = "ses_one";
    mocks.dispatchReviewRound.mockResolvedValue({
      id: "round:1",
      checkoutId: "checkout:one",
      sessionId: "ses_one",
      status: "dispatched",
      marker: "muster-review:round:1",
      noteIds: ["note:1"],
      createdAt: "1",
      updatedAt: "2",
    });
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

    // The diff is the view that sends, so the review goes out from there.
    await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
    await flushPromises();
    await wrapper.get('[data-testid="send-review"]').trigger("click");
    await flushPromises();

    expect(mocks.createAgentSession).not.toHaveBeenCalled();
    expect(mocks.dispatchReviewRound).toHaveBeenCalledTimes(1);
    const [target, ids, markdown] = mocks.dispatchReviewRound.mock.calls[0];
    expect(target).toBe("ses_one");
    expect(ids).toEqual(["note:1"]);
    expect(markdown).toContain("# Code Review ");
    expect(markdown).toContain("> `main..feature` — 1 file, 1 note");
    expect(markdown).toContain("```js{10}");
    expect(markdown).toContain("> revisit this calculation");
    wrapper.unmount();
  });

  it("sends the rounds held back for a busy agent as soon as a turn ends", async () => {
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
    expect(mocks.flushQueuedRounds).not.toHaveBeenCalled();

    mocks.completedSessionIds = ["ses_one"];
    mocks.turns!.value = 1;
    await flushPromises();

    // The flush runs before the round is acknowledged: a queued review goes out while the
    // agent is free, and the turn completion is what says it is.
    expect(mocks.flushQueuedRounds).toHaveBeenCalledTimes(1);
    expect(mocks.ackFinishedTurn).toHaveBeenCalledExactlyOnceWith("ses_one");
    expect(mocks.flushQueuedRounds.mock.invocationCallOrder[0]).toBeLessThan(
      mocks.ackFinishedTurn.mock.invocationCallOrder[0],
    );
    wrapper.unmount();
  });

  /**
   * A Pi send opens a terminal instead of naming a session, and it opens it in the checkout the
   * reader was looking at.
   *
   * Nothing about a session takes part — no dispatch, no target to resolve, and no session created
   * to have one to send to — because there is no session until this terminal has started one.
   */
  it("opens a terminal in the same checkout for Pi, with no session and no queue", async () => {
    mocks.loadReviewTarget.mockResolvedValue("pi");
    mocks.gitStatus = { branch: "feature", defaultBranch: "main" };
    mocks.reviewNotes = [reviewNoteFixture()];
    mocks.agentSessions = [agentSessionFixture("ses_one", 10)];
    mocks.agentTargetId = "ses_one";
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one"), checkout("checkout:two")));

    await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
    await flushPromises();
    await wrapper.get('[data-testid="send-review"]').trigger("click");
    await flushPromises();

    const main = wrapper.getComponent({ name: "MainPane" });
    const request = main.props("shellRequest") as {
      checkoutId: string;
      token: number;
      launch?: { kind: string; request?: { markdown: string } };
    };
    expect(request.checkoutId).toBe("checkout:one");
    expect(request.launch?.kind).toBe("pi-review");
    expect(request.launch?.request?.markdown).toContain("# Code Review ");
    expect(mocks.dispatchReviewRound).not.toHaveBeenCalled();
    expect(mocks.createAgentSession).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("reports successful Pi review launches as opened", async () => {
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
    const pane = wrapper.getComponent({ name: "SessionPane" });

    pane.vm.$emit("reviewLaunchOpened", "checkout:one");
    await flushPromises();

    expect(useToasts().toasts.value.at(-1)?.message).toBe("Review opened in Pi");
    expect(mocks.dispatchReviewRound).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("does not overwrite a saved Pi destination before compatibility loads", async () => {
    let resolveCompatibility!: (snapshot: { piInstalled: boolean; openCodeInstalled: boolean }) => void;
    mocks.getCompatibilitySnapshot.mockReturnValue(new Promise((resolve) => (resolveCompatibility = resolve)));
    mocks.loadReviewTarget.mockResolvedValue("pi");
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
    await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
    await flushPromises();

    expect(mocks.loadReviewTarget).not.toHaveBeenCalled();
    expect(mocks.saveReviewTarget).not.toHaveBeenCalled();
    expect(wrapper.get('[data-testid="review-available-targets"]').text()).toBe("markdown");

    resolveCompatibility({
      piInstalled: true,
      openCodeInstalled: false,
    });
    await flushPromises();

    expect(wrapper.get('[data-testid="review-target-state"]').text()).toBe("pi");
    expect(wrapper.get('[data-testid="review-available-targets"]').text()).toBe("markdown,pi");
    expect(mocks.loadReviewTarget).toHaveBeenCalledWith("checkout:one");
    expect(mocks.saveReviewTarget).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("falls back to Markdown and persists it when the saved program is not installed", async () => {
    mocks.loadReviewTarget.mockResolvedValue("pi");
    mocks.getCompatibilitySnapshot.mockResolvedValue({
      piInstalled: false,
      openCodeInstalled: false,
    });
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
    await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
    await flushPromises();

    expect(wrapper.get('[data-testid="review-target-state"]').text()).toBe("markdown");
    expect(mocks.saveReviewTarget).toHaveBeenCalledWith("checkout:one", "markdown");
    wrapper.unmount();
  });

  it.each([
    ["neither program", false, false, "markdown"],
    ["Pi only", true, false, "markdown,pi"],
    ["OpenCode only", false, true, "markdown,opencode"],
    ["both programs", true, true, "markdown,opencode,pi"],
  ] as const)(
    "offers only confirmed review programs when %s are installed",
    async (_label, piInstalled, openCodeInstalled, expected) => {
      mocks.getCompatibilitySnapshot.mockResolvedValue({ piInstalled, openCodeInstalled });
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
      await flushPromises();

      expect(wrapper.get('[data-testid="review-available-targets"]').text()).toBe(expected);
      wrapper.unmount();
    },
  );

  it("keeps a manual destination through a pending preference load and compatibility refresh", async () => {
    let resolveSavedTarget!: (target: "opencode" | "pi") => void;
    let resolveRefresh!: (snapshot: { piInstalled: boolean; openCodeInstalled: boolean }) => void;
    mocks.getCompatibilitySnapshot.mockResolvedValue({ piInstalled: true, openCodeInstalled: true });
    mocks.loadReviewTarget.mockReturnValue(new Promise((resolve) => (resolveSavedTarget = resolve)));
    mocks.gitStatus = { branch: "feature", defaultBranch: "main" };
    mocks.reviewNotes = [reviewNoteFixture()];
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
    await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
    await flushPromises();
    mocks.getCompatibilitySnapshot.mockReturnValue(new Promise((resolve) => (resolveRefresh = resolve)));

    await wrapper.get('[data-testid="select-review-pi"]').trigger("click");
    mocks.saveReviewTarget.mockClear();
    await wrapper.get('[data-testid="send-review"]').trigger("click");
    resolveSavedTarget("opencode");
    await flushPromises();
    expect(wrapper.get('[data-testid="review-target-state"]').text()).toBe("pi");

    resolveRefresh({ piInstalled: true, openCodeInstalled: true });
    await flushPromises();
    const request = wrapper.getComponent({ name: "MainPane" }).props("shellRequest") as {
      launch?: { kind: string };
    };
    expect(request.launch?.kind).toBe("pi-review");
    expect(mocks.saveReviewTarget).not.toHaveBeenCalledWith("checkout:one", "markdown");
    expect(mocks.exportReviewMarkdown).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it.each(["pi", "opencode"] as const)(
    "does not export or reset a selected %s target when availability refresh fails",
    async (target) => {
      mocks.loadReviewTarget.mockResolvedValue(target);
      mocks.gitStatus = { branch: "feature", defaultBranch: "main" };
      mocks.reviewNotes = [reviewNoteFixture()];
      if (target === "opencode") {
        mocks.agentSessions = [agentSessionFixture("ses_one", 10)];
        mocks.agentTargetId = "ses_one";
      }
      const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
      await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
      await flushPromises();
      mocks.getCompatibilitySnapshot.mockRejectedValueOnce(new Error("compatibility lookup failed"));

      await wrapper.get('[data-testid="send-review"]').trigger("click");
      await flushPromises();

      expect(wrapper.get('[data-testid="review-target-state"]').text()).toBe(target);
      expect(mocks.exportReviewMarkdown).not.toHaveBeenCalled();
      expect(mocks.saveReviewTarget).not.toHaveBeenCalledWith("checkout:one", "markdown");
      expect(mocks.dispatchReviewRound).not.toHaveBeenCalled();
      expect(useToasts().toasts.value.at(-1)?.message).toContain("compatibility lookup failed");
      wrapper.unmount();
    },
  );

  it("does not auto-start OpenCode or record a review without a connected session", async () => {
    mocks.loadReviewTarget.mockResolvedValue("opencode");
    mocks.gitStatus = { branch: "feature", defaultBranch: "main" };
    mocks.reviewNotes = [reviewNoteFixture()];
    mocks.agentSessions = [];
    mocks.agentTargetId = null;
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

    await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
    await flushPromises();
    await wrapper.get('[data-testid="send-review"]').trigger("click");
    await flushPromises();

    expect(mocks.createAgentSession).not.toHaveBeenCalled();
    expect(mocks.dispatchReviewRound).not.toHaveBeenCalled();
    expect(mocks.refreshReviewNotes).not.toHaveBeenCalled();
    expect(useToasts().toasts.value.at(-1)?.message).toContain("No connected OpenCode session");
    wrapper.unmount();
  });

  it("preserves validated checkout paths while applying validated scroll-only updates", async () => {
    const normalizeState = vi.spyOn(uiStateDomain, "normalizeCheckoutUiState");
    mocks.loadCheckoutUiState.mockResolvedValue({
      ...DEFAULT_CHECKOUT_UI_STATE,
      document: { checkoutId: "checkout:one", path: "README.md", origin: "checkout", source: "file", mode: "view" },
      mainView: "document",
      selectedFilePath: "../outside",
      expandedDirectories: ["src", "src/../outside"],
      filesScrollTop: 1e20,
    });
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));

    wrapper.getComponent({ name: "DocumentPane" }).vm.$emit("readingPositionChanged", { top: Infinity, left: -10 });
    expect(normalizeState).toHaveBeenLastCalledWith({ documentScrollTop: Infinity, documentScrollLeft: -10 });
    normalizeState.mockRestore();
    await vi.advanceTimersByTimeAsync(300);
    await flushPromises();
    const firstCalls = mocks.saveCheckoutUiState.mock.calls;
    const firstSavedState = firstCalls[firstCalls.length - 1][1];
    expect(firstSavedState).toMatchObject({
      selectedFilePath: null,
      expandedDirectories: ["src"],
      filesScrollTop: 10_000_000,
      documentScrollTop: 0,
      documentScrollLeft: 0,
    });
    wrapper.unmount();
  });

  it("switches the main view from the inspector and back from a crumb, without unmounting the terminal", async () => {
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "Terminal 1")])));

    // The whole change set, then one file's diff: both are the diff view.
    await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
    await flushPromises();
    expect((wrapper.get("#main-view-diff").element as HTMLElement).style.display).not.toBe("none");
    expect(wrapper.get('[data-testid="item-crumb"]').text()).toBe("All changes");
    await wrapper.get('[data-testid="diff-scroll"]').trigger("click");
    await vi.advanceTimersByTimeAsync(300);
    await flushPromises();
    expect(mocks.saveCheckoutUiState).toHaveBeenLastCalledWith(
      "checkout:one",
      expect.objectContaining({ diffScrollTop: 132, diffAllFiles: true, document: null }),
    );

    await wrapper.get('[data-testid="open-file"]').trigger("click");
    await flushPromises();
    expect((wrapper.get("#main-view-diff").element as HTMLElement).style.display).toBe("none");
    expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).not.toBe("none");
    // The crumb names the view that is open, not the session behind it.
    expect(wrapper.get('[data-testid="item-crumb"]').text()).toBe("README.md");
    expect(wrapper.get('[data-testid="document-pane"]').text()).toBe("README.md");

    await wrapper.get('[data-testid="document-scroll"]').trigger("click");
    await wrapper.get('[data-testid="changes-scroll"]').trigger("click");
    await vi.advanceTimersByTimeAsync(300);
    await flushPromises();
    expect(mocks.saveCheckoutUiState).toHaveBeenLastCalledWith(
      "checkout:one",
      expect.objectContaining({
        mainView: "document",
        document: { checkoutId: "checkout:one", path: "README.md", origin: "checkout", source: "file", mode: "view" },
        documentScrollTop: 240,
        documentScrollLeft: 12,
        // A new view starts at the top, so the diff offset does not follow the document.
        diffScrollTop: 0,
        changesScrollTop: 84,
      }),
    );

    // Back to the terminal from the sidebar: the last crumb's menu only exists while a terminal
    // is what the panel is showing, so a file leaves it as plain text with nothing to open.
    expect(wrapper.get('[data-testid="item-crumb"]').classes()).not.toContain("text-menu-control");
    expect(wrapper.findAll('[data-testid^="menu-item-session:"]')).toHaveLength(0);
    await wrapper.get('[data-testid="select-session-one"]').trigger("click");
    await flushPromises();
    expect((wrapper.get("#main-view-terminal").element as HTMLElement).style.display).not.toBe("none");
    expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).toBe("none");
    expect(wrapper.get('[data-testid="item-crumb"]').text()).toBe("Terminal 1");
    expect(mocks.sessionPaneMounts).toBe(1);
    wrapper.unmount();
  });

  it("saves the whole change set as the checkout's restored view", async () => {
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "Terminal 1")])));

    await wrapper.get('[data-testid="open-all-changes"]').trigger("click");
    await flushPromises();
    expect((wrapper.get("#main-view-diff").element as HTMLElement).style.display).not.toBe("none");
    expect((wrapper.get("#main-view-terminal").element as HTMLElement).style.display).toBe("none");
    expect(wrapper.get('[data-testid="file-diff"]').text()).toBe("all");
    expect(wrapper.get('[data-testid="item-crumb"]').text()).toBe("All changes");
    expect(mocks.sessionPaneMounts).toBe(1);

    await vi.advanceTimersByTimeAsync(300);
    await flushPromises();
    expect(mocks.saveCheckoutUiState).toHaveBeenLastCalledWith(
      "checkout:one",
      expect.objectContaining({ mainView: "terminal", diffAllFiles: true, document: null }),
    );
    wrapper.unmount();
  });

  it("merges explicit terminal activation over deferred restore and ignores an older checkout load", async () => {
    const other = session("session:two", "Other", "checkout:two");
    let resolveOne!: (state: unknown) => void;
    let resolveTwo!: (state: unknown) => void;
    mocks.loadCheckoutUiState.mockImplementation(
      (id: string) =>
        new Promise((resolve) => {
          if (id === "checkout:one") resolveOne = resolve;
          else resolveTwo = resolve;
        }),
    );
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one"), checkout("checkout:two", [other])));
    await wrapper.get('[data-testid="select-checkout-two"]').trigger("click");
    await flushPromises();
    await wrapper.get('[data-testid="select-session-two"]').trigger("click");
    await flushPromises();
    expect(mocks.saveCheckoutUiState).not.toHaveBeenCalled();

    resolveTwo({
      ...DEFAULT_CHECKOUT_UI_STATE,
      document: { checkoutId: "checkout:two", path: "two.md", source: "file", mode: "view" },
      mainView: "document",
    });
    await flushPromises();
    expect(wrapper.get('[data-testid="item-crumb"]').text()).toBe("Other");
    expect((wrapper.get("#main-view-document").element as HTMLElement).style.display).toBe("none");
    expect(wrapper.get('[data-testid="document-pane"]').text()).toBe("");

    resolveOne({
      ...DEFAULT_CHECKOUT_UI_STATE,
      document: { checkoutId: "checkout:one", path: "one.md", source: "file", mode: "view" },
      mainView: "document",
    });
    await flushPromises();
    expect(wrapper.get('[data-testid="document-pane"]').text()).toBe("");
    // The late load of the checkout left behind is dropped: the view that is open is the one
    // the active checkout was switched to.
    expect((wrapper.get("#main-view-terminal").element as HTMLElement).style.display).not.toBe("none");
    expect(mocks.saveCheckoutUiState).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("asks before stopping a running process, and cancelling leaves the window open", async () => {
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "web")])));
    wrapper.getComponent({ name: "SessionPane" }).vm.$emit("sessionStatusChanged", "session:one", {
      state: "running",
      foregroundProcess: true,
      foregroundApp: "pnpm",
    });
    await flushPromises();
    mocks.getTerminalStatus.mockResolvedValue({
      state: "running",
      foregroundProcess: true,
      foregroundApp: "pnpm",
    });

    const closing = mocks.onCloseRequested!({ preventDefault: vi.fn() });
    await flushPromises();
    // Asked before anything is written or swept, and about the terminal the way the sidebar names
    // it — which here is the program, because nothing has renamed the terminal.
    const dialog = wrapper.findAll('[role="dialog"]').at(-1)!;
    expect(dialog.text()).toContain("pnpm");
    // And the program is not repeated beside a name that already is the program.
    expect(dialog.text()).not.toContain("(pnpm)");
    expect(mocks.currentWindow!.close).not.toHaveBeenCalled();
    expect(mocks.prepareAppExit).not.toHaveBeenCalled();

    await dialog.findAll("footer button").at(0)!.trigger("click");
    await Promise.all([closing, flushPromises()]);
    // Cancelled: nothing stopped, nothing written, and the window is still there to try again.
    expect(mocks.currentWindow!.close).not.toHaveBeenCalled();
    expect(mocks.prepareAppExit).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("closes without asking when every terminal is a shell at a prompt", async () => {
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one", [session("session:one", "zsh")])));
    wrapper.getComponent({ name: "SessionPane" }).vm.$emit("sessionStatusChanged", "session:one", {
      state: "running",
      foregroundProcess: false,
    });
    await flushPromises();
    mocks.getTerminalStatus.mockResolvedValue({ state: "running", foregroundProcess: false });

    const closing = mocks.onCloseRequested!({ preventDefault: vi.fn() });
    await Promise.all([closing, flushPromises()]);
    // A shell at a prompt loses nothing, so there is nothing to ask about.
    expect(wrapper.findAll('[role="dialog"]')).toHaveLength(0);
    expect(mocks.currentWindow!.close).toHaveBeenCalledOnce();
    wrapper.unmount();
  });

  it("waits for queued UI and settings writes before allowing a native close", async () => {
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
    let resolveLayoutWrite!: () => void;
    let resolveFirstSettingsWrite!: () => void;
    let resolveSecondSettingsWrite!: () => void;
    mocks.saveAppLayout.mockImplementation(() => new Promise<void>((resolve) => (resolveLayoutWrite = resolve)));
    mocks.saveSettings
      .mockImplementationOnce(() => new Promise<void>((resolve) => (resolveFirstSettingsWrite = resolve)))
      .mockImplementationOnce(() => new Promise<void>((resolve) => (resolveSecondSettingsWrite = resolve)));
    wrapper.getComponent(SplitterGroup).vm.$emit("layout", [320, 700, 300]);
    await vi.advanceTimersByTimeAsync(300);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "+", metaKey: true, cancelable: true }));
    await flushPromises();
    expect(mocks.saveAppLayout).toHaveBeenCalled();
    expect(mocks.saveSettings).toHaveBeenCalled();

    const preventDefault = vi.fn();
    const closeHandler = mocks.onCloseRequested!;
    const closing = closeHandler({ preventDefault });
    const duplicateClose = closeHandler({ preventDefault: vi.fn() });
    await flushPromises();
    expect(preventDefault).toHaveBeenCalledOnce();
    expect(mocks.currentWindow!.close).not.toHaveBeenCalled();

    // A later preference action can arrive while the prevented close is flushing earlier work.
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "+", metaKey: true, cancelable: true }));
    await flushPromises();
    resolveLayoutWrite();
    await flushPromises();
    expect(mocks.currentWindow!.close).not.toHaveBeenCalled();
    resolveFirstSettingsWrite();
    await flushPromises();
    expect(mocks.saveSettings).toHaveBeenCalledTimes(2);
    expect(mocks.currentWindow!.close).not.toHaveBeenCalled();
    resolveSecondSettingsWrite();
    await Promise.all([closing, duplicateClose]);
    expect(mocks.currentWindow!.close).toHaveBeenCalledOnce();
    wrapper.unmount();
  });
  it("logs an incomplete UI-write deadline before allowing close", async () => {
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
    let resolveWrite!: () => void;
    let rejectWrite!: (cause: unknown) => void;
    let resolveDiagnostic!: () => void;
    mocks.saveAppLayout.mockImplementation(
      () =>
        new Promise<void>((resolve, reject) => {
          resolveWrite = resolve;
          rejectWrite = reject;
        }),
    );
    wrapper.getComponent(SplitterGroup).vm.$emit("layout", [320, 700, 300]);
    await vi.advanceTimersByTimeAsync(300);
    mocks.reportFrontendDiagnostic.mockImplementation(
      () => new Promise<void>((resolve) => (resolveDiagnostic = resolve)),
    );

    const closing = mocks.onCloseRequested!({ preventDefault: vi.fn() });
    await flushPromises();
    await vi.advanceTimersByTimeAsync(5000);
    await flushPromises();
    expect(mocks.reportFrontendDiagnostic).toHaveBeenCalledExactlyOnceWith("ui_writes_deadline");
    expect(mocks.currentWindow!.close).not.toHaveBeenCalled();

    rejectWrite(new Error("late write failure remains inconclusive after deadline"));
    await flushPromises();
    expect(mocks.currentWindow!.close).not.toHaveBeenCalled();

    resolveDiagnostic();
    await closing;
    expect(mocks.currentWindow!.close).toHaveBeenCalledOnce();
    resolveWrite();
    wrapper.unmount();
  });

  it("logs an incomplete exit-sweep deadline before allowing close", async () => {
    const wrapper = await mountApp(workspaceWith(checkout("checkout:one")));
    let resolveSweep!: () => void;
    let resolveDiagnostic!: () => void;
    mocks.prepareAppExit.mockImplementation(() => new Promise<void>((resolve) => (resolveSweep = resolve)));
    mocks.reportFrontendDiagnostic.mockImplementation(
      () => new Promise<void>((resolve) => (resolveDiagnostic = resolve)),
    );

    const closing = mocks.onCloseRequested!({ preventDefault: vi.fn() });
    await flushPromises();
    await vi.advanceTimersByTimeAsync(5000);
    await flushPromises();
    expect(mocks.reportFrontendDiagnostic).toHaveBeenCalledExactlyOnceWith("exit_sweep_deadline");
    expect(mocks.currentWindow!.close).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(250);
    await closing;
    expect(mocks.currentWindow!.close).toHaveBeenCalledOnce();
    resolveDiagnostic();
    resolveSweep();
    wrapper.unmount();
  });

  describe("the sidebar shortcut", () => {
    const pressInSidebar = (init: KeyboardEventInit, target = wrapper.get("#navigation-panel").element) => {
      const event = new KeyboardEvent("keydown", { cancelable: true, bubbles: true, ...init });
      target.dispatchEvent(event);
      return event;
    };
    let wrapper: Awaited<ReturnType<typeof mountApp>>;

    beforeEach(async () => {
      // Attached, because the assertion below is about where a keydown stops: a tree that is not in
      // the document has no capture path from a panel to the window at all.
      wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        { attachTo: document.body },
      );
    });

    it("collapses and brings back the navigation panel on Cmd+/", async () => {
      const panel = wrapper.findAllComponents({ name: "SplitterPanel" })[0]!;

      expect(pressInSidebar({ key: "/", metaKey: true }).defaultPrevented).toBe(true);
      await flushPromises();
      expect(panel.emitted("collapse")).toBeTruthy();

      pressInSidebar({ key: "/", metaKey: true });
      await flushPromises();
      expect(panel.emitted("expand")).toBeTruthy();
      wrapper.unmount();
    });

    it("takes the files and changes panel with it, so the main view is alone in the window", async () => {
      // One shortcut, both panels: a file tree and a change list standing between the main view and
      // the window's edge is the arrangement nobody asked for by hiding the sidebar.
      const panel = wrapper.findAllComponents({ name: "SplitterPanel" })[2]!;
      const inspector = () => wrapper.findComponent({ name: "InspectorPane" });

      expect(inspector().isVisible()).toBe(true);
      pressInSidebar({ key: "/", metaKey: true });
      await flushPromises();
      expect(panel.emitted("collapse")).toBeTruthy();
      expect(inspector().isVisible()).toBe(false);

      pressInSidebar({ key: "/", metaKey: true });
      await flushPromises();
      expect(panel.emitted("expand")).toBeTruthy();
      expect(inspector().isVisible()).toBe(true);
      wrapper.unmount();
    });

    it("takes the handles away with the panels, so a hidden sidebar has nothing to grab", async () => {
      const handle = (id: string) => wrapper.findAll(id)[0];

      expect(handle("#navigation-resize-handle").isVisible()).toBe(true);
      expect(handle("#inspector-resize-handle").isVisible()).toBe(true);
      pressInSidebar({ key: "/", metaKey: true });
      await flushPromises();
      expect(handle("#navigation-resize-handle").exists() && handle("#navigation-resize-handle").isVisible()).toBe(
        false,
      );
      expect(handle("#inspector-resize-handle").exists() && handle("#inspector-resize-handle").isVisible()).toBe(false);
      wrapper.unmount();
    });

    it("hides the drawer as well, in the two arrangements where the inspector floats", async () => {
      // Narrow and split both put the inspector over the main view, and a drawer has no width to
      // collapse: what shows it is the state it is bound to, which is the state the shortcut moved.
      // One arrangement at a time, because the key travels to the window and every app mounted on
      // this document answers it.
      for (const arrangement of [
        { width: 900, layout: { ...DEFAULT_APP_LAYOUT } },
        { width: 1400, layout: { ...DEFAULT_APP_LAYOUT, mode: "split" as const } },
      ]) {
        Object.defineProperty(window, "innerWidth", { configurable: true, value: arrangement.width });
        const floating = await mountApp(workspaceWith(checkout("checkout:one")), arrangement.layout, {
          attachTo: document.body,
        });

        expect(floating.findComponent({ name: "InspectorPane" }).isVisible()).toBe(true);
        pressInSidebar({ key: "/", metaKey: true }, floating.get("#navigation-panel").element);
        await flushPromises();
        expect(floating.findComponent({ name: "InspectorPane" }).isVisible()).toBe(false);
        // The strip is the drawer peeking on hover, and a strip that opens nothing is a hotspot
        // for a panel that is not there.
        expect(floating.find(".inspector-hover-strip").exists()).toBe(false);
        floating.unmount();
      }
      wrapper.unmount();
    });

    it("gives no width back to a panel that is still hidden", async () => {
      // A window that grows back is not a request to show the panel: an expanded panel nobody can
      // see is width the main view loses, so the shortcut is the only thing that brings it back.
      const resizeCalls = vi.fn();
      mocks.onProgrammaticPanelResize = resizeCalls;
      resizeCalls.mockClear();

      pressInSidebar({ key: "/", metaKey: true });
      await flushPromises();
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 900 });
      window.dispatchEvent(new Event("resize"));
      await flushPromises();
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 1400 });
      window.dispatchEvent(new Event("resize"));
      await flushPromises();
      expect(resizeCalls).not.toHaveBeenCalled();

      pressInSidebar({ key: "/", metaKey: true });
      await flushPromises();
      expect(wrapper.findComponent({ name: "InspectorPane" }).isVisible()).toBe(true);
      wrapper.unmount();
    });

    it("stops the key before the terminal or the editor can read it as input", async () => {
      // The whole reason the listener is on capture: by the time a keydown reaches the window on
      // its way up, xterm has already turned it into input, and a shell reading Cmd+/ as
      // previous-command is the failure this shortcut exists to prevent.
      const seenByTerminal: string[] = [];
      (wrapper.get("#navigation-panel").element as HTMLElement).addEventListener("keydown", (event) => {
        seenByTerminal.push((event as KeyboardEvent).key);
      });

      pressInSidebar({ key: "/", metaKey: true });
      await flushPromises();

      expect(seenByTerminal).toEqual([]);
      wrapper.unmount();
    });

    it("stops listening once the window it belongs to is gone", async () => {
      const removed = vi.spyOn(window, "removeEventListener");

      wrapper.unmount();

      // The capture flag is part of the registration: a listener removed without it is not the one
      // that was added, and this one would outlive the window and keep swallowing Cmd+/.
      expect(removed).toHaveBeenCalledWith("keydown", expect.any(Function), true);
      removed.mockRestore();
    });

    it("leaves a plain Escape alone", async () => {
      pressInSidebar({ key: "/", metaKey: true });
      await flushPromises();
      expect(wrapper.findAllComponents({ name: "SplitterPanel" })[0]!.emitted("collapse")).toBeTruthy();

      pressInSidebar({ key: "/", metaKey: true });
      await flushPromises();
      expect(wrapper.findAllComponents({ name: "SplitterPanel" })[0]!.emitted("collapse")).toHaveLength(1);

      // Escape is not a key this window answers: nothing in App.vue or the inspector has a branch
      // for it, and the ones that do own one — a field, a dialog, the toast stack — are either
      // elsewhere in the tree or have nothing to take. What is pinned here is narrower: a bare
      // Escape is not swallowed by the shortcut, and does not toggle the panels.
      const plain = pressInSidebar({ key: "Escape" });
      expect(plain.defaultPrevented).toBe(false);
      await flushPromises();
      expect(wrapper.findAllComponents({ name: "SplitterPanel" })[0]!.emitted("collapse")).toHaveLength(1);
      wrapper.unmount();
    });

    it("finds the key that spells the shortcut wherever the layout puts it", async () => {
      // With Cmd held down macOS reports the key without shift, so a layout whose `/` is Shift+7
      // delivers a `7` and never a `/`. That is what the real keystroke on such a machine is, and
      // it is the one every layout is agreed on: the position, which `code` is.
      const panel = wrapper.findAllComponents({ name: "SplitterPanel" })[0]!;

      // Hide on one, bring back on the other: the two spellings are the same chord, so the second
      // answers exactly what the first undid rather than adding a shortcut of its own.
      for (const init of [
        { key: "/", code: "Slash" },
        { key: "7", code: "Digit7", shiftKey: true },
      ]) {
        expect(pressInSidebar({ ...init, metaKey: true }).defaultPrevented, JSON.stringify(init)).toBe(true);
        await flushPromises();
      }
      expect(panel.emitted("collapse")).toHaveLength(1);
      expect(panel.emitted("expand")).toHaveLength(1);

      // Shift on its own is not the shortcut: the shift is what turns a 7 into a `/`, and a
      // shortcut key must not answer on the number the same key types without it.
      expect(pressInSidebar({ key: "7", code: "Digit7", metaKey: true }).defaultPrevented).toBe(false);
      wrapper.unmount();
    });

    it("is Ctrl where Cmd is not the platform's key, and is nothing at all on a Mac", async () => {
      // On a Mac Ctrl is the Control key and the terminal already answers to it, so the shortcut
      // takes only Cmd. Everywhere else the same chord is the platform's own shortcut key, and
      // Cmd on a Linux panel is the Super key, which belongs to the window manager.
      const panel = wrapper.findAllComponents({ name: "SplitterPanel" })[0]!;

      reportsPlatform("Linux x86_64");
      pressInSidebar({ key: "/", ctrlKey: true });
      await flushPromises();
      expect(panel.emitted("collapse")).toBeTruthy();
      pressInSidebar({ key: "/", metaKey: true });
      await flushPromises();
      expect(panel.emitted("collapse")).toHaveLength(1);

      reportsPlatform("MacIntel");
      const before = panel.emitted("collapse")?.length ?? 0;
      // The Control key is not a shortcut here, and neither is the same key with Alt held down.
      pressInSidebar({ key: "/", ctrlKey: true });
      pressInSidebar({ key: "/", metaKey: true, altKey: true });
      await flushPromises();
      expect(panel.emitted("collapse")).toHaveLength(before);
      wrapper.unmount();
    });
  });

  describe("the new terminal shortcut", () => {
    // Attached and dispatched from a panel, because that is the path the capture listener on the
    // window answers to. The request itself is visible as the checkout selection: the pane turns
    // that into the terminal, and it is the one thing about a request that is not the pane's.
    const pressInPanel = (init: KeyboardEventInit, target: Element) => {
      const event = new KeyboardEvent("keydown", { cancelable: true, bubbles: true, ...init });
      target.dispatchEvent(event);
      return event;
    };

    it("asks for a terminal in the workdir the window is on", async () => {
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one"), checkout("checkout:two")),
        { ...DEFAULT_APP_LAYOUT },
        { attachTo: document.body },
      );

      // The shell reads Cmd+N as downcase-word, so a key that reaches xterm is a key typed into
      // whatever terminal had the focus rather than one this window answered.
      expect(pressInPanel({ key: "n", metaKey: true }, wrapper.get("#navigation-panel").element).defaultPrevented).toBe(
        true,
      );
      await flushPromises();

      expect(mocks.selectCheckout).toHaveBeenCalledWith("checkout:one");
      wrapper.unmount();
    });

    it("asks for one in Home when no workdir is open", async () => {
      // Home is the checkout the app records for the user's own directory at startup, so a window
      // with nothing open answers the key with a shell rather than ignoring it.
      const homeCheckout = checkout("checkout:home");
      const wrapper = await mountApp(
        { ...workspaceWith(homeCheckout), activeCheckoutId: null, homeCheckoutId: homeCheckout.id },
        { ...DEFAULT_APP_LAYOUT },
        { attachTo: document.body },
      );

      pressInPanel({ key: "n", metaKey: true }, wrapper.get("#navigation-panel").element);
      await flushPromises();

      expect(mocks.selectCheckout).toHaveBeenCalledWith(homeCheckout.id);
      wrapper.unmount();
    });

    it("is the only key that opens one, and the panels key does not double as one", async () => {
      // Cmd+T used to open a terminal from the session pane and now does nothing: one shortcut, one
      // job, so that a muscle memory pressed in the wrong place is not answered by the wrong pane.
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        { attachTo: document.body },
      );
      const panel = () => wrapper.get("#navigation-panel").element;

      expect(pressInPanel({ key: "t", metaKey: true }, panel()).defaultPrevented).toBe(false);
      expect(pressInPanel({ key: "/", metaKey: true }, panel()).defaultPrevented).toBe(true);
      await flushPromises();

      expect(mocks.selectCheckout).not.toHaveBeenCalled();
      expect(wrapper.findAllComponents({ name: "SplitterPanel" })[0]!.emitted("collapse")).toBeTruthy();
      wrapper.unmount();
    });

    it("is on a key no shell is asking for", async () => {
      // The panels key is `/` rather than something more memorable because the memorable ones are
      // spoken for: Cmd+H belongs to the app menu's Hide item and Cmd+Esc never reaches the webview
      // at all, and every letter is a readline binding that a terminal behind this window is using.
      // This is the assertion that fails first if a future key choice lands on one of those.
      const readline = ["b", "d", "e", "f", "k", "l", "p", "s", "t", "u", "w", "y"];
      const macTeaches = ["h", "m", "w", "q", "c", "x", "v", "a", "z", ","];
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        { attachTo: document.body },
      );
      const panel = () => wrapper.get("#navigation-panel").element;

      for (const key of readline) {
        expect(pressInPanel({ key, metaKey: true }, panel()).defaultPrevented, key).toBe(false);
      }
      for (const key of macTeaches) {
        expect(pressInPanel({ key, metaKey: true }, panel()).defaultPrevented, key).toBe(false);
      }
      // And the two the window does answer are the two that are its own.
      expect(pressInPanel({ key: "n", metaKey: true }, panel()).defaultPrevented).toBe(true);
      expect(pressInPanel({ key: "/", metaKey: true }, panel()).defaultPrevented).toBe(true);
      wrapper.unmount();
    });
  });
  describe("the settings dialog", () => {
    const openSettings = async (settings = DEFAULT_SETTINGS) => {
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        {
          settings: cloneSettings(settings),
        },
      );
      await wrapper.get('[data-testid="settings-button"]').trigger("click");
      return wrapper;
    };

    it("reports detected review executables and refreshes compatibility", async () => {
      mocks.getCompatibilitySnapshot.mockResolvedValue({
        piInstalled: true,
        openCodeInstalled: false,
      });
      const wrapper = await openSettings();
      const section = wrapper.get('[data-testid="compatibility-section"]');
      const status = (label: string) => {
        const row = section.findAll("dt").find((item) => item.text() === label)!;
        return row.element.nextElementSibling?.textContent?.trim();
      };
      expect(status("Pi executable")).toBe("Installed");
      expect(status("OpenCode executable")).toBe("Not detected");

      const snapshotCalls = mocks.getCompatibilitySnapshot.mock.calls.length;
      await section
        .findAll("button")
        .find((button) => button.text() === "Refresh")!
        .trigger("click");
      await flushPromises();
      expect(mocks.getCompatibilitySnapshot).toHaveBeenCalledTimes(snapshotCalls + 1);
      wrapper.unmount();
    });

    it("keeps settings hidden and zoom inert until the saved preferences load", async () => {
      let resolveSettings!: (settings: AppSettings) => void;
      const settingsLoad = new Promise<AppSettings>((resolve) => (resolveSettings = resolve));
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        { settingsLoad },
      );

      expect(wrapper.find('[data-testid="settings-button"]').exists()).toBe(false);
      const zoom = new KeyboardEvent("keydown", { key: "+", metaKey: true, cancelable: true });
      window.dispatchEvent(zoom);
      expect(zoom.defaultPrevented).toBe(false);
      expect(mocks.saveSettings).not.toHaveBeenCalled();

      resolveSettings({
        ...cloneSettings(DEFAULT_SETTINGS),
        ui: { ...DEFAULT_SETTINGS.ui, fontSize: 18 },
      });
      await flushPromises();
      expect(wrapper.find('[data-testid="settings-button"]').exists()).toBe(true);
      expect(document.documentElement.style.getPropertyValue("--muster-ui-font-scale")).toBe(String(18 / 14));
      wrapper.unmount();
    });

    it("names the review folder, weighs it, and asks before deleting what is in it", async () => {
      mocks.getReviewFolder.mockResolvedValue({
        path: "/Users/dev/.muster/tmp/reviews",
        storage: "default",
        files: 3,
        bytes: 2048,
      });
      const wrapper = await openSettings();
      await flushPromises();

      // Asked for the checkout on screen, in the mode the selector is showing: the folder is on
      // disk and is not in the settings file.
      expect(mocks.getReviewFolder).toHaveBeenCalledWith("checkout:one", "default");
      const folder = wrapper.get('[data-testid="review-folder"]');
      expect(folder.text()).toContain("/Users/dev/.muster/tmp/reviews");
      expect(folder.text()).toContain("3 notes");
      expect(folder.text()).toContain("2.0 KB");

      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Delete notes")!
        .trigger("click");
      await flushPromises();
      // Nothing is deleted on the click: the question is the point, and it says what it leaves.
      expect(mocks.clearReviewFolder).not.toHaveBeenCalled();
      expect(wrapper.findAll('[role="dialog"]').at(-1)!.text()).toContain("are deleted");

      await wrapper
        .findAll('[role="dialog"]')
        .at(-1)!
        .findAll("button")
        .find((button) => button.text() === "Delete")!
        .trigger("click");
      await flushPromises();
      expect(mocks.clearReviewFolder).toHaveBeenCalledWith("checkout:one", "/Users/dev/.muster/tmp/reviews");

      // Moving the selector asks for the other folder without waiting for Apply: the row names
      // the folder the chosen mode writes to, and that folder is not in the settings file.
      const select = wrapper
        .findAllComponents(SelectControl)
        .find((control) => control.props("label") === "Exported notes")!;
      expect(select.props("options")).toEqual([
        { value: "default", label: "Default" },
        { value: "workdir", label: "Work directory" },
      ]);
      await select.get('button[data-value="workdir"]').trigger("click");
      await flushPromises();
      expect(mocks.getReviewFolder).toHaveBeenLastCalledWith("checkout:one", "workdir");
      wrapper.unmount();
    });

    it("ignores review-folder replies superseded by a draft change", async () => {
      type Folder = { path: string; storage: "default" | "workdir"; files: number; bytes: number };
      const pending: Array<{ resolve: (folder: Folder) => void; reject: (error: unknown) => void }> = [];
      mocks.getReviewFolder.mockImplementation(
        () => new Promise<Folder>((resolve, reject) => pending.push({ resolve, reject })),
      );
      const wrapper = await openSettings();
      const folder = () => wrapper.get('[data-testid="review-folder"]');
      const select = wrapper
        .findAllComponents(SelectControl)
        .find((control) => control.props("label") === "Exported notes")!;
      const choose = async (storage: "default" | "workdir") => {
        await select.get('button[data-value="' + storage + '"]').trigger("click");
        await flushPromises();
      };
      const answer = (path: string, storage: Folder["storage"]): Folder => ({ path, storage, files: 1, bytes: 100 });

      expect(pending).toHaveLength(1);
      await choose("workdir");
      pending[1]!.resolve(answer("/workdir-review", "workdir"));
      await flushPromises();
      expect(folder().text()).toContain("/workdir-review");
      expect(wrapper.findAll("button").some((button) => button.text() === "Delete notes")).toBe(false);

      await choose("default");
      expect(folder().text()).toBe("");
      pending[0]!.resolve(answer("/stale-default", "default"));
      await flushPromises();
      expect(folder().text()).toBe("");
      pending[2]!.resolve(answer("/latest-default", "default"));
      await flushPromises();
      expect(folder().text()).toContain("/latest-default");

      await choose("workdir");
      await choose("default");
      pending[4]!.resolve(answer("/current-default", "default"));
      await flushPromises();
      pending[3]!.reject(new Error("stale request"));
      await flushPromises();
      expect(folder().text()).toContain("/current-default");
      wrapper.unmount();
    });

    it("uses the confirmed default folder even when the saved mode is workdir", async () => {
      mocks.getReviewFolder.mockImplementation(async (_checkoutId: string, storage: "default" | "workdir") => ({
        path: storage === "default" ? "/Users/dev/.muster/tmp/reviews" : "/Users/dev/work/app/.muster/reviews",
        storage,
        files: 1,
        bytes: 128,
      }));
      mocks.clearReviewFolder.mockResolvedValue({ files: 1, bytes: 128 });
      const savedSettings = cloneSettings(DEFAULT_SETTINGS);
      savedSettings.reviews.storage = "workdir";
      const wrapper = await openSettings(savedSettings);
      await flushPromises();
      const select = wrapper
        .findAllComponents(SelectControl)
        .find((control) => control.props("label") === "Exported notes")!;
      await select.get('button[data-value="default"]').trigger("click");
      await flushPromises();

      expect(wrapper.get('[data-testid="review-folder"]').text()).toContain("/Users/dev/.muster/tmp/reviews");
      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Delete notes")!
        .trigger("click");
      await flushPromises();
      expect(wrapper.findAll('[role="dialog"]').at(-1)!.text()).toContain("/Users/dev/.muster/tmp/reviews");
      await wrapper
        .findAll('[role="dialog"]')
        .at(-1)!
        .findAll("button")
        .find((button) => button.text() === "Delete")!
        .trigger("click");
      await flushPromises();
      expect(mocks.clearReviewFolder).toHaveBeenCalledWith("checkout:one", "/Users/dev/.muster/tmp/reviews");
      wrapper.unmount();
    });

    it("does not show a folder response after its checkout disappears", async () => {
      type Folder = { path: string; storage: "default"; files: number; bytes: number };
      let resolveFolder!: (folder: Folder) => void;
      mocks.getReviewFolder.mockReturnValue(
        new Promise<Folder>((resolve) => {
          resolveFolder = resolve;
        }),
      );
      const wrapper = await openSettings();
      mocks.workspaceRef!.value = { ...mocks.workspaceRef!.value, activeCheckoutId: null };
      await flushPromises();
      resolveFolder({ path: "/stale", storage: "default", files: 1, bytes: 1 });
      await flushPromises();

      expect(wrapper.get('[data-testid="review-folder"]').text()).toBe("");
      wrapper.unmount();
    });

    it("offers no delete for a folder the working directory owns", async () => {
      mocks.getReviewFolder.mockResolvedValue({
        path: "/Users/dev/work/app/.muster/reviews",
        storage: "workdir",
        files: 1,
        bytes: 512,
      });
      const savedSettings = cloneSettings(DEFAULT_SETTINGS);
      savedSettings.reviews.storage = "workdir";
      const wrapper = await openSettings(savedSettings);
      await flushPromises();

      expect(wrapper.get('[data-testid="review-folder"]').text()).toContain("/Users/dev/work/app/.muster/reviews");
      expect(wrapper.findAll("button").some((button) => button.text() === "Delete notes")).toBe(false);
      wrapper.unmount();
    });

    it("opens off the gear, draws a control for every preference, and closes on Cancel", async () => {
      const wrapper = await openSettings();
      expect(wrapper.find('[role="dialog"]').exists()).toBe(true);

      // Every field in the schema has a control, and no control is drawn for anything else: the
      // form is the schema, and the test reads the schema rather than a copy of it.
      for (const section of SETTINGS_SECTIONS) {
        for (const field of section.fields) {
          const control = `settings-${field.path.replaceAll(".", "-")}`;
          // Inputs are labelled by `for`/`id` and pickers by the testid their trigger carries, so
          // the check asks for either rather than making the test know which kind of field it is.
          expect(wrapper.find(`#${control}, [data-testid="${control}"]`).exists(), field.path).toBe(true);
        }
      }

      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Cancel")!
        .trigger("click");
      expect(wrapper.find('[role="dialog"]').exists()).toBe(false);
      expect(mocks.saveSettings).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("draws the version it read out of the build", async () => {
      const wrapper = await openSettings();
      // What the About section is opened for, and the one fact on screen that nothing else can
      // say. It is the version the window asked the runtime for, not a string written into the
      // dialog, so a release cannot leave it naming the build before it.
      expect(wrapper.get('[data-testid="about-version"]').text()).toContain("9.9.9");
      wrapper.unmount();
    });

    it("draws no version rather than a wrong one when the build cannot be asked", async () => {
      mocks.getVersion.mockRejectedValueOnce(new Error("no version here"));
      const wrapper = await openSettings();
      // The alternative is a fallback written by hand, which is a version that is right until the
      // next release and silently wrong after it.
      expect(wrapper.get('[data-testid="about-version"]').text()).not.toMatch(/\d+\.\d+\.\d+/);
      wrapper.unmount();
    });

    it("writes every chord down as the platform it is drawn on presses it", async () => {
      const wrapper = await openSettings();
      // Read the list rather than a copy of it, for the reason the field test above does: what
      // matters is that nothing in `shortcuts.ts` is dropped on the way to the screen.
      const section = wrapper.get('[data-testid="shortcuts-section"]').text();
      for (const group of SHORTCUT_GROUPS) {
        expect(section, group.title).toContain(group.title);
        for (const shortcut of group.shortcuts) {
          // The spelling this window's platform uses: `navigator.platform` is what the default
          // argument reads, and a chord printed the other way round is one key nobody has.
          expect(section, shortcutChord(shortcut.keys)).toContain(shortcutChord(shortcut.keys));
          expect(section, shortcut.description).toContain(shortcut.description);
        }
      }
      expect(section).not.toContain("MOD");
      wrapper.unmount();
    });

    it("draws the credits folded away, and still draws all of them", async () => {
      const wrapper = await openSettings();
      // Read the list rather than a copy of it, for the reason the field test above does: what
      // matters is that nothing in `credits.ts` is dropped on the way to the screen. The whole list
      // is longer than the rest of About and is the answer to a question nobody opened the dialog
      // with, so it is one closed row — closed, not gone.
      const credits = wrapper.get('[data-testid="about-credits"]');
      expect(credits.attributes("open")).toBeUndefined();
      expect(credits.text()).toContain(CREDITS_TITLE);
      for (const group of CREDITS) {
        expect(credits.text(), group.title).toContain(group.title);
        for (const entry of group.entries) {
          expect(credits.text(), entry.name).toContain(entry.name);
          expect(credits.text(), entry.name).toContain(entry.license);
        }
      }
      wrapper.unmount();
    });

    it("names the app, its licence and where the source is", async () => {
      const wrapper = await openSettings();
      const about = wrapper.get('[data-testid="about-section"]').text();
      expect(about).toContain(ACKNOWLEDGEMENT);
      // And the repository, which is the one thing a person in this section is most likely to want.
      expect(about).toContain(REPOSITORY);
      // The acknowledgement used to promise a list that sat under it; that list is folded now, so a
      // line saying what is "below" it would be a claim about what is on screen.
      expect(about).not.toMatch(/below/);
      wrapper.unmount();
    });

    it("hands the repository to the browser instead of following it", async () => {
      const wrapper = await openSettings();
      // The webview is not a browser: following the link would replace the window that has the
      // drafts and the terminals in it. The href is kept anyway, because a link whose only way out
      // is a click cannot be copied — this window denies its own right-click menu everywhere.
      const link = wrapper.get('[data-testid="about-repository"]');
      expect(link.attributes("href")).toBe(REPOSITORY);
      const openExternalUrl = mocks.openExternalUrl;
      openExternalUrl.mockClear();
      await link.trigger("click");
      expect(openExternalUrl).toHaveBeenCalledWith(REPOSITORY);
      wrapper.unmount();
    });

    it("says so when a file the tree offered would not open, rather than leaving the row be", async () => {
      // A row that did not open and a row that opened something are the same picture from here,
      // and the person who pressed ctrl is the only one who can tell which happened.
      mocks.openExternalFile.mockRejectedValueOnce(new Error("no application is registered for it"));
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        { settings: cloneSettings(DEFAULT_SETTINGS) },
      );
      await flushPromises();

      await wrapper
        .findComponent({ name: "InspectorPane" })
        .vm.$emit("openExternalFile", { checkoutId: "checkout:one", path: "design.psd" });
      await flushPromises();

      expect(mocks.openExternalFile).toHaveBeenCalledWith("checkout:one", "design.psd");
      const { toasts } = useToasts();
      expect(toasts.value.at(-1)?.message).toContain("no application is registered");
      wrapper.unmount();
    });

    it("paints the window in the theme the preference names, and in the system's while it does not", async () => {
      const listeners: Array<() => void> = [];
      const system = {
        matches: true,
        addEventListener: (_: string, listener: () => void) => listeners.push(listener),
        removeEventListener: vi.fn(),
      };
      vi.spyOn(window, "matchMedia").mockReturnValue(system as unknown as MediaQueryList);

      // The attribute is the whole of it: the two palettes in `muster.css` are keyed off it, and a
      // palette the preference names outright is followed rather than argued with.
      const light = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        {
          settings: { ...cloneSettings(DEFAULT_SETTINGS), ui: { ...DEFAULT_SETTINGS.ui, theme: "light" } },
        },
      );
      expect(document.documentElement.dataset.theme).toBe("light");
      // A window told what to draw in does not consult the system, but it still listens for it:
      // the listener is registered once for the shell's lifetime, not per preference change.
      system.matches = false;
      for (const listener of listeners) listener();
      expect(document.documentElement.dataset.theme).toBe("light");
      light.unmount();
      expect(system.removeEventListener).toHaveBeenCalledWith("change", expect.any(Function));

      system.matches = true;
      const following = await mountApp(workspaceWith(checkout("checkout:one")));
      expect(document.documentElement.dataset.theme).toBe("dark");

      // `system` is the preference that says to keep asking: the answer changes while the window is
      // open, and a window that was asked once would sit in the palette it started in.
      system.matches = false;
      for (const listener of listeners) listener();
      expect(document.documentElement.dataset.theme).toBe("light");
      following.unmount();
    });

    it("moves focus into the dialog, traps Tab, and restores the gear on close", async () => {
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        { attachTo: document.body },
      );
      const gear = wrapper.get('[data-testid="settings-button"]').element;
      await wrapper.get('[data-testid="settings-button"]').trigger("click");
      await flushPromises();

      expect(document.activeElement).toBe(wrapper.get("button[autofocus]").element);
      const apply = wrapper.findAll("button").find((button) => button.text() === "Apply")!.element;
      apply.focus();
      apply.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true }));
      expect(document.activeElement).toBe(wrapper.get('button[aria-label="Close"]').element);

      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Cancel")!
        .trigger("click");
      await flushPromises();
      expect(wrapper.find('[role="dialog"]').exists()).toBe(false);
      expect(document.activeElement).toBe(gear);
      wrapper.unmount();
    });

    it("locks the draft while saving, ignores zoom, and retains it after a failed save", async () => {
      let rejectSave!: (error: Error) => void;
      mocks.saveSettings.mockImplementationOnce(() => new Promise<void>((_, reject) => (rejectSave = reject)));
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        { attachTo: document.body },
      );
      await wrapper.get('[data-testid="settings-button"]').trigger("click");
      const fontSize = wrapper.get("#settings-editor-fontSize");
      await fontSize.setValue("15");
      await fontSize.trigger("change");
      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Apply")!
        .trigger("click");
      await flushPromises();

      expect(fontSize.attributes("disabled")).toBeDefined();
      expect(wrapper.get("button[aria-label=Close]").attributes("disabled")).toBeDefined();
      expect(
        wrapper
          .findAll("button")
          .find((button) => button.text() === "Cancel")!
          .attributes("disabled"),
      ).toBeDefined();
      expect(
        wrapper
          .findAll("button")
          .find((button) => button.text() === "Reset to defaults")!
          .attributes("disabled"),
      ).toBeDefined();
      expect(wrapper.find('[role="dialog"]').attributes("aria-busy")).toBe("true");
      const zoom = new KeyboardEvent("keydown", { key: "+", metaKey: true, cancelable: true });
      window.dispatchEvent(zoom);
      expect(mocks.saveSettings).toHaveBeenCalledOnce();
      expect(document.documentElement.style.getPropertyValue("zoom")).toBe("1");
      await wrapper.get('[role="dialog"]').trigger("keydown", { key: "Escape" });
      expect(wrapper.find('[role="dialog"]').exists()).toBe(true);

      rejectSave(new Error("settings write failed"));
      await flushPromises();
      expect(wrapper.find('[role="dialog"]').exists()).toBe(true);
      expect((wrapper.get("#settings-editor-fontSize").element as HTMLInputElement).value).toBe("15");
      expect(wrapper.get("#settings-editor-fontSize").attributes("disabled")).toBeUndefined();
      wrapper.unmount();
    });

    it.each(["off", "cd", "agent", "both"] as const)(
      "persists worktree following as %s through Apply",
      async (mode) => {
        const wrapper = await openSettings();
        const controlId = "settings-terminal-followWorktree";
        const select = wrapper
          .findAllComponents(SelectControl)
          .find((control) => control.props("label") === "Follow worktree")!;
        expect(wrapper.get(`label[for="${controlId}"]`).text()).toContain("Follow worktree");
        expect(wrapper.get(`#${controlId}`).element.tagName).toBe("BUTTON");
        expect(select.props("modelValue")).toBe("agent");
        expect(select.props("options")).toEqual(
          ["off", "cd", "agent", "both"].map((value) => ({ value, label: value })),
        );
        await select.get(`button[data-value="${mode}"]`).trigger("click");
        await flushPromises();
        expect(mocks.saveSettings).not.toHaveBeenCalled();
        await wrapper
          .findAll("button")
          .find((button) => button.text() === "Apply")!
          .trigger("click");
        await flushPromises();
        const saved = cloneSettings(DEFAULT_SETTINGS);
        saved.terminal.followWorktree = mode;
        expect(mocks.saveSettings).toHaveBeenCalledWith(saved);
        expect(wrapper.findComponent({ name: "MainPane" }).props("terminalSettings").followWorktree).toBe(mode);
        wrapper.unmount();
        const reopened = await openSettings(saved);
        expect(reopened.get(`#${controlId}`).text()).toContain(mode);
        reopened.unmount();
      },
    );

    it("turns copy-on-selection off through Apply, and the terminal is told at once", async () => {
      // On by default, so the dialog opens showing the behaviour this build has always had, and
      // turning it off is a preference like any other: written whole, adopted at once.
      const wrapper = await openSettings();
      const toggle = wrapper.get("#settings-terminal-selectionCopy");
      expect((toggle.element as HTMLInputElement).checked).toBe(true);

      await toggle.setValue(false);
      expect(mocks.saveSettings).not.toHaveBeenCalled();
      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Apply")!
        .trigger("click");
      await flushPromises();

      const saved = cloneSettings(DEFAULT_SETTINGS);
      saved.terminal.selectionCopy = false;
      expect(mocks.saveSettings).toHaveBeenCalledWith(saved);
      // Adopted without a restart and without waiting for the file to come back, which is what
      // makes it reach the terminals that are already open.
      expect(wrapper.findComponent({ name: "MainPane" }).props("terminalSettings")).toMatchObject({
        selectionCopy: false,
      });
      wrapper.unmount();

      const reopened = await openSettings(saved);
      expect((reopened.get("#settings-terminal-selectionCopy").element as HTMLInputElement).checked).toBe(false);
      reopened.unmount();
    });

    it("writes the whole set and takes effect at once, without waiting for the file", async () => {
      const wrapper = await openSettings();
      const fontSize = wrapper.get("#settings-ui-fontSize");
      await fontSize.setValue("18");
      await fontSize.trigger("change");

      // The draft is the dialog's own until Apply, so nothing on screen has moved yet.
      expect(document.documentElement.style.getPropertyValue("--muster-ui-font-scale")).toBe("1");

      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Apply")!
        .trigger("click");
      await flushPromises();
      // Applied, the whole type scale on the document moves: the window is redrawn at 18/14 of
      // the size it was drawn at, with no reload and without waiting for the file to be written.
      expect(document.documentElement.style.getPropertyValue("--muster-ui-font-scale")).toBe(String(18 / 14));
      expect(mocks.saveSettings).toHaveBeenCalledWith({
        ...DEFAULT_SETTINGS,
        ui: { ...DEFAULT_SETTINGS.ui, fontSize: 18 },
      });
      expect(wrapper.find('[role="dialog"]').exists()).toBe(false);
      wrapper.unmount();
    });

    it("applies the selected content background in dark mode and keeps the light surface unchanged", async () => {
      const wrapper = await openSettings({
        ...DEFAULT_SETTINGS,
        ui: { ...DEFAULT_SETTINGS.ui, theme: "dark" },
      });
      const color = wrapper.get("#settings-ui-contentBackground");
      await color.setValue("#334455");
      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Apply")!
        .trigger("click");
      await flushPromises();

      expect(document.documentElement.style.getPropertyValue("--muster-content-bg-0")).toBe("#334455");
      expect(mocks.saveSettings).toHaveBeenCalledWith({
        ...DEFAULT_SETTINGS,
        ui: { ...DEFAULT_SETTINGS.ui, theme: "dark", contentBackground: "#334455" },
      });
      wrapper.unmount();

      const light = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        {
          settings: {
            ...cloneSettings(DEFAULT_SETTINGS),
            ui: { ...DEFAULT_SETTINGS.ui, theme: "light", contentBackground: "#334455" },
          },
        },
      );
      expect(document.documentElement.style.getPropertyValue("--muster-content-bg-0")).toBe("");
      light.unmount();
    });

    it("offers the swatch's own way back, and only once it has been moved", async () => {
      // A color control has no empty state, so this is the one preference a person cannot put back
      // by using the control again. The button appears on a changed value and nowhere else.
      const wrapper = await openSettings({
        ...DEFAULT_SETTINGS,
        ui: { ...DEFAULT_SETTINGS.ui, theme: "dark", contentBackground: "#334455" },
      });
      const swatch = wrapper.get("#settings-ui-contentBackground");
      const reset = () => wrapper.findAll("button").find((button) => button.text() === "Reset");
      expect((swatch.element as HTMLInputElement).value).toBe("#334455");
      expect(reset(), "a saved color that is not the default").toBeTruthy();

      // Putting it back is a draft edit like any other: nothing on screen has moved until Apply.
      await reset()!.trigger("click");
      expect((wrapper.get("#settings-ui-contentBackground").element as HTMLInputElement).value).toBe(
        DEFAULT_SETTINGS.ui.contentBackground,
      );
      expect(reset(), "a value that is already the default").toBeFalsy();

      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Apply")!
        .trigger("click");
      await flushPromises();
      expect(mocks.saveSettings).toHaveBeenCalledWith({
        ...DEFAULT_SETTINGS,
        ui: { ...DEFAULT_SETTINGS.ui, theme: "dark" },
      });
      expect(document.documentElement.style.getPropertyValue("--muster-content-bg-0")).toBe(
        DEFAULT_SETTINGS.ui.contentBackground,
      );
      wrapper.unmount();

      // And a window that has never moved the swatch is not offered a control that would do nothing.
      const untouched = await openSettings();
      expect(untouched.findAll("button").some((button) => button.text() === "Reset")).toBe(false);
      untouched.unmount();
    });

    it("says nothing has been written when Cancel throws the draft away", async () => {
      const wrapper = await openSettings();
      const fontSize = wrapper.get("#settings-terminal-fontSize");
      await fontSize.setValue("20");
      await fontSize.trigger("change");
      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Cancel")!
        .trigger("click");
      await wrapper.get('[data-testid="settings-button"]').trigger("click");

      // Reopening starts from what is saved, not from what was typed and abandoned.
      expect((wrapper.get("#settings-terminal-fontSize").element as HTMLInputElement).value).toBe("16");
      expect(mocks.saveSettings).not.toHaveBeenCalled();
      wrapper.unmount();
    });

    it("rejects fractional indentation sizes instead of sending invalid settings", async () => {
      const wrapper = await openSettings();
      const size = wrapper.get("#settings-editor-indentation-size");
      await size.setValue("2.5");
      await size.trigger("change");

      expect((size.element as HTMLInputElement).value).toBe("2");
      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Apply")!
        .trigger("click");
      await flushPromises();
      expect(mocks.saveSettings).toHaveBeenCalledWith(DEFAULT_SETTINGS);
      wrapper.unmount();
    });

    it("stays open and says so when the write fails", async () => {
      mocks.saveSettings.mockRejectedValue(new Error("could not write the settings file"));
      const wrapper = await openSettings();
      const fontSize = wrapper.get("#settings-editor-fontSize");
      await fontSize.setValue("15");
      await fontSize.trigger("change");

      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Apply")!
        .trigger("click");
      await flushPromises();

      // Closing on a failed write would leave the window showing a setting the app is not using
      // and the file not carrying, with nothing on screen to say so.
      expect(wrapper.find('[role="dialog"]').exists()).toBe(true);
      const { toasts } = useToasts();
      expect(toasts.value.at(-1)?.message).toContain("could not write the settings file");

      // And the window is back where the file says it should be, so reopening shows the saved
      // value rather than the one the file refused.
      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Cancel")!
        .trigger("click");
      await wrapper.get('[data-testid="settings-button"]').trigger("click");
      expect((wrapper.get("#settings-editor-fontSize").element as HTMLInputElement).value).toBe("13");
      wrapper.unmount();
    });

    it("puts the window back to the last saved set when a later write is refused", async () => {
      const wrapper = await mountApp(
        workspaceWith(checkout("checkout:one")),
        { ...DEFAULT_APP_LAYOUT },
        {
          settings: {
            ...cloneSettings(DEFAULT_SETTINGS),
            ui: { ...DEFAULT_SETTINGS.ui, fontSize: 16 },
          },
        },
      );
      mocks.saveSettings.mockRejectedValueOnce(new Error("the settings file could not be read"));

      window.dispatchEvent(new KeyboardEvent("keydown", { key: "+", metaKey: true, cancelable: true }));
      await flushPromises();

      // The scale is adopted first so a held key walks the steps, and taken back when the write
      // does not land: what is drawn and what the file carries are the same thing again.
      expect(document.documentElement.style.getPropertyValue("zoom")).toBe("1");
      expect(wrapper.findComponent({ name: "MainPane" }).props("zoom")).toBe(1);
      const { toasts } = useToasts();
      expect(toasts.value.at(-1)?.message).toContain("could not be read");
      wrapper.unmount();
    });

    it("puts the draft back to the defaults and applies that", async () => {
      const wrapper = await openSettings();
      const toggle = wrapper.get("#settings-terminal-ligatures");
      await toggle.setValue(false);
      expect((toggle.element as HTMLInputElement).checked).toBe(false);

      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Reset to defaults")!
        .trigger("click");
      expect((wrapper.get("#settings-terminal-ligatures").element as HTMLInputElement).checked).toBe(true);
      // Reset says where the draft would go; it is the same Apply that writes it.
      await wrapper
        .findAll("button")
        .find((button) => button.text() === "Apply")!
        .trigger("click");
      await flushPromises();
      expect(mocks.saveSettings).toHaveBeenCalledWith(DEFAULT_SETTINGS);
      wrapper.unmount();
    });

    it("hands the terminal and the editor the settings that were saved", async () => {
      const wrapper = await openSettings({
        ...DEFAULT_SETTINGS,
        terminal: { ...DEFAULT_SETTINGS.terminal, fontSize: 20, ligatures: false, scrollbar: "always" },
        editor: {
          ...DEFAULT_SETTINGS.editor,
          fontSize: 15,
          indentation: { useSpaces: false, size: 4 },
        },
      });

      const main = wrapper.findComponent({ name: "MainPane" });
      expect(main.props("terminalSettings")).toMatchObject({ fontSize: 20, ligatures: false, scrollbar: "always" });
      expect(main.props("editorSettings")).toMatchObject({ fontSize: 15 });
      wrapper.unmount();
    });
  });
});
