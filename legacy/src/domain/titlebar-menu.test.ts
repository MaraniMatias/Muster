import { describe, expect, it, vi } from "vitest";
import type { TitlebarMenuItem, TitlebarMenuSection } from "./titlebar-menu";
import { visibleSections } from "./titlebar-menu";

function item(id: string, label: string, hint?: string, rest: Partial<TitlebarMenuItem> = {}): TitlebarMenuItem {
  return { id, label, ...(hint !== undefined && { hint }), run: vi.fn(), ...rest };
}

function menu(): TitlebarMenuSection[] {
  return [
    {
      kind: "group",
      label: "This Window",
      items: [item("checkout:main", "main", "/Muster"), item("checkout:api", "api", "/Muster/api")],
    },
    { kind: "group", label: "Recent Projects", items: [item("recent:skills", "skills", "/Trabajo/skills")] },
    { kind: "separator" },
    { kind: "list", items: [item("open-directory", "Open directory", undefined, { pinned: true })] },
  ];
}

function labels(sections: TitlebarMenuSection[]): string[] {
  return sections.flatMap((section) =>
    section.kind === "group" || section.kind === "list" ? section.items.map((entry) => entry.label) : [],
  );
}

describe("visibleSections", () => {
  it("leaves the menu alone when nothing is typed", () => {
    const sections = menu();

    expect(visibleSections(sections, "")).toBe(sections);
    expect(visibleSections(sections, "   ")).toBe(sections);
  });

  it("matches a label or the hint behind it, whatever the case", () => {
    expect(labels(visibleSections(menu(), "API"))).toEqual(["api", "Open directory"]);
    // The repo name is the hint, so it is searchable even when the label is a branch.
    expect(labels(visibleSections(menu(), "muster"))).toEqual(["main", "api", "Open directory"]);
    expect(labels(visibleSections(menu(), "skills"))).toEqual(["skills", "Open directory"]);
  });

  it("takes every word typed, so a label and a hint narrow together", () => {
    expect(labels(visibleSections(menu(), "api muster"))).toEqual(["api", "Open directory"]);
    expect(labels(visibleSections(menu(), "api nothing"))).toEqual(["Open directory"]);
  });

  it("drops a group with nothing left in it", () => {
    const sections = visibleSections(menu(), "skills");

    expect(sections).not.toContainEqual(expect.objectContaining({ label: "This Window" }));
    expect(sections).toContainEqual({ kind: "separator" });
  });

  it("keeps the actions at the foot, which are not part of the list", () => {
    expect(labels(visibleSections(menu(), "zzz"))).toEqual(["Open directory"]);
  });

  it("drops a rule with nothing on one of its sides", () => {
    const sections = visibleSections(
      [
        { kind: "separator" },
        { kind: "group", label: "Nothing here", items: [item("gone", "gone")] },
        { kind: "separator" },
        { kind: "group", label: "Worktrees", items: [item("kept", "kept")] },
        { kind: "separator" },
      ],
      "kept",
    );

    expect(sections).toEqual([
      { kind: "group", label: "Worktrees", items: [expect.objectContaining({ label: "kept" })] },
    ]);
  });

  it("leaves only the pinned actions when nothing else answers the search", () => {
    const sections: TitlebarMenuSection[] = [
      { kind: "group", label: "Terminals", items: [item("zsh", "zsh")] },
      { kind: "separator" },
      { kind: "list", items: [item("keep", "keep", undefined, { pinned: true })] },
    ];

    expect(visibleSections(sections, "api")).toEqual([
      { kind: "list", items: [expect.objectContaining({ label: "keep" })] },
    ]);
  });
});
