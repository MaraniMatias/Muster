import { invoke } from "@tauri-apps/api/core";
import type { Channel } from "@tauri-apps/api/core";
import type { OpenedFolder } from "../domain/folder";
import type {
  CheckoutImage,
  FileContent,
  FileProbe,
  FileTree,
  PrettierConfig,
  ReviewExport,
  ReviewFolder,
  ReviewFolderCleared,
} from "../domain/files";
import type { GitCheckoutDiffStats, GitDiffPage, GitFileDiff, GitFileDiffStats, GitStatus } from "../domain/git";
import type { ReviewAnchorCheck, ReviewNote, ReviewRound, ReviewSide, ReviewTarget } from "../domain/review";
import type { RecentPath, Session, TerminalSessionStatus, WorkspaceState } from "../domain/workspace";
import type { CreatedWorktree, RemovedWorktree, WorktreeDefaults, WorktreeRemovalInfo } from "../domain/worktree";
import type { CheckoutTerminalLayout } from "../domain/terminal-layout";
import type { AppLayoutState, CheckoutUiState } from "../domain/ui-state";
import type { AppSettings, ReviewStorageMode } from "../domain/settings";
import type { AgentAgent, AgentRelocation, AgentSession } from "../domain/agent";
import type { DocumentOrigin } from "../domain/main-document";

export interface CreatedTerminal {
  session: Session;
  workspace: WorkspaceState;
}

export function listCheckoutFiles(checkoutId: string, path: string): Promise<FileTree> {
  return invoke<FileTree>("files_list", { checkoutId, path });
}

/** Raw response: bounded ASCII MIME, newline, then validated file bytes. */
export async function readCheckoutMedia(checkoutId: string, path: string): Promise<Blob> {
  const response = await invoke<ArrayBuffer | number[]>("file_read_media", { checkoutId, path });
  const bytes = response instanceof ArrayBuffer ? new Uint8Array(response) : Uint8Array.from(response);
  const separator = bytes.subarray(0, 64).indexOf(10);
  if (separator < 1) throw new Error("Invalid media response.");
  const mime = new TextDecoder().decode(bytes.subarray(0, separator));
  if (
    ![
      "image/png",
      "image/jpeg",
      "image/gif",
      "image/webp",
      "image/avif",
      "image/x-icon",
      "image/bmp",
      "video/mp4",
      "video/webm",
      "video/quicktime",
      "video/ogg",
    ].includes(mime)
  ) {
    throw new Error("Invalid media type.");
  }
  // A view, not a copy. The Blob constructor takes these bytes once, into the blob it builds; `slice`
  // would have made a second array of the whole payload before it. The cap on the payload is not a
  // memory guarantee either way — this only removes a copy this line did not need.
  return new Blob([bytes.subarray(separator + 1)], { type: mime });
}

export function readCheckoutFile(checkoutId: string, path: string, origin: DocumentOrigin): Promise<FileContent> {
  return invoke<FileContent>("file_read", { checkoutId, path, origin });
}

/**
 * Whether a path a terminal printed names a file the preview can open, resolved to its
 * checkout-relative spelling. `null` is the ordinary answer for a path that names nothing here.
 *
 * `workingDirectory` is where that terminal's shell is, and a bare name is relative to it rather
 * than to the root of the checkout: `ls` prints `LICENSE`, and `LICENSE` means the one in the
 * directory it was run in. It is a hint about where to look — the other end refuses anything that
 * lands outside the checkout whichever directory it was sent — and a terminal that has not reported
 * one yet falls back to the root, which is where a terminal starts. Sent as `null` rather than
 * left out, because absent is what the argument is when nobody has reported a directory yet.
 */
export function probeCheckoutFile(
  checkoutId: string,
  path: string,
  workingDirectory?: string,
): Promise<FileProbe | null> {
  return invoke<FileProbe | null>("file_probe", { checkoutId, path, workingDirectory: workingDirectory ?? null });
}

/**
 * Hands a file or a folder of a checkout to the application this machine has for it.
 *
 * The path is the checkout-relative one the tree already lists. The other end resolves it against
 * the checkout and refuses anything that leaves it, so nothing here can name a file on the rest of
 * the disk — and it waits for the opener's answer rather than spawning and forgetting, because a
 * file that did not open has to be sayable.
 */
export function openExternalFile(checkoutId: string, path: string): Promise<void> {
  return invoke<void>("file_open_externally", { checkoutId, path });
}

export function writeCheckoutFile(
  checkoutId: string,
  path: string,
  content: string,
  expectedContent: string,
  origin: DocumentOrigin,
): Promise<void> {
  return invoke<void>("file_write", { checkoutId, path, content, expectedContent, origin });
}

/**
 * The Prettier options that govern a file, resolved by walking up from its own directory to the
 * root of the checkout. `null` is the ordinary answer for a checkout that configures nothing.
 */
export function readPrettierConfig(
  checkoutId: string,
  path: string,
  origin: DocumentOrigin,
): Promise<PrettierConfig | null> {
  return invoke<PrettierConfig | null>("file_read_prettier_config", { checkoutId, path, origin });
}

/**
 * Writes one exported review round and returns the relative path with the storage origin the backend used.
 */
export function exportReviewMarkdown(
  checkoutId: string,
  date: string,
  timestamp: string,
  markdown: string,
): Promise<ReviewExport> {
  return invoke<ReviewExport>("review_export_markdown", { checkoutId, date, timestamp, markdown });
}

/**
 * Where exported reviews are kept for this checkout, and what is in there.
 *
 * `storage` is the mode the settings row is showing rather than the one on file, so a person
 * changing the selector reads the folder they are choosing before they Apply it.
 */
export function getReviewFolder(checkoutId: string, storage?: ReviewStorageMode): Promise<ReviewFolder> {
  return invoke<ReviewFolder>("review_folder", { checkoutId, storage: storage ?? null });
}

/**
 * Deletes the exported reviews in the app's own folder, and answers what it removed.
 *
 * Refused for a working directory: those notes belong to the repository, and the button that asks
 * for this is not drawn there either.
 */
export function clearReviewFolder(checkoutId: string, confirmedPath: string): Promise<ReviewFolderCleared> {
  return invoke<ReviewFolderCleared>("review_folder_clear", { checkoutId, storage: "default", confirmedPath });
}

export function loadReviewTarget(checkoutId: string): Promise<ReviewTarget> {
  return invoke<ReviewTarget>("review_target_load", { checkoutId });
}

export function saveReviewTarget(checkoutId: string, target: ReviewTarget): Promise<void> {
  return invoke<void>("review_target_save", { checkoutId, target });
}

export function readCheckoutMarkdownImage(
  checkoutId: string,
  markdownPath: string,
  imagePath: string,
): Promise<CheckoutImage> {
  return invoke<CheckoutImage>("file_read_markdown_image", { checkoutId, markdownPath, imagePath });
}

export function getGitStatus(checkoutId: string): Promise<GitStatus> {
  return invoke<GitStatus>("git_status", { checkoutId });
}

/** One call for the line counts of every registered Git checkout, so the sidebar does not
 *  have to ask checkout by checkout. The base ref is resolved in Rust, never sent from here. */
export function getGitCheckoutDiffStats(): Promise<GitCheckoutDiffStats> {
  return invoke<GitCheckoutDiffStats>("git_checkout_diff_stats");
}

/** The line counts of one checkout's changed files, against the same base ref the file list
 *  is built from. */
export function getGitDiffStats(checkoutId: string): Promise<GitFileDiffStats> {
  return invoke<GitFileDiffStats>("git_diff_stats", { checkoutId });
}

export function getGitDiff(checkoutId: string, path: string): Promise<GitFileDiff> {
  return invoke<GitFileDiff>("git_diff", { checkoutId, path });
}

export function getGitDiffPage(checkoutId: string, path: string, offset: number, limit: number): Promise<GitDiffPage> {
  return invoke<GitDiffPage>("git_diff_page", { checkoutId, path, offset, limit });
}

/** Watches a whole repository rather than one checkout. `registrationId` distinguishes this
 *  request from stale failures of an earlier registration of the same plan. */
export function watchGitRepo(repoId: string, registrationId: string, expectedCheckoutIds: string[]): Promise<void> {
  return invoke<void>("git_watch_repo", { repoId, registrationId, expectedCheckoutIds });
}

export function listReviewNotes(checkoutId: string): Promise<ReviewNote[]> {
  return invoke<ReviewNote[]>("review_notes", { checkoutId });
}

export interface ReviewNoteCreate {
  checkoutId: string;
  path: string;
  side: ReviewSide;
  lineStart: number;
  lineEnd?: number;
  content: string;
  code: string;
}

export function createReviewNote(request: ReviewNoteCreate): Promise<ReviewNote> {
  return invoke<ReviewNote>("review_note_create", {
    request: {
      checkoutId: request.checkoutId,
      path: request.path,
      side: request.side,
      lineStart: request.lineStart,
      lineEnd: request.lineEnd ?? null,
      content: request.content,
      code: request.code,
    },
  });
}

export function updateReviewNote(checkoutId: string, id: string, content: string): Promise<ReviewNote> {
  return invoke<ReviewNote>("review_note_update", { checkoutId, id, content });
}

export function deleteReviewNote(checkoutId: string, id: string): Promise<void> {
  return invoke<void>("review_note_delete", { checkoutId, id });
}

export function verifyReviewNoteAnchors(
  checkoutId: string,
  path: string,
  checks: ReviewAnchorCheck[],
): Promise<ReviewNote[]> {
  return invoke<ReviewNote[]>("review_note_anchors_verify", { request: { checkoutId, path, checks } });
}

export function clearReviewNoteOutdated(checkoutId: string, id: string): Promise<ReviewNote> {
  return invoke<ReviewNote>("review_note_outdated_clear", { checkoutId, id });
}

export function resolveReviewNote(checkoutId: string, id: string): Promise<ReviewNote> {
  return invoke<ReviewNote>("review_note_resolve", { checkoutId, id });
}

export function listReviewRounds(checkoutId: string): Promise<ReviewRound[]> {
  return invoke<ReviewRound[]>("review_rounds", { checkoutId });
}

/**
 * Records the round, then delivers it as one message. Never both or neither.
 *
 * With `queue` the round is stored and the agent is left alone: the send happens later,
 * from the stored message, when the caller flushes it.
 */
export function dispatchReviewRound(request: {
  checkoutId: string;
  sessionId: string;
  ids: string[];
  markdown: string;
  /**
   * Required, not optional: the backend refuses a round without it, because both ends of this
   * command ship in the same build and a caller that omits it is not talking to this one.
   */
  queue: boolean;
}): Promise<ReviewRound> {
  return invoke<ReviewRound>("review_round_dispatch", { request });
}

/** Sends the rounds held back while the agent was busy. Returns how many went out. */
export function flushReviewRounds(checkoutId: string): Promise<number> {
  return invoke<number>("review_round_flush", { checkoutId });
}

export function requeueReviewRounds(checkoutId: string): Promise<number> {
  return invoke<number>("review_rounds_requeue", { checkoutId });
}

/** Asks the session whether a round's message arrived, and settles the round accordingly. */
export function reconcileReviewRound(checkoutId: string, roundId: string): Promise<ReviewRound> {
  return invoke<ReviewRound>("review_round_reconcile", { checkoutId, roundId });
}

export function ackReviewRound(checkoutId: string, roundId: string): Promise<ReviewRound> {
  return invoke<ReviewRound>("review_round_ack", { checkoutId, roundId });
}

export function listAgentSessions(checkoutId: string): Promise<AgentSession[]> {
  return invoke<AgentSession[]>("agent_sessions", { checkoutId });
}

/**
 * The candidate sessions a terminal row matches its own title against.
 *
 * Deliberately not `listAgentSessions`: that list is scoped to the worktree, which is what a review
 * round needs, while a terminal's session is not necessarily located in the worktree the terminal is
 * filed under. A terminal grouped under one worktree can have a session open that lives in a sibling
 * one, and a scoped list can never name it — so the row says it identified nothing, with no state and
 * no colour, while the agent is working.
 *
 * As wide as the service answers, which is wider than this repository: nothing here is trusted, and a
 * candidate only becomes a row's state when its title matches exactly one of them. Prompting one by id
 * still refuses a session from another directory.
 */
export function listAgentCandidateSessions(checkoutId: string): Promise<AgentSession[]> {
  return invoke<AgentSession[]>("agent_candidate_sessions", { checkoutId });
}

/**
 * The sessions that have moved to another worktree since the last read.
 *
 * Takes no checkout, because a session that moved is no longer answerable by the one it left.
 * Both sides are worktrees this app holds, so the answer is a pair of checkout ids and never a
 * path.
 */
export function listAgentRelocations(baseline = false): Promise<AgentRelocation[]> {
  return invoke<AgentRelocation[]>("agent_relocations", { baseline });
}
/** Every agent the checkout's server offers, with the color OpenCode paints it with. */
export function listAgentAgents(checkoutId: string): Promise<AgentAgent[]> {
  return invoke<AgentAgent[]>("agent_agents", { checkoutId });
}

export function createAgentSession(checkoutId: string, title: string): Promise<AgentSession> {
  return invoke<AgentSession>("agent_session_create", { checkoutId, title });
}

export function stopAgent(checkoutId: string): Promise<void> {
  return invoke<void>("agent_stop", { checkoutId });
}

export function unwatchGitRepo(repoId: string): Promise<void> {
  return invoke<void>("git_unwatch_repo", { repoId });
}

export function openFolder(path: string): Promise<OpenedFolder> {
  return invoke<OpenedFolder>("open_folder", { path });
}

export function restoreWorkspace(): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("restore_workspace");
}

export function registerFolder(path: string): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("register_folder", { path });
}

/** Re-reads one repository's worktree list, for a worktree something other than this app added.
 *  `null` means the list did not move after all, so a caller already holding the workspace has
 *  nothing to apply. */
export function syncWorkspaceRepo(repoId: string): Promise<WorkspaceState | null> {
  return invoke<WorkspaceState | null>("sync_workspace_repo", { repoId });
}

/** The folders Muster has had open before, newest first. The backend keeps the ten newest. */
export function listRecentPaths(): Promise<RecentPath[]> {
  return invoke<RecentPath[]>("list_recent_paths");
}

export function setDefaultBranch(repoId: string, branch: string): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("set_default_branch", { repoId, branch });
}

/** Takes a workdir off the panel, and only off the panel: the directory, its branch and its
 *  files are all left where they are, so opening the folder again brings the workdir back. */
export function closeCheckout(checkoutId: string): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("close_checkout", { checkoutId });
}

/** Removes a checkout whose directory is gone from Muster alone: nothing on disk is deleted. */
export function closeMissingCheckout(checkoutId: string): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("close_missing_checkout", { checkoutId });
}

/** Takes a worktree off the panel and keeps it: `restoreArchivedWorktrees` brings it back. */
export function archiveCheckout(checkoutId: string): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("archive_checkout", { checkoutId });
}

/** Puts every worktree this repository archived back on the panel. */
export function restoreArchivedWorktrees(repoId: string): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("restore_archived_worktrees", { repoId });
}

export function selectCheckout(checkoutId: string | null): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("select_checkout", { checkoutId });
}

export function selectSession(sessionId: string | null): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("select_session", { sessionId });
}

export function createTerminal(
  checkoutId: string,
  cols: number,
  rows: number,
  onOutput: Channel<ArrayBuffer>,
): Promise<CreatedTerminal> {
  return invoke<CreatedTerminal>("terminal_create", {
    request: { checkoutId, cols, rows },
    onOutput,
  });
}

export function writeTerminal(checkoutId: string, sessionId: string, bytes: Uint8Array): Promise<void> {
  return invoke<void>("terminal_write", { checkoutId, sessionId, bytes: Array.from(bytes) });
}

export function resizeTerminal(checkoutId: string, sessionId: string, cols: number, rows: number): Promise<void> {
  return invoke<void>("terminal_resize", { checkoutId, sessionId, cols, rows });
}

export function getTerminalStatus(checkoutId: string, sessionId: string): Promise<TerminalSessionStatus> {
  return invoke<TerminalSessionStatus>("terminal_status", { checkoutId, sessionId });
}

export function closeTerminal(checkoutId: string, sessionId: string): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("terminal_close", { checkoutId, sessionId });
}

/** Names a session. The name is a label: it is trimmed, length-checked and never run. */
export function renameTerminal(checkoutId: string, sessionId: string, name: string): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("terminal_rename", { checkoutId, sessionId, name });
}

/**
 * Hands a live terminal to another worktree of the same repository.
 *
 * The process, its output and its name stay: what moves is the row the sidebar groups by, so the
 * terminal is listed under the worktree it now belongs to. A destination outside that repository
 * is refused, because two worktrees of one repo share a Git directory and another repo does not.
 *
 * `selectTarget` is whether the window follows the terminal. Someone dragging a row wants to be
 * there; a session that moved on its own must not take over what the window is showing.
 */
export function moveTerminal(
  checkoutId: string,
  sessionId: string,
  targetCheckoutId: string,
  selectTarget = true,
): Promise<WorkspaceState> {
  return invoke<WorkspaceState>("terminal_move", { checkoutId, sessionId, targetCheckoutId, selectTarget });
}

export function loadTerminalLayout(checkoutId: string): Promise<CheckoutTerminalLayout | null> {
  return invoke<CheckoutTerminalLayout | null>("terminal_layout_load", { checkoutId });
}

export function saveTerminalLayout(checkoutId: string, layout: CheckoutTerminalLayout): Promise<void> {
  return invoke<void>("terminal_layout_save", { checkoutId, layout });
}

export function loadAppLayout(): Promise<AppLayoutState> {
  return invoke<AppLayoutState>("ui_layout_load");
}

export function saveAppLayout(layout: AppLayoutState): Promise<void> {
  return invoke<void>("ui_layout_save", { layout });
}

/** The preferences the Settings dialog owns, from `~/.muster/config.yml`. */
export function loadSettings(): Promise<AppSettings> {
  return invoke<AppSettings>("settings_load");
}

export function saveSettings(settings: AppSettings): Promise<void> {
  return invoke<void>("settings_save", { settings });
}

export function loadCheckoutUiState(checkoutId: string): Promise<CheckoutUiState> {
  return invoke<CheckoutUiState>("checkout_ui_state_load", { checkoutId });
}

export function saveCheckoutUiState(checkoutId: string, state: CheckoutUiState): Promise<void> {
  return invoke<void>("checkout_ui_state_save", { checkoutId, state });
}

/** Ends the agent servers and the terminals the app started, before the window closes. */
export function prepareAppExit(): Promise<void> {
  return invoke<void>("app_prepare_exit");
}

/**
 * Hands a web link to the browser the machine has.
 *
 * The other end refuses anything that is not `http` or `https`: this is a URL a document printed,
 * not a command, and the process that opens it reads what it is given as its own argument.
 */
export function openExternalUrl(url: string): Promise<void> {
  return invoke<void>("open_url", { url });
}

export function getWorktreeDefaults(checkoutId: string): Promise<WorktreeDefaults> {
  return invoke<WorktreeDefaults>("worktree_defaults", { checkoutId });
}

export function createWorktree(
  checkoutId: string,
  taskName: string,
  branch: string,
  location: string,
): Promise<CreatedWorktree> {
  return invoke<CreatedWorktree>("worktree_create", { checkoutId, taskName, branch, location });
}

export function getWorktreeRemovalInfo(checkoutId: string): Promise<WorktreeRemovalInfo> {
  return invoke<WorktreeRemovalInfo>("worktree_removal_info", { checkoutId });
}

export function removeWorktree(
  checkoutId: string,
  confirmDirty: boolean,
  confirmedDirtyFiles: string[],
  confirmedSessionIds: string[],
  expectedBranch: string | null,
  expectedUnmergedCommits: number,
  deleteBranch: boolean,
): Promise<RemovedWorktree> {
  return invoke<RemovedWorktree>("worktree_remove", {
    checkoutId,
    confirmation: {
      confirmDirty,
      confirmedDirtyFiles,
      confirmedSessionIds,
      expectedBranch,
      expectedUnmergedCommits,
      deleteBranch,
    },
  });
}
