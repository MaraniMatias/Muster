import type { SessionType, WorkspaceState } from "./workspace";

export interface WorktreeDefaults {
  location: string;
  defaultBranch: string;
}

export interface CreatedWorktree {
  workspace: WorkspaceState;
  checkoutId: string;
}

export interface ActiveWorktreeSession {
  id: string;
  type: SessionType;
  name: string;
}

export interface WorktreeRemovalInfo {
  checkoutId: string;
  isPrimary: boolean;
  isMissing: boolean;
  branch: string | null;
  dirtyFiles: string[];
  unmergedCommits: number;
  activeSessions: ActiveWorktreeSession[];
  activeAgentSessions: ActiveWorktreeSession[];
}

export interface RemovedWorktree {
  workspace: WorkspaceState;
  warning?: string;
}
