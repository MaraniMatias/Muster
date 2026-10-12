<script setup lang="ts">
/* eslint-disable vue/html-self-closing, vue/html-closing-bracket-newline, vue/html-indent */
import { computed, ref, watch } from "vue";
import type { Checkout, Repo, WorkspaceState } from "../domain/workspace";
import type { WorktreeRemovalInfo } from "../domain/worktree";
import { isIpcError } from "../domain/ipc";
import { handleDialogKeydown } from "../lib/dialog-focus";
import {
  archiveCheckout,
  createWorktree,
  getWorktreeDefaults,
  getWorktreeRemovalInfo,
  removeWorktree,
} from "../lib/ipc";

const props = defineProps<{
  open: boolean;
  mode: "create" | "remove";
  repo: Repo | null;
  checkout: Checkout | null;
}>();

const emit = defineEmits<{
  close: [];
  workspaceUpdated: [workspace: WorkspaceState];
  requestShell: [checkoutId: string];
  warning: [message: string];
}>();

const loading = ref(false);
const error = ref<string | null>(null);
const location = ref("");
const taskName = ref("new-task");
const branch = ref("feature/new-task");
const defaultBranch = ref("");
const branchEdited = ref(false);
const removal = ref<WorktreeRemovalInfo | null>(null);
const dirtyConfirmed = ref(false);
const sessionsConfirmed = ref(false);
const deleteBranch = ref(false);
const isBusy = computed(() => loading.value);
const canRemove = computed(
  () =>
    !!removal.value &&
    removal.value.activeAgentSessions.length === 0 &&
    (removal.value.dirtyFiles.length === 0 || dirtyConfirmed.value) &&
    (removal.value.activeSessions.length === 0 || sessionsConfirmed.value),
);

/**
 * Archiving is the reversible answer, and it is refused while a session runs in the
 * worktree for the same reason removing one is: the process would outlive the row that
 * names it. The dialog says so rather than letting the command refuse behind it.
 */
const archiveBlocked = computed(
  () =>
    !!removal.value &&
    !removal.value.isMissing &&
    (removal.value.activeSessions.length > 0 || removal.value.activeAgentSessions.length > 0),
);

watch(
  () => [props.open, props.mode, props.checkout?.id] as const,
  async ([open]) => {
    if (!open || !props.checkout) return;
    error.value = null;
    loading.value = true;
    removal.value = null;
    dirtyConfirmed.value = false;
    sessionsConfirmed.value = false;
    deleteBranch.value = false;
    try {
      if (props.mode === "create") {
        taskName.value = "new-task";
        branch.value = "feature/new-task";
        branchEdited.value = false;
        await loadDefaults();
      } else {
        removal.value = await getWorktreeRemovalInfo(props.checkout.id);
      }
    } catch (cause) {
      error.value = messageOf(cause);
    } finally {
      loading.value = false;
    }
  },
  { immediate: true },
);

async function loadDefaults() {
  if (!props.checkout || !props.repo) return;
  const defaults = await getWorktreeDefaults(props.checkout.id);
  location.value = defaults.location;
  defaultBranch.value = defaults.defaultBranch;
}

function updateTaskName(value: string) {
  taskName.value = value;
  if (branchEdited.value) return;
  const slug =
    value
      .trim()
      .replace(/[^\p{L}\p{N}_.-]+/gu, "-")
      .replace(/^-+|-+$/g, "")
      .replace(/^\.+/, "") || "new-task";
  branch.value = `feature/${slug}`;
}

async function submitCreate() {
  if (!props.checkout || !taskName.value.trim() || !branch.value.trim() || !location.value.trim()) return;
  loading.value = true;
  error.value = null;
  try {
    const result = await createWorktree(props.checkout.id, taskName.value, branch.value, location.value);
    emit("workspaceUpdated", result.workspace);
    emit("close");
  } catch (cause) {
    error.value = messageOf(cause);
  } finally {
    loading.value = false;
  }
}

async function submitRemove() {
  if (!props.checkout || !removal.value || !canRemove.value) return;
  loading.value = true;
  error.value = null;
  try {
    const result = await removeWorktree(
      props.checkout.id,
      removal.value.dirtyFiles.length > 0 && dirtyConfirmed.value,
      removal.value.dirtyFiles,
      removal.value.activeSessions.map((session) => session.id),
      removal.value.branch,
      removal.value.unmergedCommits,
      deleteBranch.value,
    );
    emit("workspaceUpdated", result.workspace);
    if (result.warning) emit("warning", result.warning);
    emit("close");
  } catch (cause) {
    error.value = messageOf(cause);
    if (props.checkout && isIpcError(cause) && cause.code === "invalid_checkout") {
      try {
        removal.value = await getWorktreeRemovalInfo(props.checkout.id);
        dirtyConfirmed.value = false;
        sessionsConfirmed.value = false;
      } catch {
        // Preserve the removal error; reopening the dialog can refresh a stale checkout.
      }
    }
  } finally {
    loading.value = false;
  }
}

/**
 * Takes the worktree off the panel and keeps it, so the repo row can put it back.
 *
 * Nothing on disk moves: the branch, its commits and its files stay exactly where they
 * are. That is what the dialog has to say, because the row cannot: a cross with a box
 * around it reads as "delete" to anyone who has not read this comment.
 */
async function submitArchive() {
  if (!props.checkout || archiveBlocked.value) return;
  loading.value = true;
  error.value = null;
  try {
    emit("workspaceUpdated", await archiveCheckout(props.checkout.id));
    emit("close");
  } catch (cause) {
    error.value = messageOf(cause);
  } finally {
    loading.value = false;
  }
}

function openShell() {
  if (!props.checkout) return;
  emit("close");
  emit("requestShell", props.checkout.id);
}

function messageOf(cause: unknown): string {
  return isIpcError(cause) ? cause.message : cause instanceof Error ? cause.message : String(cause);
}

function onDialogKeydown(event: KeyboardEvent) {
  handleDialogKeydown(event, () => emit("close"));
}
</script>

<template>
  <div
    v-if="open"
    class="fixed inset-0 z-50 grid place-items-center bg-(--muster-bg-0)/80 p-4"
    @click.self="$emit('close')"
  >
    <section
      role="dialog"
      aria-modal="true"
      tabindex="-1"
      :aria-labelledby="mode === 'create' ? 'worktree-create-title' : 'worktree-remove-title'"
      class="surface-popover w-full max-w-xl p-5"
      @keydown="onDialogKeydown"
    >
      <header class="mb-4 flex items-start justify-between gap-4">
        <div>
          <h2
            :id="mode === 'create' ? 'worktree-create-title' : 'worktree-remove-title'"
            class="text-base font-semibold text-(--muster-text)"
          >
            {{ mode === "create" ? "Create worktree from main" : "Remove or archive worktree" }}
          </h2>
          <p
            class="mt-1 truncate text-xs text-(--muster-text-faint)"
            :title="`${checkout?.branch || checkout?.path} · ${repo?.name}`"
          >
            {{ checkout?.branch || checkout?.path }} · {{ repo?.name }}
          </p>
        </div>
        <button aria-label="Close" class="muster-icon-control" @click="$emit('close')">×</button>
      </header>

      <div v-if="loading && !removal" class="py-6 text-sm text-(--muster-text-secondary)" role="status">
        Checking Git worktree…
      </div>

      <form v-else-if="mode === 'create'" class="space-y-4" @submit.prevent="submitCreate">
        <label class="block text-xs text-(--muster-text-secondary)">
          Task name
          <input
            :value="taskName"
            autofocus
            required
            maxlength="120"
            class="muster-input mt-1.5 w-full"
            @input="updateTaskName(($event.target as HTMLInputElement).value)"
          />
          <span class="mt-1 block text-[0.6875rem] text-(--muster-text-faint)"
            >Used as a directory name; unsafe characters are cleaned by Git service validation.</span
          >
        </label>
        <label class="block text-xs text-(--muster-text-secondary)">
          New branch
          <input v-model="branch" required class="muster-input mt-1.5 w-full" @input="branchEdited = true" />
        </label>
        <p v-if="defaultBranch" class="text-xs text-(--muster-text-faint)">
          Starting point: <code class="text-(--muster-text-secondary)">{{ defaultBranch }}</code> · Worktrees are
          created under <code class="text-(--muster-text-secondary)">{{ location }}</code
          >.
        </p>
        <p v-if="error" role="alert" class="text-sm text-(--muster-danger-fg)">{{ error }}</p>
        <footer class="flex justify-end gap-2 pt-1">
          <button type="button" class="muster-button muster-button-subtle" @click="$emit('close')">Cancel</button>
          <!-- Creating and opening the shell is this dialog's primary action. -->
          <button type="submit" :disabled="isBusy || !defaultBranch" class="muster-button muster-button-tinted">
            {{ isBusy ? "Creating…" : "Create and open shell" }}
          </button>
        </footer>
      </form>

      <div v-else-if="mode === 'remove' && error && !removal" class="space-y-4">
        <p role="alert" class="text-sm text-(--muster-danger-fg)">{{ error }}</p>
        <button type="button" class="muster-button muster-button-subtle" @click="$emit('close')">Close</button>
      </div>

      <div v-else-if="removal" class="space-y-4">
        <!-- Two answers to one question, and they differ in what they leave behind, so the
             dialog says which is which before it offers either. Archive hides the row and
             keeps every file; Delete takes the directory off the disk. -->
        <p class="text-sm text-(--muster-text-secondary)">
          <span class="text-(--muster-text)">Archive</span> takes this worktree off the sidebar and keeps every file, so
          the repo row can bring it back. <span class="text-(--muster-text)">Delete</span> removes its directory from
          disk.
        </p>
        <p
          v-if="removal.isMissing"
          class="border border-(--muster-border) bg-(--muster-bg-1) p-3 text-sm text-(--muster-text-secondary)"
        >
          This checkout is missing. Git will prune its stale worktree metadata and remove it from Muster.
        </p>
        <div v-if="removal.dirtyFiles.length" class="border border-(--muster-border) bg-(--muster-bg-1) p-3">
          <p class="text-sm text-(--muster-text)">
            {{ removal.dirtyFiles.length }} uncommitted change{{ removal.dirtyFiles.length === 1 ? "" : "s" }} will be
            discarded if you continue.
          </p>
          <ul class="mt-2 max-h-24 overflow-y-auto pl-4 text-xs text-(--muster-text-faint)">
            <li v-for="file in removal.dirtyFiles.slice(0, 8)" :key="file" class="truncate" :title="file">
              {{ file }}
            </li>
            <li v-if="removal.dirtyFiles.length > 8">and {{ removal.dirtyFiles.length - 8 }} more…</li>
          </ul>
          <button
            type="button"
            class="muster-button muster-button-ghost muster-button-xs mt-2 underline"
            @click="openShell"
          >
            Open a shell to commit or stash first
          </button>
          <label class="mt-3 flex items-start gap-2 text-xs text-(--muster-text-faint)">
            <input v-model="dirtyConfirmed" type="checkbox" class="muster-check mt-0.5" />
            I understand uncommitted files may be lost; remove anyway.
          </label>
        </div>
        <p
          v-if="removal.unmergedCommits"
          class="border border-(--muster-border) bg-(--muster-bg-1) p-3 text-sm text-(--muster-text)"
        >
          This branch has {{ removal.unmergedCommits }} commit{{ removal.unmergedCommits === 1 ? "" : "s" }} not merged
          into the default branch. Keeping the branch preserves them.
        </p>
        <div v-if="removal.activeSessions.length" class="border border-(--muster-border) bg-(--muster-bg-1) p-3">
          <p class="text-sm text-(--muster-text)">These active sessions will be stopped:</p>
          <ul class="mt-2 space-y-1 text-xs text-(--muster-text-faint)">
            <li v-for="session in removal.activeSessions" :key="session.id">{{ session.name }} · {{ session.type }}</li>
          </ul>
          <label class="mt-3 flex items-start gap-2 text-xs text-(--muster-text-faint)">
            <input v-model="sessionsConfirmed" type="checkbox" class="muster-check mt-0.5" />
            Stop the listed sessions and remove this checkout.
          </label>
        </div>
        <p v-if="archiveBlocked" class="text-xs text-(--muster-warning-fg)">
          Archive stays unavailable while a session runs here: close it first.
        </p>
        <p
          v-if="removal.activeAgentSessions.length"
          role="alert"
          class="bg-(--muster-bg-1) p-3 text-sm text-(--muster-danger-fg)"
        >
          An active agent is using this checkout. Stop the agent before removing it:
          {{ removal.activeAgentSessions.map((session) => session.name).join(", ") }}.
        </p>
        <label v-if="removal.branch" class="flex items-center gap-2 text-xs text-(--muster-text-secondary)">
          <input v-model="deleteBranch" type="checkbox" class="muster-check" />
          Delete branch{{ removal.unmergedCommits ? " and its unmerged commits" : "" }}
        </label>
        <p v-if="error" role="alert" class="text-sm text-(--muster-danger-fg)">{{ error }}</p>
        <footer class="flex items-center gap-2 pt-1">
          <button
            type="button"
            :disabled="isBusy || archiveBlocked"
            class="muster-button muster-button-subtle"
            @click="submitArchive"
          >
            {{ isBusy ? "Archiving…" : "Archive" }}
          </button>
          <span class="flex-1" />
          <button type="button" autofocus class="muster-button muster-button-subtle" @click="$emit('close')">
            Cancel
          </button>
          <!-- Delete reaches the disk, so it alone uses the destructive treatment. -->
          <button type="button" :disabled="isBusy || !canRemove" class="muster-button-danger" @click="submitRemove">
            {{ isBusy ? "Deleting…" : "Delete" }}
          </button>
        </footer>
      </div>
    </section>
  </div>
</template>
