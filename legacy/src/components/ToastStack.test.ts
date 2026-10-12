// @vitest-environment happy-dom
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useToasts } from "../presentation/toasts";
import ToastStack from "./ToastStack.vue";

const { toasts, push, dismiss } = useToasts();

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

function stack() {
  return mount(ToastStack, { attachTo: document.body });
}

describe("ToastStack", () => {
  it("draws nothing when no message has been raised", () => {
    const wrapper = stack();

    expect(wrapper.find(".toast-stack").exists()).toBe(false);
    wrapper.unmount();
  });

  it("announces an error assertively and an info politely", async () => {
    const wrapper = stack();

    push("Could not read the index");
    push("Worktree removed", "info");
    await flushPromises();

    const toasts = wrapper.findAll(".toast");
    expect(toasts.map((toast) => toast.attributes("role"))).toEqual(["alert", "status"]);
    expect(toasts.map((toast) => toast.text())).toEqual(["Could not read the index", "Worktree removed"]);
    // The stripe that says which of the two is which is styled off the level class, so the class
    // has to be there: drop the binding and a failure quietly stops being red.
    expect(toasts.map((toast) => toast.classes().find((name) => name.startsWith("toast-")))).toEqual([
      "toast-error",
      "toast-info",
    ]);
    wrapper.unmount();
  });

  it("dismisses the message its own button names", async () => {
    const wrapper = stack();
    push("first");
    push("second");
    await flushPromises();

    const dismissButtons = wrapper.findAll(".toast-dismiss");
    expect(dismissButtons.map((button) => button.attributes("aria-label"))).toEqual([
      "Dismiss: first",
      "Dismiss: second",
    ]);
    await dismissButtons[0]!.trigger("click");

    expect(wrapper.findAll(".toast").map((toast) => toast.text())).toEqual(["second"]);
    wrapper.unmount();
  });

  it("stacks upward from the bottom and expires on its own", async () => {
    const wrapper = stack();
    push("first");
    push("second");
    await flushPromises();

    const container = wrapper.get(".toast-stack");
    expect(container.attributes("style")).toBeUndefined();
    expect(container.classes()).toContain("toast-stack");

    vi.advanceTimersByTime(6000);
    await flushPromises();
    expect(wrapper.findAll(".toast")).toHaveLength(0);
    wrapper.unmount();
  });

  it("takes the newest toast on Escape, and none while a field has the focus", async () => {
    const wrapper = stack();
    push("first");
    push("second");
    await flushPromises();

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    await flushPromises();
    expect(wrapper.findAll(".toast").map((toast) => toast.text())).toEqual(["first"]);

    const search = document.createElement("input");
    document.body.append(search);
    search.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await flushPromises();
    // The field owns its Escape: the toast the user is not looking at survives.
    expect(wrapper.findAll(".toast")).toHaveLength(1);

    search.remove();
    wrapper.unmount();
  });

  it("never takes the focus away from what is being typed", async () => {
    const wrapper = stack();
    const search = document.createElement("input");
    document.body.append(search);
    search.focus();

    push("a message arrived");
    await flushPromises();

    expect(document.activeElement).toBe(search);
    search.remove();
    wrapper.unmount();
  });
});
