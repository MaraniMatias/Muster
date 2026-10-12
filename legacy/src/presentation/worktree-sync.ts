import { listen } from "@tauri-apps/api/event";
import { onScopeDispose } from "vue";
import type { WorkspaceState } from "../domain/workspace";
import { syncWorkspaceRepo } from "../lib/ipc";

/**
 * Brings a worktree somebody else created into the panel.
 *
 * More than one hand adds a worktree: Muster's own dialog, an agent running `git worktree add`
 * in a terminal, a script. The watcher already sees Git register any of them: the write goes to
 * the Git directory the worktrees share, whichever way the worktree was made. But a watcher can
 * only refresh rows that exist, and a worktree nothing has registered yet is not one. So the
 * signal names the repository and this asks the backend to read that one repository's worktree
 * list again, which is the same reading a launch takes.
 *
 * The answer is often `null`: a signal that turned out to be nothing, a worktree removed and
 * already gone. Nothing is applied then, because the workspace in hand is already the truth.
 */
export function useWorktreeSync(
  getWorkspace: () => WorkspaceState,
  applyWorkspace: (next: WorkspaceState) => void,
  onError: (cause: unknown) => void,
): void {
  /** Repositories named again while their previous read is in flight. */
  const pending = new Set<string>();
  const asking = new Set<string>();
  let unlisten: (() => void) | undefined;
  let disposed = false;

  /** One read at a time per repository; if the disk changes during a read, the next answer must
   *  include that change too, not leave the row waiting for a future filesystem event. */
  async function ask(repoId: string): Promise<void> {
    if (asking.has(repoId)) {
      pending.add(repoId);
      return;
    }
    asking.add(repoId);
    do {
      pending.delete(repoId);
      try {
        const next = await syncWorkspaceRepo(repoId);
        if (next && !disposed) applyRepoWorkspace(getWorkspace(), next, repoId, applyWorkspace);
      } catch (cause) {
        // The panel keeps the rows it has, which are the ones it had before the signal. A failed
        // read is a reason to say so, not a reason to take rows away.
        if (!disposed) onError(cause);
      }
    } while (!disposed && pending.has(repoId));
    asking.delete(repoId);
  }

  async function start(): Promise<void> {
    try {
      const dispose = await listen<string[]>("git-worktrees-changed", (event) => {
        if (disposed) return;
        for (const repoId of new Set(event.payload)) void ask(repoId);
      });
      if (disposed) dispose();
      else unlisten = dispose;
    } catch (cause) {
      if (!disposed) onError(cause);
    }
  }

  void start();
  onScopeDispose(() => {
    disposed = true;
    unlisten?.();
  });
}

/** Applies only the repository this event named: an older reply cannot overwrite another repo,
 *  a newer selection, or the rest of the workspace while the user kept working. */
function applyRepoWorkspace(
  current: WorkspaceState,
  next: WorkspaceState,
  repoId: string,
  apply: (state: WorkspaceState) => void,
): void {
  const updatedRepo = next.repos.find((repo) => repo.id === repoId);
  if (!updatedRepo) return;

  const currentRepo = current.repos.find((repo) => repo.id === repoId);
  // The user may have closed this repository while its disk read was in flight. Do not reopen it
  // just because an older reply still contains it.
  if (!currentRepo) return;
  const currentArchived = (current.archivedWorktrees ?? []).filter((checkout) => checkout.repoId === repoId);
  const archivedIds = new Set(currentArchived.map((checkout) => checkout.id));
  const updatedCheckouts = new Map(updatedRepo.checkouts.map((checkout) => [checkout.id, checkout]));
  const checkouts = currentRepo.checkouts.map((checkout) => {
    const updated = updatedCheckouts.get(checkout.id);
    return updated ? { ...checkout, ...updated, sessions: checkout.sessions } : checkout;
  });
  const currentIds = new Set(checkouts.map((checkout) => checkout.id));
  for (const checkout of updatedRepo.checkouts) {
    if (!currentIds.has(checkout.id) && !archivedIds.has(checkout.id)) checkouts.push(checkout);
  }
  // Sync only changes Git membership. Keep current archive choices and other repositories, so a
  // response already in flight cannot undo an archive, restore, selection or newer row.
  const repos = current.repos.map((repo) => (repo.id === repoId ? { ...repo, ...updatedRepo, checkouts } : repo));
  apply({ ...current, repos });
}
