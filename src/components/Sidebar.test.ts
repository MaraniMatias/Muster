// @vitest-environment happy-dom
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import type { ArchivedCheckout, Checkout, Repo, Session, TerminalSessionStatus } from "../domain/workspace";
import type { TerminalAgentSession } from "../presentation/agent-sessions";

const mocks = vi.hoisted(() => ({
  getGitCheckoutDiffStats: vi.fn(),
  getGitDiffStats: vi.fn(),
}));

vi.mock("../lib/ipc", () => ({
  getGitCheckoutDiffStats: mocks.getGitCheckoutDiffStats,
  getGitDiffStats: mocks.getGitDiffStats,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => undefined) }));

import Sidebar from "./Sidebar.vue";

function repo(overrides: Partial<Repo> = {}): Repo {
  return {
    id: "repo:test",
    kind: "git",
    name: "test",
    root: "/test",
    checkouts: [],
    createdAt: "now",
    lastOpenedAt: "now",
    ...overrides,
  };
}

function checkout(overrides: Partial<Checkout> = {}): Checkout {
  return {
    id: "checkout:primary",
    repoId: "repo:test",
    path: "/test",
    canonicalPath: "/test",
    isPrimary: true,
    branch: "main",
    changedFiles: 0,
    isMissing: false,
    sessions: [],
    ...overrides,
  };
}

function session(id: string, name: string, checkoutId = "checkout:primary"): Session {
  return { id, type: "shell", checkoutId, name, createdAt: "now", status: "inactive" };
}

/**
 * One row session, as the presentation layer hands it over.
 *
 * OpenCode by default, because that is what every fixture here was before Pi arrived and a test
 * that means Pi now says so in one word. `attention` defaults to what a reader would infer from the
 * two booleans so a fixture only states it when the attention itself is what is under test.
 */
function rowSession(overrides: Partial<TerminalAgentSession> = {}): TerminalAgentSession {
  const running = overrides.running ?? false;
  const awaitingReply = overrides.awaitingReply ?? false;
  return {
    id: "ses_row",
    provider: "opencode",
    terminalId: null,
    title: "Session",
    attention: awaitingReply ? "blocked" : running ? "busy" : "none",
    agent: null,
    running,
    awaitingReply,
    updatedAt: Date.now(),
    ...overrides,
  };
}

// reka hands the list to the body, so a repo's menu is read off the document and not off the panel.
function menuItems(): string[] {
  return [...document.querySelectorAll(".group-menu .menu-item")].map((item) => item.textContent?.trim() ?? "");
}

/** A branch of the shape that used to be cut to a slug: a ticket number every branch shares. */
const LONG_TICKET_BRANCH = "bug/13133933180-copy-id-into-the-lists-that-have-them";

/** The stylesheet the panel is drawn from. */
function sidebarStyles(): string {
  return readFileSync(resolve(process.cwd(), "src/components/Sidebar.vue"), "utf8");
}

/**
 * One rule's own declarations, for anything about a size or a colour.
 *
 * Happy-dom lays nothing out and paints nothing, so those claims are claims about this text, and
 * the rule is read rather than the whole file so a declaration cannot pass by living in some other
 * rule. The selector is matched at the start of a line, because every rule in the block begins at
 * column 0, and has to be followed by a comma or a brace, because `.nm` is a prefix of
 * `.nm-something` and `.workdir-row` of `.workdir-row:hover > ...`.
 */
function rule(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return sidebarStyles().match(new RegExp(`^${escaped}(?=\\s*[,{])[^{]*\\{([^}]*)\\}`, "m"))?.[1] ?? "";
}

async function clickMenuItem(label: string) {
  const item = [...document.querySelectorAll<HTMLElement>(".group-menu .menu-item")].find((row) =>
    row.textContent?.includes(label),
  );
  if (!item) throw new Error(`no menu item "${label}" in [${menuItems().join(", ")}]`);
  item.click();
  await flushPromises();
}

describe("Sidebar workdir rows", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.getGitCheckoutDiffStats.mockResolvedValue({});
    mocks.getGitDiffStats.mockResolvedValue([]);
  });

  it("groups checkouts by repo and ends with Open directory", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });

    expect(wrapper.find("h2").exists()).toBe(false);
    expect(wrapper.find('input[aria-label="Search repositories"]').exists()).toBe(false);
    expect(wrapper.find(".group-heading").exists()).toBe(false);
    expect(wrapper.text()).toContain("Your opened folders will appear here.");
    const openDirectory = wrapper.get('button[aria-label="Open directory"]');
    expect(openDirectory.attributes("title")).toBe("Open directory");
    expect(openDirectory.text()).toBe("Open directory");
    // The panel's own last action, muted and brightening on hover, and separated from the list
    // above it by the hairline the reference draws there.
    expect(openDirectory.classes()).toContain("add-item");
    expect(rule(".add-item")).toContain("color: var(--muster-text-faint);");
    expect(rule(".sep")).toContain("background: var(--muster-border);");
    expect(rule(".sep")).toContain("margin: 0;");
    expect(rule(".sidebar-footer .workdir-row")).toContain("justify-content: center;");
    expect(rule(".sidebar-footer .lbl")).toContain("flex: none;");
    await openDirectory.trigger("click");
    expect(wrapper.emitted("openFolder")).toHaveLength(1);
  });

  it("names the repo root after its branch and keeps worktree actions apart", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                id: "checkout:primary",
                repoId: "repo:test",
                path: "/test",
                canonicalPath: "/test",
                isPrimary: true,
                branch: "main",
                changedFiles: 0,
                isMissing: false,
                sessions: [],
              },
              {
                id: "checkout:feature",
                repoId: "repo:test",
                path: "/test-feature",
                canonicalPath: "/test-feature",
                isPrimary: false,
                branch: "feature",
                changedFiles: 0,
                isMissing: false,
                sessions: [],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:feature",
        activeSessionId: null,
        isOpening: false,
      },
    });

    // The repo is a group: its name and where it lives are the header, and the three dots
    // there are everything done to the repo as a whole.
    expect(wrapper.get(".group-name").text()).toBe("test");
    // The specimen carries this class too — it is what a path is weighed in — so the query is scoped
    // to the header rather than to the panel.
    expect(wrapper.get(".group-heading-text .group-path").text()).toBe("/test");
    // The repo root is named by the branch it is on, like a worktree is, and the branch is
    // never repeated beside it.
    expect(wrapper.get(".workdir-row .nm").text()).toBe("main");
    expect(wrapper.find(".workdir-branch").exists()).toBe(false);
    // Hover says the whole name, which the row cannot fit, and where the checkout lives.
    expect(wrapper.get(".workdir-row.active .workdir-select").attributes("title")).toBe("feature — /test-feature");
    // Exactly one row in the panel wears the selection. The branch that holds the selected terminal
    // gives it up, so `main` above `zsh` stays an ordinary group row instead of a second selection.
    expect(wrapper.findAll(".workdir-row.selected").length).toBe(1);
    // It is never a branch that is merely holding the selected terminal: whichever row carries the
    // selection, the branch above it is an ordinary group row with no edge of its own.
    expect(wrapper.find(".workdir-parent.selected .workdir-child.selected").exists()).toBe(false);

    // The root row carries no action of its own: it is the repo's business, so it lives in
    // the header's menu, and only a worktree row has an action beside it.
    expect(wrapper.get('[data-workdir-checkout="checkout:primary"]').find(".workdir-action").exists()).toBe(false);
    expect(wrapper.find('button[aria-label="Remove or archive worktree main"]').exists()).toBe(false);
    expect(wrapper.find('button[aria-label="Remove or archive worktree feature"]').exists()).toBe(true);
    // A workdir with nothing open still offers a terminal: picking a workdir no longer opens
    // one by itself, so this row is the way in.
    expect(wrapper.find('button[aria-label="New terminal for main"]').exists()).toBe(true);
    expect(wrapper.find('button[aria-label="New terminal for feature"]').exists()).toBe(true);
    expect(wrapper.text()).not.toContain("Plain");

    await wrapper.get(".workdir-row.active .workdir-select").trigger("click");
    await wrapper.get('button[aria-label="Actions for test"]').trigger("click");
    await flushPromises();
    await clickMenuItem("New worktree");
    await wrapper.get('button[aria-label="Remove or archive worktree feature"]').trigger("click");
    await wrapper.get('button[aria-label="New terminal for main"]').trigger("click");

    expect(wrapper.emitted("selectCheckout")).toEqual([["checkout:feature", false]]);
    expect(wrapper.emitted("createWorktree")).toEqual([["checkout:primary"]]);
    expect(wrapper.emitted("removeWorktree")).toEqual([["checkout:feature"]]);
    expect(wrapper.emitted("newTerminal")).toEqual([["checkout:primary"]]);
  });

  it("greys the terminal it moved off once a file is what the panel is showing", async () => {
    // The session stays the checkout's active one while a document is open over it — that is a
    // fact about the checkout, and nothing about opening a file rewrites it. The row used to read
    // its selection off that session alone, so it went on claiming to be what was being read.
    const repos = [
      repo({
        checkouts: [
          {
            ...checkout({ id: "checkout:reader" }),
            sessions: [
              session("session:read", "zsh", "checkout:reader"),
              session("session:other", "zsh", "checkout:reader"),
            ],
          },
          {
            ...checkout({
              id: "checkout:beside",
              path: "/test-beside",
              canonicalPath: "/test-beside",
              isPrimary: false,
              branch: "feature",
            }),
            sessions: [session("session:elsewhere", "zsh", "checkout:beside")],
          },
        ],
      }),
    ];
    const props = {
      repos,
      activeCheckoutId: "checkout:reader",
      activeSessionId: "session:read",
      isOpening: false,
    };
    const wrapper = mount(Sidebar, { props });

    // The terminal on screen wears the accent, and it is the only row in the panel that does.
    expect(wrapper.findAll(".workdir-row.selected")).toHaveLength(1);
    expect(wrapper.get('[data-session-id="session:read"]').classes()).toContain("selected");
    expect(wrapper.findAll(".workdir-row.dimmed")).toHaveLength(0);
    expect(wrapper.get('[data-session-id="session:read"] > .workdir-select').attributes("aria-current")).toBe("page");

    // A file open in the main panel takes the selection with it, and the file wears it on its own
    // row in the inspector beside this one. Nothing here claims it: a row that says "this is what
    // you are reading" while the middle of the window is showing a document is the bug itself.
    await wrapper.setProps({ mainViewKind: "document" });
    expect(wrapper.findAll(".workdir-row.selected")).toHaveLength(0);
    // The row is greyed rather than dropped, because the terminal is still this checkout's and is
    // still on the panel: `active` is what says so, and it is untouched by the view.
    const row = wrapper.get('[data-session-id="session:read"]');
    expect(row.classes()).toContain("dimmed");
    expect(row.classes()).toContain("active");
    expect(row.get(".workdir-select").attributes("aria-current")).toBeUndefined();
    // Its sibling is not the row that was moved off, so it gets neither class.
    expect(wrapper.get('[data-session-id="session:other"]').classes()).not.toContain("dimmed");
    // A branch that holds the dimmed terminal does not take the selection up: the selection did
    // not move within this panel, it left it. Both rows of this checkout stay `active`, which is
    // context and was never the accent.
    expect(wrapper.find(".workdir-parent.selected").exists()).toBe(false);
    expect(wrapper.findAll(".workdir-row.active")).toHaveLength(2);

    // A whole change set is the same claim as a single file: a diff of every changed file has no
    // path, and nothing in here is what is on screen either.
    await wrapper.setProps({ mainViewKind: "diff" });
    expect(wrapper.findAll(".workdir-row.selected")).toHaveLength(0);
    expect(wrapper.get('[data-session-id="session:read"]').classes()).toContain("dimmed");
    expect(wrapper.get('[data-session-id="session:read"] .workdir-select').attributes("aria-current")).toBeUndefined();

    // Back to the terminal and the edge comes back with it. The session was never re-selected in
    // between, which is the point: the rule reads the panel, not the session.
    await wrapper.setProps({ mainViewKind: "terminal" });
    expect(wrapper.findAll(".workdir-row.selected")).toHaveLength(1);
    expect(wrapper.get('[data-session-id="session:read"]').classes()).toContain("selected");
    expect(wrapper.findAll(".workdir-row.dimmed")).toHaveLength(0);
    expect(wrapper.get('[data-session-id="session:read"] .workdir-select').attributes("aria-current")).toBe("page");
    wrapper.unmount();
  });

  it("draws the dimmed row in neutral ink, and leaves its glyph saying what it says", () => {
    // The accent edge with the accent taken out of it, and a neutral surface: blue is what a
    // selection looks like, and nothing in the panel is selected while a file is. There is
    // deliberately no rule for the glyph, for the same reason there is none for a selected row —
    // a rule here would outrank every `.state-*` rule below it and a terminal with a process in it
    // would go grey while still running.
    expect(rule(".workdir-row.dimmed")).toContain("background: var(--muster-el-active);");
    expect(rule(".workdir-row.dimmed")).toContain("box-shadow: inset 2px 0 0 var(--muster-border-strong);");
    expect(rule(".workdir-row.dimmed")).not.toContain("var(--muster-accent)");
    expect(rule(".workdir-row.dimmed .nm")).toContain("color: var(--muster-text-secondary);");
    expect(rule(".workdir-row.dimmed .workdir-icon")).toBe("");
    // The cross sits on the row's own surface, so a dimmed row's is the dimmed one.
    expect(rule(".workdir-row.dimmed > .workdir-close")).toContain("background: var(--muster-el-active);");
  });

  it("keeps the plain name for a git root Git has no branch to name it by", () => {
    // A detached HEAD is the only checkout Git reports without a branch, so its root is "Base".
    const wrapper = mount(Sidebar, {
      props: {
        repos: [repo({ checkouts: [{ ...checkout(), branch: undefined }] })],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });

    expect(wrapper.get(".nm").text()).toBe("Base");
    expect(wrapper.find(".workdir-branch").exists()).toBe(false);
    expect(wrapper.find('button[aria-label="New terminal for Base"]').exists()).toBe(true);
    wrapper.unmount();
  });

  it("gives a plain folder neither worktree action", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:notes",
            kind: "plain",
            name: "notes",
            root: "/notes",
            checkouts: [
              {
                id: "checkout:notes",
                repoId: "repo:notes",
                path: "/notes",
                canonicalPath: "/notes",
                isPrimary: true,
                changedFiles: 0,
                isMissing: false,
                sessions: [],
              },
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });

    // A plain folder is one checkout of its own, so it can be taken off the panel; what it
    // never has is a worktree action, which belongs to Git alone, and so its row carries none
    // at all. Its one way out is the group menu, and nothing else is in that menu.
    expect(wrapper.find(".workdir-parent .workdir-close").exists()).toBe(false);
    expect(wrapper.find("button[aria-label^='Add worktree']").exists()).toBe(false);
    expect(wrapper.find("button[aria-label^='Remove or archive']").exists()).toBe(false);
    expect(wrapper.find("button[aria-label='New terminal for notes']").exists()).toBe(true);

    await wrapper.get('button[aria-label="Actions for notes"]').trigger("click");
    await flushPromises();
    expect(menuItems()).toEqual(["Remove from panel"]);

    await clickMenuItem("Remove from panel");
    expect(wrapper.emitted("closeWorkdir")).toEqual([["checkout:notes"]]);
    wrapper.unmount();
  });

  it("protects Home by checkout identity rather than folder name", async () => {
    const home = repo({
      id: "repo:/home",
      kind: "plain",
      name: "Home",
      root: "/home",
      checkouts: [
        checkout({
          id: "checkout:/home",
          repoId: "repo:/home",
          path: "/home",
          canonicalPath: "/home",
          branch: undefined,
        }),
      ],
    });
    const namedHome = repo({
      id: "repo:/other",
      kind: "plain",
      name: "Home",
      root: "/other",
      checkouts: [
        checkout({
          id: "checkout:/other",
          repoId: "repo:/other",
          path: "/other",
          canonicalPath: "/other",
          branch: undefined,
        }),
      ],
    });
    const wrapper = mount(Sidebar, {
      props: {
        repos: [home, namedHome],
        homeCheckoutId: "checkout:/home",
        activeCheckoutId: "checkout:/home",
        activeSessionId: null,
        isOpening: false,
      },
    });

    const homeRow = wrapper.get('[data-workdir-checkout="checkout:/home"]');
    // Each checkout is a group inside its repo's block, so the repo's own header — and its menu —
    // is the block above it. `closest` alone would stop at the group and find nothing.
    const repoBlockOf = (checkoutId: string) =>
      wrapper.get(`[data-workdir-checkout="${checkoutId}"]`).element.closest(".workdir-checkouts")?.parentElement ??
      null;
    expect(homeRow.get(".nm").text()).toBe("Home");
    // The home checkout is the one folder that is never taken off the panel, so neither its
    // row nor its group is given a way to do it.
    expect(homeRow.find(".workdir-action").exists()).toBe(false);
    expect(repoBlockOf("checkout:/home")?.querySelector("button.group-more")).toBe(null);
    // Two groups are both called "Home", and only the one that is not the home checkout has a
    // menu, so the folder name alone never says which one can be closed.
    const more = repoBlockOf("checkout:/other")?.querySelector<HTMLElement>("button.group-more");
    expect(wrapper.findAll("button.group-more")).toHaveLength(1);
    expect(more?.getAttribute("aria-label")).toBe("Actions for Home");
    expect(wrapper.find('button[aria-label="New terminal for Home"]').exists()).toBe(true);

    more?.click();
    await flushPromises();
    await clickMenuItem("Remove from panel");
    expect(wrapper.emitted("closeWorkdir")).toEqual([["checkout:/other"]]);
    wrapper.unmount();
  });

  it("offers a terminal on every workdir that has a directory, and on none that does not", () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              checkout({ id: "checkout:empty", isPrimary: true, branch: "main" }),
              {
                ...checkout({
                  id: "checkout:busy",
                  path: "/test-busy",
                  canonicalPath: "/test-busy",
                  isPrimary: false,
                  branch: "feature",
                }),
                sessions: [session("session:one", "zsh", "checkout:busy")],
              },
              {
                ...checkout({
                  id: "checkout:gone",
                  path: "/test-gone",
                  canonicalPath: "/test-gone",
                  isPrimary: false,
                  branch: "temporary",
                }),
                isMissing: true,
              },
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });

    // The row is not gated on having terminals open, unlike the mockup: selecting a workdir no
    // longer opens one, so a workdir with nothing running still needs its way in. What does
    // hide it is a directory that is gone, which has no shell to run.
    expect(
      wrapper.findAll('button[aria-label^="New terminal for"]').map((row) => row.attributes("aria-label")),
    ).toEqual(["New terminal for main", "New terminal for feature"]);
    wrapper.unmount();
  });

  it("says in the click whether the row is advertising changes, so the panel can open the diff", async () => {
    mocks.getGitCheckoutDiffStats.mockResolvedValue({
      "checkout:changed": { additions: 12, deletions: 4 },
      "checkout:clean": { additions: 0, deletions: 0 },
    });
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              { ...checkout({ id: "checkout:changed", isPrimary: true, branch: "main" }) },
              {
                ...checkout({
                  id: "checkout:clean",
                  path: "/test-clean",
                  canonicalPath: "/test-clean",
                  isPrimary: false,
                  branch: "clean",
                }),
              },
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });
    await flushPromises();

    const rows = wrapper.findAll(".workdir-parent > .workdir-select");
    await rows[0]!.trigger("click");
    await rows[1]!.trigger("click");

    // The flag travels with the click, taken from the same counts the row paints, so the panel
    // opens the change set exactly when the row is advertising one.
    expect(wrapper.emitted("selectCheckout")).toEqual([
      ["checkout:changed", true],
      ["checkout:clean", false],
    ]);
    wrapper.unmount();
  });

  it("makes the whole branch row the click, and leaves its two controls to themselves", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({
                  id: "checkout:feature",
                  path: "/feature",
                  canonicalPath: "/feature",
                  isPrimary: false,
                  branch: "feature",
                }),
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:primary",
        activeSessionId: null,
        isOpening: false,
      },
    });
    await flushPromises();

    const row = wrapper.get(".workdir-parent");

    // The hover surface is the whole row, so the whole row is the click: the trailing slot with
    // the counts in it is part of the name's own click rather than dead space beside it.
    await row.get(".workdir-end").trigger("click");
    expect(wrapper.emitted("selectCheckout")).toEqual([["checkout:feature", false]]);

    // The name still is the button, and its click still arrives by bubbling up to the row, so the
    // keyboard path is the same one a pointer takes.
    await row.get(".workdir-select").trigger("click");
    expect(wrapper.emitted("selectCheckout")).toHaveLength(2);

    // The chevron and the cross are controls of their own inside the row, and clicking either one
    // is not also clicking what it sits on.
    await row.get(".workdir-fold").trigger("click");
    await row.get(".workdir-close").trigger("click");
    expect(wrapper.emitted("selectCheckout")).toHaveLength(2);
    expect(wrapper.emitted("removeWorktree")).toEqual([["checkout:feature"]]);
    wrapper.unmount();
  });

  it("lists every terminal child, marks the active one and offers a close", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:activity",
            kind: "plain",
            name: "activity",
            root: "/activity",
            checkouts: [
              {
                id: "checkout:activity",
                repoId: "repo:activity",
                path: "/activity",
                canonicalPath: "/activity",
                isPrimary: true,
                changedFiles: 0,
                isMissing: false,
                sessions: [
                  {
                    id: "session:running",
                    type: "shell",
                    checkoutId: "checkout:activity",
                    name: "Running",
                    createdAt: "now",
                    status: "inactive",
                  },
                  {
                    id: "session:exited",
                    type: "shell",
                    checkoutId: "checkout:activity",
                    name: "Exited",
                    createdAt: "now",
                    status: "active",
                  },
                  {
                    id: "session:unknown",
                    type: "shell",
                    checkoutId: "checkout:activity",
                    name: "Unknown",
                    createdAt: "now",
                    status: "active",
                  },
                ],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:activity",
        activeSessionId: "session:exited",
        isOpening: false,
        sessionRuntimeStatuses: { "session:exited": { state: "exited", exitCode: 1 } },
      },
    });

    const sessions = wrapper.findAll('button[aria-label^="Terminal session:"]');
    expect(sessions).toHaveLength(3);
    // A terminal's accessible name is its own name, the group it sits in, and its state when it has
    // one to announce. None of these three has an identified agent session, so none has a state
    // word: inventing one ("Exited") would be a claim the panel had not observed, and the detail
    // beside them is the group, which is what says where each of them lives.
    expect(sessions.map((session) => session.attributes("aria-label"))).toEqual([
      "Terminal session: Running",
      "Terminal session: Exited",
      "Terminal session: Unknown",
    ]);
    expect(sessions[1].attributes("aria-current")).toBe("page");
    // The detail is inline and muted rather than a chip, and the state's colour is on the glyph —
    // so nothing on the row is a second badge saying the same thing.
    expect(sessions.map((session) => (session.find(".dm").exists() ? session.get(".dm").text() : null))).toEqual([
      null,
      null,
      null,
    ]);
    expect(wrapper.find(".agent-chip").exists()).toBe(false);
    expect(wrapper.find(".row-dot").exists()).toBe(false);
    expect(wrapper.find(".workdir-child.state-failed").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("Concurrent");
    expect(wrapper.get(".workdir-checkouts.has-active .nm").text()).toBe("activity");

    expect(wrapper.find('button[aria-label="Close terminal session: Exited"]').exists()).toBe(true);
    await wrapper.get('button[aria-label="Terminal session: Unknown"]').trigger("click");
    await wrapper.get('button[aria-label="Close terminal session: Exited"]').trigger("click");
    expect(wrapper.emitted("selectSession")).toEqual([["session:unknown"]]);
    expect(wrapper.emitted("closeSession")).toEqual([["session:exited"]]);
  });

  it("paints a live terminal red when its last command failed, and green once one is running", async () => {
    const ids = ["session:failed", "session:passed", "session:busy", "session:idle"];
    const statuses = {
      // A command that fails does not end the shell, so the only thing that separates a live shell that
      // failed from one that did not is the exit code the OSC 133 hook read out of `$?`.
      "session:failed": { state: "running", foregroundProcess: false, lastCommandExit: 1 },
      "session:passed": { state: "running", foregroundProcess: false, lastCommandExit: 0 },
      "session:busy": { state: "running", foregroundProcess: true, foregroundApp: "cargo" },
      "session:idle": { state: "running", foregroundProcess: false },
    } as const;
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:tone" }),
                sessions: ids.map((id) => session(id, "zsh", "checkout:tone")),
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:tone",
        activeSessionId: null,
        isOpening: false,
        sessionRuntimeStatuses: { ...statuses },
      },
    });

    const stateOf = (id: string) => {
      const row = wrapper.get(`.workdir-child[data-session-id="${id}"]`);
      return row.classes().find((name) => name.startsWith("state-"));
    };

    expect(stateOf("session:failed")).toBe("state-failed");
    // A command that succeeded leaves nothing running and nothing failed, so the row is simply idle.
    expect(stateOf("session:passed")).toBe("state-idle");
    expect(stateOf("session:busy")).toBe("state-running");
    expect(stateOf("session:idle")).toBe("state-idle");

    // The hook clears the field when the next command starts, and the colour goes with it: red means
    // "the last command you ran failed", not "this shell has ever failed".
    await wrapper.setProps({
      sessionRuntimeStatuses: {
        ...statuses,
        "session:failed": { state: "running", foregroundProcess: true, foregroundApp: "cargo" },
      },
    });
    expect(stateOf("session:failed")).toBe("state-running");
    wrapper.unmount();
  });

  it("names the program in front of the shell, and the shell with its directory when nothing is", () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:apps",
            kind: "plain",
            name: "apps",
            root: "/apps",
            checkouts: [
              {
                id: "checkout:apps",
                repoId: "repo:apps",
                path: "/apps",
                canonicalPath: "/apps",
                isPrimary: true,
                changedFiles: 0,
                isMissing: false,
                sessions: [
                  session("session:agent", "zsh", "checkout:apps"),
                  session("session:editing", "zsh", "checkout:apps"),
                  session("session:idle", "zsh", "checkout:apps"),
                ],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:apps",
        activeSessionId: null,
        isOpening: false,
        sessionRuntimeStatuses: {
          "session:agent": {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            // What a shell writes into a terminal's title when it gets there first. It says
            // where the terminal is, which the row above it already says, and it is longer than
            // the row: naming the row after it put a truncated `someone@host:~/ruta` in the panel.
            terminalTitle: "marani@msi:~/Trabajo/apps",
          },
          "session:editing": { state: "running", foregroundProcess: true, foregroundApp: "nvim" },
          // Running with nothing in front: the resting state, which has no program to name.
          "session:idle": { state: "running", foregroundProcess: false },
        },
      },
    });

    // A row is named after what is running in it, and a shell with nothing in front of it is named
    // by the shell it was opened as beside the directory it is sitting in.
    const names = wrapper.findAll(".workdir-child .nm").map((row) => row.text());
    expect(names.slice(0, 3)).toEqual(["opencode", "nvim", "zsh"]);
    // The agent row is the one thing nobody answered for, so it says the program and the silence
    // side by side: `opencode` is what the row is, `sin sesión` is what could not be observed, and
    // no third thing claims an idle turn nobody reported.
    expect(wrapper.get(".workdir-child .dm").text()).toBe("sin sesión");
    expect(wrapper.get(".workdir-child .workdir-select").attributes("title")).toBe(
      "opencode, sin sesión — Session not identified",
    );
    // The glyph says what the row is and how it is doing, so nothing in front of the name repeats
    // it and nothing carries it twice.
    expect(wrapper.text()).not.toContain("OC |");
    expect(wrapper.find(".agent-chip").exists()).toBe(false);
    expect(wrapper.find(".row-dot").exists()).toBe(false);
    wrapper.unmount();
  });

  it("names an OpenCode terminal after the session its own title names", () => {
    // The one thing that can say which session a terminal has open is the title that terminal's
    // TUI wrote, and this row reads it. So the row is named after the session and not after the
    // process, and the state beside it is that session's.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              { ...checkout({ id: "checkout:titled" }), sessions: [session("session:one", "zsh", "checkout:titled")] },
            ],
          }),
        ],
        activeCheckoutId: "checkout:titled",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:titled": {
            sessions: [
              {
                id: "ses_row_1",
                title: "Copy ids into lists",
                provider: "opencode",
                attention: "busy",
                agent: { label: "coder", color: null, attention: "busy" },
                terminalId: null,
                awaitingReply: false,
                running: true,
                updatedAt: Date.now(),
              },
              {
                id: "ses_row_2",
                title: "An old session from another checkout's checkout",
                provider: "opencode",
                attention: "none",
                agent: null,
                terminalId: null,
                awaitingReply: false,
                running: false,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          "session:one": {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Copy ids into lists",
          },
        },
      },
    });

    const row = wrapper.get(".workdir-child");
    // The session's own name, and only the prefix OpenCode wrote is dropped: the row says what the
    // terminal is doing rather than what program it is running.
    expect(row.get(".nm").text()).toBe("Copy ids into lists");
    // The mode is the detail beside it, and the state is the colour of the glyph: one line, no
    // chip, no badge, no second line. The words are in the accessible name instead.
    expect(row.get(".dm").text()).toBe("coder");
    expect(row.classes()).toContain("state-working");
    expect(row.get(".workdir-icon").classes()).toContain("lucide-loader-circle");
    expect(row.get(".workdir-end-time").text()).toMatch(/^\d+s$/);
    wrapper.unmount();
  });

  describe("a Pi terminal", () => {
    /**
     * One Pi terminal in one worktree, drawn from whatever the runtime and the bridge say.
     *
     * This fixture reports `node` to prove the row's identity does not depend on a process name.
     * That name varies with the runtime and platform; the bridge's terminal id is the authoritative
     * link between a Pi session and its terminal.
     */
    function mountPi(
      row: TerminalAgentSession | null,
      overrides: {
        terminalId?: string;
        status?: TerminalSessionStatus;
        checkoutId?: string;
      } = {},
    ) {
      const checkoutId = overrides.checkoutId ?? "checkout:pi";
      const terminalId = overrides.terminalId ?? "session:pi";
      return mount(Sidebar, {
        props: {
          repos: [
            repo({
              checkouts: [{ ...checkout({ id: checkoutId }), sessions: [session(terminalId, "zsh", checkoutId)] }],
            }),
          ],
          activeCheckoutId: checkoutId,
          activeSessionId: null,
          isOpening: false,
          agentRows: { [checkoutId]: { sessions: row ? [row] : [] } },
          sessionRuntimeStatuses: {
            [terminalId]: overrides.status ?? {
              state: "running",
              foregroundProcess: true,
              foregroundApp: "node",
            },
          },
        },
      });
    }

    it("names the session the bridge reported, not the process the OS calls", () => {
      // Pi writes no `OC | …` title, and this fixture reports `node`; neither the title nor the
      // process name identifies the session. What names it is the terminal the bridge said the
      // session is running in, which is a value Muster minted for that terminal itself.
      const wrapper = mountPi(
        rowSession({
          id: "ses_pi",
          provider: "pi",
          terminalId: "session:pi",
          title: "Review the sidebar",
          running: true,
          attention: "busy",
        }),
      );

      const row = wrapper.get(".workdir-child");
      expect(row.get(".nm").text()).toBe("Review the sidebar");
      // Pi has no agent catalog and no colors, so there is no mode to name and nothing to tint
      // with. The label says which program this is, and the row keeps the accent.
      expect(row.get(".dm").text()).toBe("pi");
      expect(row.classes()).toContain("state-working");
      expect(row.get(".workdir-icon").classes()).toContain("lucide-loader-circle");
      expect(row.classes()).not.toContain("agent-tinted");
      // The clock is the session's own last update, read from the bridge.
      expect(row.get(".workdir-end-time").text()).toMatch(/^\d+s$/);
      wrapper.unmount();
    });

    it.each([
      ["busy", false, "state-working", "lucide-loader-circle", "Working"],
      ["blocked", true, "state-waiting", "lucide-circle-question-mark", "Waiting"],
      ["failed", false, "state-failed", "lucide-sparkles", "failed"],
      ["none", false, "state-idle", "lucide-sparkles", "Idle"],
    ] as const)("draws a Pi session whose attention is %s", (attention, awaitingReply, state, icon, note) => {
      // Every state a row can draw has to work for Pi too, and the attention comes from the session
      // rather than from an agent: Pi never has one, so reading it out of the agent would draw a
      // working turn as idle grey and a failed one as idle rather than in the danger colour.
      const wrapper = mountPi(
        rowSession({
          id: "ses_pi",
          provider: "pi",
          terminalId: "session:pi",
          title: "Review the sidebar",
          running: attention === "busy",
          awaitingReply,
          attention,
        }),
      );

      const row = wrapper.get(".workdir-child");
      expect(row.classes()).toContain(state);
      expect(row.get(".workdir-icon").classes()).toContain(icon);
      // A failure keeps the danger colour and gives up the tint, exactly as an OpenCode row does.
      expect(row.classes()).not.toContain("agent-tinted");
      expect(row.get(".workdir-select").attributes("title")).toContain(note);
      wrapper.unmount();
    });

    it("keeps a disconnected Pi session unavailable rather than calling it idle", () => {
      // The bridge dropped mid-turn: nothing is answering, so whether a reply is owed is unknown.
      // `null` means that, and it is drawn as the fact it is — never as a confirmed quiet row.
      const wrapper = mountPi(
        rowSession({
          id: "ses_pi",
          provider: "pi",
          terminalId: "session:pi",
          title: "Review the sidebar",
          running: true,
          awaitingReply: null,
          attention: "busy",
        }),
      );

      const row = wrapper.get(".workdir-child");
      expect(row.classes()).toContain("state-idle");
      const label = row.get(".workdir-select").attributes("title");
      expect(label).toContain("Session state unavailable");
      expect(label).not.toContain("Idle");
      wrapper.unmount();
    });

    it("draws nothing from another terminal's session when Pi exits and a new one starts", () => {
      // The reported failure this guards: a terminal whose Pi ended, with a second Pi already
      // running in a different terminal of the same worktree. The first row must go back to saying
      // what it knows — a plain terminal — rather than wearing the live session's title and state.
      const checkoutId = "checkout:pi";
      const wrapper = mount(Sidebar, {
        props: {
          repos: [
            repo({
              checkouts: [
                {
                  ...checkout({ id: checkoutId }),
                  sessions: [session("session:pi", "zsh", checkoutId), session("session:other", "zsh", checkoutId)],
                },
              ],
            }),
          ],
          activeCheckoutId: checkoutId,
          activeSessionId: null,
          isOpening: false,
          agentRows: {
            [checkoutId]: {
              sessions: [
                rowSession({
                  id: "ses_live",
                  provider: "pi",
                  terminalId: "session:other",
                  title: "Working elsewhere",
                  running: true,
                  attention: "busy",
                }),
              ],
            },
          },
          sessionRuntimeStatuses: {
            // The terminal the live session is NOT in reports a plain shell: Pi there has exited.
            "session:pi": { state: "running", foregroundProcess: false },
            "session:other": { state: "running", foregroundProcess: true, foregroundApp: "node" },
          },
        },
      });

      const exited = wrapper.get('.workdir-child[data-session-id="session:pi"]');
      expect(exited.get(".nm").text()).toBe("zsh");
      // A plain terminal draws no detail at all, which is the half of the claim that matters: there
      // is no agent line here for the live session's title or state to have leaked into.
      expect(exited.find(".dm").exists()).toBe(false);
      expect(exited.classes()).toContain("state-idle");
      // And the terminal it really is in still speaks for itself.
      const live = wrapper.get('.workdir-child[data-session-id="session:other"]');
      expect(live.get(".nm").text()).toBe("Working elsewhere");
      expect(live.classes()).toContain("state-working");
      wrapper.unmount();
    });

    it("never lends an OpenCode terminal a Pi session that shares its title", () => {
      // The two providers are folded into one list, so one title can be on both sides of it. The
      // title inference belongs to OpenCode alone, and the identity match belongs to Pi alone; a row
      // that let either path reach across would be reading one provider's answer as the other's.
      const wrapper = mount(Sidebar, {
        props: {
          repos: [
            repo({
              checkouts: [
                {
                  ...checkout({ id: "checkout:mix" }),
                  sessions: [
                    session("session:oc", "zsh", "checkout:mix"),
                    session("session:pi", "zsh", "checkout:mix"),
                  ],
                },
              ],
            }),
          ],
          activeCheckoutId: "checkout:mix",
          activeSessionId: null,
          isOpening: false,
          agentRows: {
            "checkout:mix": {
              sessions: [
                rowSession({
                  id: "ses_oc",
                  provider: "opencode",
                  title: "Ship the panel",
                  running: false,
                  awaitingReply: false,
                  attention: "none",
                  agent: { label: "coder", color: null, attention: "none" },
                }),
                rowSession({
                  id: "ses_pi",
                  provider: "pi",
                  terminalId: "session:pi",
                  title: "Ship the panel",
                  running: true,
                  attention: "busy",
                }),
              ],
            },
          },
          sessionRuntimeStatuses: {
            "session:oc": {
              state: "running",
              foregroundProcess: true,
              foregroundApp: "opencode",
              terminalTitle: "OC | Ship the panel",
            },
            "session:pi": { state: "running", foregroundProcess: true, foregroundApp: "node" },
          },
        },
      });

      // The OpenCode terminal keeps its own session's answer, rather than going ambiguous or
      // resolving onto the Pi one that happens to carry the same title.
      const opencodeRow = wrapper.get('.workdir-child[data-session-id="session:oc"]');
      expect(opencodeRow.get(".dm").text()).toBe("coder");
      expect(opencodeRow.classes()).toContain("state-idle");
      // And the Pi terminal is identified by its terminal, so the shared title is not what claims it.
      const piRow = wrapper.get('.workdir-child[data-session-id="session:pi"]');
      expect(piRow.get(".dm").text()).toBe("pi");
      expect(piRow.classes()).toContain("state-working");
      wrapper.unmount();
    });
  });

  it("matches a terminal whose title was cut, which is what a long session title gets", () => {
    // A title longer than 40 characters arrives cut to 37 with an ellipsis, so a real long session
    // title reaches the row as a prefix. A row that refused a genuine cut would draw the program
    // instead of the session for exactly the sessions with the longest names — and the stem here is
    // built from the length rather than typed, because 37 is the whole test.
    const long = "Plan de implementación para la sidebar y sus estados";
    const cut = `${long.slice(0, 37)}…`;
    expect(cut).toHaveLength(38);
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [{ ...checkout({ id: "checkout:cut" }), sessions: [session("s", "zsh", "checkout:cut")] }],
          }),
        ],
        activeCheckoutId: "checkout:cut",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:cut": {
            sessions: [
              {
                id: "ses_row_3",
                title: long,
                provider: "opencode",
                attention: "none",
                agent: { label: "plan", color: null, attention: "none" },
                terminalId: null,
                awaitingReply: false,
                running: false,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          s: {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: `OC | ${cut}`,
          },
        },
      },
    });

    const row = wrapper.get(".workdir-child");
    // The name drawn is the title the terminal carried, which is the session's own name cut; the
    // session it belongs to is found behind it, so the row still knows what it is looking at.
    expect(row.get(".nm").text()).toBe(cut);
    expect(row.get(".dm").text()).toBe("plan");
    wrapper.unmount();
  });

  it("never lends one terminal another terminal's session", () => {
    // The reported case: a worktree holding an old checkout's sessions, one OpenCode terminal, and
    // a headline that names a session this terminal is not showing. Nothing here observed which
    // session this terminal has open, so the row names the program and says the session is unknown
    // — rather than wearing the loudest or the newest session's name and state as its own.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [{ ...checkout({ id: "checkout:loan" }), sessions: [session("only", "zsh", "checkout:loan")] }],
          }),
        ],
        activeCheckoutId: "checkout:loan",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:loan": {
            sessions: [
              {
                id: "ses_row_4",
                title: "Add icon box, tinted chip, dot ring",
                provider: "opencode",
                attention: "none",
                agent: { label: "coder", color: null, attention: "none" },
                terminalId: null,
                awaitingReply: false,
                running: false,
                updatedAt: Date.now(),
              },
              {
                id: "ses_row_5",
                title: "Plan de implementación para la sidebar",
                provider: "opencode",
                attention: "busy",
                agent: { label: "plan", color: null, attention: "busy" },
                terminalId: null,
                awaitingReply: false,
                running: true,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        // OpenCode is in front and wrote no session title: nothing identifies anything.
        sessionRuntimeStatuses: {
          only: { state: "running", foregroundProcess: true, foregroundApp: "opencode" },
        },
      },
    });

    const row = wrapper.get(".workdir-child");
    // Not either of the two session titles, and not a mode borrowed from one of them.
    expect(row.get(".nm").text()).toBe("opencode");
    expect(wrapper.text()).not.toContain("Add icon box");
    expect(wrapper.text()).not.toContain("Plan de implementación");
    // The row says the program and the silence on one line and stops there: the program is already
    // the row's name, so a mode chip saying `OpenCode` would put the same word on the row twice, and
    // nothing here observed a turn to be still. So the glyph stays the agent's and goes grey rather
    // than spinning, and the missing session is said in the one word the reference uses.
    expect(row.get(".dm").text()).toBe("sin sesi\u00f3n");
    expect(row.classes()).toContain("state-idle");
    expect(row.find(".agent-chip").exists()).toBe(false);
    expect(row.find(".row-dot").exists()).toBe(false);
    // And no time: the service's clock belongs to a session, and this row identified none.
    expect(row.find(".workdir-end-time").exists()).toBe(false);
    expect(row.get(".workdir-select").attributes("aria-label")).toBe(
      "Terminal session: opencode, sin sesi\u00f3n \u2014 Session not identified",
    );
    expect(row.get(".workdir-select").attributes("title")).toBe(
      "opencode, sin sesi\u00f3n \u2014 Session not identified",
    );
    wrapper.unmount();
  });

  it("tells a duplicated session name apart from a session it could not find", () => {
    // The candidates are the whole service's sessions, because a terminal's session is not
    // necessarily in the worktree the terminal is filed under — so two sessions sharing a title is a
    // finding, not an edge case (measured: `Humanizer` twice, `Read-only worktree probe` twice). The
    // row draws no state either way, because neither may claim the other, but it must not claim that
    // no session exists: the list holds one, the title just cannot say which of the two.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [{ ...checkout({ id: "checkout:twin" }), sessions: [session("only", "zsh", "checkout:twin")] }],
          }),
        ],
        activeCheckoutId: "checkout:twin",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:twin": {
            sessions: [
              {
                id: "ses_row_6",
                title: "Humanizer",
                provider: "opencode",
                attention: "busy",
                agent: { label: "coder", color: null, attention: "busy" },
                terminalId: null,
                awaitingReply: false,
                running: true,
                updatedAt: Date.now(),
              },
              {
                id: "ses_row_7",
                title: "Humanizer",
                provider: "opencode",
                attention: "none",
                agent: { label: "plan", color: null, attention: "none" },
                terminalId: null,
                awaitingReply: false,
                running: false,
                updatedAt: Date.now() - 60_000,
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          only: {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Humanizer",
          },
        },
      },
    });

    const row = wrapper.get(".workdir-child");
    // Neither of the two sessions' states, and no time: the service's clock belongs to a session this
    // row did not identify.
    expect(row.classes()).toContain("state-idle");
    expect(row.find(".workdir-end-time").exists()).toBe(false);
    expect(row.get(".dm").text()).toBe("varias con este nombre");
    expect(row.get(".workdir-select").attributes("aria-label")).toBe(
      "Terminal session: Humanizer, varias con este nombre \u2014 Several sessions share this name",
    );
    wrapper.unmount();
  });

  it("drops an agent's name when the agent has left the terminal", () => {
    // A PTY title outlives the process that wrote it. OpenCode sets `OC | …`, exits, and the shell
    // takes the terminal back without writing anything, so the status still carries the old string —
    // and every answer the service still holds for that worktree is still in `agentRows`, because
    // the worktree's sessions did not go anywhere. Reading the stale title would put a live
    // session's name and its state on a terminal running something else entirely.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [{ ...checkout({ id: "checkout:gone" }), sessions: [session("only", "zsh", "checkout:gone")] }],
          }),
        ],
        activeCheckoutId: "checkout:gone",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:gone": {
            sessions: [
              {
                id: "ses_row_8",
                title: "Copy ids into lists",
                provider: "opencode",
                attention: "busy",
                agent: { label: "coder", color: null, attention: "busy" },
                terminalId: null,
                awaitingReply: false,
                running: true,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        // `sleep` is what the same terminal reports the moment after OpenCode exits. The title is
        // exactly the one the TUI left behind.
        sessionRuntimeStatuses: {
          only: {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "sleep",
            terminalTitle: "OC | Copy ids into lists",
          },
        },
      },
    });

    const row = wrapper.get(".workdir-child");
    expect(row.get(".nm").text()).toBe("sleep");
    expect(wrapper.text()).not.toContain("Copy ids into lists");
    // Nothing of the agent\'s on this row: the program in front is not an agent\'s, so there is no
    // mode to name and no session to time. What it is instead is a process that is up, which is the
    // one thing in the panel that wears green.
    expect(row.find(".agent-chip").exists()).toBe(false);
    expect(row.find(".dm").exists()).toBe(false);
    expect(row.find(".workdir-end-time").exists()).toBe(false);
    expect(row.classes()).toContain("state-running");
    wrapper.unmount();

    // The same terminal one step later, sitting at a prompt with nothing in front: a terminal glyph,
    // no dot, and nothing said about an agent. Its name is the shell beside the repo, and there is
    // no second fact to put under it.
    const resting = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [{ ...checkout({ id: "checkout:gone" }), sessions: [session("only", "zsh", "checkout:gone")] }],
          }),
        ],
        activeCheckoutId: "checkout:gone",
        activeSessionId: null,
        isOpening: false,
        agentRows: wrapper.props("agentRows")!,
        sessionRuntimeStatuses: {
          only: {
            state: "running",
            foregroundProcess: false,
            terminalTitle: "OC | Copy ids into lists",
          },
        },
      },
    });
    const idle = resting.get(".workdir-child");
    expect(idle.get(".nm").text()).toBe("zsh");
    expect(resting.text()).not.toContain("Copy ids into lists");
    // A terminal at a prompt is idle, and idle is the same grey glyph with no motion and no time.
    expect(idle.classes()).toContain("state-idle");
    expect(idle.find(".workdir-end-time").exists()).toBe(false);
    expect(idle.find(".row-dot").exists()).toBe(false);
    resting.unmount();
  });

  it("keeps a terminal named by a rename when no program is in front of it", () => {
    // A rename is stored as the session's name, and it is the last answer rather than the first, so
    // it shows whenever nothing better does. This is the existing contract and it is not changed
    // here: a terminal with a program in front is named after that program, and one with nothing in
    // front is named by what the person called it.
    const renamed = session("only", "deploy");
    const wrapper = mount(Sidebar, {
      props: {
        repos: [repo({ checkouts: [{ ...checkout({ id: "checkout:renamed" }), sessions: [renamed] }] })],
        activeCheckoutId: "checkout:renamed",
        activeSessionId: null,
        isOpening: false,
        sessionRuntimeStatuses: {
          only: { state: "running", foregroundProcess: false, terminalTitle: "dev@mbp:~/Trabajo/Muster" },
        },
      },
    });

    // The shell prompt is not the name; the name the person gave it is.
    expect(wrapper.get(".workdir-child .nm").text()).toBe("deploy");
    wrapper.unmount();
  });

  it("gives two OpenCode terminals in one worktree the same session and no borrowed one", async () => {
    // Two terminals in one worktree and one session of theirs. Both carry the title, so both are
    // that session: the states match because the evidence does, not because one row copied the other.
    const both = (titles: (string | undefined)[]) => {
      const wrapper = mount(Sidebar, {
        props: {
          repos: [
            repo({
              checkouts: [
                {
                  ...checkout({ id: "checkout:two" }),
                  sessions: [
                    session("one", "zsh", "checkout:two"),
                    session("two", "zsh", "checkout:two"),
                    session("three", "zsh", "checkout:two"),
                  ],
                },
              ],
            }),
          ],
          activeCheckoutId: "checkout:two",
          activeSessionId: null,
          isOpening: false,
          agentRows: {
            "checkout:two": {
              sessions: [
                {
                  id: "ses_row_9",
                  title: "Copy ids into lists",
                  provider: "opencode",
                  attention: "busy",
                  agent: { label: "plan", color: null, attention: "busy" },
                  terminalId: null,
                  awaitingReply: false,
                  running: true,
                  updatedAt: Date.now(),
                },
              ],
            },
          },
          sessionRuntimeStatuses: {
            one: { state: "running", foregroundProcess: true, foregroundApp: "opencode", terminalTitle: titles[0] },
            two: { state: "running", foregroundProcess: true, foregroundApp: "opencode", terminalTitle: titles[1] },
            three: { state: "running", foregroundProcess: false },
          },
        },
      });
      return wrapper;
    };

    const same = both(["OC | Copy ids into lists", "OC | Copy ids into lists"]);
    expect(
      same
        .findAll(".workdir-child .nm")
        .map((row) => row.text())
        .slice(0, 3),
    ).toEqual(["Copy ids into lists", "Copy ids into lists", "zsh"]);
    expect(same.findAll(".workdir-child.state-working")).toHaveLength(2);
    same.unmount();

    // One of them is showing something else: only the one that said so is named. The other keeps the
    // program and says `sin sesión`, because borrowing the session next to it is the claim the whole
    // match exists to refuse.
    const other = both(["OC | Copy ids into lists", undefined]);
    expect(
      other
        .findAll(".workdir-child .nm")
        .map((row) => row.text())
        .slice(0, 3),
    ).toEqual(["Copy ids into lists", "opencode", "zsh"]);
    expect(other.findAll(".workdir-child.state-working")).toHaveLength(1);
    expect(other.findAll(".workdir-child .dm")[1]!.text()).toBe("sin sesi\u00f3n");
    other.unmount();
  });

  it("uses the agent colour on a working glyph and keeps the mode beside the name", () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:agent",
            kind: "plain",
            name: "agent",
            root: "/agent",
            checkouts: [
              {
                ...checkout({ id: "checkout:agent" }),
                sessions: [session("session:one", "zsh", "checkout:agent")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:agent",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:agent": {
            sessions: [
              {
                id: "ses_row_10",
                title: "Copy ids into lists",
                provider: "opencode",
                attention: "busy",
                agent: { label: "plan", color: "#FF966C", attention: "busy" },
                terminalId: null,
                awaitingReply: false,
                running: true,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          "session:one": {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Copy ids into lists",
          },
        },
      },
    });

    // The session's own title is the row's name and the mode is the detail beside it. The spinner
    // still says that a turn is moving, but now wears the agent colour OpenCode reports.
    expect(wrapper.get(".workdir-child .nm").text()).toBe("Copy ids into lists");
    expect(wrapper.get(".workdir-child .dm").text()).toBe("plan");
    const row = wrapper.get(".workdir-child");
    expect(row.classes()).toContain("state-working");
    expect(row.classes()).toContain("agent-tinted");
    expect(row.attributes("style")).toContain("--agent-color: #FF966C");
    expect(row.get(".workdir-icon").classes()).toContain("lucide-loader-circle");
    expect(rule(".state-working.agent-tinted .workdir-icon")).toContain(
      "color: color-mix(in srgb, var(--agent-color) 75%, var(--muster-text-muted));",
    );
    // A colour is not something a screen reader reads, so the state is spelled out where the glyph
    // is not: the accessible name, built from the same `item.state` the colour came from.
    expect(wrapper.get(".workdir-child > .workdir-select").attributes("aria-label")).toBe(
      "Terminal session: Copy ids into lists, plan \u2014 0s \u2014 Working",
    );
    wrapper.unmount();
  });

  it("does not call unreadable candidate state idle or discard its ambiguous title", async () => {
    const checkoutId = "checkout:unknown";
    const candidate: TerminalAgentSession = {
      id: "ses_row_11",
      title: "Unknown",
      provider: "opencode",
      attention: "none",
      agent: null,
      running: false,
      terminalId: null,
      awaitingReply: null,
      updatedAt: 1,
    };
    const wrapper = mount(Sidebar, {
      props: {
        repos: [repo({ checkouts: [checkout({ id: checkoutId, sessions: [session("only", "zsh", checkoutId)] })] })],
        activeCheckoutId: checkoutId,
        activeSessionId: null,
        isOpening: false,
        agentRows: { [checkoutId]: { sessions: [candidate] } },
        sessionRuntimeStatuses: {
          only: { state: "running", foregroundProcess: true, foregroundApp: "opencode", terminalTitle: "OC | Unknown" },
        },
      },
    });
    const label = () => wrapper.get(".workdir-child > .workdir-select").attributes("aria-label");
    expect(label()).toContain("Session state unavailable");
    expect(label()).not.toContain("Idle");
    await wrapper.setProps({
      agentRows: { [checkoutId]: { sessions: [candidate, { ...candidate, awaitingReply: true }] } },
    });
    expect(label()).toContain("Several sessions share this name");
    expect(wrapper.get(".workdir-child").classes()).not.toContain("state-waiting");
    wrapper.unmount();
  });

  it("tints idle and asking agents but keeps failures and other harnesses unchanged", () => {
    const checkoutId = "checkout:agent-colors";
    const sessions = [
      { id: "idle", title: "Idle agent", attention: "none" },
      { id: "waiting", title: "Waiting agent", attention: "blocked" },
      { id: "failed", title: "Failed agent", attention: "failed" },
    ] as const;
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              checkout({
                id: checkoutId,
                sessions: [
                  ...sessions.map(({ id }) => session(`session:${id}`, "zsh", checkoutId)),
                  session("session:nvim", "zsh", checkoutId),
                ],
              }),
            ],
          }),
        ],
        activeCheckoutId: checkoutId,
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          [checkoutId]: {
            sessions: sessions.map(({ id, title, attention }) =>
              rowSession({ id, title, attention, agent: { label: "coder", color: "#4ED6BF", attention } }),
            ),
          },
        },
        sessionRuntimeStatuses: {
          ...Object.fromEntries(
            sessions.map(({ id, title }) => [
              `session:${id}`,
              {
                state: "running",
                foregroundProcess: true,
                foregroundApp: "opencode",
                terminalTitle: `OC | ${title}`,
              },
            ]),
          ),
          "session:nvim": { state: "running", foregroundProcess: true, foregroundApp: "nvim" },
        },
      },
    });

    const row = (id: string) => wrapper.get(`[data-session-id="session:${id}"]`);
    expect(row("idle").classes()).toContain("agent-tinted");
    expect(row("idle").attributes("style")).toContain("--agent-color: #4ED6BF");
    expect(row("idle").get(".workdir-icon").classes()).toContain("lucide-sparkles");
    expect(row("waiting").classes()).toContain("state-waiting");
    // A turn that stopped to ask is the agent at work, not a row going bad: it asks with a question
    // mark and it asks in the agent's own colour, which is what says *which* agent is waiting.
    expect(row("waiting").get(".workdir-icon").classes()).toContain("lucide-circle-question-mark");
    expect(row("waiting").classes()).toContain("agent-tinted");
    expect(row("waiting").attributes("style")).toContain("--agent-color: #4ED6BF");
    // Three classes against two, so the agent's colour wins the warning rule above without either
    // rule having to know about the other.
    expect(rule(".state-waiting.agent-tinted .workdir-icon")).toContain(
      "color: color-mix(in srgb, var(--agent-color) 75%, var(--muster-text-muted));",
    );
    // An agent with no colour of its own never reaches that rule, so the warning is all it has left.
    expect(rule(".state-waiting .workdir-icon")).toContain("color: var(--muster-warning);");
    expect(row("failed").classes()).toContain("state-failed");
    expect(row("failed").classes()).not.toContain("agent-tinted");
    expect(row("nvim").classes()).toContain("state-running");
    expect(row("nvim").classes()).not.toContain("agent-tinted");
    wrapper.unmount();
  });

  it("gives every terminal one line, whatever it is doing", () => {
    // One line per row is the whole shape of the panel, so there is nothing to gate on: an agent
    // row with a mode beside it and a plain row with nothing beside it are the same height, and what
    // changes between them is what they say, never how tall they are. A row with no second fact gets
    // no filler one either \u2014 `No process running` and `Exited` were both claims the panel had not
    // observed, and both are gone.
    const plain = repo({
      id: "repo:plain",
      kind: "plain",
      name: "plain",
      root: "/plain",
      checkouts: [
        {
          ...checkout({ id: "checkout:plain", path: "/plain", branch: undefined }),
          sessions: [
            session("serving", "zsh", "checkout:plain"),
            session("sitting", "zsh", "checkout:plain"),
            session("exited", "zsh", "checkout:plain"),
          ],
        },
      ],
    });

    const rows = mount(Sidebar, {
      props: {
        repos: [plain],
        activeCheckoutId: "checkout:plain",
        activeSessionId: null,
        isOpening: false,
        sessionRuntimeStatuses: {
          serving: { state: "running", foregroundProcess: true, foregroundApp: "pnpm" },
          sitting: { state: "running", foregroundProcess: false },
          exited: { state: "exited", exitCode: 1 },
        },
      },
    })
      .findAll(".workdir-child")
      .filter((row) => row.find("button[aria-label^='Terminal session:']").exists());

    // The names are the session; the detail is the group they sit in, which is what tells two idle
    // shells in one worktree apart. A branch, a chip or a sentence is nowhere on the row.
    expect(rows.map((row) => row.find(".nm").text())).toEqual(["pnpm", "zsh", "zsh"]);
    expect(rows.map((row) => (row.find(".dm").exists() ? row.get(".dm").text() : null))).toEqual([null, null, null]);
    // Nothing about the row is a second line, and nothing is time on a row with no session.
    expect(rows.every((row) => row.find(".workdir-meta").exists() === false)).toBe(true);
    expect(rows.every((row) => row.find(".workdir-end-time").exists() === false)).toBe(true);
    const drawn = rows.map((row) => row.text()).join(" ");
    expect(drawn).not.toContain("No process running");
    expect(drawn).not.toContain("Exited");
    // And the row's own height is the reference's, whatever it is saying.
    expect(rule(".workdir-row")).toContain("height: 26px;");
  });

  it("draws no edge of its own, because the handle that moves the panel is the only divider", () => {
    // A border down the panel's right, next to the five pixels of handle that move it, is one line
    // drawn twice — and a reader cannot tell a divider from a panel edge. What is left is the
    // hairline, which is there only while the pointer is on the handle or a drag is in progress, and
    // the two backgrounds meeting.
    expect(sidebarStyles()).not.toMatch(/class="app-sidebar[^"]*\bborder-/);
    expect(rule(".app-sidebar")).not.toContain("border");
  });

  it("swaps a row's time for its cross in the one slot, so the time is never pushed", () => {
    // The slot is `min-width: 20px`, which is the width of the cross, so the two are the same size
    // and the cross is drawn exactly where the time stood. The cross is out of flow, so it cannot
    // push the name or the row's right edge, and the time is not removed on hover \u2014 it yields, like
    // every other trailing content in the panel, and both are readable at once. A terminal's elapsed
    // time yields to the cross and to nothing else: it is not a change figure, so the reveal below
    // never touches it.
    expect(rule(".workdir-end")).toContain("min-width: 20px;");
    expect(rule(".workdir-end")).toContain("flex: none;");
    expect(rule(".workdir-close")).toContain("position: absolute;");
    expect(rule(".workdir-close")).toContain("width: 20px;");
    // At rest the slot holds what it holds and the cross paints nothing, so no row in the panel
    // carries a band reserved for an action it is not showing.
    expect(rule(".workdir-end")).not.toContain("padding-right");
    expect(sidebarStyles()).not.toContain("padding-right: 32px");
  });

  it("opens the slot into the counts on hover, out of the figures' own box", () => {
    // The figures are one pointer away, so the slot has to be able to grow into them. It grows out
    // of its OWN box — `margin-right` on the figures, never padding on the slot — because the slot
    // also carries a missing-directory note and a terminal's elapsed time, and neither of those is
    // a count that has anything to reveal.
    const hovered = sidebarStyles().match(/\.workdir-row:hover[^{]*\.workdir-diff,[^{]*\{([^}]*)\}/)?.[1] ?? "";
    expect(hovered).toContain("grid-template-columns: 0px 1fr;");
    // The strip is NOT in this rule: only a row that draws a cross has anything standing in the
    // space its figures give up. On every other row those 28px were a gap trailing the numbers with
    // nothing drawn in it, which is what made a hovered repo root look like it was padded wrong.
    expect(hovered).not.toContain("margin-right");
    const stripped =
      sidebarStyles().match(/\.workdir-row\.has-close:hover[^{]*\.workdir-diff,[^{]*\{([^}]*)\}/)?.[1] ?? "";
    expect(stripped).toContain("margin-right: 28px;");
    expect(sidebarStyles()).not.toMatch(/\.workdir-row:hover[^{]*workdir-end[^{]*\{[^}]*padding/);
    expect(sidebarStyles()).not.toMatch(/\.workdir-row:focus-within[^{]*workdir-end[^{]*\{[^}]*padding/);
    // The slot's own minimum is still the cross's width, and it never grows one: nothing is reserved
    // at rest for an action the row is not showing.
    expect(rule(".workdir-end")).toContain("min-width: 20px;");
    expect(rule(".workdir-end")).not.toContain("padding-right");
    expect(sidebarStyles()).not.toContain("padding-right: 32px");
    // The cross is out of flow, so the row never reflows and nothing is pushed along by it.
    expect(rule(".workdir-close")).toContain("position: absolute;");
    expect(rule(".workdir-close")).toContain("right: 8px;");
    // It paints the surface of the row it belongs to, so whatever it does land over, it lands over
    // the row rather than letting text show through it.
    expect(rule(".workdir-close")).toContain("background: var(--muster-bg-1);");
    expect(rule(".workdir-row:hover > .workdir-close")).toContain("background: var(--muster-el-hover);");
    expect(rule(".workdir-row.selected > .workdir-close")).toContain("background: var(--muster-el-selected);");
    // The cross gives no hover fill of its own, so a cross on a row is a glyph and not a cell: the
    // shared `.workdir-action` rule is all it would have, and the row's own rule outranks it.
    expect(rule(".workdir-close:hover")).toBe("");
    // The title is the row's own first flex child and no hover rule touches its padding, so the name
    // gives up width to the figures and nothing else on the row gives up anything.
    expect(sidebarStyles()).not.toMatch(/\.workdir-row:hover[^{]*workdir-select[^{]*\{[^}]*padding/);
    expect(sidebarStyles()).not.toMatch(/\.workdir-row:hover[^{]*\.lbl[^{]*\{[^}]*padding/);
  });

  it("keeps the hovered branch row's cross inside its own slot, in the figures' own box", async () => {
    // The guarantee as a shape in the DOM: the cross is the row's last child and the figures are in
    // the slot immediately before it, inside the same row box, so a cross can only ever be over its
    // own row\u0027s slot \u2014 and the slot is the thing that yields.
    mocks.getGitCheckoutDiffStats.mockResolvedValue({ "checkout:tree": { additions: 1299, deletions: 4 } });
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({
                  id: "checkout:tree",
                  isPrimary: false,
                  branch: LONG_TICKET_BRANCH,
                  path: "/dev/figs/.worktrees/feat/13133933180-copy-id-into-the-lists",
                  canonicalPath: "/dev/figs/.worktrees/feat/13133933180-copy-id-into-the-lists",
                }),
                sessions: [session("child", "zsh", "checkout:tree")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:tree",
        activeSessionId: null,
        isOpening: false,
        sessionRuntimeStatuses: { child: { state: "running", foregroundProcess: true, foregroundApp: "pnpm" } },
      },
    });
    await flushPromises();

    const row = wrapper.get(".workdir-parent");
    // The row says it has a cross, and it does. The class and the button are the same question, so
    // the strip the hover rule opens is standing in exactly the row that draws the thing in it.
    expect(row.classes()).toContain("has-close");
    expect(row.find(".workdir-close").exists()).toBe(true);
    // The slot and the cross are siblings inside the row, in that order, so the cross is drawn over
    // the slot and over nothing else on the row.
    const children = [...row.element.children];
    expect(children.map((child) => child.className)).toEqual([
      "workdir-fold",
      "workdir-select",
      "workdir-end",
      "workdir-action workdir-close",
    ]);
    // The figures are in that slot as two layers \u2014 the bare `+−`, and one pointer away,
    // `+1299 −4`. The mark is the one hidden from a screen reader, because the figures are the
    // row's real content and a reader who never hovers the row still has to be told what changed.
    expect(row.get(".workdir-end .workdir-diff-mark").text()).toBe("+−");
    expect(row.get(".workdir-end .workdir-diff-mark").attributes("aria-hidden")).toBe("true");
    // The mark is the figures with the digits left off, so it is drawn in the figures' own colours
    // rather than in one neutral ink that would claim the row has neither additions nor deletions.
    expect(row.get(".workdir-end .workdir-diff-mark .ad").text()).toBe("+");
    expect(row.get(".workdir-end .workdir-diff-mark .rm").text()).toBe("−");
    expect(row.get(".workdir-end .workdir-diff-nums").text()).toBe("+1299\u22124");
    expect(row.get(".workdir-close").attributes("aria-label")).toBe(
      "Remove or archive worktree bug/13133933180-copy-id-into-the-lists-that-have-them",
    );
    // And the title is the row\u0027s own first flex child, which no hover rule touches its padding
    // on: the yield is the figures\u0027 own margin, spent out of the figures\u0027 box and nowhere else.
    const title = row.get(".workdir-select");
    expect(rule(".workdir-end")).not.toContain("padding-right");
    expect(rule(".workdir-end")).not.toContain("margin-right");
    expect(sidebarStyles()).not.toMatch(/\.workdir-row:hover[^{]*workdir-select[^{]*\{[^}]*padding/);
    const atRest = title.element.getBoundingClientRect().width;
    await title.trigger("mouseenter");
    expect(title.element.getBoundingClientRect().width).toBe(atRest);
    wrapper.unmount();
  });

  it("gives a row with no cross of its own no strip to give up on hover", async () => {
    // The same counts on the same slot, on the row that is not a worktree. The figures still
    // arrive on hover — that is every row's behaviour and none of it changed — but the 28px the
    // cross stands in are not opened, because a repo root never draws one and was leaving an
    // empty band between its numbers and the edge of the panel.
    mocks.getGitCheckoutDiffStats.mockResolvedValue({ "checkout:primary": { additions: 1944, deletions: 265 } });
    const wrapper = mount(Sidebar, {
      props: {
        repos: [repo({ checkouts: [checkout({ id: "checkout:primary", isPrimary: true, branch: "main" })] })],
        activeCheckoutId: "checkout:primary",
        activeSessionId: null,
        isOpening: false,
      },
    });
    await flushPromises();

    const row = wrapper.get(".workdir-parent");
    expect(row.find(".workdir-close").exists()).toBe(false);
    expect(row.classes()).not.toContain("has-close");
    // The counts themselves are untouched: the mark at rest, the figures one pointer away.
    expect(row.get(".workdir-end .workdir-diff-mark").text()).toBe("+\u2212");
    expect(row.get(".workdir-end .workdir-diff-nums").text()).toBe("+1944\u2212265");
    // And the rule that would open the strip does not name this row.
    const general = sidebarStyles().match(/\.workdir-row:hover[^{]*\.workdir-diff,[^{]*\{([^}]*)\}/)?.[1] ?? "";
    expect(general).not.toContain("margin-right");
    wrapper.unmount();
  });

  it("draws the counts in monospace, green for additions and red for deletions", () => {
    // The counts are read down a column of rows rather than one at a time, so they are monospaced:
    // a proportional digit moves them sideways and a whole list of them reads as noise.
    expect(rule(".workdir-diff")).toContain("font-family: var(--muster-font);");
    expect(rule(".ad")).toContain("color: var(--muster-content-added);");
    expect(rule(".rm")).toContain("color: var(--muster-content-removed);");
    // Tabular figures on the slot, so the digits inside each figure keep their own width.
    expect(rule(".workdir-end")).toContain("font-variant-numeric: tabular-nums;");
    // The slot is right against the row's edge, so the figures line up down the list.
    expect(rule(".workdir-end")).toContain("justify-content: flex-end;");
    // The resting mark is the figures with the digits left off, so it wears the figures' own two
    // colours and their own 5px gap: the `+` lands at the same offset in the mark as in the figures,
    // so the swap reads as digits being added rather than as one thing becoming another.
    expect(rule(".workdir-diff-mark")).toContain("gap: 5px;");
    expect(rule(".workdir-diff-mark")).not.toContain("color:");
    // The mark and the figures swap by width, not by display, so neither is ever removed from the
    // row: the figures are there for a reader who cannot hover and the mark is there for a reader
    // who is looking at the list.
    expect(rule(".workdir-diff > *")).toContain("overflow: hidden;");
    expect(rule(".workdir-diff > *")).toContain("min-width: 0;");
    // And the closed track is `0px` and NOT `0fr`. This container is sized by its own contents, and
    // a flexible track in a container of indefinite size is measured by its max-content contribution
    // whatever its flex factor is: `0fr` came out as wide as the figures, which stranded the mark at
    // the left of a slot twice as wide as it needed and squeezed the branch name for nothing. A
    // length is definite whatever the container is doing, which is the whole of the fix.
    expect(rule(".workdir-diff")).toContain("grid-template-columns: 1fr 0px;");
    expect(rule(".workdir-diff")).not.toContain("0fr");
  });

  it("names a terminal row after the session, and repeats nothing its parent already says", () => {
    // The reported duplicate came back a second way. The rows had already stopped repeating the
    // workdir in their NAME, and then the muted detail beside the name started repeating the group row
    // directly above it: three `zsh` rows under `feat/feedback-shell`, each spelling that same branch
    // again, so one block said the same thing four times. Indentation already says which group a row
    // belongs to, so the detail now carries only what the name and the group cannot: the agent mode,
    // or `sin sesión`.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:muster",
            kind: "git",
            name: "Muster",
            root: "/Users/dev/Muster",
            checkouts: [
              {
                ...checkout({
                  id: "checkout:worktree",
                  path: "/Users/dev/Muster/.worktrees/bug/13133933180-disable-adguard",
                  canonicalPath: "/Users/dev/Muster/.worktrees/bug/13133933180-disable-adguard",
                  isPrimary: false,
                  branch: "bug/13133933180-disable-adguard",
                }),
                sessions: [session("only", "zsh", "checkout:worktree")],
              },
              {
                ...checkout({ id: "checkout:root" }),
                sessions: [session("at-root", "zsh", "checkout:root")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:worktree",
        activeSessionId: null,
        isOpening: false,
        sessionRuntimeStatuses: {
          only: { state: "running", foregroundProcess: false },
          "at-root": { state: "running", foregroundProcess: false },
        },
      },
    });

    const names = wrapper.findAll('button[aria-label^="Terminal session:"]').map((row) => row.get(".nm").text());
    expect(names).toEqual(["zsh", "zsh"]);
    // The detail is the branch, drawn whole: there is no slug cut out of it any more, because the row
    // no longer spends its width deciding which half of a branch name is the interesting one. It
    // simply gets the room that is left and ellipsizes at the end.
    // Nothing observed two idle shells apart, so two of them are two identical rows, and that is true.
    expect(wrapper.findAll(".workdir-child .dm").map((row) => row.text())).toEqual([]);
    // says the branch, and the repo header says where the repo is.
    const childText = wrapper
      .findAll(".workdir-child")
      .map((row) => row.text())
      .join(" ");
    expect(childText).not.toContain("13133933180-disable-adguard\u2026");
    // The full branch, ticket and all, is the parent row's tooltip \u2014 so nothing was lost, it is
    // just not on the terminal row where it was never asked for.
    expect(wrapper.get(".workdir-parent .workdir-select").attributes("title")).toContain("13133933180");
    expect(wrapper.find(".workdir-parent .nm").text()).toBe("bug/13133933180-disable-adguard");
    wrapper.unmount();
  });

  it("folds a group away on its chevron, and folds nothing else", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:open" }),
                sessions: [session("a", "zsh", "checkout:open")],
              },
              {
                ...checkout({
                  id: "checkout:closed",
                  path: "/closed",
                  canonicalPath: "/closed",
                  isPrimary: false,
                  branch: "closed",
                }),
                sessions: [session("b", "zsh", "checkout:closed")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:open",
        activeSessionId: null,
        isOpening: false,
      },
    });

    const groups = () => wrapper.findAll(".workdir-checkouts");
    const folds = wrapper.findAll(".workdir-fold");
    expect(groups().every((group) => !group.classes().includes("col"))).toBe(true);
    expect(folds.every((fold) => fold.attributes("aria-expanded") === "true")).toBe(true);
    expect(wrapper.findAll('button[aria-label^="Terminal session:"]')).toHaveLength(2);

    await folds[1]!.trigger("click");
    // One class hides the children and turns the chevron, so the two can never disagree about
    // whether the group is open, and the control says which of the two it is.
    expect(groups()[1]!.classes()).toContain("col");
    expect(groups()[0]!.classes()).not.toContain("col");
    expect(folds[1]!.attributes("aria-expanded")).toBe("false");
    expect(folds[1]!.attributes("aria-label")).toBe("Expand closed");
    // `display: none` on the group's own children, which is what folding means: the rows are not
    // drawn over each other and there is no second empty band where they were.
    expect(rule(".col > .kids")).toContain("display: none;");
    expect(rule(".col .chv")).toContain("transform: rotate(-90deg);");
    // The row itself is still there, and folding is not selecting: the group row and the chevron are
    // separate controls, so a fold never activates the checkout behind it.
    expect(wrapper.get(".workdir-parent [aria-current]").attributes("aria-current")).toBe("page");
    expect(wrapper.get(".workdir-parent[aria-current], .workdir-parent.active").get(".nm").text()).toBe("main");
    expect(wrapper.emitted("selectCheckout")).toBeUndefined();

    await folds[1]!.trigger("click");
    expect(groups()[1]!.classes()).not.toContain("col");
    expect(folds[1]!.attributes("aria-expanded")).toBe("true");
    wrapper.unmount();
  });

  it("keeps the guide to the group under the pointer, and off every other row", () => {
    // The indent guide is transparent at rest and takes the border colour only while the pointer is
    // over the group. A guide that is always drawn is a second vertical line in a panel whose left
    // edge is already busy, and a reader cannot tell an indent from a selection.
    expect(rule(".kids")).toContain("border-left: 1px solid transparent;");
    expect(rule(".grp:hover > .kids")).toContain("border-left-color: var(--muster-border);");
    // It belongs to the group and to nothing else: no rule lights it from a child row or from a
    // sibling group, which is what would let one group's pointer draw another's line.
    expect(rule(".workdir-child:hover > .kids")).toBe("");
    expect(sidebarStyles()).not.toMatch(/\.workdir-child:hover[^{]*\.kids/);
    expect(rule(".kids .workdir-row")).toContain("padding-left: 12px;");
  });

  it("says `sin sesi\u00f3n` where nothing identified the session, and never fills the gap", () => {
    // The honest empty case, in the only place a word can be short enough to fit: beside the name,
    // where the reference puts it. It is not `idle`, because nothing observed a turn to be still,
    // and it is not the program's own name again, because the row is already called that.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:blank" }),
                sessions: [session("a", "zsh", "checkout:blank")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:blank",
        activeSessionId: null,
        isOpening: false,
        sessionRuntimeStatuses: {
          a: { state: "running", foregroundProcess: true, foregroundApp: "opencode" },
        },
      },
    });

    const row = wrapper.get(".workdir-child");
    expect(row.get(".nm").text()).toBe("opencode");
    expect(row.get(".dm").text()).toBe("sin sesi\u00f3n");
    // No time either: the clock belongs to a session and this row identified none.
    expect(row.find(".workdir-end-time").exists()).toBe(false);
    // And no invented state anywhere: the row says what it is and what could not be observed, and
    // stops.
    expect(row.text()).not.toContain("Idle");
    expect(row.text()).not.toContain("No process running");
    wrapper.unmount();
  });

  it("keeps two sibling terminals individually targetable when nothing distinguishes them", async () => {
    // The follow-on of dropping the workdir suffix, and it is a real cost worth pinning. Two idle
    // shells in one worktree now both read `zsh`, because nothing observed tells them apart. That
    // is truthful, and the user's answer was to live with it rather than invent an ordinal, a
    // timestamp or an id — so what this asserts is that each row is still its own row, still
    // reachable, and still announces something.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:two-shells" }),
                sessions: [
                  session("first", "zsh", "checkout:two-shells"),
                  session("second", "zsh", "checkout:two-shells"),
                ],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:two-shells",
        activeSessionId: "first",
        isOpening: false,
        sessionRuntimeStatuses: {
          first: { state: "running", foregroundProcess: false },
          second: { state: "running", foregroundProcess: false },
        },
      },
    });

    const rows = wrapper.findAll('button[aria-label^="Terminal session:"]');
    expect(rows).toHaveLength(2);
    // Both names are `zsh` — truthful, and the point of the change.
    expect(rows.map((row) => row.get(".nm").text())).toEqual(["zsh", "zsh"]);
    // Each is still its own control: two distinct accessible names, two distinct close actions, and
    // selecting one does not select the other.
    expect(rows[0]!.attributes("aria-current")).toBe("page");
    expect(rows[1]!.attributes("aria-current")).toBeUndefined();
    expect(wrapper.findAll(".workdir-child").filter((row) => row.classes().includes("active"))).toHaveLength(1);
    // And each has its own close, so one row is never acted on through the other's button.
    expect(wrapper.findAll('button[aria-label^="Close terminal session:"]')).toHaveLength(2);

    // Clicking the second selects the second: same text, different rows.
    await rows[1]!.trigger("click");
    expect(wrapper.emitted("selectSession")).toEqual([["second"]]);
    wrapper.unmount();
  });

  it("still renames a terminal, which is how two same-named shells are told apart", async () => {
    // A rename is stored as the session's name and is the row's name whenever nothing else is, so it
    // remains the answer to the duplicate-name problem the suffix used to paper over: renaming one of
    // two `zsh` rows gives that row a name of its own, with no new field involved.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:rename-pair" }),
                sessions: [
                  session("one", "zsh", "checkout:rename-pair"),
                  session("two", "zsh", "checkout:rename-pair"),
                ],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:rename-pair",
        activeSessionId: null,
        isOpening: false,
        sessionRuntimeStatuses: {
          one: { state: "running", foregroundProcess: false },
          two: { state: "running", foregroundProcess: false },
        },
      },
    });

    const row = () => wrapper.get('button[aria-label="Terminal session: zsh"]');
    await row().trigger("dblclick");
    const field = wrapper.get('input[aria-label="Terminal session name"]');
    await field.setValue("deploy");
    await field.trigger("keydown.enter");

    // One row now reads `deploy` and the other still reads `zsh`, from the store's own answer.
    expect(wrapper.emitted("renameSession")).toEqual([["one", "deploy"]]);
    const renamed = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:rename-pair" }),
                sessions: [
                  { ...session("one", "zsh", "checkout:rename-pair"), name: "deploy" },
                  session("two", "zsh", "checkout:rename-pair"),
                ],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:rename-pair",
        activeSessionId: null,
        isOpening: false,
        sessionRuntimeStatuses: {
          one: { state: "running", foregroundProcess: false },
          two: { state: "running", foregroundProcess: false },
        },
      },
    });
    expect(
      renamed.findAll('button[aria-label^="Terminal session:"]').map((element) => element.get(".nm").text()),
    ).toEqual(["deploy", "zsh"]);
    renamed.unmount();
    wrapper.unmount();
  });

  it("leaves a row nothing was observed about at one line rather than filling it", () => {
    // Honesty about the empty case: a terminal with no runtime status has nothing to say, and a word
    // invented to keep the column even would be a claim about nothing. The row is the same height as
    // every other row, says the program that is in front of it \u2014 nothing \u2014 and stops.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [{ ...checkout({ id: "checkout:silent" }), sessions: [session("s", "zsh", "checkout:silent")] }],
          }),
        ],
        activeCheckoutId: "checkout:silent",
        activeSessionId: null,
        isOpening: false,
      },
    });

    const row = wrapper.get(".workdir-child");
    expect(row.get(".nm").text()).toBe("zsh");
    expect(row.find(".dm").exists()).toBe(false);
    // Idle is the resting state, said by the grey terminal glyph and by nothing else: no time, no
    // state word, and no empty box standing in for a line there is nothing to put on.
    expect(row.classes()).toContain("state-idle");
    expect(row.find(".workdir-end-time").exists()).toBe(false);
    expect(row.find(".agent-state").exists()).toBe(false);
    expect(row.get(".workdir-select").attributes("aria-label")).toBe("Terminal session: zsh");
    wrapper.unmount();
  });

  it("keeps the cross invisible at rest and reachable by keyboard", async () => {
    // The reference removes the cross at rest and puts it back on hover, which is right for the
    // drawing and wrong for the tab order: a `display: none` button cannot be reached, so a close a
    // keyboard cannot find is a close that does not exist for half the people who use this. The
    // resolution is invisible and focusable \u2014 `opacity: 0`, never `display` or `visibility` \u2014 with all
    // three ways of arriving at it turning it on.
    const close = rule(".workdir-close");
    expect(close).toContain("opacity: 0;");
    expect(close).not.toContain("display");
    expect(close).not.toContain("visibility");
    expect(sidebarStyles()).toMatch(
      /\.workdir-row:hover > \.workdir-close,\n\.workdir-row:focus-within > \.workdir-close,\n\.workdir-close:focus-visible \{\n {2}opacity: 1;/,
    );
    // Focus itself is the shared rule every button follows, so the panel states no rule of its own
    // and paints focus nowhere near the accent: a ring in that colour would say the selection had
    // moved when it had not.
    expect(sidebarStyles()).not.toMatch(/\.workdir-select:focus-visible[^{]*\{[^}]*outline/);
    expect(sidebarStyles()).not.toMatch(/:focus-visible[^{]*\{[^}]*var\(--muster-accent\)/);
    const shared = readFileSync(resolve(process.cwd(), "src/muster.css"), "utf8");
    expect(shared).toMatch(/:focus-visible[^{]*\{[^}]*outline: 1px solid var\(--muster-control-focus\)/);

    // And it is a real, focusable button in the DOM on a resting row \u2014 not a span, not removed, not
    // given a tabindex of its own to fake it. Attached, because happy-dom only focuses what is in the
    // document, which is what a reader's Tab is reaching into.
    const host = document.createElement("div");
    document.body.append(host);
    const wrapper = mount(Sidebar, {
      attachTo: host,
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:busy", isPrimary: false, branch: "busy" }),
                sessions: [session("a", "zsh", "checkout:busy")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:busy",
        activeSessionId: null,
        isOpening: false,
      },
    });
    const crosses = wrapper.findAll("button.workdir-close");
    expect(crosses).toHaveLength(2);
    for (const cross of crosses) {
      expect(cross.element.tagName).toBe("BUTTON");
      expect(cross.attributes("type")).toBe("button");
      expect(cross.attributes("tabindex")).toBeUndefined();
      expect(cross.attributes("aria-hidden")).toBeUndefined();
      expect(cross.attributes("disabled")).toBeUndefined();
      expect(cross.attributes("aria-label")).toMatch(/^(Close terminal session: zsh|Remove or archive worktree busy)$/);
      // Reachable by keyboard: the element takes focus as it stands, which is the whole reason it is
      // faded rather than removed.
      (cross.element as HTMLElement).focus();
      expect(document.activeElement).toBe(cross.element);
    }
    wrapper.unmount();
    host.remove();
  });

  it("claims the agent only for the terminal OpenCode is the one running in it", () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:mixed" }),
                sessions: [
                  session("session:agent", "zsh", "checkout:mixed"),
                  session("session:editor", "zsh", "checkout:mixed"),
                  session("session:idle", "zsh", "checkout:mixed"),
                ],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:mixed",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:mixed": {
            sessions: [
              {
                id: "ses_row_12",
                title: "Copy ids",
                provider: "opencode",
                attention: "busy",
                agent: { label: "coder", color: null, attention: "busy" },
                terminalId: null,
                awaitingReply: false,
                running: true,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          "session:agent": {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Copy ids",
          },
          "session:editor": { state: "running", foregroundProcess: true, foregroundApp: "nvim" },
          "session:idle": { state: "running", foregroundProcess: false },
        },
      },
    });

    // The workdir has a session behind it and exactly one of its three terminals is showing it. A
    // Neovim row and an idle row naming one would be a claim about a process that is not in front,
    // and the mode is what would give it away: a mode beside a row means a session was identified.
    const modes = wrapper.findAll(".workdir-child .dm");
    expect(modes.map((mode) => (mode.exists() ? mode.text() : null))).toEqual(["coder"]);
    // The other two say the group they are in, which is a fact rather than a claim.
    expect(wrapper.findAll(".workdir-child.state-working")).toHaveLength(1);
    expect(wrapper.findAll(".workdir-child.state-running")).toHaveLength(1);
    expect(wrapper.findAll(".workdir-child.state-idle")).toHaveLength(1);
    wrapper.unmount();
  });

  it("leaves a quiet agent unpinned, and names no agent on a workdir with no server", async () => {
    const quiet = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [{ ...checkout({ id: "checkout:agent" }), sessions: [session("s", "zsh", "checkout:agent")] }],
          }),
        ],
        activeCheckoutId: "checkout:agent",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:agent": {
            sessions: [
              {
                id: "ses_row_13",
                title: "Copy ids",
                provider: "opencode",
                attention: "none",
                agent: { label: "coder", color: null, attention: "none" },
                terminalId: null,
                awaitingReply: false,
                running: false,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          s: {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Copy ids",
          },
        },
      },
    });

    // A quiet session is still named and still shows its mode, but its glyph is the agent's in
    // grey and does not move: idle is a colour, not an animation, and a spinner on a session that is
    // not working would be the panel claiming a turn nobody reported.
    expect(quiet.get(".workdir-child .dm").text()).toBe("coder");
    expect(quiet.get(".workdir-child").classes()).toContain("state-idle");
    expect(quiet.get(".workdir-child .workdir-icon").classes()).toContain("lucide-sparkles");
    // Idle has no colour of its own, so its ink is the panel's muted foreground rather than the faintest
    // token: the glyph also sits on the selected row's tint, where faint does not read.
    expect(rule(".state-idle .workdir-icon")).toContain("color: var(--muster-text-muted);");
    expect(quiet.get(".workdir-child > .workdir-select").attributes("aria-label")).toBe(
      "Terminal session: Copy ids, coder \u2014 0s \u2014 Idle",
    );
    quiet.unmount();

    // Two worktrees, two terminals, two agents. Each row reads its own worktree, so the one in a
    // worktree nobody has selected is still named — and named from its own answer, not the
    // selected worktree's. This is the case a single active-checkout answer got wrong.
    const elsewhere = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              { ...checkout({ id: "checkout:one" }), sessions: [session("a", "zsh", "checkout:one")] },
              {
                ...checkout({
                  id: "checkout:two",
                  path: "/two",
                  canonicalPath: "/two",
                  isPrimary: false,
                  branch: "two",
                }),
                sessions: [session("b", "zsh", "checkout:two")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:two",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:one": {
            sessions: [
              {
                id: "ses_row_14",
                title: "Copy ids",
                provider: "opencode",
                attention: "none",
                agent: { label: "coder", color: "#4ED6BF", attention: "none" },
                terminalId: null,
                awaitingReply: false,
                running: false,
                updatedAt: Date.now(),
              },
            ],
          },
          "checkout:two": {
            sessions: [
              {
                id: "ses_row_15",
                title: "Plan it",
                provider: "opencode",
                attention: "busy",
                agent: { label: "plan", color: "#FF966C", attention: "busy" },
                terminalId: null,
                awaitingReply: false,
                running: true,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          a: { state: "running", foregroundProcess: true, foregroundApp: "opencode", terminalTitle: "OC | Copy ids" },
          b: { state: "running", foregroundProcess: true, foregroundApp: "opencode", terminalTitle: "OC | Plan it" },
        },
      },
    });
    await flushPromises();
    expect(elsewhere.findAll(".workdir-child .dm")).toHaveLength(2);
    const [firstItems, activeItems] = elsewhere.findAll(".workdir-kids");
    // The unselected worktree names its own session, and the selected one names its own: neither
    // row is painted with the other worktree's agent.
    expect(firstItems!.get(".dm").text()).toBe("coder");
    expect(activeItems!.get(".dm").text()).toBe("plan");
    expect(elsewhere.findAll(".workdir-child.state-working")).toHaveLength(1);
    elsewhere.unmount();
  });

  it("gives each terminal in its own worktree its own agent and busy state", async () => {
    // The reported case: several Muster terminals, `opencode` run in each, independently. Both
    // rows are live, and each says what its own worktree is doing.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:one" }),
                sessions: [session("a", "zsh", "checkout:one")],
              },
              {
                ...checkout({
                  id: "checkout:two",
                  path: "/two",
                  canonicalPath: "/two",
                  isPrimary: false,
                  branch: "two",
                }),
                sessions: [session("b", "zsh", "checkout:two")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:one",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:one": {
            sessions: [
              {
                id: "ses_row_16",
                title: "Copy ids",
                provider: "opencode",
                attention: "busy",
                agent: { label: "coder", color: "#4ED6BF", attention: "busy" },
                terminalId: null,
                awaitingReply: false,
                running: true,
                updatedAt: Date.now(),
              },
            ],
          },
          "checkout:two": {
            sessions: [
              {
                id: "ses_row_17",
                title: "Plan it",
                provider: "opencode",
                attention: "none",
                agent: { label: "plan", color: "#FF966C", attention: "none" },
                terminalId: null,
                awaitingReply: false,
                running: false,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          a: { state: "running", foregroundProcess: true, foregroundApp: "opencode", terminalTitle: "OC | Copy ids" },
          b: { state: "running", foregroundProcess: true, foregroundApp: "opencode", terminalTitle: "OC | Plan it" },
        },
      },
    });
    await flushPromises();

    const [working, idle] = wrapper.findAll(".workdir-kids");
    // Busy in one worktree is not busy in the other: they are separate services' sessions.
    expect(working!.get(".dm").text()).toBe("coder");
    expect(working!.get(".workdir-child").classes()).toContain("state-working");
    expect(working!.get(".workdir-icon").classes()).toContain("lucide-loader-circle");
    expect(idle!.get(".dm").text()).toBe("plan");
    expect(idle!.get(".workdir-child").classes()).toContain("state-idle");
    expect(idle!.get(".workdir-icon").classes()).toContain("lucide-sparkles");
    wrapper.unmount();
  });

  it("paints the two states a grey idle glyph cannot be mistaken for", async () => {
    // Waiting is amber and failed is red, and neither is reachable by accident from the other, so a
    // row that needs a person cannot be drawn as a quiet one. The reference has no failed state;
    // Muster has one, and it stays: the danger token is the honest colour for a turn that ended badly.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:waiting" }),
                sessions: [session("a", "zsh", "checkout:waiting")],
              },
              {
                ...checkout({
                  id: "checkout:failed",
                  path: "/failed",
                  canonicalPath: "/failed",
                  isPrimary: false,
                  branch: "failed",
                }),
                sessions: [session("b", "zsh", "checkout:failed")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:waiting",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:waiting": {
            sessions: [
              {
                id: "ses_row_18",
                title: "Copy ids into lists",
                provider: "opencode",
                attention: "blocked",
                agent: { label: "plan", color: "#FF966C", attention: "blocked" },
                terminalId: null,
                awaitingReply: false,
                running: true,
                updatedAt: Date.now(),
              },
            ],
          },
          "checkout:failed": {
            sessions: [
              {
                id: "ses_row_19",
                title: "Fix it",
                provider: "opencode",
                attention: "failed",
                agent: { label: "coder", color: "#4ED6BF", attention: "failed" },
                terminalId: null,
                awaitingReply: false,
                running: false,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          a: {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Copy ids into lists",
          },
          b: { state: "running", foregroundProcess: true, foregroundApp: "opencode", terminalTitle: "OC | Fix it" },
        },
      },
    });
    await flushPromises();

    const [waiting, failed] = wrapper.findAll(".workdir-kids");
    // A turn stuck on this server version is not a reply the row can ask for, so it says it is
    // waiting rather than working — and it asks with a question mark rather than with the amber the
    // other rows' failure wears, because the two are not the same kind of news.
    expect(waiting!.get(".workdir-child").classes()).toContain("state-waiting");
    expect(waiting!.get(".workdir-child .workdir-icon").classes()).toContain("lucide-circle-question-mark");
    expect(rule(".state-waiting .workdir-icon")).toContain("color: var(--muster-warning);");
    expect(waiting!.get(".workdir-child").classes()).toContain("agent-tinted");
    // A red glyph on a row that is not merely idle, and a last turn that ended badly, which is a
    // different sentence from the one a waiting turn gets.
    expect(failed!.get(".workdir-child").classes()).toContain("state-failed");
    expect(rule(".state-failed .workdir-icon")).toContain("color: var(--muster-danger-fg);");
    // A colour is not something a screen reader reads, so both states are spelled out where it is
    // not, and both words come from the same state the colour did.
    expect(waiting!.get(".workdir-select").attributes("aria-label")).toBe(
      "Terminal session: Copy ids into lists, plan \u2014 0s \u2014 Waiting for your reply",
    );
    expect(failed!.get(".workdir-select").attributes("aria-label")).toBe(
      "Terminal session: Fix it, coder \u2014 0s \u2014 Last turn failed",
    );
    wrapper.unmount();
  });

  it("gives every row one square box, and tells hover from selection", () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:rows" }),
                sessions: [
                  session("session:serve", "pnpm", "checkout:rows"),
                  session("session:shell", "zsh", "checkout:rows"),
                  session("session:agent", "zsh", "checkout:rows"),
                ],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:rows",
        activeSessionId: "session:serve",
        isOpening: false,
        agentRows: {
          "checkout:rows": {
            sessions: [
              {
                id: "ses_row_20",
                title: "Copy ids",
                provider: "opencode",
                attention: "busy",
                agent: { label: "coder", color: "#4ED6BF", attention: "busy" },
                terminalId: null,
                awaitingReply: false,
                running: true,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          "session:serve": { state: "running", foregroundProcess: true, foregroundApp: "pnpm" },
          "session:shell": { state: "running", foregroundProcess: false },
          "session:agent": {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Copy ids",
          },
        },
      },
    });

    // Every terminal wears a glyph, whether or not there is a state to colour it with.
    const terminals = wrapper.findAll('button[aria-label^="Terminal session:"]');
    expect(terminals).toHaveLength(3);
    expect(terminals.filter((row) => row.find(".workdir-icon").exists())).toHaveLength(3);
    // The row that opens a terminal sits on the terminals' axis, so it wears a glyph too.
    expect(wrapper.get('button[aria-label^="New terminal for"]').find(".workdir-icon").exists()).toBe(true);
    // The branch row is one level up and carries a chevron as well: the glyph is its own, in the
    // same slot, because a fold control is a control like any other.
    expect(wrapper.find(".workdir-parent .workdir-icon").exists()).toBe(true);
    expect(wrapper.find(".workdir-parent .workdir-fold").exists()).toBe(true);
    wrapper.unmount();

    // **No rounded corners anywhere in the panel.** A rounded row with a two-pixel accent edge down
    // its left says "this one" twice, in two shapes, and the edge is the one that says it. Muster
    // draws square boxes everywhere else, and a row that rounds off only here is a shape the rest of
    // the app has never had.
    expect(sidebarStyles()).not.toContain("border-radius");
    expect(rule(".workdir-row")).not.toContain("border-radius");
    expect(rule(".workdir-fold")).not.toContain("border-radius");
    expect(rule(".workdir-close")).not.toContain("border-radius");
    expect(rule(".group-heading")).not.toContain("border-radius");

    // The row itself: one line at the height every row in the panel is.
    const row = rule(".workdir-row");
    expect(row).toContain("height: 26px;");
    expect(row).toContain("padding: 0 8px;");
    expect(row).toContain("gap: 7px;");
    expect(row).toContain("display: flex;");
    expect(row).toContain("align-items: center;");
    // The glyph is one line of this type, drawn in no box of its own: only the row is a control, so a
    // surface behind a glyph would read as a second, smaller button inside a row that is already one.
    const glyph = rule(".workdir-icon");
    expect(glyph).toContain("width: 14px;");
    expect(glyph).toContain("height: 14px;");
    expect(glyph).toContain("flex-shrink: 0;");
    expect(glyph).not.toMatch(/background|border|box-shadow/);
    // The guide belongs to the group and takes the reference's indent step.
    expect(rule(".kids")).toContain("margin-left: 14px;");
    expect(rule(".kids .workdir-row")).toContain("padding-left: 12px;");
  });

  it("paints hover and selection differently, and only selection with the accent edge", () => {
    // From the real app: with hover and selection sharing one surface and nothing but the ink
    // between them, a selected row could not be picked out. So a hover is the subtle surface alone
    // and a selection is a different surface plus a two-pixel accent edge down its own left \u2014 which
    // is readable at a glance and does not depend on reading the colour of any text.
    const hover = rule(".workdir-row:hover");
    expect(hover).toContain("background: var(--muster-el-hover);");
    // A hovered row is a row being acted on, not a row being chosen: no edge and no accent on it.
    expect(hover).not.toContain("box-shadow");
    expect(hover).not.toContain("accent");

    const selected = rule(".workdir-row.selected");
    expect(selected).toContain("background: var(--muster-el-selected);");
    expect(selected).toContain("box-shadow: inset 2px 0 0 var(--muster-accent);");
    // The two surfaces are genuinely two, not one surface and an ink colour.
    expect(selected).not.toContain("var(--muster-el-hover)");
    // The edge is an inset shadow, so it is drawn inside the row's own box and cannot spill onto the
    // group guide hanging 14px to its left.
    expect(selected).toMatch(/box-shadow: inset /);
    expect(sidebarStyles()).not.toMatch(/\.workdir-row:hover[^{]*\{[^}]*box-shadow/);
    // No exception for what a row is doing: selecting a row may never look different depending on
    // whether a turn is running in it, and the glyph's colour already says that.
    expect(sidebarStyles()).not.toMatch(/\.workdir-child\.active:not\(/);
    // The accent edge belongs to the selected TERMINAL alone. A checkout that merely holds the
    // selected terminal used to carry the same tint and the same blue edge, so the panel showed two
    // rows claiming the selection at once. Nothing generic may paint the edge any more.
    expect(sidebarStyles()).not.toMatch(/\.workdir-child\.active[^{]*\{[^}]*box-shadow/);
    expect(sidebarStyles()).not.toMatch(/\.workdir-parent\.active[^{]*\{[^}]*box-shadow/);
    // The edge rides on `selected` and on nothing else, so exactly the rows that are selected and no
    // others can carry it.
    expect(rule(".workdir-row.selected")).toContain("box-shadow: inset 2px 0 0 var(--muster-accent);");
    // One vocabulary for the whole panel, so the repo header hovers like a row as well.
    expect(rule(".group-heading:hover")).toContain("background: var(--muster-el-hover);");
    // And a group that merely holds the terminal being read takes no surface of its own: two tinted
    // rows in one list would put two claims about "where you are" side by side.
    expect(rule(".workdir-checkouts.has-active .workdir-parent .workdir-icon")).toContain(
      "color: var(--muster-text-secondary);",
    );
    expect(sidebarStyles()).not.toMatch(/has-active[^{]*\{[^}]*background/);
  });

  it("gives the detail a share it can lose gradually, so it truncates and never vanishes", () => {
    // The detail is the first thing to give way and the last thing to disappear. `flex: 1 1 0` looked
    // like it did that and did not: a zero basis means the detail's own share of the shrink is zero,
    // so a long name absorbed every pixel taken and the detail collapsed to nothing rather than being
    // cut \u2014 `Review the duplicated rows in the sidebar` with no mode beside it at all, which is the one
    // thing a two-part label cannot do. So it is sized by its own contents like the name.
    expect(rule(".dm")).toContain("flex: 0 2 auto;");
    expect(rule(".dm")).not.toContain("flex: 1 1 0;");
    expect(rule(".dm")).toContain("min-width: 6ch;");
    // Shrink 2 against the name's 1 is what makes it go first: both lose room in proportion to what
    // each brought, and the detail gives up twice as fast, so its tail is cut before the name moves.
    expect(rule(".nm")).toContain("flex: 0 1 auto;");
    // And NO cap. A `max-width` was tried and cut a branch name to `feat/feedbac\u2026` on a 203px label
    // with 176px spare: a ceiling trims a row that has nothing competing for the space. The detail is
    // cut only when something is actually in the way.
    expect(rule(".dm")).not.toContain("max-width");
    expect(rule(".lbl")).toContain("flex: 1;");
    // The name still gives way after it, and `min-width: 0` is what lets it be narrower than its text
    // so the ellipsis has a width to work in at all.
    expect(rule(".nm")).toContain("flex: 0 1 auto;");
    expect(rule(".nm")).toContain("min-width: 0;");
    expect(rule(".nm")).toContain("text-overflow: ellipsis;");
    // Both ellipsize, which is the whole claim: truncated, not removed.
    expect(rule(".dm")).toContain("overflow: hidden;");
    expect(rule(".dm")).toContain("text-overflow: ellipsis;");
    // And neither half may push the right slot out of the row: it is the last flex item on the line
    // and it is `flex: none` with a reservation of its own.
    expect(rule(".workdir-end")).toContain("flex: none;");
    expect(rule(".workdir-end")).toContain("min-width: 20px;");
  });

  it("keeps the detail drawn beside a long name, and lets both ellipsize", async () => {
    // The measured defect, as a shape: a name far longer than the sidebar's minimum width beside a
    // detail that is present. Both halves are in the DOM, the detail is not empty and not dropped,
    // and the name has not pushed the trailing slot \u2014 the time \u2014 off the row.
    const long = "Review the duplicated rows in the sidebar";
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:long" }),
                sessions: [session("a", "zsh", "checkout:long")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:long",
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:long": {
            sessions: [
              {
                id: "ses_row_21",
                title: long,
                provider: "opencode",
                attention: "none",
                agent: { label: "plan", color: null, attention: "none" },
                terminalId: null,
                awaitingReply: false,
                running: false,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          a: { state: "running", foregroundProcess: true, foregroundApp: "opencode", terminalTitle: `OC | ${long}` },
        },
      },
    });

    const row = wrapper.get(".workdir-child");
    // Both halves are drawn, and the detail is the whole mode rather than an empty span.
    expect(row.get(".nm").text()).toBe(long);
    expect(row.get(".dm").text()).toBe("plan");
    expect(row.find(".dm").exists()).toBe(true);
    // The trailing slot is still the last thing on the row, holding the real elapsed time, so the
    // two halves of the label have not crowded it out.
    const end = row.get(".workdir-end");
    expect(end.find(".workdir-end-time").exists()).toBe(true);
    expect(row.element.lastElementChild?.classList.contains("workdir-close")).toBe(true);
    expect(row.element.firstElementChild?.classList.contains("workdir-select")).toBe(true);
    // And the name is drawn before the detail, so the detail is the half that is cut from the right.
    const halves = row.get(".lbl").element.children;
    expect([...halves].map((half) => half.className)).toEqual(["nm", "dm"]);
    wrapper.unmount();
  });

  it("maps each of the five states to a glyph and a colour of its own", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              { ...checkout({ id: "checkout:one" }), sessions: [session("a", "zsh", "checkout:one")] },
              {
                ...checkout({
                  id: "checkout:two",
                  path: "/two",
                  canonicalPath: "/two",
                  isPrimary: false,
                  branch: "two",
                }),
                sessions: [session("b", "zsh", "checkout:two")],
              },
              {
                ...checkout({
                  id: "checkout:three",
                  path: "/three",
                  canonicalPath: "/three",
                  isPrimary: false,
                  branch: "three",
                }),
                sessions: [session("c", "zsh", "checkout:three")],
              },
              {
                ...checkout({
                  id: "checkout:four",
                  path: "/four",
                  canonicalPath: "/four",
                  isPrimary: false,
                  branch: "four",
                }),
                sessions: [session("d", "zsh", "checkout:four")],
              },
              {
                ...checkout({
                  id: "checkout:five",
                  path: "/five",
                  canonicalPath: "/five",
                  isPrimary: false,
                  branch: "five",
                }),
                sessions: [session("e", "zsh", "checkout:five")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:one",
        activeSessionId: null,
        isOpening: false,
        agentRows: Object.fromEntries(
          ["one", "two", "three", "four"].map((id, index) => [
            `checkout:${id}`,
            {
              sessions: [
                rowSession({
                  id: "ses_row_22",
                  title: `Session ${id}`,
                  attention: (["busy", "blocked", "failed", "none"] as const)[index]!,
                  agent: {
                    label: "coder",
                    color: null,
                    attention: (["busy", "blocked", "failed", "none"] as const)[index]!,
                  },
                  awaitingReply: index === 1,
                  running: index < 2,
                }),
              ],
            },
          ]),
        ),
        sessionRuntimeStatuses: {
          a: {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Session one",
          },
          b: {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Session two",
          },
          c: {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Session three",
          },
          d: {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Session four",
          },
          e: { state: "running", foregroundProcess: true, foregroundApp: "pnpm" },
        },
      },
    });
    await flushPromises();

    // Five rows, five states, one class each — and the states are on the row rather than on a badge
    // hung off a glyph, because there is no badge left.
    const states = ["working", "waiting", "failed", "idle", "running"];
    const rows = wrapper
      .findAll(".workdir-child")
      .filter((row) => row.find('button[aria-label^="Terminal session:"]').exists());
    expect(wrapper.findAll(".row-dot")).toHaveLength(0);
    expect(rows).toHaveLength(5);
    expect(rows.map((row) => row.classes().find((name) => states.some((state) => name === `state-${state}`)))).toEqual(
      states.map((state) => `state-${state}`),
    );
    // The glyph follows the state too: a spinner only while a turn is running, a question mark only
    // while one is asking, and a terminal's square only for a process that is up.
    expect(
      rows.map((row) =>
        row
          .get(".workdir-icon")
          .classes()
          .find((n) => n.startsWith("lucide-")),
      ),
    ).toEqual([
      "lucide-loader-circle",
      "lucide-circle-question-mark",
      "lucide-sparkles",
      "lucide-sparkles",
      "lucide-square-terminal",
    ]);
    wrapper.unmount();

    // Each state is a colour of its own, or a row that needs a person could be drawn as a quiet one.
    const colours = states.map((state) => rule(`.state-${state} .workdir-icon`));
    expect(colours.every((colour) => colour.includes("color: var(--muster-"))).toBe(true);
    expect(new Set(colours).size).toBe(states.length);
    // And only the one that is working moves, and only while the reader has not asked it not to.
    expect(rule(".state-working .workdir-icon")).toContain("animation: row-spin 1.1s linear infinite;");
    expect(sidebarStyles()).toMatch(
      /@media \(prefers-reduced-motion: reduce\) \{\s*\.state-working \.workdir-icon \{\s*animation: none;/,
    );
  });

  it("claims no state for an OpenCode that answered nothing", async () => {
    // The service being unreachable and the agent being idle are two different facts, and only one
    // of them was ever observed. This is the case where nothing was: no agent behind the row to name
    // and no turn to report, and a grey spinner-free glyph here would say "idle" about a turn nobody
    // ever asked — so the row says the missing thing in words instead.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({ id: "checkout:quiet" }),
                sessions: [session("a", "zsh", "checkout:quiet")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:quiet",
        activeSessionId: null,
        isOpening: false,
        // The checkout is absent from the answer: OpenCode is in front and the service did not
        // reply for this workdir.
        agentRows: {},
        sessionRuntimeStatuses: {
          a: { state: "running", foregroundProcess: true, foregroundApp: "opencode" },
        },
      },
    });
    await flushPromises();

    const row = wrapper.get(".workdir-child");
    expect(row.find(".row-dot").exists()).toBe(false);
    expect(row.find(".agent-chip").exists()).toBe(false);
    expect(row.find(".workdir-end-time").exists()).toBe(false);
    // The program is an observed fact and the row is named after it; what could not be observed is
    // said beside it, in the one word the reference uses for exactly this.
    expect(row.get(".nm").text()).toBe("opencode");
    expect(row.get(".dm").text()).toBe("sin sesi\u00f3n");
    // And both words live where nothing truncates them.
    expect(row.get(".workdir-select").attributes("aria-label")).toBe(
      "Terminal session: opencode, sin sesi\u00f3n \u2014 Session not identified",
    );
    expect(row.get(".workdir-select").attributes("title")).toBe(
      "opencode, sin sesi\u00f3n \u2014 Session not identified",
    );
    wrapper.unmount();
  });

  it("renames a session in place, and only when the name actually changed", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:rename",
            kind: "plain",
            name: "rename",
            root: "/rename",
            checkouts: [
              {
                ...checkout({ id: "checkout:rename" }),
                sessions: [session("session:one", "zsh", "checkout:rename")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:rename",
        activeSessionId: null,
        isOpening: false,
      },
      // In the document, because the field takes the focus and that is half of what a rename
      // has to do.
      attachTo: document.body,
    });

    const row = () => wrapper.get('button[aria-label="Terminal session: zsh"]');
    await row().trigger("dblclick");
    // The field appears and takes the focus on the tick after the click, not the same one.
    await flushPromises();

    // The button is replaced by the field, not wrapped in it: a control inside a control
    // cannot be focused or announced on its own.
    expect(wrapper.find(".workdir-rename").exists()).toBe(true);
    const field = wrapper.get<HTMLInputElement>(".workdir-rename");
    expect(field.element.value).toBe("zsh");
    expect(document.activeElement).toBe(field.element);

    // Enter keeps what was typed. The spaces around it are not part of a name, and the backend
    // trims them again on its own side: the database decides, not this row.
    await field.setValue("  build logs  ");
    await field.trigger("keydown.enter");
    expect(wrapper.emitted("renameSession")).toEqual([["session:one", "build logs"]]);
    expect(wrapper.find(".workdir-rename").exists()).toBe(false);

    // Escape is not a rename: the field closes and nothing is emitted.
    await row().trigger("dblclick");
    await flushPromises();
    await wrapper.get(".workdir-rename").setValue("discarded");
    await wrapper.get(".workdir-rename").trigger("keydown.esc");
    expect(wrapper.find(".workdir-rename").exists()).toBe(false);
    expect(wrapper.emitted("renameSession")).toHaveLength(1);

    // Nor is retyping the name it already had.
    await row().trigger("dblclick");
    await flushPromises();
    await wrapper.get(".workdir-rename").trigger("keydown.enter");
    expect(wrapper.emitted("renameSession")).toHaveLength(1);
    wrapper.unmount();
  });

  it("moves a terminal with a tree-style pointer drag after the threshold", async () => {
    const worktree = checkout({ id: "checkout:wt", isPrimary: false, branch: "feature/x", path: "/test-wt" });
    const wrapper = mount(Sidebar, {
      props: {
        repos: [repo({ checkouts: [{ ...checkout(), sessions: [session("session:one", "zsh")] }, worktree] })],
        activeCheckoutId: "checkout:primary",
        activeSessionId: null,
        isOpening: false,
      },
      attachTo: document.body,
    });
    const oldHitTest = Object.getOwnPropertyDescriptor(document, "elementFromPoint");
    const hitTest = vi.fn();
    Object.defineProperty(document, "elementFromPoint", { configurable: true, value: hitTest });
    const pointer = (type: string, x: number) => {
      const event = new Event(type, { bubbles: true, cancelable: true });
      Object.assign(event, { pointerId: 1, pointerType: "mouse", isPrimary: true, button: 0, clientX: x, clientY: 20 });
      return event;
    };

    try {
      const row = wrapper.get('button[aria-label="Terminal session: zsh"]');
      const target = wrapper.findAll(".workdir-parent").at(-1)!;
      const targetList = wrapper.findAll(".workdir-kids").at(-1)!;

      // A short move does not start a drag or interfere with selecting the session.
      row.element.dispatchEvent(pointer("pointerdown", 10));
      window.dispatchEvent(pointer("pointermove", 13));
      expect(document.body.querySelector(".terminal-drag-ghost")).toBeNull();
      window.dispatchEvent(pointer("pointerup", 13));
      await row.trigger("click");
      expect(wrapper.emitted("selectSession")).toEqual([["session:one"]]);

      // Beyond 5px, the row dims, a copy follows the pointer, and the worktree shows an insertion.
      row.element.dispatchEvent(pointer("pointerdown", 10));
      hitTest.mockReturnValue(target.element);
      window.dispatchEvent(pointer("pointermove", 18));
      await flushPromises();
      expect(wrapper.findAll(".is-being-dragged")).toHaveLength(1);
      expect(wrapper.findAll(".is-drop-target")).toHaveLength(1);
      expect(wrapper.findAll(".terminal-drop-insertion")).toHaveLength(1);
      expect(document.body.querySelector(".terminal-drag-ghost")?.textContent).toContain("zsh");

      // Dropping over the child list moves the item and suppresses the click browsers synthesize.
      hitTest.mockReturnValue(targetList.element);
      window.dispatchEvent(pointer("pointerup", 18));
      await row.trigger("click", { detail: 1 });
      expect(wrapper.emitted("moveSession")).toEqual([["session:one", "checkout:wt", 0]]);
      expect(wrapper.emitted("selectSession")).toEqual([["session:one"]]);
      expect(wrapper.findAll(".is-drop-target")).toHaveLength(0);
      expect(document.body.querySelector(".terminal-drag-ghost")).toBeNull();
    } finally {
      if (oldHitTest) Object.defineProperty(document, "elementFromPoint", oldHitTest);
      else Reflect.deleteProperty(document, "elementFromPoint");
      wrapper.unmount();
    }
  });

  it("cancels on Escape and rejects missing worktrees", async () => {
    const valid = checkout({ id: "checkout:valid", isPrimary: false, branch: "valid" });
    const gone = checkout({ id: "checkout:gone", isPrimary: false, branch: "gone", isMissing: true });
    const elsewhere = repo({
      id: "repo:elsewhere",
      name: "elsewhere",
      root: "/elsewhere",
      checkouts: [checkout({ id: "checkout:elsewhere", repoId: "repo:elsewhere", path: "/elsewhere" })],
    });
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({ checkouts: [{ ...checkout(), sessions: [session("session:one", "zsh")] }, valid, gone] }),
          elsewhere,
        ],
        activeCheckoutId: "checkout:primary",
        activeSessionId: null,
        isOpening: false,
      },
      attachTo: document.body,
    });
    const oldHitTest = Object.getOwnPropertyDescriptor(document, "elementFromPoint");
    const hitTest = vi.fn();
    Object.defineProperty(document, "elementFromPoint", { configurable: true, value: hitTest });
    const pointer = (type: string, pointerId: number, x: number) => {
      const event = new Event(type, { bubbles: true, cancelable: true });
      Object.assign(event, { pointerId, pointerType: "mouse", isPrimary: true, button: 0, clientX: x, clientY: 20 });
      return event;
    };
    const checkoutRow = (id: string) =>
      wrapper.findAll("[data-workdir-checkout]").find((row) => row.attributes("data-workdir-checkout") === id)!;

    try {
      const row = wrapper.get('button[aria-label="Terminal session: zsh"]');
      // A valid hover lights the destination, but Escape cancels without changing ownership.
      row.element.dispatchEvent(pointer("pointerdown", 1, 10));
      hitTest.mockReturnValue(checkoutRow("checkout:valid").element);
      window.dispatchEvent(pointer("pointermove", 1, 20));
      await flushPromises();
      expect(wrapper.find(".app-sidebar").classes()).toContain("is-terminal-dragging");
      expect(hitTest).toHaveBeenCalled();
      expect(wrapper.findAll(".is-drop-target")).toHaveLength(1);
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
      await flushPromises();
      expect(wrapper.findAll(".is-drop-target")).toHaveLength(0);
      expect(document.body.querySelector(".terminal-drag-ghost")).toBeNull();
      expect(wrapper.emitted("moveSession")).toBeUndefined();

      // A missing worktree never shows a drop target and cannot take it.
      for (const [pointerId, targetId] of [[2, "checkout:gone"]] as const) {
        row.element.dispatchEvent(pointer("pointerdown", pointerId, 10));
        hitTest.mockReturnValue(checkoutRow(targetId).element);
        window.dispatchEvent(pointer("pointermove", pointerId, 20));
        expect(wrapper.findAll(".is-drop-target")).toHaveLength(0);
        window.dispatchEvent(pointer("pointerup", pointerId, 20));
      }

      // Pointer cancellation is scoped to the captured pointer; releasing outside never moves it.
      row.element.dispatchEvent(pointer("pointerdown", 4, 10));
      hitTest.mockReturnValue(checkoutRow("checkout:valid").element);
      window.dispatchEvent(pointer("pointermove", 4, 20));
      await flushPromises();
      window.dispatchEvent(pointer("pointercancel", 99, 20));
      expect(wrapper.find(".app-sidebar").classes()).toContain("is-terminal-dragging");
      window.dispatchEvent(pointer("pointercancel", 4, 20));
      await flushPromises();
      expect(wrapper.findAll(".is-drop-target")).toHaveLength(0);

      row.element.dispatchEvent(pointer("pointerdown", 5, 10));
      hitTest.mockReturnValue(checkoutRow("checkout:valid").element);
      window.dispatchEvent(pointer("pointermove", 5, 20));
      hitTest.mockReturnValue(null);
      window.dispatchEvent(pointer("pointerup", 5, 20));
      await flushPromises();
      expect(wrapper.emitted("moveSession")).toBeUndefined();
    } finally {
      if (oldHitTest) Object.defineProperty(document, "elementFromPoint", oldHitTest);
      else Reflect.deleteProperty(document, "elementFromPoint");
      wrapper.unmount();
    }
  });

  it("offers registered destinations to a keyboard, but not missing checkouts", async () => {
    const worktree = checkout({ id: "checkout:wt", isPrimary: false, branch: "feature/x", path: "/test-wt" });
    const otherWorktree = checkout({ id: "checkout:other-wt", isPrimary: false, branch: "y", path: "/test-other" });
    const gone = checkout({ id: "checkout:gone", isPrimary: false, branch: "gone", isMissing: true });
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [{ ...checkout(), sessions: [session("session:one", "zsh")] }, worktree, otherWorktree, gone],
          }),
          repo({
            id: "repo:elsewhere",
            name: "elsewhere",
            root: "/elsewhere",
            checkouts: [
              {
                ...checkout({
                  id: "checkout:elsewhere",
                  repoId: "repo:elsewhere",
                  path: "/elsewhere",
                  branch: "foreign",
                }),
                // It has no sibling, but the other registered repo can still take its terminal.
                sessions: [session("session:two", "fish", "checkout:elsewhere")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:primary",
        activeSessionId: null,
        isOpening: false,
      },
      attachTo: document.body,
    });

    const rows = wrapper.findAll('button[aria-label^="Terminal session:"]');
    // Native browser dragging is off; a row with a destination advertises the menu it opens.
    expect(rows.map((row) => row.attributes("draggable"))).toEqual([undefined, undefined]);
    expect(rows.map((row) => row.attributes("aria-haspopup"))).toEqual(["menu", "menu"]);

    await rows[0].trigger("contextmenu");
    const destinations = wrapper.findAll('[role="menu"] [role="menuitem"]');
    // A registered foreign checkout is available; a directory that is gone is nowhere to put a
    // shell that is running.
    expect(destinations.map((item) => item.text())).toEqual(["feature/x", "y", "foreign"]);

    await destinations[1].trigger("click");
    expect(wrapper.emitted("moveSession")).toEqual([["session:one", "checkout:other-wt", 0]]);
    // And the list closes, so the move can be aimed again from the same row.
    expect(wrapper.find('[role="menu"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("keeps a long workdir title ellipsizable inside its own label", () => {
    // A branch with a ticket number in it is longer than the sidebar's minimum width, and the row
    // spends its room where it always did: on the name, which gets the ellipsis at the end. Nothing
    // is cut out of the middle and nothing is hidden, so the full name is the row's tooltip.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({
                  id: "checkout:long",
                  path: "/test-feature",
                  canonicalPath: "/test-feature",
                  isPrimary: false,
                  branch: "bug/13104984920-timeline-element-boundaries",
                }),
              },
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });

    const name = wrapper.get(".workdir-parent .nm");
    expect(name.text()).toBe("bug/13104984920-timeline-element-boundaries");
    // One span, not two: the row no longer splits a branch into halves to decide where the ellipsis
    // goes. CSS cannot truncate the middle of a string, and the end of a branch is where its identity
    // is, so the whole name is given to one span and cut at the end.
    expect(wrapper.find(".workdir-name-head").exists()).toBe(false);
    expect(wrapper.find(".workdir-name-tail").exists()).toBe(false);
    // And it is the name that ellipsizes, at whatever width is left after the glyph, the label's own
    // minimum and the counts.
    expect(rule(".nm")).toContain("overflow: hidden;");
    expect(rule(".nm")).toContain("text-overflow: ellipsis;");
    expect(rule(".lbl")).toContain("white-space: nowrap;");
    // The name the row cannot fit whole is the one the tooltip says.
    expect(wrapper.get(".workdir-parent .workdir-select").attributes("title")).toBe(
      "bug/13104984920-timeline-element-boundaries \u2014 /test-feature",
    );
    wrapper.unmount();
  });

  it("shows every checkout's own line counts, not only the active one's", async () => {
    mocks.getGitCheckoutDiffStats.mockResolvedValue({
      "checkout:primary": { additions: 12, deletions: 4 },
      "checkout:feature": { additions: 0, deletions: 7 },
    });
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                id: "checkout:primary",
                repoId: "repo:test",
                path: "/test",
                canonicalPath: "/test",
                isPrimary: true,
                branch: "main",
                changedFiles: 0,
                isMissing: false,
                sessions: [],
              },
              {
                id: "checkout:feature",
                repoId: "repo:test",
                path: "/test-feature",
                canonicalPath: "/test-feature",
                isPrimary: false,
                branch: "feature",
                changedFiles: 0,
                isMissing: false,
                sessions: [],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:feature",
        activeSessionId: null,
        isOpening: false,
      },
    });
    await flushPromises();

    // One call covers the sidebar: the inactive checkout is counted without being visited.
    expect(mocks.getGitCheckoutDiffStats).toHaveBeenCalledTimes(1);
    const [base, feature] = wrapper.findAll(".workdir-row .workdir-end");
    expect(base.get(".workdir-diff-nums .ad").text()).toBe("+12");
    expect(base.get(".workdir-diff-nums .rm").text()).toBe("\u22124");
    // A zero addition has nothing to draw, and the deletion stands on its own. The `.ad` and the `.rm`
    // are read inside the figures and not inside the whole slot, because the mark above them is
    // drawn in the same two colours and carries a bare sign of its own for each count the row has.
    expect(feature.find(".workdir-diff-nums .ad").exists()).toBe(false);
    expect(feature.get(".workdir-diff-nums .rm").text()).toBe("\u22127");
    // And the resting mark claims the same: a checkout that only lost lines has no `+` to draw, so
    // drawing one would be a claim about a change set this row does not have. A checkout that has
    // both keeps both, which is the case the mark was drawn for.
    expect(feature.find(".workdir-diff-mark .ad").exists()).toBe(false);
    expect(feature.get(".workdir-diff-mark .rm").text()).toBe("\u2212");
    expect(base.get(".workdir-diff-mark").text()).toBe("+\u2212");
    wrapper.unmount();
  });

  it("draws no counts for a checkout Git cannot answer for", async () => {
    mocks.getGitCheckoutDiffStats.mockResolvedValue({ "checkout:primary": { additions: 0, deletions: 0 } });
    const wrapper = mount(Sidebar, {
      props: {
        repos: [repo({ checkouts: [{ ...checkout(), id: "checkout:primary" }] })],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });
    await flushPromises();

    // An absent checkout is not a clean bill of health, and neither is a real zero.
    expect(wrapper.get(".workdir-end").text()).toBe("");
    wrapper.unmount();
  });

  it("names a missing directory in the row that owns it, in place of its counts", async () => {
    mocks.getGitCheckoutDiffStats.mockResolvedValue({
      "checkout:primary": { additions: 12, deletions: 4 },
      "checkout:gone": { additions: 3, deletions: 1 },
    });
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              checkout({ id: "checkout:primary" }),
              checkout({ id: "checkout:gone", path: "/test-gone", isMissing: true }),
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });
    await flushPromises();

    const [base, gone] = wrapper.findAll(".workdir-row .workdir-end");
    expect(base.get(".workdir-diff-nums").text()).toBe("+12\u22124");
    // E.4: a failure of one workdir is written in that row, and it takes the slot the counts
    // would have used, so a row never shows a failure next to figures it cannot have.
    expect(gone.text()).toBe("Directory missing");
    expect(gone.classes()).toContain("workdir-end-error");
    expect(gone.get(".workdir-end-note").text()).toBe("Directory missing");
    expect(gone.find(".ad").exists()).toBe(false);
    wrapper.unmount();
  });

  it("disables a missing directory and leaves closing it as its only action", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              {
                ...checkout({
                  id: "checkout:feature",
                  path: "/test-feature",
                  canonicalPath: "/test-feature",
                  isPrimary: false,
                  branch: "feature",
                }),
                sessions: [session("session:live", "Live", "checkout:feature")],
              },
              {
                ...checkout({
                  id: "checkout:gone",
                  path: "/test-gone",
                  canonicalPath: "/test-gone",
                  isPrimary: false,
                  branch: "temporary",
                  isMissing: true,
                }),
                sessions: [session("session:stale", "Stale", "checkout:gone")],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:feature",
        activeSessionId: null,
        isOpening: false,
      },
    });

    // The rows themselves, not the session children hanging under them.
    const [live, gone] = wrapper.findAll(".workdir-parent > .workdir-select");

    expect(live!.attributes("aria-disabled")).toBeUndefined();
    expect(gone!.attributes("aria-disabled")).toBe("true");
    // The reason the row is disabled stays in the row \u2014 in its own trailing slot, which is where the
    // counts would otherwise be: a failure and a line count are two different facts and a row never
    // shows both.
    expect(wrapper.get(".workdir-parent.missing .workdir-end-note").text()).toBe("Directory missing");
    // Nothing behind a directory that is gone is offered: none of its sessions, no shell. The row's
    // name is the session alone — nothing from the path reaches a terminal row.
    expect(
      wrapper.findAll('button[aria-label^="Terminal session:"]').map((row) => row.attributes("aria-label")),
    ).toEqual(["Terminal session: Live"]);
    expect(
      wrapper.findAll('button[aria-label^="New terminal for"]').map((row) => row.attributes("aria-label")),
    ).toEqual(["New terminal for feature"]);
    // A live worktree offers one way out, and it asks before it acts: archiving and deleting
    // are both answers to the same question, so the cross opens the dialog that holds them.
    expect(wrapper.find('button[aria-label="Remove or archive worktree feature"]').exists()).toBe(true);
    expect(wrapper.find('button[aria-label="Remove or archive worktree temporary"]').exists()).toBe(false);
    // One action per thing the row can do: the live worktree's own one, and the missing
    // one's close.
    expect(wrapper.findAll(".workdir-parent > .workdir-close")).toHaveLength(2);
    expect(wrapper.get('button[aria-label="Remove or archive worktree feature"]').attributes("title")).toBe(
      "Remove or archive worktree",
    );
    const close = wrapper.get('button[aria-label="Close missing checkout: temporary"]');
    expect(close.attributes("title")).toBe("Remove from list");

    await gone!.trigger("click");
    await live!.trigger("click");
    await close.trigger("click");
    await wrapper.get('button[aria-label="Remove or archive worktree feature"]').trigger("click");

    expect(wrapper.emitted("selectCheckout")).toEqual([["checkout:feature", false]]);
    expect(wrapper.emitted("closeMissing")).toEqual([["checkout:gone"]]);
    expect(wrapper.emitted("removeWorktree")).toEqual([["checkout:feature"]]);
    wrapper.unmount();
  });

  it("puts the actions of a repo in one menu, and says which one is open", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            checkouts: [
              { ...checkout({ id: "checkout:primary" }), branch: "main" },
              { ...checkout({ id: "checkout:feature", isPrimary: false }), branch: "feature" },
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
        archivedWorktrees: [{ id: "checkout:gone", repoId: "repo:test", path: "/test-gone", branch: "temporary" }],
      },
    });

    const more = wrapper.get('button[aria-label="Actions for test"]');
    // reka hands the list to the body, so the menu is read off the document and not off the panel.
    const menu = () => document.querySelector(".group-menu");
    expect(menu()).toBe(null);

    // The button is the way in, and what it opens is the repo's three actions in one list.
    await more.trigger("click");
    await flushPromises();
    expect(menu()?.getAttribute("role")).toBe("menu");
    expect(menu()?.textContent).toContain("New worktree");
    expect(menu()?.textContent).toContain("Restore archived worktrees");
    expect(menu()?.textContent).toContain("Remove from panel");
    // The count says how much there is to bring back, and nothing is offered for nothing.
    expect(menu()?.textContent).toContain("1");

    const restore = document.querySelector('[aria-label="Restore 1 archived worktree"]') as HTMLElement;
    restore.click();
    await flushPromises();
    expect(wrapper.emitted("restoreArchived")).toEqual([["repo:test"]]);
    // A row that was chosen closes the list behind it.
    expect(menu()).toBe(null);
    wrapper.unmount();
  });

  it("opens the repo menu from a right click on the header, and closes it on Escape", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [repo({ checkouts: [{ ...checkout({ id: "checkout:primary" }), branch: "main" }] })],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });
    const menu = () => document.querySelector(".group-menu");

    // A right click anywhere on the header is the way in that the button is not part of.
    await wrapper.get(".group-heading").trigger("contextmenu");
    await flushPromises();
    expect(menu()).not.toBe(null);

    // The list answers the Escape, which the panel used to answer with a listener of its own.
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    await flushPromises();
    expect(menu()).toBe(null);
    wrapper.unmount();
  });

  it("hangs the restore action on the repo root, and only while it has something to restore", async () => {
    const gitdir = { ...checkout({ id: "checkout:primary" }), branch: "main" };
    const worktree = {
      ...checkout({
        id: "checkout:feature",
        path: "/test-feature",
        canonicalPath: "/test-feature",
        isPrimary: false,
        branch: "feature",
      }),
    };
    const mounted = (archivedWorktrees: ArchivedCheckout[]) =>
      mount(Sidebar, {
        props: {
          repos: [repo({ checkouts: [gitdir, worktree] })],
          activeCheckoutId: null,
          activeSessionId: null,
          isOpening: false,
          archivedWorktrees,
        },
      });

    // Nothing archived, nothing to offer: a menu row that can only answer "nothing archived"
    // is a row that teaches the repo it sits on means nothing.
    const empty = mounted([]);
    await empty.get('button[aria-label="Actions for test"]').trigger("click");
    await flushPromises();
    expect(menuItems()).toEqual(["New worktree", "Remove from panel"]);
    empty.unmount();

    const wrapper = mounted([{ id: "checkout:gone", repoId: "repo:test", path: "/test-gone", branch: "temporary" }]);
    // The restore belongs to the repo, so it is nowhere in the rows, and the row's long branch
    // name is left the whole row to itself.
    expect(wrapper.find('button[aria-label^="Restore"]').exists()).toBe(false);

    await wrapper.get('button[aria-label="Actions for test"]').trigger("click");
    await flushPromises();
    const restore = document.querySelector('[aria-label="Restore 1 archived worktree"]') as HTMLElement;
    restore.click();
    await flushPromises();

    expect(wrapper.emitted("restoreArchived")).toEqual([["repo:test"]]);
    wrapper.unmount();
  });

  it("counts the plural in what it says, so the row never promises one of many", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [repo({ checkouts: [checkout({ id: "checkout:primary", branch: "main" })] })],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
        archivedWorktrees: [
          { id: "checkout:a", repoId: "repo:test", path: "/test-a", branch: "a" },
          { id: "checkout:b", repoId: "repo:test", path: "/test-b", branch: "b" },
        ],
      },
    });

    await wrapper.get('button[aria-label="Actions for test"]').trigger("click");
    await flushPromises();
    expect(document.querySelector('[aria-label="Restore 2 archived worktrees"]')).not.toBe(null);
    // And it is the plural that is said, never the singular about a pair.
    expect(document.querySelector('[aria-label="Restore 1 archived worktree"]')).toBe(null);
    wrapper.unmount();
  });
});

it.each([false, true])("draws a running session with no agent, awaiting reply: %s", (awaitingReply) => {
  // `attention` is only carried for a session that HAS an agent, so reading it alone drew a running
  // turn as idle grey — the row said nothing was happening while the service said a turn was open.
  // The session's own answer wins over whether there happens to be a mode to show.
  const wrapper = mount(Sidebar, {
    props: {
      repos: [
        repo({
          checkouts: [{ ...checkout({ id: "checkout:agent" }), sessions: [session("s", "zsh", "checkout:agent")] }],
        }),
      ],
      activeCheckoutId: "checkout:agent",
      activeSessionId: null,
      isOpening: false,
      agentRows: {
        "checkout:agent": {
          sessions: [
            {
              id: "ses_row_23",
              terminalId: null,
              title: "Untitled session",
              provider: "opencode",
              attention: awaitingReply ? "blocked" : "busy",
              agent: null,
              awaitingReply,
              running: true,
              updatedAt: Date.now(),
            },
          ],
        },
      },
      sessionRuntimeStatuses: {
        s: {
          state: "running",
          foregroundProcess: true,
          foregroundApp: "opencode",
          terminalTitle: "OC | Untitled session",
        },
      },
    },
  });
  const row = wrapper.get(".workdir-child");
  expect(row.classes()).toContain(awaitingReply ? "state-waiting" : "state-working");
  expect(wrapper.get('.workdir-child [aria-label^="Terminal session:"]').attributes("aria-label")).toContain(
    awaitingReply ? "Waiting" : "Working",
  );
  // And it says what is missing beside the name, which is not the same as saying nothing is happening.
  expect(row.get(".dm").text()).toBe("sin sesión");
  wrapper.unmount();
});

describe("folding a repository", () => {
  function twoRepos() {
    return mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:one",
            name: "Muster",
            root: "/dev/Muster",
            checkouts: [{ ...checkout({ id: "checkout:wt" }), sessions: [session("one", "zsh", "checkout:wt")] }],
          }),
          repo({
            id: "repo:two",
            name: "webapp",
            root: "/xstudio/webapp",
            checkouts: [{ ...checkout({ id: "checkout:app" }), sessions: [session("app", "zsh", "checkout:app")] }],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });
  }
  const groupOf = (wrapper: ReturnType<typeof twoRepos>, index: number) => wrapper.findAll(".workdir-group")[index]!;

  it("folds and unfolds the repository when its name is clicked", async () => {
    const wrapper = twoRepos();
    // Hiding is a class and a stylesheet rule, not a `v-if`: the list stays in the DOM and out of
    // sight, which is what a fold is. Nothing in the sidebar owns a process that a missing node
    // would have to tear down, and a rule that removed it would have to rebuild the whole list on
    // every expand.
    expect(groupOf(wrapper, 0).classes()).not.toContain("is-collapsed");
    expect(groupOf(wrapper, 0).findAll(".workdir-checkouts")).toHaveLength(1);

    await groupOf(wrapper, 0).get(".group-heading-text").trigger("click");
    expect(groupOf(wrapper, 0).classes()).toContain("is-collapsed");
    // And the other repository is untouched — one repo's fold is not the panel's.
    expect(groupOf(wrapper, 1).classes()).not.toContain("is-collapsed");
    expect(groupOf(wrapper, 1).findAll(".workdir-checkouts")).toHaveLength(1);

    await groupOf(wrapper, 0).get(".group-heading-text").trigger("click");
    expect(groupOf(wrapper, 0).classes()).not.toContain("is-collapsed");
    wrapper.unmount();
  });

  it("says which way the fold goes, to a keyboard, without painting it on the row", async () => {
    const wrapper = twoRepos();
    const control = groupOf(wrapper, 0).get(".group-heading-text");
    // A `div` with a click on it is not a control a keyboard can reach, so the name is a button and
    // carries the state the chevron draws.
    expect(control.element.tagName).toBe("BUTTON");
    expect(control.attributes("aria-expanded")).toBe("true");

    await control.trigger("click");
    expect(groupOf(wrapper, 0).get(".group-heading-text").attributes("aria-expanded")).toBe("false");
    // The name carries no glyph for the fold: the empty space under the header says so, and an icon
    // beside it would put a row's glyph on a row that is a heading.
    expect(groupOf(wrapper, 0).get(".group-heading-text").find("svg").exists()).toBe(false);
    expect(rule(".workdir-group.is-collapsed .workdir-checkouts")).toContain("display: none;");
    wrapper.unmount();
  });

  it("leaves the repository's own menu alone: its trigger must not fold it as well", async () => {
    const wrapper = twoRepos();
    await wrapper.get('[aria-label="Actions for Muster"]').trigger("click");
    await flushPromises();
    // The menu opened and the repository is still as it was.
    expect(groupOf(wrapper, 0).findAll(".workdir-checkouts")).toHaveLength(1);
    expect(groupOf(wrapper, 0).get(".group-heading-text").attributes("aria-expanded")).toBe("true");
    wrapper.unmount();
  });

  it("points at a clickable header, because a row that answers a click has to say so", () => {
    expect(rule(".group-heading-text")).toContain("cursor: pointer;");
  });
});

describe("sidebar drag and drop", () => {
  it("draws the terminals in the order the pane saved, not the order the database hands out", async () => {
    // The order is the pane's: it is what gets persisted and what a drag writes into. A sidebar that
    // ordered the sessions itself made every reorder invisible — saved, correct, and not drawn.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:test",
            kind: "git",
            name: "Muster",
            root: "/dev/Muster",
            checkouts: [
              {
                ...checkout({ id: "checkout:wt", path: "/dev/Muster/.worktrees/wt" }),
                sessions: [
                  session("one", "zsh", "checkout:wt"),
                  session("two", "zsh", "checkout:wt"),
                  session("three", "zsh", "checkout:wt"),
                ],
              },
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
        sessionOrder: { "checkout:wt": ["three", "one"] },
      },
    });
    // What the pane named first comes first, and the one it knows nothing about keeps its own place
    // after them rather than disappearing.
    expect(wrapper.findAll(".workdir-child[data-session-id]").map((row) => row.attributes("data-session-id"))).toEqual([
      "three",
      "one",
      "two",
    ]);

    // And an id the checkout no longer has is not drawn as a row.
    await wrapper.setProps({
      sessionOrder: { "checkout:wt": ["gone", "two"] },
    });
    expect(wrapper.findAll(".workdir-child[data-session-id]").map((row) => row.attributes("data-session-id"))).toEqual([
      "two",
      "one",
      "three",
    ]);
    wrapper.unmount();
  });

  function pointer(type: string, y: number) {
    const event = new Event(type, { bubbles: true, cancelable: true });
    Object.assign(event, { pointerId: 1, pointerType: "mouse", isPrimary: true, button: 0, clientX: 40, clientY: y });
    return event;
  }

  async function dragTo(source: Element, target: Element) {
    const oldHitTest = Object.getOwnPropertyDescriptor(document, "elementFromPoint");
    Object.defineProperty(document, "elementFromPoint", {
      configurable: true,
      value: vi.fn(() => target),
    });
    try {
      source.dispatchEvent(pointer("pointerdown", 100));
      window.dispatchEvent(pointer("pointermove", 140));
      await nextTick();
      window.dispatchEvent(pointer("pointerup", 140));
      await flushPromises();
    } finally {
      if (oldHitTest) Object.defineProperty(document, "elementFromPoint", oldHitTest);
      else Reflect.deleteProperty(document, "elementFromPoint");
    }
  }

  it("lets a Home terminal move to a Git checkout", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:home",
            kind: "plain",
            name: "Home",
            root: "/home",
            checkouts: [
              {
                ...checkout({ id: "checkout:home", repoId: "repo:home", path: "/home" }),
                sessions: [session("home", "zsh", "checkout:home")],
              },
            ],
          }),
          repo({
            id: "repo:git",
            name: "Muster",
            root: "/dev/Muster",
            checkouts: [checkout({ id: "checkout:git", repoId: "repo:git", path: "/dev/Muster" })],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        homeCheckoutId: "checkout:home",
        isOpening: false,
      },
    });
    const source = wrapper.get('[data-session-id="home"] .workdir-select').element;
    const target = wrapper.get('[data-workdir-checkout="checkout:git"]').element;
    try {
      await wrapper.get('[data-session-id="home"] .workdir-select').trigger("contextmenu");
      expect(wrapper.findAll(".move-menu [role=menuitem]").map((item) => item.text())).toEqual(["main"]);
      await dragTo(source, target);
      expect(wrapper.emitted("moveSession")).toEqual([["home", "checkout:git", 0]]);
    } finally {
      wrapper.unmount();
    }
  });

  it("lets a Git terminal move to a foreign repository", async () => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:source",
            name: "source",
            root: "/source",
            checkouts: [
              {
                ...checkout({ id: "checkout:source", repoId: "repo:source", path: "/source" }),
                sessions: [session("git", "zsh", "checkout:source")],
              },
              checkout({
                id: "checkout:sibling",
                repoId: "repo:source",
                path: "/source/.worktrees/feature",
                isPrimary: false,
                branch: "feature",
              }),
            ],
          }),
          repo({
            id: "repo:foreign",
            name: "foreign",
            root: "/foreign",
            checkouts: [
              checkout({ id: "checkout:foreign", repoId: "repo:foreign", path: "/foreign", branch: "other" }),
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });
    const source = wrapper.get('[data-session-id="git"] .workdir-select').element;
    const foreign = wrapper.get('[data-workdir-checkout="checkout:foreign"]').element;
    try {
      await wrapper.get('[data-session-id="git"] .workdir-select').trigger("contextmenu");
      const destinations = wrapper.findAll(".move-menu [role=menuitem]");
      expect(destinations.map((item) => item.text())).toEqual(["feature", "other"]);
      await destinations[1].trigger("click");
      expect(wrapper.emitted("moveSession")).toEqual([["git", "checkout:foreign", 0]]);
      await dragTo(source, foreign);
      expect(wrapper.emitted("moveSession")).toEqual([
        ["git", "checkout:foreign", 0],
        ["git", "checkout:foreign", 0],
      ]);
    } finally {
      wrapper.unmount();
    }
  });

  it.each(["missing", "unknown"] as const)("rejects a %s checkout as a move target", async (targetKind) => {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:home",
            kind: "plain",
            name: "Home",
            root: "/home",
            checkouts: [
              {
                ...checkout({ id: "checkout:home", repoId: "repo:home", path: "/home" }),
                sessions: [session("home", "zsh", "checkout:home")],
              },
            ],
          }),
          repo({
            id: "repo:git",
            name: "Muster",
            root: "/dev/Muster",
            checkouts: [
              checkout({ id: "checkout:git", repoId: "repo:git", path: "/dev/Muster" }),
              ...(targetKind === "missing"
                ? [checkout({ id: "checkout:missing", repoId: "repo:git", path: "/gone", isMissing: true })]
                : []),
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });
    const unknown = document.createElement("div");
    unknown.dataset.workdirCheckout = "checkout:unknown";
    const target =
      targetKind === "missing" ? wrapper.get('[data-workdir-checkout="checkout:missing"]').element : unknown;
    try {
      await dragTo(wrapper.get('[data-session-id="home"] .workdir-select').element, target);
      expect(wrapper.emitted("moveSession")).toBeUndefined();
    } finally {
      wrapper.unmount();
    }
  });

  it("draws the drop line in the slot the terminal lands in, not once at the end", () => {
    // The line that says where a terminal lands used to be drawn once, after the last row. A line
    // that stays at the end while the rows above it move says nothing about the position being offered,
    // so it is drawn per slot: inside the loop, before the row that sits in that slot.
    const styles = sidebarStyles();
    expect(styles).toContain('v-if="dropLineBefore(workdir.checkout, itemIndex)"');
    // And there is still a slot past the last row, because the pointer can land there.
    expect(styles).toContain('v-if="dropLineAfter(workdir.checkout)"');
    // Both count the same way the measurement counts: the dragged terminal is out of the way, or the
    // line is drawn one row above where it belongs and disappears on the next move — the test above
    // draws a real drag and would not have passed without this.
    expect(styles).toContain("orderedSessions(checkout).length - (isDraggedFrom(checkout) ? 1 : 0)");
    expect(styles).toContain("dropPointerY.value < box.top + box.height / 2");
    expect(styles).toContain('!row.classList.contains("new-item") && row.dataset.sessionId !== dragging?.id');
    // A terminal is a destination for its own worktree now, except the slot it already sits in:
    // dropping a row where it is would report a move that changes nothing. Its own slot is measured
    // in the order it is drawn, which is not the order the database hands out.
    expect(styles).toContain(
      "const current = orderedSessions(registeredTarget).findIndex((item) => item.id === session.id)",
    );
    expect(styles).toContain("return current < 0 || current === index ? null : target.id;");
    // Every terminal row carries its session id, which is what the slot is measured against.
    expect(styles).toContain(':data-session-id="item.session.id"');
  });

  it("draws the drop line as a row of the list, so its glyph sits on the terminals' axis", async () => {
    // The line was a box with a margin of its own, which put its icon four pixels left of every
    // terminal's: the same drag, two different columns. It is now a `workdir-row`, so the box, the
    // indent and the glyph axis are the terminals' own rules rather than a second set of them.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:test",
            kind: "git",
            name: "Muster",
            root: "/dev/Muster",
            checkouts: [
              {
                ...checkout({ id: "checkout:wt", path: "/dev/Muster/.worktrees/wt" }),
                sessions: [session("one", "zsh", "checkout:wt"), session("two", "zsh", "checkout:wt")],
              },
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
      attachTo: document.body,
    });

    const oldHitTest = Object.getOwnPropertyDescriptor(document, "elementFromPoint");
    const rows = wrapper.findAll(".workdir-child[data-session-id]");
    rows.forEach((row) => {
      Object.defineProperty(row.element, "getBoundingClientRect", { value: () => ({ top: 100, height: 26 }) });
    });
    Object.defineProperty(document, "elementFromPoint", {
      configurable: true,
      value: vi.fn(() => rows[1].element),
    });
    const pointer = (type: string, y: number) => {
      const event = new Event(type, { bubbles: true, cancelable: true });
      Object.assign(event, { pointerId: 1, pointerType: "mouse", isPrimary: true, button: 0, clientX: 40, clientY: y });
      return event;
    };

    try {
      rows[0].get("button").element.dispatchEvent(pointer("pointerdown", 100));
      window.dispatchEvent(pointer("pointermove", 140));
      // The drag state is reactive and the line is drawn from it, so the render is waited for rather
      // than read off a tick that has not happened yet.
      await nextTick();
      const line = wrapper.find(".terminal-drop-insertion");
      expect(line.exists()).toBe(true);
      // A row of the list, not a box beside it: the classes are what put it on the terminals' axis.
      expect(line.classes()).toContain("workdir-row");
      expect(line.classes()).toContain("workdir-child");
      // And it says nothing of its own about its box: no geometry that could disagree with the row's.
      expect(rule(".terminal-drop-insertion")).not.toMatch(/padding|margin|height|gap/);
      // The ghost carries the same indent, so the glyph under the cursor is on the column it will
      // land in rather than on the panel's own 8px.
      expect(rule(".terminal-drag-ghost")).toContain("padding: 0 12px;");
      // It is teleported to the body, so it is outside `.app-sidebar` and has to name the panel's
      // own type: inherited from the body it drew a name larger than the row it was copying.
      expect(rule(".terminal-drag-ghost")).toContain("font-size: 13px;");
      // And it stays a chip over the workspace rather than a banner across it, which is what a ghost
      // with no cap became.
      expect(rule(".terminal-drag-ghost")).toContain("max-width: 240px;");
      window.dispatchEvent(pointer("pointerup", 140));
      await flushPromises();
      expect(wrapper.emitted("moveSession")?.at(-1)).toEqual(["one", "checkout:wt", 1]);
    } finally {
      if (oldHitTest) Object.defineProperty(document, "elementFromPoint", oldHitTest);
      else Reflect.deleteProperty(document, "elementFromPoint");
      wrapper.unmount();
    }
  });
});

describe("a row's glyph keeps its state when the row is selected", () => {
  it("has no selected rule that could outrank the state rules", () => {
    // A rule for the selected row's glyph outranks every `.state-*` rule: three classes against two.
    // Selecting a terminal with a process in it therefore repainted its green glyph in primary ink, and
    // the row said, for as long as it was selected, that nothing was running in it. Selection is said by
    // the tint and the accent edge instead, so no such rule may exist.
    const styles = sidebarStyles();
    expect(styles).not.toMatch(/\.workdir-row\.selected[^{]*\.workdir-icon[^{]*\{[^}]*color/);
    // And each state still names its own colour.
    expect(rule(".state-working .workdir-icon")).toContain("color: var(--muster-accent);");
    expect(rule(".state-waiting .workdir-icon")).toContain("color: var(--muster-warning);");
    expect(rule(".state-failed .workdir-icon")).toContain("color: var(--muster-danger-fg);");
    expect(rule(".state-running .workdir-icon")).toContain("color: var(--muster-success);");
  });
});

/**
 * What one character is worth, in a font that is only that.
 *
 * The panel weighs a label by putting the words in a specimen and reading it, so a test that says
 * "the room is this wide" is saying the same thing as "this many characters fit" — which is the
 * only way a claim about a truncation can be written down.
 */
const CHAR_PX = 7;

type FitCallback = () => void;
const fitObservers: { callback: FitCallback; targets: Element[] }[] = [];

class FitResizeObserver {
  callback: FitCallback;
  targets: Element[] = [];

  constructor(callback: FitCallback) {
    this.callback = callback;
    fitObservers.push(this);
  }

  observe(target: Element) {
    this.targets.push(target);
  }

  unobserve(target: Element) {
    this.targets = this.targets.filter((element) => element !== target);
  }

  disconnect() {
    this.targets = [];
  }
}

/** How wide one box is, which happy-dom has no opinion about at all. */
function room(element: Element, px: number) {
  Object.defineProperty(element, "clientWidth", { configurable: true, value: px });
}

/** The panel has been resized, which is the only box it watches. */
function resizePanel() {
  for (const observer of fitObservers) observer.callback();
}

/** One pass per frame, and the frame has to happen. */
async function weighed() {
  await new Promise((resolve) => setTimeout(resolve, 5));
  await nextTick();
}

/**
 * Counts the readings a weighing pass makes of a row's own box, which is what the pass costs.
 *
 * `contentWidth` and `gapOf` both ask the browser what a row is drawn at, once per row, so the
 * count of those readings is the count of rows weighed — and a pass that changed nothing is a pass
 * the panel does not have to make.
 */
function countRowMeasurements() {
  const real = window.getComputedStyle;
  let readings = 0;
  window.getComputedStyle = ((element: Element, pseudo?: string | null) => {
    if (element.classList.contains("workdir-select")) readings += 1;
    return real.call(window, element, pseudo);
  }) as typeof window.getComputedStyle;
  return Object.assign(() => readings, { restore: () => (window.getComputedStyle = real) });
}

describe("a label is weighed against its row rather than cut between its halves", () => {
  beforeEach(() => {
    fitObservers.length = 0;
    vi.stubGlobal("ResizeObserver", FitResizeObserver);
    // Only the specimens are measured, so only the specimens answer with a width.
    Object.defineProperty(HTMLElement.prototype, "scrollWidth", {
      configurable: true,
      get(this: HTMLElement) {
        return this.classList.contains("fit-probe") ? (this.textContent?.length ?? 0) * CHAR_PX : 0;
      },
    });
  });

  afterEach(() => {
    Reflect.deleteProperty(HTMLElement.prototype, "scrollWidth");
    vi.unstubAllGlobals();
  });

  async function mountWebapp() {
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:webapp",
            name: "webapp",
            root: "/Users/matiasmarani/Trabajo/xStudio/webapp",
            checkouts: [
              {
                ...checkout({
                  id: "checkout:webapp",
                  repoId: "repo:webapp",
                  path: "/Users/matiasmarani/Trabajo/xStudio/webapp",
                  canonicalPath: "/Users/matiasmarani/Trabajo/xStudio/webapp",
                  branch: "main",
                }),
                sessions: [session("a", "zsh", "checkout:webapp")],
              },
              {
                ...checkout({
                  id: "checkout:feature",
                  repoId: "repo:webapp",
                  path: "/Users/matiasmarani/Trabajo/xStudio/webapp-feature",
                  canonicalPath: "/Users/matiasmarani/Trabajo/xStudio/webapp-feature",
                  isPrimary: false,
                  branch: "feature/sidebar/weigh-a-label",
                }),
                sessions: [],
              },
            ],
          }),
        ],
        activeCheckoutId: "checkout:webapp",
        activeSessionId: "a",
        isOpening: false,
        agentRows: {
          "checkout:webapp": {
            sessions: [
              {
                id: "ses_row_24",
                title: "Review the duplicated rows",
                provider: "opencode",
                attention: "none",
                agent: { label: "plan", color: null, attention: "none" },
                running: false,
                terminalId: null,
                awaitingReply: false,
                updatedAt: Date.now(),
              },
            ],
          },
        },
        sessionRuntimeStatuses: {
          a: {
            state: "running",
            foregroundProcess: true,
            foregroundApp: "opencode",
            terminalTitle: "OC | Review the duplicated rows",
          },
        },
      },
    });
    await weighed();
    return wrapper;
  }

  it("draws a repo's whole path beside its whole name when the panel has the room", async () => {
    const wrapper = await mountWebapp();
    // `webapp` is 6 characters and `…/xStudio/webapp` is 16, so 170px of header holds the two.
    room(wrapper.get(".group-heading-text").element, 170);
    resizePanel();
    await weighed();

    expect(wrapper.get(".group-name").text()).toBe("webapp");
    expect(wrapper.get(".group-heading-text .group-path").text()).toBe("…/xStudio/webapp");
    wrapper.unmount();
  });

  it("gives the path a segment away before it lets the name lose a character", async () => {
    const wrapper = await mountWebapp();
    // 120px holds the name and one segment of the path and not the two it started with. The measured
    // defect was both halves ellipsized at once, which is what two halves sharing one shrink factor
    // does: `weba…` beside `…/xStudio/weba…`.
    room(wrapper.get(".group-heading-text").element, 120);
    resizePanel();
    await weighed();

    expect(wrapper.get(".group-name").text()).toBe("webapp");
    expect(wrapper.get(".group-heading-text .group-path").text()).toBe("…/webapp");
    wrapper.unmount();
  });

  it("drops the path rather than the name, and keeps saying it to whatever is not reading pixels", async () => {
    const wrapper = await mountWebapp();
    // 60px holds the name and neither form of the path.
    room(wrapper.get(".group-heading-text").element, 60);
    resizePanel();
    await weighed();

    // The name is the whole row now, and it is whole.
    expect(wrapper.get(".group-name").text()).toBe("webapp");
    expect(wrapper.find(".group-heading-text .group-path").exists()).toBe(false);
    // What stopped being drawn did not stop being said: the row still reads whole to a screen
    // reader, and its tooltip carries the whole path.
    expect(wrapper.get(".group-heading-text").text()).toContain("…/xStudio/webapp");
    expect(wrapper.get(".group-heading-text").attributes("title")).toBe("/Users/matiasmarani/Trabajo/xStudio/webapp");
    wrapper.unmount();
  });

  it("gives a branch its namespace away rather than cutting it out of the middle", async () => {
    const wrapper = await mountWebapp();
    const control = wrapper.get('[data-workdir-checkout="checkout:feature"] .workdir-select').element;
    // `feature/sidebar/weigh-a-label` is 27 characters; 150px holds it and `…/weigh-a-label`.
    room(control, 150);
    room(wrapper.get(".group-heading-text").element, 170);
    resizePanel();
    await weighed();

    expect(wrapper.get('[data-workdir-checkout="checkout:feature"] .nm').text()).toBe("…/weigh-a-label");
    // And the whole branch is still what the row says on hover.
    expect(wrapper.get('[data-workdir-checkout="checkout:feature"] .workdir-select').attributes("title")).toBe(
      "feature/sidebar/weigh-a-label — /Users/matiasmarani/Trabajo/xStudio/webapp-feature",
    );
    wrapper.unmount();
  });

  it("takes the mode word away whole before the terminal's name is touched", async () => {
    const wrapper = await mountWebapp();
    const row = wrapper.get(".workdir-child");
    const control = row.get(".workdir-select").element;
    // `Review the duplicated rows` is 26 characters and `plan` is 4, so the two together want 210px
    // and the name alone wants 182. 240px holds both and 200px does not, and a name cut in half to
    // keep half a mode word is the worse of the two rungs.
    room(control, 240);
    room(wrapper.get(".group-heading-text").element, 170);
    resizePanel();
    await weighed();
    expect(row.get(".dm").text()).toBe("plan");
    expect(row.get(".nm").text()).toBe("Review the duplicated rows");

    room(control, 200);
    resizePanel();
    await weighed();
    expect(row.find(".dm").exists()).toBe(false);
    expect(row.get(".nm").text()).toBe("Review the duplicated rows");
    // The word the row dropped is in its accessible name and its tooltip, which is where a row that
    // cannot fit it still says it.
    expect(row.get(".workdir-select").attributes("aria-label")).toContain("plan");
    wrapper.unmount();
  });

  it("reads the room off the row rather than off the label that is being squeezed", async () => {
    const wrapper = await mountWebapp();
    const row = wrapper.get(".workdir-child");
    // The row is wide and the label reports itself as clipped to nothing, which is what a label
    // that overflows says about itself. Weighing the row is what keeps this from narrowing on every
    // pass until the name is one word long.
    room(row.get(".workdir-select").element, 240);
    room(row.get(".lbl").element, 0);
    room(wrapper.get(".group-heading-text").element, 170);
    resizePanel();
    await weighed();
    resizePanel();
    await weighed();

    expect(row.get(".dm").text()).toBe("plan");
    expect(row.get(".nm").text()).toBe("Review the duplicated rows");
    wrapper.unmount();
  });

  it("draws every label whole on a panel that has never been measured", async () => {
    // No room is known before the panel is laid out, and a room of zero is not a room nothing fits:
    // happy-dom lays nothing out, and a panel that has not been mounted has nothing to weigh either.
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:webapp",
            name: "webapp",
            root: "/Users/matiasmarani/Trabajo/xStudio/webapp",
            checkouts: [
              {
                ...checkout({
                  id: "checkout:webapp",
                  repoId: "repo:webapp",
                  path: "/Users/matiasmarani/Trabajo/xStudio/webapp",
                  canonicalPath: "/Users/matiasmarani/Trabajo/xStudio/webapp",
                }),
              },
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });
    await weighed();
    resizePanel();
    await weighed();

    expect(wrapper.get(".group-name").text()).toBe("webapp");
    expect(wrapper.get(".group-heading-text .group-path").text()).toBe("…/xStudio/webapp");
    wrapper.unmount();
  });

  it("re-weighs when the type changes, which is the one thing that moves no box", async () => {
    const wrapper = await mountWebapp();
    room(wrapper.get(".group-heading-text").element, 60);
    resizePanel();
    await weighed();
    expect(wrapper.find(".group-heading-text .group-path").exists()).toBe(false);

    // A font scale moves every word and not one box, so nothing this panel watches would say so; the
    // widths it kept are in the old type and are of no use in the new one.
    await wrapper.setProps({ fontScale: 1.5 });
    await weighed();
    expect(wrapper.get(".group-name").text()).toBe("webapp");
    wrapper.unmount();
  });

  it("holds its specimens out of the flow and out of sight", () => {
    // Four specimens, one per type size a label is drawn in, each carrying the class of the thing it
    // weighs: a specimen that is not the type that row is drawn in reports a width the row has not.
    const styles = sidebarStyles();
    expect(rule(".fit-probe")).toContain("position: absolute;");
    expect(rule(".fit-probe")).toContain("visibility: hidden;");
    expect(rule(".fit-probe.group-name")).toContain("min-width: 0;");
    expect(rule(".fit-probe.dm")).toContain("min-width: 0;");
    expect(styles).toContain('class="fit-probe group-name"');
    expect(styles).toContain('class="fit-probe group-path"');
    expect(styles).toContain('class="fit-probe nm"');
    expect(styles).toContain('class="fit-probe dm"');
  });

  it("re-weighs labels when collapsed or edited rows become visible again", async () => {
    const measures = countRowMeasurements();
    const wrapper = await mountWebapp();
    const before = measures();

    await wrapper.get(".workdir-fold").trigger("click");
    await wrapper.setProps({ fontScale: 1.25 });
    await weighed();
    const hidden = measures();
    expect(hidden).toBeGreaterThan(before);
    await wrapper.get(".workdir-fold").trigger("click");
    await weighed();
    expect(measures()).toBeGreaterThan(hidden);

    const row = wrapper.get(".workdir-child .workdir-select");
    await row.trigger("dblclick");
    await wrapper.setProps({ fontScale: 1.5 });
    await weighed();
    const editing = measures();
    await wrapper.get(".workdir-rename").trigger("keydown.esc");
    await weighed();
    expect(measures()).toBeGreaterThan(editing);
    measures.restore();
    wrapper.unmount();
  });

  it("does not weigh the panel again when a terminal only reports its status", async () => {
    // A running terminal publishes a fresh status on every poll, whether or not anything about it
    // changed, and the panel's rows are rebuilt from it every time. What a weighing pass reads is the
    // words of the rows and the panel's own box, so a poll that changes neither is not a reason to
    // walk every row's layout again — which is what the pass costs.
    const measures = countRowMeasurements();
    // The row's time is read from the clock each time the rows are rebuilt, and the rows are rebuilt
    // on every poll: that is where this row's time comes from, and it keeps coming from there.
    let now = 1_000_000;
    const dateNow = vi.spyOn(Date, "now").mockImplementation(() => now);
    const runtime: TerminalSessionStatus = {
      state: "running",
      foregroundProcess: true,
      foregroundApp: "opencode",
      terminalTitle: "OC | Review the duplicated rows",
    };
    const wrapper = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:webapp",
            name: "webapp",
            root: "/webapp",
            checkouts: [
              {
                ...checkout({ id: "checkout:webapp", repoId: "repo:webapp", path: "/webapp" }),
                sessions: [session("a", "zsh", "checkout:webapp")],
              },
              checkout({ id: "checkout:other", repoId: "repo:webapp", path: "/other", isPrimary: false }),
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
        agentRows: {
          "checkout:webapp": {
            sessions: [
              {
                id: "ses_row_24",
                title: "Review the duplicated rows",
                provider: "opencode",
                attention: "none",
                agent: { label: "plan", color: null, attention: "none" },
                running: false,
                terminalId: null,
                awaitingReply: false,
                updatedAt: now - 5_000,
              },
            ],
          },
        },
        sessionRuntimeStatuses: { a: runtime },
      },
    });
    await weighed();
    const afterMount = measures();
    expect(wrapper.get(".workdir-end-time").text()).toBe("5s");

    // The same terminal, saying the same thing in a new object a second later: the rows are rebuilt,
    // the words are identical and the clock still moves, so nothing is weighed.
    now += 1_000;
    await wrapper.setProps({ sessionRuntimeStatuses: { a: { ...runtime } } });
    await weighed();
    expect(measures()).toBe(afterMount);
    expect(wrapper.get(".workdir-end-time").text()).toBe("6s");

    // A different word is a different width, and that is a reason.
    await wrapper.setProps({ sessionRuntimeStatuses: { a: { ...runtime, foregroundApp: "vim" } } });
    await weighed();
    expect(measures()).toBeGreaterThan(afterMount);
    measures.restore();
    dateNow.mockRestore();
    wrapper.unmount();
  });

  it("offers the move menu only where there is somewhere to move to", async () => {
    // The row asks whether it can move at all, and a panel of one checkout has nowhere to send any of
    // its terminals: its own worktree is not a destination. A second checkout is, so the same panel
    // does offer one — which is why the answer is asked once per pass and not once per candidate.
    const one = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:solo",
            name: "solo",
            root: "/solo",
            checkouts: [
              {
                ...checkout({ id: "checkout:solo", repoId: "repo:solo", path: "/solo" }),
                sessions: [session("one", "zsh", "checkout:solo"), session("two", "zsh", "checkout:solo")],
              },
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });
    expect(
      one.findAll('button[aria-label^="Terminal session:"]').map((row) => row.attributes("aria-haspopup")),
    ).toEqual([undefined, undefined]);
    await one.get('[data-session-id="one"] .workdir-select').trigger("contextmenu");
    expect(one.find(".move-menu").exists()).toBe(false);
    one.unmount();

    const two = mount(Sidebar, {
      props: {
        repos: [
          repo({
            id: "repo:solo",
            name: "solo",
            root: "/solo",
            checkouts: [
              {
                ...checkout({ id: "checkout:solo", repoId: "repo:solo", path: "/solo" }),
                sessions: [session("one", "zsh", "checkout:solo")],
              },
              checkout({ id: "checkout:other", repoId: "repo:solo", path: "/other", isPrimary: false, branch: "y" }),
            ],
          }),
        ],
        activeCheckoutId: null,
        activeSessionId: null,
        isOpening: false,
      },
    });
    expect(two.get('button[aria-label^="Terminal session:"]').attributes("aria-haspopup")).toBe("menu");
    // The list is built when the menu opens, and it is the other checkout and nothing else.
    await two.get('[data-session-id="one"] .workdir-select').trigger("contextmenu");
    expect(two.findAll(".move-menu [role=menuitem]").map((item) => item.text())).toEqual(["y"]);

    const movedRepos = (two.props("repos") as Repo[]).map((item) => ({
      ...item,
      checkouts: item.checkouts.map((checkout, index) => ({
        ...checkout,
        sessions: index === 0 ? [] : [session("one", "zsh", "checkout:other")],
      })),
    }));
    await two.setProps({ repos: movedRepos });
    expect(two.findAll(".move-menu [role=menuitem]").map((item) => item.text())).toEqual(["main"]);
    await two.find(".move-menu [role=menuitem]").trigger("click");
    expect(two.emitted("moveSession")).toEqual([["one", "checkout:solo", 0]]);
    two.unmount();
  });
});
