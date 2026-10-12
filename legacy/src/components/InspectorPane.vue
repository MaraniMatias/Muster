<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { ChevronRight as ChevronRightIcon, SquareArrowOutUpRight as SquareArrowOutUpRightIcon } from "@lucide/vue";
import { isIpcError } from "../domain/ipc";
import { absoluteFilePath, type FileEntry } from "../domain/files";
import { displayCheckoutPath, type Checkout, type Repo } from "../domain/workspace";
import type { CheckoutUiState } from "../domain/ui-state";
import type { ActiveGitSnapshot } from "../presentation/active-git-snapshot";
import { useDiffStats } from "../presentation/diff-stats";
import { listCheckoutFiles } from "../lib/ipc";
import FileIcon from "./FileIcon.vue";
import OverlayScrollbar from "./OverlayScrollbar.vue";

const props = withDefaults(
  defineProps<{
    checkout: Checkout | null;
    homePath?: string;
    repo?: Repo | null;
    gitSnapshot: ActiveGitSnapshot;
    savedState?: CheckoutUiState | null;
    /** `ui.fontSize` as a ratio of the 14px the panel is drawn at. Defaults to 1, the design size. */
    fontScale?: number;
    treeStickyScroll?: boolean;
  }>(),
  { treeStickyScroll: true },
);

const emit = defineEmits<{
  openFile: [value: { checkoutId: string; path: string }];
  /** Hands the entry to the machine's own application, which is the only place some of them open. */
  openExternalFile: [value: { checkoutId: string; path: string }];
  openChange: [value: { checkoutId: string; path: string }];
  /** Every changed file in one diff, which is the one view with no path. */
  openAllChanges: [value: { checkoutId: string }];
  updateUiState: [
    value: Pick<
      CheckoutUiState,
      | "inspectorTab"
      | "selectedFilePath"
      | "selectedChangePath"
      | "expandedDirectories"
      | "filesScrollTop"
      | "changesScrollTop"
    >,
  ];
}>();

type InspectorTab = "files" | "changes";
type DirectoryState = "loading" | "error" | "empty" | "truncated";
/** The file tree rows carry the same optional counts as the change rows, but nothing fills
 *  them in yet: the tree lists the whole checkout, not its diff, and the counts belong to the
 *  diff view that the file rows are about to become. */
interface DiffStats {
  additions?: number;
  deletions?: number;
}
interface VisibleEntry extends DiffStats {
  entry?: FileEntry;
  depth: number;
  message?: string;
}
/** A change row is either a group header or one of its files. Both are one row tall, so the
 *  grouped list virtualizes with the same math as the tree. */
type ChangeFile = { kind: "file"; key: string; name: string; status: string; oldPath?: string } & DiffStats;
type ChangeRow = ChangeFile | { kind: "group"; key: string; dir: string };

// The counts come from the same store the sidebar reads and from the same base ref the file
// list below is built from, so a row's number always describes the change it sits next to.
const diffStats = useDiffStats(
  computed(() => (props.repo ? [props.repo] : [])),
  computed(() => (props.repo?.kind === "git" ? (props.checkout?.id ?? null) : null)),
);

const directories = ref<Record<string, FileEntry[]>>({});
const directoryStates = ref<Record<string, DirectoryState>>({});
const expanded = ref<string[]>([]);
const rootState = ref<"idle" | "loading" | "ready" | "error" | "missing">("idle");
/** The last reason the tree's root could not be listed. It has no row of its own, so the
 *  panel itself is where the reason stays: a toast expires and leaves the tree unexplained. */
const rootError = ref("");
/** The last reason a folder could not be listed, which its own row in the tree shows (E.2). */
const folderError = ref("");
const selectedPaths = ref<Record<string, string | null>>({});
const selectedChangedPaths = ref<Record<string, string | null>>({});
const selectedFolderPath = ref<string | null>(null);
const selectedPath = computed(() => (props.checkout ? (selectedPaths.value[props.checkout.id] ?? null) : null));
const selectedChangedPath = computed(() =>
  props.checkout ? (selectedChangedPaths.value[props.checkout.id] ?? null) : null,
);
const activeTab = ref<InspectorTab>("files");
const treeScrollTop = ref(0);
const changesScrollTop = ref(0);
const treeViewport = ref<HTMLElement | null>(null);
const changesViewport = ref<HTMLElement | null>(null);
let generation = 0;
let refreshMicrotaskQueued = false;
let refreshInFlight = false;
let pendingRefreshCheckoutId: string | null = null;

/**
 * Every row of both lists is this tall, and `--tree-row-height` below is set from it, so the
 * `.file-row` box is `3px + (26 - 6px) line box + 3px` = 26px exactly. Change one without the
 * other and the virtual window drifts from the rendered rows.
 *
 * 26px is the sidebar's own row height (`.workdir-row`), and it is the height at the 14px the
 * design is drawn at. Both lists read as a column of one-line rows beside the sidebar rather than
 * as a list of its own, and a row height is what says that before any color does. The cost is
 * rows: the inspector is about 1400px of scroll at the default window, so 26px is ~54 of them
 * against 22px's ~64.
 *
 * It scales with `ui.fontSize` because the row's own `font-size` does: a row left at 26px held a
 * 17px label in a 20px line box at the top of the range and clipped it. The scale is a prop
 * rather than read back off the root so the number the window math multiplies is the number the
 * box is drawn at.
 */
const TREE_ROW_HEIGHT = 26;
const TREE_WINDOW_SIZE = 64;
const TREE_OVERSCAN = 10;

/** The status vocabulary the row decorations know (E.5): no `??`, no per-folder bubble. */
const ROW_STATUSES = new Set(["M", "A", "D", "U"]);

const showChanges = computed(() => props.repo?.kind === "git" && !!props.checkout && !props.checkout.isMissing);
const tabs = computed((): { id: InspectorTab; label: string }[] => {
  if (!props.checkout) return [];
  const result: { id: InspectorTab; label: string }[] = [{ id: "files", label: "Files" }];
  if (showChanges.value) result.push({ id: "changes", label: "Changes" });
  return result;
});

function errorText(error: unknown): string {
  if (isIpcError(error)) {
    switch (error.code) {
      case "folder_missing":
        return "File or folder no longer exists.";
      case "permission_denied":
        return "Permission denied while reading this folder or file.";
      case "file_too_large":
        return "This file is larger than the preview size limit.";
      case "binary_file":
        return "This file is binary or is not valid UTF-8.";
      case "path_outside_checkout":
        return "This path resolves outside the active checkout and cannot be opened.";
      case "invalid_path":
        return "This path is not a valid file or folder in the checkout.";
      default:
        return error.message;
    }
  }
  return error instanceof Error ? error.message : String(error);
}

async function loadDirectory(checkoutId: string, path: string, requestGeneration: number) {
  const hasCachedEntries = Object.hasOwn(directories.value, path);
  if (!hasCachedEntries) {
    directoryStates.value = { ...directoryStates.value, [path]: "loading" };
    if (path === ".") rootState.value = "loading";
  } else {
    const currentState = directoryStates.value[path];
    if (currentState === "loading" || currentState === "error") {
      const nextStates = { ...directoryStates.value };
      delete nextStates[path];
      if (directories.value[path].length === 0) nextStates[path] = "empty";
      directoryStates.value = nextStates;
    }
    if (path === "." && rootState.value === "error") rootState.value = "ready";
  }
  try {
    const result = await listCheckoutFiles(checkoutId, path);
    if (requestGeneration !== generation) return;
    directories.value = { ...directories.value, [path]: result.entries };
    const nextStates = { ...directoryStates.value };
    delete nextStates[path];
    if (result.truncated) nextStates[path] = "truncated";
    else if (result.entries.length === 0) nextStates[path] = "empty";
    directoryStates.value = nextStates;
    if (path === ".") rootState.value = "ready";
  } catch (error) {
    if (requestGeneration !== generation || hasCachedEntries) return;
    const message = errorText(error);
    if (path === ".") {
      // The root has no row of its own to hold the reason, so the panel says it: a toast
      // expires and would leave the tree empty with nothing to explain it.
      if (isIpcError(error) && error.code === "folder_missing") {
        rootState.value = "missing";
        rootError.value = "";
      } else {
        rootState.value = "error";
        rootError.value = message;
      }
    } else {
      // A folder keeps it on its own row, where it lasts as long as the row does.
      directoryStates.value = { ...directoryStates.value, [path]: "error" };
      folderError.value = message;
    }
  }
}

watch(
  () => [props.checkout?.id, props.checkout?.isMissing] as const,
  async ([checkoutId, isMissing]) => {
    const requestGeneration = ++generation;
    directories.value = {};
    directoryStates.value = {};
    expanded.value = [];
    selectedFolderPath.value = null;
    const saved = props.savedState;
    treeScrollTop.value = saved?.filesScrollTop ?? 0;
    changesScrollTop.value = saved?.changesScrollTop ?? 0;
    expanded.value = saved?.expandedDirectories ?? [];
    activeTab.value = saved?.inspectorTab === "changes" && showChanges.value ? "changes" : "files";
    if (checkoutId && saved?.selectedFilePath)
      selectedPaths.value = { ...selectedPaths.value, [checkoutId]: saved.selectedFilePath };
    if (checkoutId && saved?.selectedChangePath)
      selectedChangedPaths.value = { ...selectedChangedPaths.value, [checkoutId]: saved.selectedChangePath };
    folderError.value = "";
    rootError.value = "";
    if (!checkoutId) {
      rootState.value = "idle";
      return;
    }
    if (isMissing) {
      rootState.value = "missing";
      return;
    }
    rootState.value = "loading";
    await loadDirectory(checkoutId, ".", requestGeneration);
    const restoredDirectories = [...expanded.value];
    for (let index = 0; index < restoredDirectories.length; index += 16) {
      if (requestGeneration !== generation) return;
      await Promise.all(
        restoredDirectories.slice(index, index + 16).map((path) => loadDirectory(checkoutId, path, requestGeneration)),
      );
    }
    await nextTick();
    if (requestGeneration === generation && treeViewport.value) treeViewport.value.scrollTop = treeScrollTop.value;
  },
  { immediate: true },
);

watch([activeTab, selectedPath, selectedChangedPath, expanded, treeScrollTop, changesScrollTop], () => {
  if (!props.checkout) return;
  emit("updateUiState", {
    inspectorTab: activeTab.value,
    selectedFilePath: selectedPath.value,
    selectedChangePath: selectedChangedPath.value,
    expandedDirectories: expanded.value,
    filesScrollTop: Math.round(treeScrollTop.value),
    changesScrollTop: Math.round(changesScrollTop.value),
  });
});

function onInspectorTabKeydown(event: KeyboardEvent) {
  if (!(event.target instanceof HTMLElement) || event.target.getAttribute("role") !== "tab") return;
  if (event.key !== "Home" && event.key !== "End") return;
  const ids = tabs.value.map((tab) => tab.id);
  const current: InspectorTab = event.target.id === "inspector-tab-changes" ? "changes" : "files";
  const next = event.key === "Home" ? ids[0] : ids[ids.length - 1];
  if (current === next) return;
  event.preventDefault();
  activeTab.value = next;
  void nextTick(() => document.getElementById(`inspector-tab-${next}`)?.focus());
}

watch(
  () => [props.gitSnapshot.statusEventRevision, props.gitSnapshot.statusEventCheckoutId] as const,
  ([, eventCheckoutId]) => {
    const checkoutId = props.checkout?.id;
    if (
      checkoutId &&
      eventCheckoutId === checkoutId &&
      checkoutId === props.gitSnapshot.checkoutId &&
      !props.checkout?.isMissing &&
      props.repo?.kind === "git" &&
      activeTab.value !== "changes"
    ) {
      requestRefresh(checkoutId);
    }
  },
);

watch(activeTab, (tab) => {
  const checkoutId = props.checkout?.id;
  if (tab === "files" && checkoutId && props.repo?.kind === "git") {
    requestRefresh(checkoutId);
  }
});

watch(
  () => props.repo?.kind,
  (kind) => {
    if (kind !== "git") activeTab.value = "files";
  },
);

function requestRefresh(checkoutId: string) {
  pendingRefreshCheckoutId = checkoutId;
  if (refreshInFlight || refreshMicrotaskQueued) return;
  refreshMicrotaskQueued = true;
  queueMicrotask(() => {
    refreshMicrotaskQueued = false;
    const requestedCheckoutId = pendingRefreshCheckoutId;
    pendingRefreshCheckoutId = null;
    if (!requestedCheckoutId) return;
    refreshInFlight = true;
    void refreshAfterGitChange(requestedCheckoutId).finally(() => {
      refreshInFlight = false;
      if (pendingRefreshCheckoutId) requestRefresh(pendingRefreshCheckoutId);
    });
  });
}

async function refreshAfterGitChange(checkoutId: string) {
  const requestGeneration = generation;
  const paths = [
    ".",
    ...Object.keys(directories.value).filter((path) => path !== "." && expanded.value.includes(path)),
  ];
  for (const path of paths) {
    if (requestGeneration !== generation || props.checkout?.id !== checkoutId) return;
    await loadDirectory(checkoutId, path, requestGeneration);
  }
}

async function toggleDirectory(entry: FileEntry) {
  if (expanded.value.includes(entry.path)) {
    expanded.value = expanded.value.filter((path) => path !== entry.path);
    await nextTick();
    const row = [...(treeViewport.value?.querySelectorAll<HTMLButtonElement>("[data-folder-path]") ?? [])].find(
      (button) => button.dataset.folderPath === entry.path && !button.closest(".sticky-folders"),
    );
    row?.focus();
    return;
  }
  expanded.value = [...expanded.value, entry.path];
  const checkoutId = props.checkout?.id;
  if (checkoutId && !Object.hasOwn(directories.value, entry.path)) {
    await loadDirectory(checkoutId, entry.path, generation);
  }
}

async function revealFolder(entry: FileEntry) {
  selectedFolderPath.value = entry.path;
  const index = visibleEntries.value.findIndex((row) => row.entry?.path === entry.path);
  if (index < 0 || !treeViewport.value) return;
  const scrollTop = Math.max(0, index - visibleEntries.value[index]!.depth) * rowHeight.value;
  treeScrollTop.value = scrollTop;
  treeViewport.value.scrollTop = scrollTop;
  await nextTick();
  const row = [...treeViewport.value.querySelectorAll<HTMLButtonElement>("[data-folder-path]")].find(
    (button) => button.dataset.folderPath === entry.path && !button.closest(".sticky-folders"),
  );
  row?.focus();
}

function selectFile(entry: FileEntry) {
  const checkoutId = props.checkout?.id;
  if (!checkoutId) return;
  selectedPaths.value = { ...selectedPaths.value, [checkoutId]: entry.path };
  emit("openFile", { checkoutId, path: entry.path });
}

/**
 * A row of the tree, where ctrl+click is not the plain click with a key held down.
 *
 * Held, it hands the entry to whatever application this machine has for it, which is the only place
 * a `.psd` or an `.ipynb` opens at all. Not held, it is the row's own action: a file selects, a
 * folder expands. The two never both happen, because a row that opened somewhere else and also
 * changed the view here would be answering a gesture nobody asked.
 *
 * Meta is accepted beside ctrl because this app runs on both, and on macOS the muscle memory for
 * "open it in its own window" is cmd rather than ctrl. The chevron and the sticky rows keep their
 * own clicks: a chevron is a 12px target that is never the thing being pointed at, and a sticky row
 * is a copy of a row above, not a row of its own.
 */
function onEntryRowClick(entry: FileEntry, event: MouseEvent) {
  const checkoutId = props.checkout?.id;
  if (!checkoutId) return;
  if (event.ctrlKey || event.metaKey) {
    emit("openExternalFile", { checkoutId, path: entry.path });
    return;
  }
  if (entry.kind === "directory") toggleDirectory(entry);
  else selectFile(entry);
}

function selectChange(path: string) {
  const checkoutId = props.checkout?.id;
  if (!checkoutId) return;
  selectedChangedPaths.value = { ...selectedChangedPaths.value, [checkoutId]: path };
  emit("openChange", { checkoutId, path });
}

function openAllChanges() {
  const checkoutId = props.checkout?.id;
  if (checkoutId) emit("openAllChanges", { checkoutId });
}

/** One open folder on the way to a row: the sticky stack a row under it is drawn from. */
type Ancestor = { entry: FileEntry; depth: number };
type AncestorStack = { ancestor: Ancestor; parent: AncestorStack | null } | null;

/** A `Set` because the question is asked once per row of the tree, and an array answers it in the
 *  length of the tree. */
const expandedSet = computed(() => new Set(expanded.value));

/** Flatten expanded rows and index each row's open ancestors for O(depth) sticky queries. */
const tree = computed(() => {
  const rows: VisibleEntry[] = [];
  const stacks: AncestorStack[] = [];
  const push = (item: VisibleEntry, open: AncestorStack) => {
    rows.push(item);
    stacks.push(open);
  };
  function append(path: string, depth: number, ancestors: AncestorStack) {
    for (const entry of directories.value[path] ?? []) {
      const open =
        entry.kind === "directory" && expandedSet.value.has(entry.path)
          ? { ancestor: { entry, depth }, parent: ancestors }
          : null;
      push({ entry, depth }, open ?? ancestors);
      if (!open) continue;
      const state = directoryStates.value[entry.path];
      if (state === "loading") push({ depth: depth + 1, message: "Loading folder…" }, open);
      else if (state === "error")
        push({ depth: depth + 1, message: folderError.value || "Could not load folder." }, open);
      // An empty folder says nothing: it stays, and a line of prose under every one of them
      // is noise on a real tree.
      else {
        if (state === "truncated") push({ depth: depth + 1, message: "Some entries omitted (folder is large)." }, open);
        append(entry.path, depth + 1, open);
      }
    }
  }
  append(".", 0, null);
  return { rows, stacks };
});

const visibleEntries = computed<VisibleEntry[]>(() => tree.value.rows);

const changedFiles = computed(() => props.gitSnapshot.status?.files ?? []);
const changedFileCount = computed(() => changedFiles.value.length);
/** Grouped by parent directory, then flattened: a group header is a row of the same height, so
 *  the whole list windows with `virtualWindow`. Files at the root are labelled "/". */
const changeRows = computed<ChangeRow[]>(() => {
  const counts = diffStats.fileCounts;
  const groups = new Map<string, ChangeFile[]>();
  for (const file of changedFiles.value) {
    const slash = file.path.lastIndexOf("/");
    const dir = slash < 0 ? "" : file.path.slice(0, slash);
    const row: ChangeFile = {
      kind: "file",
      key: file.path,
      name: slash < 0 ? file.path : file.path.slice(slash + 1),
      status: file.status,
      oldPath: file.oldPath,
      // A file Git cannot count keeps both absent, so the row shows no number at all.
      additions: counts[file.path]?.additions,
      deletions: counts[file.path]?.deletions,
    };
    const bucket = groups.get(dir);
    if (bucket) bucket.push(row);
    else groups.set(dir, [row]);
  }
  const rows: ChangeRow[] = [];
  for (const [dir, files] of groups) rows.push({ kind: "group", key: `group:${dir}`, dir }, ...files);
  return rows;
});

const gitStatuses = computed(() => {
  const statuses = new Map<string, string>();
  for (const file of props.gitSnapshot.status?.files ?? []) {
    if (ROW_STATUSES.has(file.status)) statuses.set(file.path, file.status);
  }
  return statuses;
});

/**
 * How loud a row is drawn. A dotfile steps down once and a file the checkout ignores steps down
 * again, so a build directory reads as present but uninteresting. The flag comes from the
 * backend; the dot comes from the name, since nothing in Git treats it specially.
 */
function prominenceOf(entry: FileEntry): "normal" | "hidden" | "ignored" {
  if (entry.ignored) return "ignored";
  return entry.name.startsWith(".") ? "hidden" : "normal";
}

function rowStatus(path: string): string | undefined {
  return gitStatuses.value.get(path);
}

function entryTooltip(path: string): string {
  if (!props.checkout) return path;
  return displayCheckoutPath(absoluteFilePath(props.checkout.canonicalPath, path), props.homePath);
}

/** The gutter, and one 14px step per level: the sidebar's own indent, at its own 8px base. */
function indentOf(depth: number): number {
  return 8 + depth * 14;
}

function rowIndent(depth: number): string {
  return `${indentOf(depth)}px`;
}

/**
 * What a row is worth across, from `.tree-width-row`'s own numbers: the padding it is drawn with,
 * the gutter that stands in for the chevron and the icon, and the gap that separates them.
 */
const ROW_PADDING = 8;
const ROW_GAP = 7;
const SIZER_GUTTER = 37;

const rowTextProbe = ref<HTMLElement | null>(null);
const statusTextProbe = ref<HTMLElement | null>(null);
const measuredTexts = new Map<string, number>();
const measuredFonts = new Map<"row" | "status", { font: string; letterSpacing: number }>();
const fontMetricsRevision = ref(0);
const textCanvas = document.createElement("canvas");
const textContext = textCanvas.getContext("2d");

function textWidth(style: "row" | "status", text: string): number {
  const probe = style === "row" ? rowTextProbe.value : statusTextProbe.value;
  if (!probe || !text) return 0;
  const key = `${props.fontScale} ${style} ${text}`;
  const known = measuredTexts.get(key);
  if (known !== undefined) return known;

  let width = 0;
  if (textContext) {
    let metrics = measuredFonts.get(style);
    if (!metrics) {
      const computed = window.getComputedStyle(probe);
      metrics = { font: computed.font, letterSpacing: Number.parseFloat(computed.letterSpacing) || 0 };
      measuredFonts.set(style, metrics);
    }
    textContext.font = metrics.font;
    width = Math.ceil(textContext.measureText(text).width + metrics.letterSpacing * Math.max(0, [...text].length - 1));
  } else {
    probe.textContent = text;
    width = probe.scrollWidth;
  }
  if (width > 0) measuredTexts.set(key, width);
  return width;
}

function sizerWidth(item: VisibleEntry): number {
  const entry = item.entry;
  if (!entry) return indentOf(item.depth) + ROW_PADDING + textWidth("row", item.message ?? "");
  let width = indentOf(item.depth) + ROW_PADDING + SIZER_GUTTER + ROW_GAP + textWidth("row", entry.name);
  const status = rowStatus(entry.path);
  if (status) width += ROW_GAP + textWidth("status", status);
  if (item.additions) width += ROW_GAP + textWidth("row", `+${item.additions}`);
  if (item.deletions) width += ROW_GAP + textWidth("row", `-${item.deletions}`);
  return width;
}

/** Whether the tree is on screen, because a row that is not laid out has no width to read. */
const treeVisible = ref(true);

/** Preserve full-tree horizontal width while keeping the sizing DOM constant-sized. */
const treeWidth = computed(() => {
  void fontMetricsRevision.value;
  if (!treeVisible.value) return { px: 0, rows: [] as VisibleEntry[] };
  let px = 0;
  let widest: VisibleEntry | undefined;
  for (const item of visibleEntries.value) {
    const width = sizerWidth(item);
    if (width > px) {
      px = width;
      widest = item;
    }
  }
  return { px, rows: widest ? [widest] : [] };
});

/** Both lists are flat and every row is rowHeight tall, so one window serves both. */
function virtualWindow<T>(rows: T[], scrollTop: number, rowHeight: number) {
  const maximumStart = Math.max(0, rows.length - TREE_WINDOW_SIZE);
  const start = Math.min(maximumStart, Math.max(0, Math.floor(scrollTop / rowHeight) - TREE_OVERSCAN));
  const end = Math.min(rows.length, start + TREE_WINDOW_SIZE);
  return {
    rows: rows.slice(start, end),
    paddingTop: start * rowHeight,
    paddingBottom: (rows.length - end) * rowHeight,
  };
}

/** The row height this font size draws at. See TREE_ROW_HEIGHT. */
const rowHeight = computed(() => Math.round(TREE_ROW_HEIGHT * (props.fontScale ?? 1)));

const stickyFolders = computed<Ancestor[]>(() => {
  if (!props.treeStickyScroll) return [];
  const viewport = treeViewport.value;
  const rowHeightPx = rowHeight.value;
  // Account for the rows hidden beneath the sticky stack when anticipating ancestors.
  const maxStack = Math.max(0, Math.floor(((viewport?.clientHeight ?? rowHeightPx) - 8) / rowHeightPx));
  const { rows, stacks } = tree.value;
  const bannerOffset = directoryStates.value["."] === "truncated" ? rowHeightPx : 0;
  let top = Math.floor(Math.max(0, treeScrollTop.value - bannerOffset) / rowHeightPx);
  let ancestors: Ancestor[] = [];
  for (let pass = 0; pass <= maxStack; pass++) {
    let stack = stacks[Math.min(top, rows.length) - 1] ?? null;
    ancestors = [];
    while (stack) {
      ancestors.push(stack.ancestor);
      stack = stack.parent;
    }
    ancestors.reverse();
    const nextTop = Math.floor(
      Math.max(0, treeScrollTop.value + (Math.min(maxStack, ancestors.length) + 1) * rowHeightPx - bannerOffset) /
        rowHeightPx,
    );
    if (nextTop <= top) break;
    top = nextTop;
  }
  return ancestors.slice(-maxStack);
});
const treeWindow = computed(() => {
  const window = virtualWindow(visibleEntries.value, treeScrollTop.value, rowHeight.value);
  return window;
});
const changeWindow = computed(() => virtualWindow(changeRows.value, changesScrollTop.value, rowHeight.value));

function onTreeScroll(event: Event) {
  treeScrollTop.value = (event.currentTarget as HTMLElement).scrollTop;
}

function onChangesScroll(event: Event) {
  changesScrollTop.value = (event.currentTarget as HTMLElement).scrollTop;
}

let scrollRestoreObserver: ResizeObserver | undefined;
const observedViewports = new Set<HTMLElement>();
const wasVisible = new WeakMap<HTMLElement, boolean>();
function observeScrollViewports() {
  if (!scrollRestoreObserver) return;
  const viewports = [treeViewport.value, changesViewport.value].filter(
    (viewport): viewport is HTMLElement => !!viewport,
  );
  for (const viewport of observedViewports) {
    if (viewports.includes(viewport)) continue;
    scrollRestoreObserver.unobserve(viewport);
    observedViewports.delete(viewport);
  }
  for (const viewport of viewports) {
    if (observedViewports.has(viewport)) continue;
    observedViewports.add(viewport);
    wasVisible.set(viewport, viewport.clientHeight > 0);
    scrollRestoreObserver.observe(viewport);
  }
}
watch([treeViewport, changesViewport], observeScrollViewports, { flush: "post" });
onMounted(() => {
  scrollRestoreObserver = new ResizeObserver((entries) => {
    for (const entry of entries) {
      const viewport = entry.target as HTMLElement;
      const isTree = viewport === treeViewport.value;
      if (!isTree && viewport !== changesViewport.value) continue;
      const visible = entry.contentRect.height > 0;
      const hadBeenVisible = wasVisible.get(viewport) ?? false;
      wasVisible.set(viewport, visible);
      // The tree's own width is measured from rows that only have boxes while they are on screen,
      // so this is what says the next time they do.
      if (isTree) treeVisible.value = visible;
      if (!visible || hadBeenVisible) continue;

      viewport.scrollTop = isTree ? treeScrollTop.value : changesScrollTop.value;
      if (isTree) treeScrollTop.value = viewport.scrollTop;
      else changesScrollTop.value = viewport.scrollTop;
    }
  });
  observeScrollViewports();
});
onUnmounted(() => scrollRestoreObserver?.disconnect());
watch(visibleEntries, () => measuredTexts.clear(), { flush: "sync" });
watch(
  () => props.fontScale,
  () => {
    measuredTexts.clear();
    measuredFonts.clear();
  },
);

function onFontsLoaded() {
  measuredTexts.clear();
  measuredFonts.clear();
  fontMetricsRevision.value += 1;
}

onMounted(() => {
  document.fonts?.addEventListener("loadingdone", onFontsLoaded);
  void document.fonts?.ready.then(onFontsLoaded);
});
onUnmounted(() => document.fonts?.removeEventListener("loadingdone", onFontsLoaded));

watch(
  () => [props.checkout?.id, props.gitSnapshot.statusState] as const,
  async ([, state]) => {
    if (state !== "ready") return;
    const savedTop = changesScrollTop.value;
    await nextTick();
    if (changesViewport.value) changesViewport.value.scrollTop = savedTop;
  },
  { immediate: true, flush: "post" },
);
</script>

<template>
  <aside class="app-inspector flex h-full w-full min-w-0 flex-col" :style="{ '--tree-row-height': `${rowHeight}px` }">
    <div class="details-tabs" role="tablist" aria-label="Inspector sections" @keydown="onInspectorTabKeydown">
      <button
        v-for="tab in tabs"
        :id="`inspector-tab-${tab.id}`"
        :key="tab.id"
        type="button"
        role="tab"
        class="details-tab"
        :aria-selected="activeTab === tab.id"
        :aria-controls="`inspector-panel-${tab.id}`"
        :tabindex="activeTab === tab.id ? 0 : -1"
        @click="activeTab = tab.id"
      >
        {{ tab.label }}
        <span v-if="tab.id === 'changes' && changedFileCount > 0" class="details-tab-count">
          {{ changedFileCount }}
        </span>
      </button>
    </div>

    <div
      v-show="activeTab === 'files'"
      :id="checkout ? 'inspector-panel-files' : undefined"
      :role="checkout ? 'tabpanel' : undefined"
      :aria-labelledby="checkout ? 'inspector-tab-files' : undefined"
      :tabindex="checkout ? 0 : undefined"
      class="relative flex min-h-0 flex-1 flex-col"
    >
      <div ref="treeViewport" class="details-scroll" aria-label="Checkout files" @scroll="onTreeScroll">
        <div class="file-tree-content">
          <!-- Intrinsic sizing uses every expanded row, not the changing virtual window. The words
               are measured instead of drawn, so this is the row that sets the width rather than
               one per row; interactive rows and icons stay virtualized. -->
          <div class="tree-width-sizer" aria-hidden="true" inert>
            <div
              v-for="item in treeWidth.rows"
              :key="item.entry?.path ?? item.message"
              class="tree-width-row"
              :style="{ paddingLeft: rowIndent(item.depth), width: `${treeWidth.px}px` }"
            >
              <template v-if="item.entry">
                <span class="tree-width-gutter" />
                <span class="tree-width-name">{{ item.entry.name }}</span>
                <span v-if="rowStatus(item.entry.path)" class="file-status">{{ rowStatus(item.entry.path) }}</span>
                <span v-if="item.additions" class="diff-add">+{{ item.additions }}</span>
                <span v-if="item.deletions" class="diff-del">-{{ item.deletions }}</span>
              </template>
              <span v-else>{{ item.message }}</span>
            </div>
          </div>
          <!-- The two measurers, one per type a row's words are drawn at. They carry the classes of
               the spans they stand in for, because a word weighed in another type is a width the row
               does not draw. Out of the flow, so they contribute nothing to the scroller. -->
          <span class="tree-width-probe" aria-hidden="true">
            <span ref="rowTextProbe" class="tree-width-name" />
            <span ref="statusTextProbe" class="file-status" />
          </span>
          <div
            v-if="stickyFolders.length"
            class="sticky-folders"
            :style="{
              marginBottom: `-${rowHeight * stickyFolders.length}px`,
            }"
            aria-label="Ancestor folders"
          >
            <div
              v-for="folder in stickyFolders"
              :key="folder.entry.path"
              class="file-row file-folder"
              :class="{ 'is-selected': selectedFolderPath === folder.entry.path }"
              :style="{ paddingLeft: rowIndent(folder.depth) }"
            >
              <button
                type="button"
                class="folder-toggle"
                :aria-label="`Collapse ${folder.entry.name}`"
                aria-expanded="true"
                @click="toggleDirectory(folder.entry)"
              >
                <ChevronRightIcon class="icon-xxs chevron expanded" aria-hidden="true" />
              </button>
              <FileIcon
                class="file-icon"
                :name="folder.entry.name"
                kind="directory"
                :prominence="prominenceOf(folder.entry)"
              />
              <button
                type="button"
                class="file-name folder-name"
                :class="`file-name-${prominenceOf(folder.entry)}`"
                :data-folder-path="folder.entry.path"
                :title="entryTooltip(folder.entry.path)"
                :aria-label="`Show ${folder.entry.name} in tree`"
                :aria-current="selectedFolderPath === folder.entry.path ? 'true' : undefined"
                @click="revealFolder(folder.entry)"
              >
                {{ folder.entry.name }}
              </button>
            </div>
          </div>
          <p v-if="rootState === 'idle'" class="pane-state">Open a checkout to browse files.</p>
          <p v-else-if="rootState === 'loading'" role="status" class="pane-state">Loading files…</p>
          <!-- The root's own failure is the panel's whole content, so it is drawn here instead of
             expiring in a toast over an empty tree. -->
          <p v-else-if="rootState === 'error'" role="alert" class="pane-state">
            {{ rootError || "Could not list this checkout." }}
          </p>
          <p v-else-if="rootState === 'missing'" role="status" class="pane-state">Checkout is missing.</p>
          <p
            v-else-if="directories['.']?.length === 0 && directoryStates['.'] !== 'truncated'"
            role="status"
            class="pane-state"
          >
            This checkout is empty.
          </p>
          <template v-else>
            <p v-if="directoryStates['.'] === 'truncated'" class="file-row file-note" :style="{ paddingLeft: '8px' }">
              Some entries omitted (folder is large).
            </p>
            <div
              class="file-tree-window"
              :style="{
                paddingTop: `${treeWindow.paddingTop}px`,
                paddingBottom: `${treeWindow.paddingBottom}px`,
              }"
            >
              <template
                v-for="(item, index) in treeWindow.rows"
                :key="item.entry?.path ?? `${item.depth}-${index}-${item.message}`"
              >
                <p
                  v-if="!item.entry"
                  class="file-row file-note"
                  :style="{ paddingLeft: rowIndent(item.depth) }"
                  :title="item.message"
                >
                  {{ item.message }}
                </p>
                <div
                  v-else-if="item.entry.kind === 'directory'"
                  class="file-row file-folder"
                  :class="{ 'is-selected': selectedFolderPath === item.entry.path }"
                  :style="{ paddingLeft: rowIndent(item.depth) }"
                >
                  <button
                    type="button"
                    class="folder-toggle"
                    :aria-label="`${expandedSet.has(item.entry.path) ? 'Collapse' : 'Expand'} ${item.entry.name}`"
                    :aria-expanded="expandedSet.has(item.entry.path)"
                    @click="toggleDirectory(item.entry)"
                  >
                    <ChevronRightIcon
                      class="icon-xxs chevron"
                      :class="{ expanded: expandedSet.has(item.entry.path) }"
                      aria-hidden="true"
                    />
                  </button>
                  <FileIcon
                    class="file-icon"
                    :name="item.entry.name"
                    kind="directory"
                    :prominence="prominenceOf(item.entry)"
                  />
                  <button
                    type="button"
                    tabindex="0"
                    class="file-name folder-name"
                    :class="`file-name-${prominenceOf(item.entry)}`"
                    :data-folder-path="item.entry.path"
                    :title="entryTooltip(item.entry.path)"
                    :aria-label="`${expandedSet.has(item.entry.path) ? 'Collapse' : 'Expand'} ${item.entry.name}`"
                    :aria-current="selectedFolderPath === item.entry.path ? 'true' : undefined"
                    @click="onEntryRowClick(item.entry, $event)"
                  >
                    {{ item.entry.name }}
                  </button>
                </div>
                <button
                  v-else
                  type="button"
                  class="file-row"
                  :class="{ 'is-selected': selectedPath === item.entry.path }"
                  :style="{ paddingLeft: rowIndent(item.depth) }"
                  :title="entryTooltip(item.entry.path)"
                  :aria-current="selectedPath === item.entry.path ? 'true' : undefined"
                  @click="onEntryRowClick(item.entry, $event)"
                >
                  <span class="chevron-spacer" aria-hidden="true" />
                  <FileIcon
                    class="file-icon"
                    :name="item.entry.name"
                    kind="file"
                    :prominence="prominenceOf(item.entry)"
                  />
                  <span class="file-name" :class="`file-name-${prominenceOf(item.entry)}`">
                    {{ item.entry.name }}
                  </span>
                  <span v-if="rowStatus(item.entry.path)" class="file-status" :data-status="rowStatus(item.entry.path)">
                    {{ rowStatus(item.entry.path) }}
                  </span>
                  <span v-if="item.additions" class="diff-add">+{{ item.additions }}</span>
                  <span v-if="item.deletions" class="diff-del">-{{ item.deletions }}</span>
                </button>
              </template>
            </div>
          </template>
        </div>
      </div>
      <OverlayScrollbar :target="treeViewport" label="Checkout files" />
    </div>

    <div v-if="showChanges" v-show="activeTab === 'changes'" class="relative flex min-h-0 flex-1 flex-col">
      <div class="details-all-changes">
        <button type="button" class="file-row new-item" @click="openAllChanges">
          <SquareArrowOutUpRightIcon class="icon-xs file-icon" aria-hidden="true" />
          <span>All changes</span>
        </button>
      </div>
      <div
        id="inspector-panel-changes"
        role="tabpanel"
        aria-labelledby="inspector-tab-changes"
        tabindex="0"
        class="flex min-h-0 flex-1 flex-col"
      >
        <div ref="changesViewport" class="details-scroll" aria-label="Changed files" @scroll="onChangesScroll">
          <p v-if="gitSnapshot.statusState === 'loading'" role="status" class="pane-state">Loading Git status…</p>
          <p v-else-if="gitSnapshot.statusState === 'error'" role="alert" class="pane-state">
            {{ gitSnapshot.changesStatusError || gitSnapshot.statusError }}
          </p>
          <p v-else-if="changedFileCount === 0" role="status" class="pane-state">No changed files.</p>
          <template v-else>
            <p
              v-if="gitSnapshot.changesWatchError"
              class="file-row file-note"
              :style="{ paddingLeft: '8px' }"
              :title="`Live updates unavailable: ${gitSnapshot.changesWatchError}`"
            >
              Live updates unavailable: {{ gitSnapshot.changesWatchError }}
            </p>
            <div
              :style="{
                paddingTop: `${changeWindow.paddingTop}px`,
                paddingBottom: `${changeWindow.paddingBottom}px`,
              }"
            >
              <template v-for="row in changeWindow.rows" :key="row.key">
                <div v-if="row.kind === 'group'" class="details-group-header" :title="row.dir || '/'">
                  <span class="file-name">{{ row.dir || "/" }}</span>
                </div>
                <button
                  v-else
                  type="button"
                  class="file-row change-row"
                  :class="{ 'is-selected': selectedChangedPath === row.key }"
                  :aria-current="selectedChangedPath === row.key ? 'true' : undefined"
                  :title="row.oldPath ? `${row.oldPath} → ${row.key}` : row.key"
                  @click="selectChange(row.key)"
                >
                  <span class="file-status" :data-status="row.status">{{ row.status }}</span>
                  <span class="file-name">{{ row.name }}</span>
                  <span v-if="row.additions" class="diff-add">+{{ row.additions }}</span>
                  <span v-if="row.deletions" class="diff-del">-{{ row.deletions }}</span>
                </button>
              </template>
            </div>
          </template>
        </div>
        <OverlayScrollbar :target="changesViewport" label="Changed files" />
      </div>
    </div>
  </aside>
</template>

<style scoped>
/* The tab strip stays put */
.details-tabs {
  display: flex;
  align-items: stretch;
  flex-shrink: 0;
  border-bottom: 1px solid var(--muster-border);
}

.details-tab {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: -1px;
  padding: 7px 6px;
  border: none;
  border-bottom: 1px solid transparent;
  background: transparent;
  color: var(--muster-text-faint);
  font-family: inherit;
  font-size: 0.6875rem;
  letter-spacing: 0.02em;
  text-align: left;
  cursor: pointer;
}

/* A tab is a segment of the strip, not a standalone control, so it keeps the mockup's
   transparent rest and accent underline, and takes the hover and pressed states on top. A
   resting surface here would break the strip into separate blocks. */
.details-tab:hover {
  color: var(--muster-text);
  background: transparent;
}

.details-tab:active {
  background: var(--muster-control-pressed);
  color: var(--muster-text);
}

.details-tab:active .details-tab-count {
  color: var(--muster-text);
}

.details-tab[aria-selected="true"] {
  color: var(--muster-text);
  border-bottom-color: var(--muster-accent);
}

.details-tab-count {
  color: var(--muster-text-faint);
  font-size: 0.625rem;
}

/* The tab strip stays put, each list scrolls on its own, and each draws its own bar over its right
   edge: the browser's is off (in `style.css`) so a row whose status letter or +/- counts end at the
   edge are not read through a scrollbar. */
.file-tree-content {
  width: max-content;
  min-width: 100%;
}

.tree-width-sizer {
  height: 0;
  overflow: hidden;
  visibility: hidden;
}

/* The measurer. Absolutely positioned, so it is out of the scroll container's flow whatever the
   tree holds, and carrying the row's own type, so what it reports is what a row is drawn at. */
.tree-width-probe {
  position: absolute;
  top: 0;
  left: 0;
  display: flex;
  visibility: hidden;
  pointer-events: none;
  font-family: inherit;
  font-size: 13px;
  white-space: nowrap;
}

.tree-width-gutter {
  width: 37px; /* 16px chevron + 14px icon + their 7px gap; the name has the next gap. */
  flex: 0 0 37px;
}

.sticky-folders {
  position: sticky;
  top: -4px;
  z-index: 2;
  background: var(--muster-bg-0);
}

.details-scroll {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 4px 0;
}

/*
 * Every row in both lists is exactly `--tree-row-height` tall, which the component publishes from
 * its window math: 3px of padding + a (height - 6px) line box + 3px of padding. The virtual
 * window multiplies and divides by the same number, so this box and that constant have to move
 * together.
 */
/* The row, and it is the sidebar's row. 26px, an 8px gutter, a 7px gap and the panel's own 13px:
   the same box in the same panel type, so a file and a terminal are read as two rows of one app
   rather than as two lists that happen to sit next to each other. Nothing here is rounded, and
   nothing here moves on hover. */
.file-row,
.tree-width-row {
  display: flex;
  align-items: center;
  gap: 7px;
  width: max-content;
  min-width: 100%;
  height: var(--tree-row-height);
  padding: 3px 8px;
  border: none;
  background: transparent;
  color: var(--muster-text-secondary);
  font-family: inherit;
  font-size: 13px;
  line-height: calc(var(--tree-row-height) - 6px);
  text-align: left;
  white-space: nowrap;
  cursor: pointer;
}

/* Hover and selection are the panel's own vocabulary, taken from the sidebar rather than invented
   again: the subtle surface for the row being acted on, a different one plus a two-pixel accent
   edge for the row being chosen. */
.file-row:hover {
  background: var(--muster-el-hover);
  color: var(--muster-text);
}

/* What is open, or the change under review, is the one row with a surface (E.3).

   The edge is an inset shadow for the same reason the sidebar's is: drawn inside the row's box it
   cannot spill onto the gutter a row is indented into, so a deep row's accent still starts at the
   panel's own edge instead of one level in. */
.file-row.is-selected {
  background: var(--muster-el-selected);
  color: var(--muster-text);
  box-shadow: inset 2px 0 0 var(--muster-accent);
}

.file-folder {
  color: var(--muster-text);
}

.folder-toggle,
.folder-name {
  border: 0;
  background: transparent;
  color: inherit;
  font: inherit;
  cursor: pointer;
}

.folder-toggle {
  display: flex;
  flex: 0 0 16px;
  align-items: center;
  justify-content: center;
  padding: 0;
}

.folder-name {
  flex: 0 0 auto;
  padding: 0;
  text-align: left;
}

.file-name,
.tree-width-name {
  flex: 0 0 auto;
  white-space: nowrap;
}

/* The name steps with the icon, a dotfile and an ignored file both still readable rather than
   fading out. Quieting is opacity and not a mixed color, which is the whole reason: a color of its
   own would win over the one the row hands down, and these two numbers are the name half of the
   ramp that FileIcon's opacity rules make up the icon half. */
.file-name-hidden {
  opacity: 0.72;
}

.file-name-ignored {
  opacity: 0.48;
}

.file-icon,
.chevron {
  flex-shrink: 0;
  color: var(--muster-text-faint);
}

/* The fold, in a fixed 16px slot, so a folder's glyph and a file's icon land on one column the way
   they do in the sidebar. A file row draws the slot empty rather than nothing at all: a file with
   no chevron is still a file under a folder, and a row whose name moves a glyph's width when the
   pointer arrives is a name a reader has to re-find. */
.chevron {
  width: 16px;
  height: 16px;
  transition: transform 0.12s ease;
}

.chevron.expanded {
  transform: rotate(90deg);
}

.chevron-spacer {
  width: 16px;
  height: 16px;
  flex-shrink: 0;
}

/* Folder states and watch errors, one line tall like any other row */
.file-note {
  color: var(--muster-text-faint);
  cursor: default;
}

.file-note:hover {
  background: transparent;
}

.file-status {
  flex-shrink: 0;
  font-size: 0.625rem;
  color: var(--muster-text-secondary);
}

.file-status[data-status="M"] {
  color: var(--muster-text);
}

.file-status[data-status="A"] {
  color: var(--muster-success-fg);
}

.file-status[data-status="D"] {
  color: var(--muster-danger-fg);
}

.file-status[data-status="U"] {
  color: var(--muster-text-faint);
}

.file-row:hover .file-status[data-status="U"],
.file-row.is-selected .file-status[data-status="U"] {
  color: var(--muster-text);
}

.details-all-changes {
  flex-shrink: 0;
  padding: 2px 0;
}

.file-row.new-item {
  color: var(--muster-text-dim);
}

.file-row.new-item:hover {
  color: var(--muster-text);
}

/* The group header is a heading and not a row, and says so by not taking one: no surface at rest, no
   hover, nothing to select. That is the sidebar's repo heading exactly — the name reads at the
   panel's own 13px in the faint ink, set apart by size rather than by a box, and the rows under it
   are the ones that answer the pointer. */
.details-group-header {
  display: flex;
  align-items: center;
  height: var(--tree-row-height);
  padding: 0 8px;
  color: var(--muster-text-faint);
  font-size: 13px;
  line-height: var(--tree-row-height);
  letter-spacing: 0.02em;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* One level in from its header: the 8px gutter plus the same 14px step the file tree indents by,
   so a change row and a file row are at the same depth under the same name. */
.change-row {
  padding-left: 22px;
}
</style>
