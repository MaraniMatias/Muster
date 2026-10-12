import { onScopeDispose } from "vue";
import { AGENT_APP } from "../domain/agent";
import type { AgentRelocation } from "../domain/agent";
import { checkoutForWorkingDirectory, type TerminalSessionStatus, type WorkspaceState } from "../domain/workspace";

const MANUAL_DIRECTORY_CHANGE_TIMEOUT_MS = 15_000;
type Selection = Pick<WorkspaceState, "activeCheckoutId" | "activeSessionId">;
type MoveResult = { moved: boolean; directoryChange: "not-requested" | "written" | "not-written" };
type ManualChange = {
  sourceCheckoutId: string;
  targetCheckoutId: string;
  requestedAt: number;
  operations: number;
  phase: "moving" | "awaiting-target";
  targetObserved: boolean;
  timeoutExpired: boolean;
  timeout: ReturnType<typeof globalThis.setTimeout> | 0;
};

interface Adapters {
  getWorkspace(): WorkspaceState;
  getFollowMode(): "off" | "cd" | "agent" | "both";
  terminalShowsSession(checkoutId: string, terminalId: string, agentSessionId: string): boolean;
  shouldFollowWindow(sessionId: string, fromCheckoutId: string): boolean;
  moveSession(
    sessionId: string,
    checkoutId: string,
    index: number,
    changeDirectory: boolean,
    selectTarget: boolean,
  ): Promise<MoveResult | undefined>;
  followMovedTerminal(sessionId: string, checkoutId: string, isCurrent: () => boolean): Promise<void>;
  onError(cause: unknown): void;
}

/** Coordinates manual and detected terminal moves for one App scope. */
export function useTerminalMovement(adapters: Adapters) {
  const moving = new Map<string, number>();
  const automatic = new Map<string, Promise<boolean>>();
  const manualQueues = new Map<string, Promise<void>>();
  const queued = new Map<string, AgentRelocation>();
  const handled = new Map<string, number>();
  const manualCutoffs = new Map<string, number>();
  const manual = new Map<string, ManualChange>();
  const inactiveTerminals = new Set<string>();
  let navigationRevision = 0;
  let navigationSelection: Selection | null = null;
  let disposed = false;

  const clearTimeoutFor = (change: ManualChange) => globalThis.clearTimeout(change.timeout);
  function drain(sessionId: string) {
    if (disposed || manual.has(sessionId) || manualQueues.has(sessionId) || moving.has(sessionId)) return;
    const relocation = queued.get(sessionId);
    if (!relocation) return;
    queued.delete(sessionId);
    followAgentRelocation(relocation);
  }
  function clearManual(sessionId: string, expected: ManualChange, resume = true) {
    if (manual.get(sessionId) !== expected) return;
    clearTimeoutFor(expected);
    manual.delete(sessionId);
    if (resume) drain(sessionId);
    else queued.delete(sessionId);
  }
  function queueRelocation(sessionId: string, relocation: AgentRelocation) {
    const current = queued.get(sessionId);
    if (!current || relocation.observedAt > current.observedAt) queued.set(sessionId, relocation);
  }
  function followAgentRelocation(relocation: AgentRelocation) {
    if (disposed || !["agent", "both"].includes(adapters.getFollowMode()) || !Number.isFinite(relocation.observedAt))
      return;
    const { repos } = adapters.getWorkspace();
    const { sessionId, toCheckoutId, observedAt } = relocation;
    const targetRepo = repos.find((repo) => repo.checkouts.some((checkout) => checkout.id === toCheckoutId));
    const target = targetRepo?.checkouts.find((checkout) => checkout.id === toCheckoutId);
    if (!targetRepo || targetRepo.kind !== "git" || !target || target.isMissing) return;
    const matches = repos.flatMap((repo) =>
      repo.checkouts
        .filter((checkout) => !checkout.isMissing)
        .flatMap((checkout) =>
          checkout.sessions
            .filter(
              (session) =>
                !inactiveTerminals.has(session.id) && adapters.terminalShowsSession(checkout.id, session.id, sessionId),
            )
            .map((session) => ({ checkoutId: checkout.id, sessionId: session.id })),
        ),
    );
    if (matches.length !== 1) return;
    const terminal = matches[0]!;
    if (observedAt <= (handled.get(terminal.sessionId) ?? -Infinity)) return;
    const cutoff = manualCutoffs.get(terminal.sessionId);
    if (cutoff !== undefined && observedAt <= cutoff) {
      handled.set(terminal.sessionId, observedAt);
      return;
    }
    const pending = manual.get(terminal.sessionId);
    if (pending) {
      if (observedAt <= pending.requestedAt) handled.set(terminal.sessionId, observedAt);
      else queueRelocation(terminal.sessionId, relocation);
      return;
    }
    if (moving.has(terminal.sessionId) || manualQueues.has(terminal.sessionId)) {
      queueRelocation(terminal.sessionId, relocation);
      return;
    }
    if (terminal.checkoutId === toCheckoutId) {
      handled.set(terminal.sessionId, observedAt);
      return;
    }
    void moveAutomatically(terminal.sessionId, terminal.checkoutId, toCheckoutId, observedAt);
  }
  async function moveAutomatically(
    sessionId: string,
    fromCheckoutId: string,
    toCheckoutId: string,
    observedAt?: number,
  ): Promise<boolean> {
    const workspace = adapters.getWorkspace();
    const sourceExists = workspace.repos.some((repo) =>
      repo.checkouts.some((checkout) => checkout.id === fromCheckoutId),
    );
    const target = workspace.repos.flatMap((repo) => repo.checkouts).find((checkout) => checkout.id === toCheckoutId);
    if (disposed || !sourceExists || !target || target.isMissing || manual.has(sessionId) || moving.has(sessionId))
      return false;
    const follow = adapters.shouldFollowWindow(sessionId, fromCheckoutId);
    const revision = navigationRevision;
    moving.set(sessionId, revision);
    let moved = false;
    const operation = Promise.resolve().then(async () => {
      if (disposed || inactiveTerminals.has(sessionId)) return false;
      moved = (await adapters.moveSession(sessionId, toCheckoutId, 0, false, follow))?.moved ?? false;
      if (disposed || inactiveTerminals.has(sessionId) || !moved) return false;
      if (observedAt !== undefined) handled.set(sessionId, Math.max(observedAt, handled.get(sessionId) ?? -Infinity));
      if (follow && revision === navigationRevision && !manual.has(sessionId)) {
        await adapters.followMovedTerminal(
          sessionId,
          toCheckoutId,
          () =>
            !disposed && !inactiveTerminals.has(sessionId) && !manual.has(sessionId) && revision === navigationRevision,
        );
      }
      return true;
    });
    automatic.set(sessionId, operation);
    try {
      return await operation;
    } catch (cause) {
      if (!disposed) adapters.onError(cause);
      return false;
    } finally {
      if (!disposed) {
        moving.delete(sessionId);
        if (automatic.get(sessionId) === operation) automatic.delete(sessionId);
        const pending = queued.get(sessionId);
        // An identical event received in flight is a duplicate, not an immediate retry.
        // A later observation remains queued and is processed regardless of this attempt's result.
        if (!moved && observedAt !== undefined && pending && pending.observedAt <= observedAt) queued.delete(sessionId);
        drain(sessionId);
      }
    }
  }
  function handleWorkingDirectory(sessionId: string, status: TerminalSessionStatus) {
    if (disposed) return;
    const pending = manual.get(sessionId);
    if (pending) {
      const directory = status.workingDirectory;
      const target = directory
        ? checkoutForWorkingDirectory(adapters.getWorkspace().repos, pending.targetCheckoutId, directory)
        : null;
      if (pending.phase === "moving") {
        if (target?.id === pending.targetCheckoutId) pending.targetObserved = true;
        return;
      }
      if (!directory) return;
      if (target?.id === pending.targetCheckoutId) clearManual(sessionId, pending);
      else {
        const source = checkoutForWorkingDirectory(adapters.getWorkspace().repos, pending.sourceCheckoutId, directory);
        if (source?.id === pending.sourceCheckoutId) return;
        clearManual(sessionId, pending);
      }
    }
    if (
      !["cd", "both"].includes(adapters.getFollowMode()) ||
      !status.workingDirectory ||
      status.foregroundApp === AGENT_APP
    )
      return;
    const source = adapters
      .getWorkspace()
      .repos.flatMap((repo) => repo.checkouts)
      .find((checkout) => checkout.sessions.some((session) => session.id === sessionId));
    const target =
      source && checkoutForWorkingDirectory(adapters.getWorkspace().repos, source.id, status.workingDirectory);
    if (source && target && source.id !== target.id) void moveAutomatically(sessionId, source.id, target.id);
  }
  function onStatus(sessionId: string, status: TerminalSessionStatus | null) {
    if (disposed) return;
    if (status) {
      inactiveTerminals.delete(sessionId);
      handleWorkingDirectory(sessionId, status);
    } else {
      inactiveTerminals.add(sessionId);
      const pending = manual.get(sessionId);
      if (pending) clearManual(sessionId, pending, false);
      manualCutoffs.delete(sessionId);
      queued.delete(sessionId);
      handled.delete(sessionId);
    }
  }
  async function moveManually(
    sessionId: string,
    targetCheckoutId: string,
    index: number,
    selectTarget = true,
  ): Promise<boolean> {
    if (disposed) return false;
    const workspace = adapters.getWorkspace();
    const source = workspace.repos
      .flatMap((repo) => repo.checkouts)
      .find((checkout) => checkout.sessions.some((session) => session.id === sessionId));
    const pending = manual.get(sessionId) ?? {
      sourceCheckoutId: source?.id ?? workspace.activeCheckoutId ?? "",
      targetCheckoutId,
      requestedAt: Date.now(),
      operations: 0,
      phase: "moving" as const,
      targetObserved: false,
      timeoutExpired: false,
      timeout: 0 as const,
    };
    clearTimeoutFor(pending);
    pending.sourceCheckoutId = source?.id ?? pending.sourceCheckoutId;
    pending.targetCheckoutId = targetCheckoutId;
    pending.requestedAt = Date.now();
    pending.operations += 1;
    pending.phase = "moving";
    pending.targetObserved = false;
    pending.timeoutExpired = false;
    pending.timeout = globalThis.setTimeout(() => {
      if (disposed) return;
      if (pending.phase === "moving") pending.timeoutExpired = true;
      else clearManual(sessionId, pending);
    }, MANUAL_DIRECTORY_CHANGE_TIMEOUT_MS);
    manualCutoffs.set(sessionId, pending.requestedAt);
    const prior = queued.get(sessionId);
    if (prior && prior.observedAt <= pending.requestedAt) queued.delete(sessionId);
    manual.set(sessionId, pending);
    const previous = manualQueues.get(sessionId) ?? Promise.resolve();
    const move = previous.then(async () => {
      const inFlight = automatic.get(sessionId);
      if (inFlight) await inFlight.catch(() => false);
      if (disposed) return undefined;
      const currentSource = adapters
        .getWorkspace()
        .repos.flatMap((repo) => repo.checkouts)
        .find((checkout) => checkout.sessions.some((session) => session.id === sessionId));
      if (currentSource && manual.get(sessionId) === pending) pending.sourceCheckoutId = currentSource.id;
      return adapters.moveSession(sessionId, targetCheckoutId, index, true, selectTarget);
    });
    const queuedMove = move.then(
      () => undefined,
      () => undefined,
    );
    manualQueues.set(sessionId, queuedMove);
    let result: MoveResult | undefined;
    try {
      result = await move;
      return result?.moved ?? false;
    } finally {
      if (!disposed) {
        if (manual.get(sessionId) === pending) {
          pending.operations -= 1;
          if (pending.operations === 0) {
            if (
              result?.moved &&
              result.directoryChange === "written" &&
              !pending.targetObserved &&
              !pending.timeoutExpired
            )
              pending.phase = "awaiting-target";
            else clearManual(sessionId, pending);
          }
        }
        if (manualQueues.get(sessionId) === queuedMove) manualQueues.delete(sessionId);
        drain(sessionId);
      }
    }
  }
  function workspaceUpdateSelection(
    pendingShell: { token: number; revision: number } | null,
    shellToken?: number,
  ): Selection | null {
    if (disposed) return null;
    const navigationChangedWhileMoving = [...moving.values()].some((revision) => revision !== navigationRevision);
    const staleShell =
      shellToken !== undefined &&
      (!pendingShell || pendingShell.token !== shellToken || pendingShell.revision !== navigationRevision);
    const navigationPending = pendingShell?.revision === navigationRevision;
    return navigationChangedWhileMoving || staleShell || navigationPending ? navigationSelection : null;
  }
  function noteNavigation(selection: Selection) {
    if (disposed) return navigationRevision;
    navigationRevision += 1;
    navigationSelection = selection;
    return navigationRevision;
  }
  onScopeDispose(() => {
    disposed = true;
    for (const change of manual.values()) clearTimeoutFor(change);
    manual.clear();
    manualCutoffs.clear();
    queued.clear();
    handled.clear();
    inactiveTerminals.clear();
    automatic.clear();
    manualQueues.clear();
    moving.clear();
  });
  return {
    noteNavigation,
    get navigationRevision() {
      return navigationRevision;
    },
    workspaceUpdateSelection,
    onStatus,
    onAgentRelocation: followAgentRelocation,
    moveManually,
  };
}
