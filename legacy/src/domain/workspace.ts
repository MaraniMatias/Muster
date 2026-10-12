import type { OpenedFolder } from "./folder";

export type RepoKind = "git" | "plain";
export type SessionType = "shell" | "agent";
export type SessionStatus = "active" | "inactive";
export type TerminalProcessState = "running" | "exited";

export interface TerminalSessionStatus {
  state: TerminalProcessState;
  exitCode?: number;
  foregroundProcess?: boolean;
  /** The program in front of the shell, by name: `opencode`, `nvim`. Absent when it is idle. */
  foregroundApp?: string;
  /** Title most recently set by the PTY via OSC 0/2, if any. */
  terminalTitle?: string | null;
  /**
   * `$?` of the last command the shell ran, from the OSC 133 hook in `services/terminal.rs`.
   *
   * A frontend-only field like `terminalTitle`, and for the same reason: the backend cannot know it,
   * because a command that fails does not exit the shell and is a grandchild of the process it
   * spawns. Absent when the shell has not reported one yet, which is every shell with no integration
   * installed and the moment before the first command finishes.
   */
  lastCommandExit?: number;
  /**
   * Where the shell itself is, as the OS records it. Absent once the terminal has exited, and when
   * the OS will not say.
   *
   * This is what says a plain shell changed worktree: an agent that moved itself reports where it
   * is working, but its shell is still sitting where it was launched.
   */
  workingDirectory?: string;
}

export interface Repo {
  id: string;
  kind: RepoKind;
  name: string;
  root: string;
  defaultBranch?: string;
  checkouts: Checkout[];
  createdAt: string;
  lastOpenedAt: string;
}

export interface Checkout {
  id: string;
  repoId: string;
  path: string;
  canonicalPath: string;
  isPrimary: boolean;
  branch?: string;
  head?: string;
  aheadOfDefault?: number;
  changedFiles: number;
  isMissing: boolean;
  sessions: Session[];
}

export interface Session {
  id: string;
  type: SessionType;
  checkoutId: string;
  name: string;
  createdAt: string;
  status: SessionStatus;
}

/**
 * A worktree that is registered and alive but off the panel.
 *
 * It is what archiving leaves behind: the row goes, the directory and its branch stay,
 * and the repo root is the one row that can put it back.
 */
export interface ArchivedCheckout {
  id: string;
  repoId: string;
  path: string;
  branch?: string;
}

export interface WorkspaceState {
  repos: Repo[];
  /**
   * The worktrees that could be put back, so the repo root can offer them.
   *
   * Absent when there are none, which is every workspace that has never archived one.
   */
  archivedWorktrees?: ArchivedCheckout[];
  activeCheckoutId: string | null;
  activeSessionId: string | null;
  homeCheckoutId?: string | null;
}

/** A folder Muster has opened before, as the workdir menu lists it. */
export interface RecentPath {
  canonicalPath: string;
  lastOpenedAt: string;
}

/**
 * How a checkout is named wherever it is listed.
 *
 * Git checkouts are named by their branch, with "Base" for a detached repo root. Plain
 * workdirs use their repo name so the sidebar and titlebar agree.
 */
export function workdirTitle(repo: Repo, checkout: Checkout): string {
  return checkout.branch || (repo.kind === "plain" ? repo.name : checkout.isPrimary ? "Base" : checkout.path);
}

/** Show checkout paths relative to the user's home without abbreviating other prefixes. */
export function displayCheckoutPath(path: string, homePath: string | undefined): string {
  if (!homePath) return path;
  if (path === homePath) return "~";
  if (!path.startsWith(homePath)) return path;
  const remainder = path.slice(homePath.length);
  return remainder.startsWith("/") || remainder.startsWith("\\") ? `~${remainder}` : path;
}

/**
 * How a session is named wherever it is listed: the sidebar row and the titlebar crumb are the
 * same list read twice, so the rule is written once and neither of them keeps its own.
 *
 * A program-set terminal title wins; otherwise the program in front of the shell, and the name it
 * was opened with when nothing is in front of it, which is also what a terminal with no live
 * status, one that is not on the panel, has to be named by.
 */
export function sessionTitle(session: Session, status?: TerminalSessionStatus | null): string {
  return status?.terminalTitle || status?.foregroundApp || session.name;
}

/**
 * Whether a terminal has a process in front of its shell, which is what makes closing it a question
 * rather than a cleanup.
 *
 * Two conditions, and the second is the one that matters: a shell at a prompt is `running` too, and
 * nothing is lost by closing that. What closing destroys is a process with its own output in flight,
 * so the status alone never answers it.
 *
 * Written here because three places have to agree and only one of them is visible: the close button,
 * the close that runs when a shell exits on purpose, and the close of the whole window. A rule kept
 * per caller is how the window ends up stopping a build without ever asking.
 */
export function terminalHasProcess(status: TerminalSessionStatus | null | undefined): boolean {
  return status?.state === "running" && status.foregroundProcess === true;
}

/**
 * The program whose TUI writes a session title, and the only program this panel treats as one.
 *
 * Named here because two things need it and must agree: a terminal's own title is only read as an
 * agent's session title while this program is the one in front of the shell, and the row's glyph
 * and its second line are drawn from the same answer. `App.vue`'s foreground answer comes from the
 * OS as a process name, so this is that name and not a guess at what a TUI calls itself.
 */
export const AGENT_APP = "opencode";

/**
 * What OpenCode's TUI puts in front of the title of the session it has open, as observed in the
 * installed build: `setTerminalTitle(\`OC | ${title.length > 40 ? title.slice(0, 37) + "…" : title}\`)`.
 *
 * The prefix is the whole of the filter. A shell writes its prompt into the terminal title and an
 * editor writes its file, neither with this, and both say where the terminal is rather than what it
 * is doing — which the row above already says. Whether the string behind the prefix belongs to a
 * session the service still lists is a separate question, and `matchAgentSessionTitle` is where that
 * is asked.
 */
const AGENT_TITLE_PREFIX = "OC | ";

/** What OpenCode titles its TUI with while no session is open, which names the program and nothing else. */
const AGENT_HOME_TITLE = "OpenCode";

/**
 * The session title a terminal's own program wrote, or null when it wrote nothing that names one.
 *
 * **The runtime must confirm the program.** A terminal title is whatever last wrote one, and a PTY
 * title outlives the process that set it: when OpenCode exits, the shell behind it takes the
 * terminal back and the title stays `OC | …` until something else writes one. So this asks the
 * current foreground answer first, and a stale title over a shell, over `sleep`, or over any other
 * program is no title at all — which is the only honest reading, since the panel cannot tell a
 * leftover string from a live one and the alternative is naming a terminal after an agent that is
 * no longer running in it.
 *
 * What comes back is still **inferred identity, not a mapping**. OpenCode 2.0.22 exposes no route,
 * header or event that ties a TUI process to a session id, so this is a string the program itself
 * wrote about itself, read by a client that cannot verify it. Two terminals in one worktree showing
 * the same session would both report this title, and nothing here can tell that from two terminals
 * each showing their own session with one title.
 */
export function agentSessionTitle(status: TerminalSessionStatus | null | undefined): string | null {
  if (status?.foregroundApp !== AGENT_APP) return null;
  const title = status.terminalTitle;
  if (!title?.startsWith(AGENT_TITLE_PREFIX)) return null;
  const session = title.slice(AGENT_TITLE_PREFIX.length).trim();
  return session && session !== AGENT_HOME_TITLE ? session : null;
}

/**
 * How a terminal is named in the sidebar row, which is a narrower question than `sessionTitle`.
 *
 * Three answers, in the order `sessionTitle` uses them, because a sidebar row and the titlebar
 * crumb list the same terminals and cannot disagree about what one is called:
 *
 * - The agent session's own title, when this terminal's program is confirmed to be the agent and
 *   wrote one. It is the only name that came from the thing running rather than from the place it
 *   runs.
 * - The program in front of the shell, read from the OS.
 * - The name the session was opened with, which is where a rename shows.
 *
 * **The row says nothing about where the terminal is.** The worktree or branch row directly above
 * already carries that identity, and repeating it on every child gave two sibling shells the same
 * text — `zsh · Muster` twice, side by side — which is the one thing a list of terminals cannot be:
 * two rows the reader cannot tell apart. So the name is the session and nothing else: an idle shell
 * reads `zsh`, a running one reads `pnpm` or `opencode`, a renamed one reads its rename, and an
 * identified agent reads its session's title.
 *
 * Two idle shells in one worktree therefore both read `zsh`. That is truthful — nothing observed
 * distinguishes them — and it is why the row's accessible name carries its state as well and why a
 * rename remains the way to tell two of them apart. No ordinal, timestamp or id is invented here to
 * fill the gap.
 *
 * Unlike `sessionTitle`, a terminal's own title is read only behind the agent's own prefix: a shell
 * writes its prompt there and an editor writes its file, and both name a place rather than a session.
 */
export function sessionRowTitle(session: Session, status: TerminalSessionStatus | null | undefined): string {
  return agentSessionTitle(status) || status?.foregroundApp || session.name;
}

/**
 * What stands for a checkout, as one word, wherever a checkout is drawn.
 *
 * The sidebar row that lists a workdir and the rule that names it are one thing split in two, so
 * the word is written here and the row asks for it: nothing about the glyph belongs to the panel
 * that happens to be showing it. `terminal`, `agent`, `asking` and `working` are in the set because
 * the row of a session asks for one too, but they are not what a checkout answers: they name an item
 * open inside a workdir, not the workdir itself.
 */
export type WorkdirIconKind =
  "git" | "worktree" | "folder" | "home" | "missing" | "terminal" | "agent" | "asking" | "working";

/**
 * Which icon a checkout wears: a directory that is gone before anything else, the home one, a
 * plain folder, a repo root, and a worktree of that root.
 *
 * The order is the whole rule. A missing directory is still the row it always was, and the home
 * one is still the place rather than the worktree of a repository, so neither of them can be
 * decided further down.
 */
export function workdirIconKind(
  repo: Repo | null,
  checkout: Checkout,
  homeCheckoutId?: string | null,
): Exclude<WorkdirIconKind, "terminal" | "agent" | "asking" | "working"> {
  if (checkout.isMissing) return "missing";
  if (checkout.id === homeCheckoutId) return "home";
  if (repo?.kind !== "git") return "folder";
  return checkout.isPrimary ? "git" : "worktree";
}

/**
 * Whether `child` is `parent` or somewhere under it, compared a whole segment at a time.
 *
 * A trailing separator is dropped and nothing else is normalised: the string this is given comes
 * from the OS, and `/work/repo-wt-2` is not inside `/work/repo-wt` no matter how one prefixes the
 * other.
 */
function containsDirectory(parent: string, child: string): boolean {
  const trim = (value: string) => value.replace(/\/+$/, "");
  const root = trim(parent);
  return child === root || child.startsWith(`${root}/`);
}

/**
 * The registered checkout matching a terminal's working directory.
 *
 * The answer may be the checkout the terminal is already under, and that is not a corner case: a
 * worktree created inside its own repository lives under the repository's directory, so the two
 * both contain it. The most specific one is the worktree the directory is really in, and answering
 * with the one the row already names is what stops a shell sitting still from being moved back and
 * forth between a worktree and the repository above it on every poll.
 *
 * Both `path` and `canonicalPath` are tried because the OS reports the directory as the kernel
 * recorded it and the two are not always spelled the same way, as `/tmp` and `/private/tmp` show.
 * Equal-depth matches or duplicate canonical paths are ambiguous and name no checkout.
 */
export function checkoutForWorkingDirectory(repos: Repo[], fromCheckoutId: string, directory: string): Checkout | null {
  if (!repos.some((repo) => repo.checkouts.some((checkout) => checkout.id === fromCheckoutId))) return null;
  const checkouts = repos.flatMap((repo) => repo.checkouts.filter((checkout) => !checkout.isMissing));
  const matches = checkouts.flatMap((checkout) => {
    const depth = [checkout.path, checkout.canonicalPath]
      .filter((path) => containsDirectory(path, directory))
      .reduce((longest, path) => Math.max(longest, path.length), -1);
    return depth < 0 ? [] : [{ checkout, depth }];
  });
  matches.sort((left, right) => right.depth - left.depth);
  const target = matches[0];
  if (
    !target ||
    checkouts.some(
      (checkout) => checkout !== target.checkout && checkout.canonicalPath === target.checkout.canonicalPath,
    ) ||
    matches[1]?.depth === target.depth
  )
    return null;
  return target.checkout;
}

export function repoIdForPath(canonicalPath: string): string {
  return `repo:${canonicalPath}`;
}

export function checkoutIdForPath(canonicalPath: string): string {
  return `checkout:${canonicalPath}`;
}

export function createCheckout(
  input: Omit<Checkout, "id" | "changedFiles" | "isMissing" | "sessions"> &
    Partial<Pick<Checkout, "changedFiles" | "isMissing" | "sessions">>,
): Checkout {
  return {
    ...input,
    id: checkoutIdForPath(input.canonicalPath),
    changedFiles: input.changedFiles ?? 0,
    isMissing: input.isMissing ?? false,
    sessions: input.sessions ?? [],
  };
}

export function createPlainRepo(folder: OpenedFolder, now = new Date().toISOString()): Repo {
  const checkout = createCheckout({
    repoId: repoIdForPath(folder.path),
    path: folder.path,
    canonicalPath: folder.path,
    isPrimary: true,
  });

  return {
    id: repoIdForPath(folder.path),
    kind: "plain",
    name: folder.name,
    root: folder.path,
    checkouts: [checkout],
    createdAt: now,
    lastOpenedAt: now,
  };
}

export function createWorkspaceState(): WorkspaceState {
  return { repos: [], activeCheckoutId: null, activeSessionId: null };
}

export function openRepo(
  state: WorkspaceState,
  repo: Repo,
  focusedCheckoutId: string | null = repo.checkouts[0]?.id ?? null,
): WorkspaceState {
  const existingRepo = state.repos.find(
    (knownRepo) =>
      knownRepo.id === repo.id ||
      knownRepo.checkouts.some((checkout) => repo.checkouts.some((opened) => opened.id === checkout.id)),
  );
  const repoId = existingRepo?.id ?? repo.id;
  const checkouts = repo.checkouts.map((opened) => {
    const existing = existingRepo?.checkouts.find((checkout) => checkout.id === opened.id);
    if (opened.repoId === repoId && (!existing || opened.sessions === existing.sessions)) return opened;
    return { ...opened, repoId, ...(existing ? { sessions: existing.sessions } : {}) };
  });
  const resolvedRepo = {
    ...repo,
    id: repoId,
    checkouts,
  };
  const repos = existingRepo
    ? state.repos.map((knownRepo) =>
        knownRepo.id === existingRepo.id ? { ...resolvedRepo, createdAt: knownRepo.createdAt } : knownRepo,
      )
    : [...state.repos, resolvedRepo];
  return selectCheckout({ ...state, repos }, focusedCheckoutId);
}

export function selectCheckout(state: WorkspaceState, checkoutId: string | null): WorkspaceState {
  if (checkoutId === null) return { ...state, activeCheckoutId: null, activeSessionId: null };
  const checkout = state.repos.flatMap((repo) => repo.checkouts).find((item) => item.id === checkoutId);
  if (!checkout) return state;

  return {
    ...state,
    activeCheckoutId: checkout.id,
    activeSessionId: checkout.id === state.activeCheckoutId ? state.activeSessionId : null,
  };
}

export function selectSession(state: WorkspaceState, sessionId: string): WorkspaceState {
  for (const repo of state.repos) {
    const checkout = repo.checkouts.find((item) => item.sessions.some((session) => session.id === sessionId));
    if (checkout) return { ...state, activeCheckoutId: checkout.id, activeSessionId: sessionId };
  }
  return state;
}

/**
 * The terminal the window means, for a workdir that has any: the selected one, or the newest.
 *
 * The selection is stored (`activeSessionId`) but it is not the whole answer, and a rule kept per
 * reader is how the sidebar and the main panel come to disagree about it. They are one terminal
 * drawn twice — a row in the sidebar, a live pane beside it — and every way of losing the stored id
 * leaves the panel still showing something while the row says nothing: closing the selected terminal
 * drops it in the database, and selecting another workdir drops it in `selectCheckout`. The row used
 * to answer "nothing is selected" there and the pane answered "the last terminal it opened", so the
 * two named different terminals for the same panel.
 *
 * So the fallback is part of the rule rather than each reader's own: the newest terminal of this
 * workdir. `sessions` arrives in creation order (the backend reads them by rowid) and the pane
 * creates its views in that same order, so "newest" is the one terminal both readings already mean.
 *
 * An id that names no session of this workdir — stale, or a session of a workdir that is no longer
 * open — falls to that newest one too rather than to nothing, because the panel is about to show it.
 */
export function resolveActiveSession(checkout: Checkout | null, activeSessionId: string | null): Session | null {
  if (!checkout) return null;
  return checkout.sessions.find((session) => session.id === activeSessionId) ?? checkout.sessions.at(-1) ?? null;
}

export function getActiveCheckout(state: WorkspaceState): Checkout | null {
  return (
    state.repos.flatMap((repo) => repo.checkouts).find((checkout) => checkout.id === state.activeCheckoutId) ?? null
  );
}
