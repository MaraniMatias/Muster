// @vitest-environment happy-dom
import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import ConfirmDialog from "./ConfirmDialog.vue";

function dialog(overrides: Partial<InstanceType<typeof ConfirmDialog>["$props"]> = {}) {
  return mount(ConfirmDialog, {
    props: {
      open: true,
      title: "Archive worktree",
      message: "No files will be deleted.",
      confirmLabel: "Archive",
      ...overrides,
    },
  });
}

describe("ConfirmDialog", () => {
  it("draws nothing at all while it is closed", () => {
    // A dialog that is not open is not in the tree: an invisible overlay still eats clicks.
    expect(dialog({ open: false }).find('[role="dialog"]').exists()).toBe(false);
  });

  it("names what it does, and says what the action leaves alone", () => {
    const wrapper = dialog();

    // The heading is the dialog's name, so it is what a screen reader announces on open.
    const title = wrapper.get("h2");
    expect(title.text()).toBe("Archive worktree");
    expect(wrapper.get('[role="dialog"]').attributes("aria-labelledby")).toBe(title.attributes("id"));
    expect(wrapper.get('[role="dialog"]').attributes("aria-modal")).toBe("true");
    expect(wrapper.text()).toContain("No files will be deleted.");
    expect(wrapper.get("button:last-of-type").text()).toBe("Archive");
  });

  it("answers both ways, and only closes the question it was asked", async () => {
    const wrapper = dialog();

    await wrapper.findAll("footer button")[0]!.trigger("click");
    await wrapper.findAll("footer button")[1]!.trigger("click");

    // Cancel says no and confirm says yes, and the dialog tells them apart.
    expect(wrapper.emitted("close")).toHaveLength(1);
    expect(wrapper.emitted("confirm")).toHaveLength(1);
  });

  it("paints the confirming button in the failure colour only when the answer is one", () => {
    expect(dialog({ destructive: true }).get("button:last-of-type").classes()).toContain("muster-button-danger");
    expect(dialog().get("button:last-of-type").classes()).not.toContain("muster-button-danger");
  });

  it("opens on the answer that does nothing, and Escape backs out", async () => {
    const wrapper = dialog();

    // Focus opens on Cancel, so a stray Enter cannot answer a question nobody read.
    expect(wrapper.get("button[autofocus]").text()).toBe("Cancel");
    expect(wrapper.emitted("confirm")).toBeUndefined();

    await wrapper.get('[role="dialog"]').trigger("keydown", { key: "Escape" });
    expect(wrapper.emitted("close")).toEqual([[]]);
    expect(wrapper.emitted("confirm")).toBeUndefined();
  });

  it("keeps Tab inside the modal", async () => {
    const wrapper = dialog();
    document.body.append(wrapper.element);
    const [cancel, confirm] = wrapper.findAll("footer button");
    (confirm!.element as HTMLButtonElement).focus();

    await confirm!.trigger("keydown", { key: "Tab" });
    expect(document.activeElement).toBe(cancel!.element);

    (cancel!.element as HTMLButtonElement).focus();
    await cancel!.trigger("keydown", { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(confirm!.element);
    wrapper.unmount();
  });

  it("cannot be answered twice while the action behind it is still running", () => {
    const wrapper = dialog({ busy: true });

    // A second click would archive the same worktree twice, and the dialog is the only
    // thing standing between a double click and a repeated command.
    expect(wrapper.get("button:last-of-type").attributes("disabled")).toBeDefined();
    expect(wrapper.get("button:last-of-type").text()).toBe("Working…");
  });
});
