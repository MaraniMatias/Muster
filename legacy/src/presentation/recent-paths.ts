import { onMounted, ref } from "vue";
import type { RecentPath } from "../domain/workspace";
import { listRecentPaths } from "../lib/ipc";
import { useToasts } from "./toasts";

const { pushCause: reportCause } = useToasts();

/**
 * The folders this machine has had open, newest first.
 *
 * A module singleton like the toast stack: the backend raises the list on every folder that is
 * registered and only the workdir menu reads it, so nothing has to own it in between. It loads
 * with the app and refreshes each time the menu opens, which is what keeps a folder opened a
 * moment ago already in the list.
 */
const recentPaths = ref<RecentPath[]>([]);
let pending: Promise<void> | null = null;

export function useRecentPaths() {
  async function refresh() {
    pending ??= load();
    await pending;
  }

  async function load() {
    try {
      recentPaths.value = await listRecentPaths();
    } catch (cause) {
      reportCause(cause);
    } finally {
      pending = null;
    }
  }

  onMounted(() => void refresh());

  return { recentPaths, refresh };
}
