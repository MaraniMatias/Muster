/**
 * What a titlebar crumb opens: the list of workdirs, worktrees or sessions it names.
 *
 * The shape is the sidebar this line mirrors, read sideways: a search field, groups of rows
 * with the current one checked, a rule, and the actions at the foot. The rows carry what they do instead of an event name, so a caller
 * builds a menu by describing it and the component only has to draw it.
 */

export interface TitlebarMenuItem {
  /** Stable across renders, so the row keeps its identity while the search filters. */
  id: string;
  label: string;
  /** The dim text at the right edge: the repo a workdir belongs to, its path on disk. */
  hint?: string;
  /** The whole name, for a row whose label is truncated. */
  title?: string;
  /** The one that is current, marked with a check. */
  checked?: boolean;
  /**
   * That this row is one of a set of alternatives, of which exactly one is current (a mode, a
   * density, an alignment. It changes nothing about how the row looks and everything about how it
   * announces itself: a checkmark on its own says "this is true" and leaves a reader with no way to
   * know there were three answers, so these say `menuitemradio` and mean it. A check on a row that
   * only names what is open, like the session a crumb is on, is not one of a set and stays a glyph.
   */
  choice?: boolean;
  /** A workdir whose directory is gone: listed so it says so, and not selectable. */
  disabled?: boolean;
  /** An action at the foot of the menu, which the search leaves alone. */
  pinned?: boolean;
  run(): void;
}

export type TitlebarMenuSection =
  | { kind: "group"; label: string; items: TitlebarMenuItem[] }
  | { kind: "list"; items: TitlebarMenuItem[] }
  | { kind: "separator" };

/**
 * The sections a query leaves standing.
 *
 * An empty query is the whole menu, untouched. Otherwise every word typed has to appear in a
 * row's label or hint, a group with nothing left to show goes with it, and what is left is the
 * rows that match, under the headers they arrived in, with the pinned actions of the foot
 * still there.
 */
export function visibleSections(sections: TitlebarMenuSection[], query: string): TitlebarMenuSection[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return sections;

  const kept: TitlebarMenuSection[] = [];
  for (const section of sections) {
    if (section.kind === "separator") {
      kept.push(section);
    } else {
      const items = section.items.filter((item) => item.pinned || matches(item, words));
      if (items.length) kept.push({ ...section, items });
    }
  }
  return withoutDanglingRules(kept);
}

function matches(item: TitlebarMenuItem, words: string[]): boolean {
  const haystack = `${item.label} ${item.hint ?? ""}`.toLowerCase();
  return words.every((word) => haystack.includes(word));
}

/** A rule with nothing on one of its sides draws an empty line, so it goes instead. */
function withoutDanglingRules(sections: TitlebarMenuSection[]): TitlebarMenuSection[] {
  return sections.filter(
    (section, index) =>
      section.kind !== "separator" ||
      (index > 0 && index < sections.length - 1 && sections[index - 1]!.kind !== "separator"),
  );
}
