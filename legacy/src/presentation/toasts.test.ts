// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useToasts } from "./toasts";

const { toasts, push, pushCause, dismiss, dismissNewest, hold, release } = useToasts();

function clear() {
  for (const toast of [...toasts.value]) dismiss(toast.id);
}

beforeEach(() => {
  vi.useFakeTimers();
  clear();
});

afterEach(() => {
  clear();
  vi.useRealTimers();
});

describe("toast store", () => {
  it("keeps an error for six seconds and an info for three", () => {
    push("Save failed");
    expect(toasts.value).toHaveLength(1);
    vi.advanceTimersByTime(5999);
    expect(toasts.value).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(toasts.value).toHaveLength(0);

    push("Worktree removed", "info");
    vi.advanceTimersByTime(3000);
    expect(toasts.value).toHaveLength(0);
  });

  it("reports a thrown value with the message the panels would have shown", () => {
    pushCause(new Error("no such checkout"));
    pushCause({ code: "permission_denied", message: "denied" });
    pushCause("plain string");

    expect(toasts.value.map((toast) => toast.message)).toEqual(["no such checkout", "denied", "plain string"]);
  });

  it("dismisses by id and the newest one, leaving the rest up", () => {
    push("first");
    const second = push("second");
    const third = push("third");

    dismiss(second);
    expect(toasts.value.map((toast) => toast.message)).toEqual(["first", "third"]);

    dismissNewest();
    expect(toasts.value.map((toast) => toast.message)).toEqual(["first"]);
    dismissNewest();
    expect(toasts.value).toHaveLength(0);
    expect(third).toBeGreaterThan(0);
  });

  it("holds a hovered toast and gives it its full life back on release", () => {
    const id = push("hover me");
    hold(id);
    vi.advanceTimersByTime(60_000);
    expect(toasts.value).toHaveLength(1);

    release(id);
    vi.advanceTimersByTime(6000);
    expect(toasts.value).toHaveLength(0);
  });

  it("caps the stack and refreshes a repeat instead of stacking it", () => {
    for (const message of ["one", "two", "three", "four"]) push(message);
    // The oldest gives way, so a burst cannot take the panel over.
    expect(toasts.value.map((toast) => toast.message)).toEqual(["two", "three", "four"]);

    // A repeat restarts the lifetime of the toast that is already up instead of adding one.
    vi.advanceTimersByTime(4000);
    push("four");
    expect(toasts.value).toHaveLength(3);

    // The other two expire on schedule; the repeated one is still up.
    vi.advanceTimersByTime(2000);
    expect(toasts.value.map((toast) => toast.message)).toEqual(["four"]);
    vi.advanceTimersByTime(4000);
    expect(toasts.value).toHaveLength(0);
  });
});
