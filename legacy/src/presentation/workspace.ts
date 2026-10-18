import { computed, onMounted, ref } from "vue";
import { open } from "@tauri-apps/plugin-dialog";
import { createWorkspaceState, getActiveCheckout } from "../domain/workspace";
import type { WorkspaceState } from "../domain/workspace";
import {
  registerFolder,
  restoreWorkspace,
  selectCheckout as persistCheckoutSelection,
  selectSession as persistSessionSelection,
  setDefaultBranch as persistDefaultBranch,
} from "../lib/ipc";
import { useToasts } from "./toasts";

const { pushCause: reportCause } = useToasts();

export function useWorkspaceState() {
  const workspace = ref<WorkspaceState>(createWorkspaceState());
  const activeCheckout = computed(() => getActiveCheckout(workspace.value));
  const isOpening = ref(true);
  const launchCheckoutId = ref<string | null>(null);
  /** The last failure, for callers that want to read it; the toast is what shows it (A.6). */
  const error = ref<string | null>(null);
  const promptingDefaultBranch = new Set<string>();

  function reportError(cause: unknown) {
    error.value = reportCause(cause);
  }

  async function promptForDefaultBranchIfNeeded(force = false) {
    const checkoutId = workspace.value.activeCheckoutId;
    const repo = workspace.value.repos.find((item) => item.checkouts.some((checkout) => checkout.id === checkoutId));
    const checkout = repo?.checkouts.find((item) => item.id === checkoutId);
    if (
      !repo ||
      !checkout ||
      checkout.isMissing ||
      repo.kind !== "git" ||
      (!force && repo.defaultBranch) ||
      promptingDefaultBranch.has(repo.id)
    )
      return;

    promptingDefaultBranch.add(repo.id);
    try {
      const branch = window.prompt(
        `Git could not determine the default branch for “${repo.name}”. Enter the branch to use (for example, trunk):`,
        "",
      );
      if (branch === null || !branch.trim()) return;
      workspace.value = await persistDefaultBranch(repo.id, branch);
    } catch (cause) {
      reportError(cause);
    } finally {
      promptingDefaultBranch.delete(repo.id);
    }
  }

  onMounted(async () => {
    try {
      workspace.value = await restoreWorkspace();
      const homeCheckout = workspace.value.repos
        .flatMap((repo) => repo.checkouts)
        .find((checkout) => checkout.id === workspace.value.homeCheckoutId);
      if (homeCheckout && homeCheckout.sessions.length === 0) launchCheckoutId.value = homeCheckout.id;
    } catch (cause) {
      reportError(cause);
    } finally {
      isOpening.value = false;
    }
    await promptForDefaultBranchIfNeeded();
  });

  async function chooseFolder() {
    error.value = null;
    isOpening.value = true;
    try {
      const selectedPath = await open({ directory: true, multiple: false, title: "Open folder" });
      if (typeof selectedPath !== "string") return;

      workspace.value = await registerFolder(selectedPath);
      await promptForDefaultBranchIfNeeded();
    } catch (cause) {
      reportError(cause);
    } finally {
      isOpening.value = false;
    }
  }

  return {
    workspace,
    updateWorkspace: (next: WorkspaceState) => {
      workspace.value = next;
    },
    activeCheckout,
    launchCheckoutId,
    isOpening,
    error,
    chooseFolder,
    /** Opens a folder this machine had before, the way the workdir menu lists it. */
    openPath: async (path: string) => {
      isOpening.value = true;
      try {
        workspace.value = await registerFolder(path);
        await promptForDefaultBranchIfNeeded();
      } catch (cause) {
        reportError(cause);
      } finally {
        isOpening.value = false;
      }
    },
    promptForDefaultBranchIfNeeded,
    selectCheckout: async (checkoutId: string | null) => {
      try {
        workspace.value = await persistCheckoutSelection(checkoutId);
        await promptForDefaultBranchIfNeeded();
      } catch (cause) {
        reportError(cause);
      }
    },
    selectSession: async (sessionId: string | null) => {
      try {
        workspace.value = await persistSessionSelection(sessionId);
        await promptForDefaultBranchIfNeeded();
      } catch (cause) {
        reportError(cause);
      }
    },
  };
}
