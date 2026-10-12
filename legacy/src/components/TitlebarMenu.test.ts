// @vitest-environment happy-dom
// Test doubles intentionally colocate small component shells and omit production prop defaults.
/* eslint-disable vue/one-component-per-file, vue/require-default-prop */
import { mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import type { TitlebarMenuItem, TitlebarMenuSection } from "../domain/titlebar-menu";
import { WORKDIR_ICONS } from "../presentation/workdir-icons";

// reka-ui is stubbed the way App.test.ts stubs it: the keyboard and the focus handling are the
// library's to get right, so what is under test here is what this component does with a menu.
vi.mock("reka-ui", async () => {
  const { defineComponent, h } = await import("vue");
  const passThrough = (name: string) =>
    defineComponent({
      name,
      setup(_, { attrs, slots }) {
        return () => h("div", attrs, slots.default?.());
      },
    });
  // The search is a real input here, because filtering is what the menu is judged on.
  const filter = defineComponent({
    name: "DropdownMenuFilter",
    props: { modelValue: String, placeholder: String },
    emits: ["update:modelValue"],
    setup(props, { emit }) {
      return () =>
        h("input", {
          class: "muster-menu-search",
          placeholder: props.placeholder,
          value: props.modelValue,
          onInput: (event: Event) => emit("update:modelValue", (event.target as HTMLInputElement).value),
        });
    },
  });
  // A row stands in for a menu item: it takes the attrs it is given and reports a selection.
  const row = defineComponent({
    name: "DropdownMenuItem",
    inheritAttrs: false,
    props: { disabled: Boolean },
    setup(_, { attrs, emit, slots }) {
      return () => h("button", { ...attrs, onClick: () => emit("select") }, slots.default?.());
    },
  });
  // The root takes the open name as a prop, the way reka's does, so the shell's word for it can
  // be read back here.
  const root = defineComponent({
    name: "DropdownMenuRoot",
    props: { open: Boolean },
    setup(_, { slots }) {
      return () => h("div", slots.default?.());
    },
  });
  return {
    DropdownMenuRoot: root,
    DropdownMenuTrigger: passThrough("DropdownMenuTrigger"),
    DropdownMenuContent: passThrough("DropdownMenuContent"),
    DropdownMenuPortal: passThrough("DropdownMenuPortal"),
    DropdownMenuSeparator: passThrough("DropdownMenuSeparator"),
    DropdownMenuFilter: filter,
    DropdownMenuItem: row,
    // A group and its label only wrap the rows, so the stub draws them the way it draws the rest.
    DropdownMenuGroup: passThrough("DropdownMenuGroup"),
    DropdownMenuLabel: passThrough("DropdownMenuLabel"),
  };
});

import TitlebarMenu from "./TitlebarMenu.vue";

function item(id: string, label: string, rest: Partial<TitlebarMenuItem> = {}): TitlebarMenuItem {
  return { id, label, run: vi.fn(), ...rest };
}

function sections(): TitlebarMenuSection[] {
  return [
    {
      kind: "group",
      label: "This Window",
      items: [
        item("checkout:main", "main", { hint: "/Muster", checked: true, title: "/Muster" }),
        item("checkout:gone", "temporary", { hint: "/Muster/.worktrees/temporary", disabled: true }),
      ],
    },
    { kind: "separator" },
    { kind: "list", items: [item("open-directory", "Open directory", { pinned: true })] },
  ];
}

function mountMenu(props: Record<string, unknown> = {}) {
  return mount(TitlebarMenu, {
    props: { label: "Muster", sections: sections(), open: true, searchPlaceholder: "Search workdirs…", ...props },
  });
}

describe("TitlebarMenu", () => {
  it("reads as text, not as a button: the label, nothing else", () => {
    const trigger = mountMenu({ testid: "repo-crumb" }).get('[data-testid="repo-crumb"]');

    expect(trigger.text()).toBe("Muster");
    expect(trigger.attributes("title")).toBe("Muster");
    // No chip, no chevron: the crumb is a line of text that happens to open a list, and the
    // glyph that stands for what it names is a thing it is given, not one it draws.
    expect(trigger.find("svg").exists()).toBe(false);
    expect(trigger.classes()).not.toContain("muster-control");
  });

  it("wears the glyph it is given ahead of the name, and nothing when it is given none", () => {
    const trigger = mountMenu({ testid: "repo-crumb", icon: WORKDIR_ICONS.git }).get('[data-testid="repo-crumb"]');

    // The icon is the one the sidebar gives the same row, so the line is that panel sideways.
    expect(trigger.findComponent(WORKDIR_ICONS.git).exists()).toBe(true);
    // The name is still what the crumb reads as, glyph and all, so the glyph adds nothing to it.
    expect(trigger.text()).toBe("Muster");
    expect(mountMenu({ testid: "repo-crumb" }).get('[data-testid="repo-crumb"]').find("svg").exists()).toBe(false);
  });

  it("marks the current row with a check and nothing else", () => {
    const wrapper = mountMenu();

    expect(wrapper.get('[data-testid="menu-item-checkout:main"]').find("svg").exists()).toBe(true);
    expect(wrapper.get('[data-testid="menu-item-checkout:gone"]').find("svg").exists()).toBe(false);
  });

  it("names the group and keeps the hint out of the way of the label", () => {
    const wrapper = mountMenu();

    expect(wrapper.text()).toContain("This Window");
    expect(wrapper.get('[data-testid="menu-item-checkout:gone"]').text()).toContain("/Muster/.worktrees/temporary");
  });

  it("narrows the rows as the search is typed, and takes a group with nothing left in it", async () => {
    const wrapper = mountMenu();

    await wrapper.get("input").setValue("temporary");

    expect(wrapper.findAll('[data-testid^="menu-item-checkout:"]').map((row) => row.text())).toEqual([
      "temporary/Muster/.worktrees/temporary",
    ]);
    expect(wrapper.text()).toContain("This Window");

    // Only the action at the foot answers this one, so the list above it goes with its header.
    await wrapper.get("input").setValue("directory");

    expect(wrapper.findAll('[data-testid^="menu-item-checkout:"]')).toHaveLength(0);
    expect(wrapper.text()).not.toContain("This Window");
  });

  it("leaves the action at the foot standing while the list is searched", async () => {
    const run = vi.fn();
    const wrapper = mountMenu({
      sections: [
        { kind: "group", label: "This Window", items: [item("checkout:main", "main")] },
        { kind: "list", items: [item("open-directory", "Open directory", { pinned: true, run })] },
      ],
    });

    await wrapper.get("input").setValue("nothing matches this");
    expect(wrapper.findAll("button")).toHaveLength(1);

    await wrapper.get('[data-testid="menu-item-open-directory"]').trigger("click");
    expect(run).toHaveBeenCalledOnce();
  });

  it("says so when the search empties the list", async () => {
    const wrapper = mountMenu({ sections: [{ kind: "group", label: "Worktrees", items: [item("one", "main")] }] });

    await wrapper.get("input").setValue("zzz");

    expect(wrapper.text()).toContain("No matches");
  });

  it("has nothing to report when there is no search and no rows to draw", () => {
    const wrapper = mountMenu({
      searchPlaceholder: undefined,
      sections: [{ kind: "list", items: [item("new-terminal", "New terminal", { pinned: true })] }],
    });

    expect(wrapper.text()).toContain("New terminal");
    expect(wrapper.text()).not.toContain("No matches");
  });

  it("has no search to show when none is asked for", () => {
    const wrapper = mountMenu({ searchPlaceholder: undefined });

    expect(wrapper.find("input").exists()).toBe(false);
  });

  it("forgets the search when it closes, and reports what the shell decides", async () => {
    const wrapper = mountMenu();
    const root = wrapper.findComponent({ name: "DropdownMenuRoot" });

    await wrapper.get("input").setValue("temporary");
    root.vm.$emit("update:open", false);
    await wrapper.vm.$nextTick();
    expect(wrapper.get("input").element.value).toBe("");
    expect(wrapper.emitted("update:open")).toEqual([[false]]);

    // The open name lives in the shell, so the root is told what to do rather than deciding.
    await wrapper.setProps({ open: false });
    expect(root.props("open")).toBe(false);
  });
});
