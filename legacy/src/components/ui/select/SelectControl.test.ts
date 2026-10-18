// @vitest-environment happy-dom
import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import SelectControl from "./SelectControl.vue";

const options = [
  { value: "keep", label: "Keep branch (recommended)" },
  { value: "delete", label: "Delete branch" },
];

function select(overrides: Partial<InstanceType<typeof SelectControl>["$props"]> = {}) {
  return mount(SelectControl, {
    props: { modelValue: "keep", options, label: "Local branch", ...overrides },
  });
}

async function open(wrapper: ReturnType<typeof select>) {
  await wrapper.get('[aria-label="Local branch"]').trigger("pointerdown", { button: 0 });
  await flushPromises();
  return [...document.querySelectorAll<HTMLElement>('[aria-label="Local branch"][role="listbox"] [role="option"]')];
}

describe("SelectControl", () => {
  it("names the choice in the trigger, in the app's own type rather than the system's", async () => {
    const wrapper = select();
    // The closed list still mounts its rows into a hidden fragment, which is how the trigger
    // knows what the current choice is called before anything has been opened.
    await flushPromises();

    // The native control this replaces paints its own popup, which on Linux arrives unstyled;
    // the whole point of this one is that the trigger and the list both come from the app.
    const trigger = wrapper.get('[aria-label="Local branch"]');
    expect(trigger.text()).toContain("Keep branch (recommended)");
    expect(trigger.classes()).toContain("muster-select");
    wrapper.unmount();
  });

  it("lists the choices and reports the one that was picked", async () => {
    const wrapper = select();

    const rows = await open(wrapper);
    expect(rows.map((row) => row.textContent)).toEqual(["Keep branch (recommended)", "Delete branch"]);

    // A row is an option and says which one is current, so a screen reader hears the whole set
    // rather than inferring the size of it from the one row that carries the check.
    expect(rows[0].getAttribute("aria-selected")).toBe("true");
    expect(rows[1].getAttribute("aria-selected")).toBe("false");

    rows[1].dispatchEvent(new Event("pointerup", { bubbles: true }));
    await flushPromises();

    expect(wrapper.emitted("update:modelValue")).toEqual([["delete"]]);
    wrapper.unmount();
  });

  it("keeps Escape on the open list instead of handing it to whatever is behind it", async () => {
    const wrapper = select();

    const rows = await open(wrapper);
    const trigger = wrapper.get('[aria-label="Local branch"]');
    expect(trigger.attributes("aria-expanded")).toBe("true");
    rows[1].dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await flushPromises();

    // Escape says "close this list"; it is the dialog's own Escape that says "close me", and
    // the list is portalled out of the dialog's tree so it cannot answer for it.
    expect(wrapper.emitted("update:modelValue")).toBeUndefined();
    expect(trigger.attributes("aria-expanded")).toBe("false");
    wrapper.unmount();
  });

  it("carries the class its caller passed to the trigger it draws", () => {
    const wrapper = select({ variant: "field" });

    expect(wrapper.get('[aria-label="Local branch"]').classes()).toContain("muster-select-field");
    wrapper.unmount();
  });
});
