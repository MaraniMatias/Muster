import { describe, expect, it } from "vitest";
import type { Session } from "./workspace";
import {
  addSessionToLayout,
  createTerminalLayout,
  findSessionTab,
  firstSessionId,
  normalizeTerminalLayout,
  removeSessionFromLayout,
  reorderSession,
  resizeSplit,
  moveSessionId,
  orderedSessionIds,
  restoreTerminalLayout,
} from "./terminal-layout";

const session = (id: string, checkoutId = "checkout:first"): Session => ({
  id,
  checkoutId,
  type: "shell",
  name: id,
  createdAt: "now",
  status: "inactive",
});

describe("terminal layout", () => {
  it("persists nested splits and restores them without inventing live sessions", () => {
    let layout = createTerminalLayout([session("one")]);
    layout = addSessionToLayout(layout, session("two"), {
      targetSessionId: "one",
      direction: "horizontal",
      splitId: "split:one-two",
    });
    layout = addSessionToLayout(layout, session("three"), {
      targetSessionId: "two",
      direction: "vertical",
      splitId: "split:two-three",
    });

    const restored = restoreTerminalLayout(JSON.parse(JSON.stringify(layout)), [
      session("one"),
      session("two"),
      session("three"),
    ]);

    expect(restored).toEqual(layout);
    expect(restored.tabs).toHaveLength(1);
    expect(restored.tabs[0].root).toMatchObject({
      kind: "split",
      direction: "horizontal",
      second: {
        kind: "split",
        direction: "vertical",
        second: { kind: "session", sessionId: "three" },
      },
    });
  });

  it("drops removed checkout sessions and creates historical tabs for sessions missing in saved layout", () => {
    const layout = createTerminalLayout([session("first"), session("second")]);
    const restored = restoreTerminalLayout(
      { ...layout, tabs: [{ id: "broken", root: { kind: "session", sessionId: "removed" } }] },
      [session("second")],
    );

    expect(restored.tabs.map((tab) => firstSessionId(tab.root))).toEqual(["second"]);
    expect(restored.sessionOrder).toEqual(["second"]);
  });

  it("reorders sessions and collapses a split after one session is removed", () => {
    let layout = addSessionToLayout(createTerminalLayout([session("one")]), session("two"), {
      targetSessionId: "one",
      direction: "vertical",
      splitId: "split:one-two",
    });
    layout = reorderSession(layout, "two", "one");
    expect(layout.sessionOrder).toEqual(["two", "one"]);
    expect(findSessionTab(layout, "two")?.root.kind).toBe("split");

    layout = removeSessionFromLayout(layout, "two");
    expect(layout.tabs).toHaveLength(1);
    expect(layout.tabs[0].root).toEqual({ kind: "session", sessionId: "one" });
    expect(layout.sessionOrder).toEqual(["one"]);
  });

  it("keeps layout state isolated by checkout session membership", () => {
    const first = restoreTerminalLayout(createTerminalLayout([session("same-id", "checkout:first")]), [
      session("same-id", "checkout:first"),
    ]);
    const second = restoreTerminalLayout(createTerminalLayout([session("same-id", "checkout:second")]), [
      session("same-id", "checkout:second"),
    ]);
    expect(first.tabs).toEqual(second.tabs);
    expect(first).not.toBe(second);
  });

  it("resizes one nested split and clamps panes away from zero size", () => {
    let layout = addSessionToLayout(createTerminalLayout([session("one")]), session("two"), {
      targetSessionId: "one",
      direction: "horizontal",
      splitId: "outer",
    });
    layout = addSessionToLayout(layout, session("three"), {
      targetSessionId: "two",
      direction: "vertical",
      splitId: "inner",
    });

    layout = resizeSplit(layout, "inner", 0.91);
    const outer = layout.tabs[0].root;
    expect(outer).toMatchObject({ kind: "split", id: "outer", ratio: 0.5 });
    if (outer.kind !== "split" || outer.second.kind !== "split") throw new Error("nested layout expected");
    expect(outer.second.ratio).toBe(0.85);
  });

  it("normalizes persisted tabs and splits to one selected session", () => {
    let layout = createTerminalLayout([session("one"), session("two")]);
    layout = addSessionToLayout(layout, session("three"), {
      targetSessionId: "two",
      direction: "vertical",
      splitId: "split:legacy",
    });

    const normalized = normalizeTerminalLayout(layout, [session("one"), session("two"), session("three")], "one");

    expect(normalized).toEqual({
      activeTabId: "layout:one",
      tabs: [{ id: "layout:one", root: { kind: "session", sessionId: "one" } }],
      sessionOrder: ["one"],
    });
  });
});

describe("the order a checkout's terminals are listed in", () => {
  const layoutOf = (...ids: string[]) => createTerminalLayout(ids.map((id) => session(id)));

  it("puts what the layout names first and keeps the rest after it", () => {
    // The layout knows two of the three; the one it never saved keeps its own place rather than
    // disappearing from the list.
    expect(orderedSessionIds([session("one"), session("two"), session("three")], layoutOf("three", "one"))).toEqual([
      "three",
      "one",
      "two",
    ]);
    // And a layout that names nothing is not a list of nothing.
    expect(orderedSessionIds([session("one"), session("two")], layoutOf())).toEqual(["one", "two"]);
    expect(orderedSessionIds([session("one")], null)).toEqual(["one"]);
  });

  it("drops an id the checkout no longer has and never repeats one", () => {
    expect(orderedSessionIds([session("one")], layoutOf("one", "gone", "one"))).toEqual(["one"]);
  });

  describe("moveSessionId", () => {
    it("lands the terminal in the slot it was dropped into, counted without itself", () => {
      // [A, B, C] with A out of the way is [B, C]: dropping on the second terminal means slot 1.
      expect(moveSessionId(["one", "two", "three"], "one", 1)).toEqual(["two", "one", "three"]);
      // The same list, the last slot: past B and C is slot 2.
      expect(moveSessionId(["one", "two", "three"], "one", 2)).toEqual(["two", "three", "one"]);
      // And the first, which is the same rule and not a special case.
      expect(moveSessionId(["one", "two", "three"], "three", 0)).toEqual(["three", "one", "two"]);
    });

    it("inserts a terminal that is not in the list yet, and clamps one past the end", () => {
      // A terminal arriving from another worktree is inserted where it was aimed.
      expect(moveSessionId(["one", "three"], "two", 1)).toEqual(["one", "two", "three"]);
      expect(moveSessionId(["one", "two"], "one", 99)).toEqual(["two", "one"]);
      expect(moveSessionId(["one", "two"], "one", -5)).toEqual(["one", "two"]);
    });
  });
});
