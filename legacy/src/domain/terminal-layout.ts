import type { Session } from "./workspace";

export type SplitDirection = "horizontal" | "vertical";

export type TerminalLayoutNode =
  | { kind: "session"; sessionId: string }
  | {
      kind: "split";
      id: string;
      direction: SplitDirection;
      ratio: number;
      first: TerminalLayoutNode;
      second: TerminalLayoutNode;
    };

export interface TerminalLayoutTab {
  id: string;
  root: TerminalLayoutNode;
}

export interface CheckoutTerminalLayout {
  activeTabId: string | null;
  tabs: TerminalLayoutTab[];
  sessionOrder: string[];
}

export function createTerminalLayout(sessions: Session[] = []): CheckoutTerminalLayout {
  const tabs = sessions.map((session) => ({
    id: `layout:${session.id}`,
    root: { kind: "session", sessionId: session.id } as TerminalLayoutNode,
  }));
  return {
    activeTabId: tabs[0]?.id ?? null,
    tabs,
    sessionOrder: sessions.map((session) => session.id),
  };
}

/**
 * Reads a stored layout back, keeping only what the sessions it is read against can account for.
 *
 * This is the defense against a corrupt row, not a reading of an older format: there is no version
 * to compare and no shape older than this one that is read on purpose. A node that names a session
 * that is gone, a duplicate tab id, a split with only one live child, a ratio outside what can be
 * dragged, a scroll target that is not a number — each falls back rather than throwing, because a
 * row that cannot be parsed must not cost the person the terminal they are looking at.
 */
export function restoreTerminalLayout(value: unknown, sessions: Session[]): CheckoutTerminalLayout {
  const validIds = new Set(sessions.map((session) => session.id));
  const seen = new Set<string>();
  const raw = value && typeof value === "object" ? (value as Partial<CheckoutTerminalLayout>) : {};
  const tabs: TerminalLayoutTab[] = [];

  function restoreNode(node: unknown): TerminalLayoutNode | null {
    if (!node || typeof node !== "object") return null;
    const value = node as Partial<TerminalLayoutNode>;
    if (value.kind === "session") {
      const sessionId = value.sessionId;
      if (typeof sessionId !== "string" || !validIds.has(sessionId) || seen.has(sessionId)) return null;
      seen.add(sessionId);
      return { kind: "session", sessionId };
    }
    if (value.kind !== "split") return null;
    const split = value as Extract<TerminalLayoutNode, { kind: "split" }>;
    const first = restoreNode(split.first);
    const second = restoreNode(split.second);
    if (!first) return second;
    if (!second) return first;
    const direction = split.direction === "vertical" ? "vertical" : "horizontal";
    const ratio = typeof split.ratio === "number" && Number.isFinite(split.ratio) ? split.ratio : 0.5;
    return {
      kind: "split",
      id: typeof split.id === "string" ? split.id : `split:${tabs.length}`,
      direction,
      ratio: Math.max(0.15, Math.min(0.85, ratio)),
      first,
      second,
    };
  }

  if (Array.isArray(raw.tabs)) {
    for (const rawTab of raw.tabs) {
      if (!rawTab || typeof rawTab !== "object") continue;
      const tab = rawTab as TerminalLayoutTab;
      const root = restoreNode(tab.root);
      if (root) tabs.push({ id: typeof tab.id === "string" ? tab.id : `layout:${firstSessionId(root)}`, root });
    }
  }

  for (const session of sessions) {
    if (seen.has(session.id)) continue;
    const tab = createTerminalLayout([session]).tabs[0];
    tabs.push(tab);
    seen.add(session.id);
  }

  const tabIds = new Set<string>();
  for (const tab of tabs) {
    if (tabIds.has(tab.id)) tab.id = `layout:${firstSessionId(tab.root)}`;
    tabIds.add(tab.id);
  }
  const requestedOrder = Array.isArray(raw.sessionOrder) ? raw.sessionOrder : [];
  const sessionOrder = requestedOrder.filter((id): id is string => typeof id === "string" && validIds.has(id));
  const orderSet = new Set(sessionOrder);
  for (const session of sessions) {
    if (!orderSet.has(session.id)) sessionOrder.push(session.id);
  }
  const activeTabId = tabs.some((tab) => tab.id === raw.activeTabId) ? raw.activeTabId! : (tabs[0]?.id ?? null);
  return { activeTabId, tabs, sessionOrder: [...new Set(sessionOrder)] };
}

/**
 * Collapse whatever was stored to the one session a pane can show.
 *
 * This is not a reading of an older shape. A stored layout is dropped to a single session because
 * that is what this build opens when a terminal pane is mounted, and restoring one never starts a
 * PTY: `restoreTerminalLayout` above is the defensive reader, and every id it cannot account for
 * falls here rather than into a terminal that is not there.
 */
export function normalizeTerminalLayout(
  value: unknown,
  sessions: Session[],
  preferredSessionId?: string | null,
): CheckoutTerminalLayout {
  const restored = restoreTerminalLayout(value, sessions);
  const activeTab = restored.tabs.find((tab) => tab.id === restored.activeTabId);
  const selectedId =
    preferredSessionId && sessions.some((session) => session.id === preferredSessionId)
      ? preferredSessionId
      : activeTab
        ? firstSessionId(activeTab.root)
        : sessions.at(-1)?.id;
  const selected = sessions.find((session) => session.id === selectedId);
  return createTerminalLayout(selected ? [selected] : []);
}

export function addSessionToLayout(
  layout: CheckoutTerminalLayout,
  session: Session,
  split?: { targetSessionId: string; direction: SplitDirection; splitId: string },
): CheckoutTerminalLayout {
  const tabs = layout.tabs.map((tab) => {
    if (!split || !containsSession(tab.root, split.targetSessionId)) return tab;
    return {
      ...tab,
      root: replaceSession(tab.root, split.targetSessionId, {
        kind: "split",
        id: split.splitId,
        direction: split.direction,
        ratio: 0.5,
        first: { kind: "session", sessionId: split.targetSessionId },
        second: { kind: "session", sessionId: session.id },
      }),
    };
  });
  if (!split || !tabs.some((tab) => containsSession(tab.root, split.targetSessionId))) {
    tabs.push({ id: `layout:${session.id}`, root: { kind: "session", sessionId: session.id } });
  }
  const targetIndex = split ? layout.sessionOrder.indexOf(split.targetSessionId) + 1 : -1;
  const sessionOrder = [...layout.sessionOrder];
  sessionOrder.splice(targetIndex < 0 ? sessionOrder.length : targetIndex, 0, session.id);
  return { tabs, sessionOrder, activeTabId: split ? layout.activeTabId : `layout:${session.id}` };
}

export function removeSessionFromLayout(layout: CheckoutTerminalLayout, sessionId: string): CheckoutTerminalLayout {
  const tabs = layout.tabs.flatMap((tab) => {
    const root = removeSession(tab.root, sessionId);
    return root ? [{ ...tab, root }] : [];
  });
  const activeTabId = tabs.some((tab) => tab.id === layout.activeTabId) ? layout.activeTabId : (tabs[0]?.id ?? null);
  return {
    tabs,
    activeTabId,
    sessionOrder: layout.sessionOrder.filter((id) => id !== sessionId),
  };
}

export function reorderSession(
  layout: CheckoutTerminalLayout,
  sessionId: string,
  targetSessionId: string,
): CheckoutTerminalLayout {
  if (sessionId === targetSessionId) return layout;
  const sessionOrder = layout.sessionOrder.filter((id) => id !== sessionId);
  const targetIndex = sessionOrder.indexOf(targetSessionId);
  if (targetIndex < 0) return layout;
  sessionOrder.splice(targetIndex, 0, sessionId);
  return { ...layout, sessionOrder };
}

export function resizeSplit(layout: CheckoutTerminalLayout, splitId: string, ratio: number): CheckoutTerminalLayout {
  const nextRatio = Math.max(0.15, Math.min(0.85, ratio));
  return {
    ...layout,
    tabs: layout.tabs.map((tab) => ({ ...tab, root: resizeNode(tab.root, splitId, nextRatio) })),
  };
}

export function findSessionTab(layout: CheckoutTerminalLayout, sessionId: string): TerminalLayoutTab | undefined {
  return layout.tabs.find((tab) => containsSession(tab.root, sessionId));
}

export function firstSessionId(node: TerminalLayoutNode): string {
  return node.kind === "session" ? node.sessionId : firstSessionId(node.first);
}

function containsSession(node: TerminalLayoutNode, sessionId: string): boolean {
  return node.kind === "session"
    ? node.sessionId === sessionId
    : containsSession(node.first, sessionId) || containsSession(node.second, sessionId);
}

function replaceSession(
  node: TerminalLayoutNode,
  sessionId: string,
  replacement: TerminalLayoutNode,
): TerminalLayoutNode {
  if (node.kind === "session") return node.sessionId === sessionId ? replacement : node;
  return {
    ...node,
    first: replaceSession(node.first, sessionId, replacement),
    second: replaceSession(node.second, sessionId, replacement),
  };
}

function removeSession(node: TerminalLayoutNode, sessionId: string): TerminalLayoutNode | null {
  if (node.kind === "session") return node.sessionId === sessionId ? null : node;
  const first = removeSession(node.first, sessionId);
  const second = removeSession(node.second, sessionId);
  if (!first) return second;
  if (!second) return first;
  return { ...node, first, second };
}

function resizeNode(node: TerminalLayoutNode, splitId: string, ratio: number): TerminalLayoutNode {
  if (node.kind === "session") return node;
  return {
    ...node,
    ratio: node.id === splitId ? ratio : node.ratio,
    first: resizeNode(node.first, splitId, ratio),
    second: resizeNode(node.second, splitId, ratio),
  };
}

/**
 * The order a checkout's terminals are listed in: what the saved layout says, then whatever it says
 * nothing about in the order it arrived.
 *
 * One answer for the sidebar row and for the saved layout, because they are the same list drawn
 * twice. A terminal the layout never heard of — every one that predates the layout, and every one
 * created in a run that has not been saved yet — keeps its place after the ones it does know rather
 * than disappearing, and an id it names that no longer exists is dropped rather than drawn.
 */
export function orderedSessionIds(sessions: Session[], layout: CheckoutTerminalLayout | null | undefined): string[] {
  const byId = new Map(sessions.map((session) => [session.id, session] as const));
  const order: string[] = [];
  for (const id of layout?.sessionOrder ?? []) {
    if (!byId.has(id) || order.includes(id)) continue;
    order.push(id);
  }
  for (const session of sessions) if (!order.includes(session.id)) order.push(session.id);
  return order;
}

/**
 * The order a terminal lands in, given the slot it was dropped into.
 *
 * `order` is the list AS DRAWN and the slot is counted against it **with the dragged terminal
 * already out of the way**, which is what makes one number mean the same thing here, in the sidebar's
 * drop line and in the layout it saves. A slot that counted the dragged row would shift by one
 * whenever the row travelled from below the pointer to above it, which is the half of the drag where
 * it silently drops a terminal in the wrong place.
 *
 * The slot is clamped rather than refused, because the pointer can land past the last row, and an
 * order that does not hold the session simply gets one: a terminal arriving from another worktree is
 * not in this list yet and is inserted, not refused.
 */
export function moveSessionId(order: readonly string[], sessionId: string, index: number): string[] {
  const next = order.filter((id) => id !== sessionId);
  next.splice(Math.max(0, Math.min(next.length, index)), 0, sessionId);
  return next;
}
