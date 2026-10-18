import { ref } from "vue";
import { isIpcError } from "../domain/ipc";

/** How loud a message is. The level drives the accent color and how long the toast lives. */
export type ToastLevel = "error" | "info";

export interface Toast {
  id: number;
  level: ToastLevel;
  message: string;
}

/** An error has to be read and acted on; the other is a note in passing. */
const LIFETIME_MS: Record<ToastLevel, number> = { error: 6000, info: 3000 };
/** A burst of failures must not take the panel over: once the stack is full the oldest goes. */
const MAX_TOASTS = 3;

const toasts = ref<Toast[]>([]);
const timers = new Map<number, number>();
let nextToastId = 1;

/**
 * The one live toast stack.
 *
 * It is a module singleton on purpose (A.6): a message about the app itself is raised from
 * wherever the failure happened, so it cannot be wired down through props and events. The
 * component that draws it reads the same list, so nothing else has to own it either.
 */
export function useToasts() {
  function clearTimer(id: number) {
    const timer = timers.get(id);
    if (timer === undefined) return;
    window.clearTimeout(timer);
    timers.delete(id);
  }

  /** (Re)starts the clock; a repeat calls this on the toast that is already up. */
  function startTimer(toast: Toast) {
    clearTimer(toast.id);
    timers.set(
      toast.id,
      window.setTimeout(() => dismiss(toast.id), LIFETIME_MS[toast.level]),
    );
  }

  function dismiss(id: number) {
    clearTimer(id);
    if (!toasts.value.some((toast) => toast.id === id)) return;
    toasts.value = toasts.value.filter((toast) => toast.id !== id);
  }

  /** The newest one, which is what Escape takes away. */
  function dismissNewest() {
    const newest = toasts.value.at(-1);
    if (newest) dismiss(newest.id);
  }

  /**
   * Raises a message. The same one twice in a row refreshes the toast that is already up
   * instead of stacking a second copy, so a failure that repeats on every scroll settles
   * into one message that keeps its lifetime instead of a wall of duplicates.
   */
  function push(message: string, level: ToastLevel = "error") {
    const newest = toasts.value.at(-1);
    if (newest && newest.message === message && newest.level === level) {
      startTimer(newest);
      return newest.id;
    }
    const toast: Toast = { id: nextToastId++, level, message };
    toasts.value = [...toasts.value, toast].slice(-MAX_TOASTS);
    // Capping the stack can have dropped a toast that still owns a timer.
    for (const id of [...timers.keys()]) {
      if (!toasts.value.some((item) => item.id === id)) clearTimer(id);
    }
    startTimer(toast);
    return toast.id;
  }

  /** Raises whatever was thrown, with the same wording every panel would have given it. */
  function pushCause(cause: unknown, level: ToastLevel = "error") {
    const message = isIpcError(cause) ? cause.message : cause instanceof Error ? cause.message : String(cause);
    push(message, level);
    return message;
  }

  /** A hovered toast waits: the pointer is on it, so it is being read. */
  function hold(id: number) {
    clearTimer(id);
  }

  /** Leaving it gives the message its full lifetime again. */
  function release(id: number) {
    const toast = toasts.value.find((item) => item.id === id);
    if (toast && !timers.has(id)) startTimer(toast);
  }

  return { toasts, push, pushCause, dismiss, dismissNewest, hold, release };
}
