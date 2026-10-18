import { describe, expect, it } from "vitest";
import {
  agentSessionTitle,
  checkoutForWorkingDirectory,
  createCheckout,
  displayCheckoutPath,
  createPlainRepo,
  createWorkspaceState,
  getActiveCheckout,
  openRepo,
  resolveActiveSession,
  selectCheckout,
  selectSession,
  sessionRowTitle,
  sessionTitle,
  workdirIconKind,
  type Checkout,
  type Repo,
  type Session,
} from "./workspace";

const openedFolder = (path: string, name = "repo") => ({ path, name });

/** A repo with a root and one worktree, which is the shape the icon rule is asked about. */
function gitRepo(): Repo {
  const repoId = "repo:/work/app";
  return {
    id: repoId,
    kind: "git",
    name: "app",
    root: "/work/app",
    checkouts: [
      createCheckout({ repoId, path: "/work/app", canonicalPath: "/work/app", isPrimary: true, branch: "main" }),
      createCheckout({
        repoId,
        path: "/work/app-feature",
        canonicalPath: "/work/app-feature",
        isPrimary: false,
        branch: "feature",
      }),
    ],
    createdAt: "2026-01-01T00:00:00Z",
    lastOpenedAt: "2026-01-01T00:00:00Z",
  };
}

describe("checkoutForWorkingDirectory", () => {
  function reposWith(checkouts: Checkout[]): Repo[] {
    return [...new Set(checkouts.map((checkout) => checkout.repoId))].map((id) => ({
      ...gitRepo(),
      id,
      checkouts: checkouts.filter((checkout) => checkout.repoId === id),
    }));
  }
  const sibling = createCheckout({
    repoId: "repo:/work/app",
    path: "/work/app-feature",
    canonicalPath: "/work/app-feature",
    isPrimary: false,
    branch: "feature",
  });
  const elsewhere = createCheckout({
    repoId: "repo:/work/other",
    path: "/work/other",
    canonicalPath: "/work/other",
    isPrimary: true,
    branch: "main",
  });
  const root = createCheckout({
    repoId: "repo:/work/app",
    path: "/work/app",
    canonicalPath: "/work/app",
    isPrimary: true,
    branch: "main",
  });

  it("follows a shell into any registered checkout from Home or Git", () => {
    const home = createPlainRepo({ path: "/work", name: "Home" }, "now");
    const repo = gitRepo();
    const plain = createPlainRepo({ path: "/notes", name: "Notes" }, "now");
    const other = { ...gitRepo(), id: elsewhere.repoId, root: elsewhere.path, checkouts: [elsewhere] };
    const repos = [home, repo, plain, other];
    expect(checkoutForWorkingDirectory(repos, home.checkouts[0].id, root.path)?.id).toBe(root.id);
    expect(checkoutForWorkingDirectory(repos, home.checkouts[0].id, sibling.path)?.id).toBe(sibling.id);
    expect(checkoutForWorkingDirectory(repos, home.checkouts[0].id, plain.root)?.id).toBe(plain.checkouts[0].id);
    expect(checkoutForWorkingDirectory(repos, root.id, sibling.path)?.id).toBe(sibling.id);
    expect(checkoutForWorkingDirectory(repos, sibling.id, root.path)?.id).toBe(root.id);
    expect(checkoutForWorkingDirectory(repos, root.id, plain.root)?.id).toBe(plain.checkouts[0].id);
    expect(checkoutForWorkingDirectory(repos, root.id, elsewhere.path)?.id).toBe(elsewhere.id);
    expect(checkoutForWorkingDirectory(repos, home.checkouts[0].id, "/work/other-folder")?.id).toBe(
      home.checkouts[0].id,
    );
    expect(checkoutForWorkingDirectory(repos, root.id, root.path + "/src")?.id).toBe(root.id);
    expect(checkoutForWorkingDirectory(repos, "checkout:unknown", root.path)).toBeNull();
    repo.checkouts[1].isMissing = true;
    expect(checkoutForWorkingDirectory(repos, home.checkouts[0].id, sibling.path)?.id).not.toBe(sibling.id);
  });

  it("names the sibling worktree a shell changed directory into, however deep", () => {
    const checkouts = [root, sibling, elsewhere];

    expect(checkoutForWorkingDirectory(reposWith(checkouts), root.id, "/work/app-feature")?.id).toBe(sibling.id);
    // A directory inside the worktree is still that worktree: nobody `cd`s into its root exactly.
    expect(checkoutForWorkingDirectory(reposWith(checkouts), root.id, "/work/app-feature/src/deep")?.id).toBe(
      sibling.id,
    );
    // A trailing separator is how a shell spells the root of the same directory.
    expect(checkoutForWorkingDirectory(reposWith(checkouts), root.id, "/work/app-feature/")?.id).toBe(sibling.id);
  });

  it("names the worktree the shell is already under, which is what stops a row moving again", () => {
    // The invariant used to live here as a null: a row already under the worktree the shell sits in
    // is answered with itself, and the caller compares ids. A shell sitting still in a worktree
    // nested inside its own repository would otherwise be handed back and forth between the two,
    // because both contain it and only one of them is the one it is in.
    const checkouts = [root, sibling, elsewhere];

    expect(checkoutForWorkingDirectory(reposWith(checkouts), root.id, "/work/app")?.id).toBe(root.id);
    expect(checkoutForWorkingDirectory(reposWith(checkouts), root.id, "/work/app/src")?.id).toBe(root.id);
    expect(checkoutForWorkingDirectory(reposWith(checkouts), sibling.id, "/work/app-feature/src")?.id).toBe(sibling.id);
  });

  it("names the worktree itself rather than the repository holding it", () => {
    // A worktree created inside its own repository — what `git worktree add ../app/.worktrees/x`
    // makes — has both directories containing it, and only the longer one is where the shell is.
    const nested = createCheckout({
      repoId: root.repoId,
      path: "/work/app/.worktrees/feature",
      canonicalPath: "/work/app/.worktrees/feature",
      isPrimary: false,
      branch: "feature",
    });
    // The order is the one that used to answer with whichever came first.
    for (const checkouts of [
      [root, nested],
      [nested, root],
    ]) {
      expect(checkoutForWorkingDirectory(reposWith(checkouts), root.id, "/work/app/.worktrees/feature/src")?.id).toBe(
        nested.id,
      );
    }
    // Once the row is under the worktree, the same directory answers with the worktree still.
    expect(checkoutForWorkingDirectory(reposWith([root, nested]), nested.id, "/work/app/.worktrees/feature")?.id).toBe(
      nested.id,
    );
  });

  it("names nothing for a directory in no checkout or a missing one", () => {
    const missing = createCheckout({
      repoId: "repo:/work/app",
      path: "/work/app-gone",
      canonicalPath: "/work/app-gone",
      isPrimary: false,
      branch: "gone",
      isMissing: true,
    });

    expect(checkoutForWorkingDirectory(reposWith([root, sibling]), root.id, "/tmp")).toBeNull();
    // A shell may cross repository boundaries when the directory names a registered checkout.
    expect(checkoutForWorkingDirectory(reposWith([root, elsewhere]), root.id, "/work/other")?.id).toBe(elsewhere.id);
    // A worktree whose directory is gone is not a place to work.
    expect(checkoutForWorkingDirectory(reposWith([root, missing]), root.id, "/work/app-gone")).toBeNull();
  });

  it("refuses equally deep matches from distinct checkouts", () => {
    const duplicate = {
      ...createCheckout({
        repoId: "repo:/work/other",
        path: "/private/work/app-feature",
        canonicalPath: sibling.canonicalPath,
        isPrimary: true,
        branch: "main",
      }),
      id: "checkout:duplicate-path",
    };

    const repos = reposWith([root, sibling, duplicate]);
    expect(checkoutForWorkingDirectory(repos, root.id, sibling.path)).toBeNull();
    expect(checkoutForWorkingDirectory(repos, root.id, duplicate.path + "/src")).toBeNull();
  });

  it("compares the directory the OS spells against the worktree both ways", () => {
    // macOS records `/var/...` as `/private/var/...`, and which of the two a checkout was stored
    // under is not something this can normalise away.
    const stored = createCheckout({
      repoId: "repo:/work/app",
      path: "/var/tmp/app",
      canonicalPath: "/var/tmp/app",
      isPrimary: true,
      branch: "main",
    });
    const linked = createCheckout({
      repoId: "repo:/work/app",
      path: "/private/var/tmp/app",
      canonicalPath: "/private/var/tmp/app",
      isPrimary: false,
      branch: "feature",
    });

    expect(checkoutForWorkingDirectory(reposWith([stored, linked]), stored.id, "/private/var/tmp/app")?.id).toBe(
      linked.id,
    );
  });

  it("does not treat a longer sibling name as containing the shorter one", () => {
    // `/work/app-feature-2` and `/work/app-feature` share a prefix and nothing else.
    const longer = createCheckout({
      repoId: "repo:/work/app",
      path: "/work/app-feature-2",
      canonicalPath: "/work/app-feature-2",
      isPrimary: false,
      branch: "other",
    });

    expect(checkoutForWorkingDirectory(reposWith([root, sibling, longer]), root.id, "/work/app-feature-2")?.id).toBe(
      longer.id,
    );
  });
});

describe("workspace domain", () => {
  it("shortens only paths inside the user's home directory", () => {
    expect(displayCheckoutPath("/Users/me/.docker/config", "/Users/me")).toBe("~/.docker/config");
    expect(displayCheckoutPath("/Users/other/project", "/Users/me")).toBe("/Users/other/project");
    expect(displayCheckoutPath("/Users/meet/project", "/Users/me")).toBe("/Users/meet/project");
  });
  it("models a plain directory as one primary checkout with path-based identity", () => {
    const repo = createPlainRepo(openedFolder("/work/plain", "plain"), "2026-01-01T00:00:00Z");

    expect(repo).toMatchObject({
      id: "repo:/work/plain",
      kind: "plain",
      root: "/work/plain",
      createdAt: "2026-01-01T00:00:00Z",
    });
    expect(repo.checkouts).toHaveLength(1);
    expect(repo.checkouts[0]).toMatchObject({
      id: "checkout:/work/plain",
      repoId: repo.id,
      canonicalPath: "/work/plain",
      isPrimary: true,
      changedFiles: 0,
      sessions: [],
    });
    expect("sessions" in repo).toBe(false);
  });

  it("gives the primary checkout and worktrees the same checkout capabilities", () => {
    const repoId = "repo:/work/app";
    const primary = createCheckout({
      repoId,
      path: "/work/app",
      canonicalPath: "/work/app",
      isPrimary: true,
      branch: "main",
    });
    const worktree = createCheckout({
      repoId,
      path: "/work/app-feature",
      canonicalPath: "/work/app-feature",
      isPrimary: false,
      branch: "feature",
    });
    const repo: Repo = {
      id: repoId,
      kind: "git",
      name: "app",
      root: "/work/app",
      checkouts: [primary, worktree],
      createdAt: "2026-01-01T00:00:00Z",
      lastOpenedAt: "2026-01-01T00:00:00Z",
    };

    expect(primary).toHaveProperty("sessions", []);
    expect(worktree).toHaveProperty("sessions", []);
    expect(primary).toHaveProperty("changedFiles", 0);
    expect(worktree).toHaveProperty("changedFiles", 0);
    expect(repo.checkouts.map(({ isPrimary }) => isPrimary)).toEqual([true, false]);
  });

  it("focuses an existing canonical checkout instead of duplicating a symlink alias", () => {
    const state = openRepo(createWorkspaceState(), createPlainRepo(openedFolder("/work/app", "app"), "first-open"));
    const alias: Repo = {
      ...createPlainRepo(openedFolder("/work/app", "alias"), "reopened"),
      checkouts: [
        createCheckout({
          repoId: "repo:/work/app",
          path: "/work/app-link",
          canonicalPath: "/work/app",
          isPrimary: true,
        }),
      ],
    };

    const reopened = openRepo(state, alias);

    expect(reopened.repos).toHaveLength(1);
    expect(reopened.activeCheckoutId).toBe("checkout:/work/app");
    expect(reopened.repos[0].lastOpenedAt).toBe("reopened");
  });

  it("focuses the specifically opened worktree instead of falling back to primary", () => {
    const repoId = "repo:/work/app";
    const primary = createCheckout({
      repoId,
      path: "/work/app",
      canonicalPath: "/work/app",
      isPrimary: true,
    });
    const worktree = createCheckout({
      repoId,
      path: "/work/app-feature",
      canonicalPath: "/work/app-feature",
      isPrimary: false,
      branch: "feature",
    });
    const repo: Repo = {
      id: repoId,
      kind: "git",
      name: "app",
      root: "/work/app",
      checkouts: [primary, worktree],
      createdAt: "first-open",
      lastOpenedAt: "first-open",
    };

    const opened = openRepo(createWorkspaceState(), repo, worktree.id);

    expect(opened.activeCheckoutId).toBe(worktree.id);
    expect(opened.repos[0].checkouts.map((checkout) => checkout.id)).toEqual([primary.id, worktree.id]);
  });

  it("switches checkouts and sessions independently while keeping sessions owned", () => {
    const repoId = "repo:/work/app";
    const shell = {
      id: "session-shell",
      type: "shell" as const,
      checkoutId: "checkout:/work/app",
      name: "zsh",
      createdAt: "now",
      status: "inactive" as const,
    };
    const worktreeShell = {
      id: "session-worktree-shell",
      type: "shell" as const,
      checkoutId: "checkout:/work/app-feature",
      name: "bash",
      createdAt: "now",
      status: "inactive" as const,
    };
    const primary = createCheckout({
      repoId,
      path: "/work/app",
      canonicalPath: "/work/app",
      isPrimary: true,
      sessions: [shell],
    });
    const worktree = createCheckout({
      repoId,
      path: "/work/app-feature",
      canonicalPath: "/work/app-feature",
      isPrimary: false,
      sessions: [worktreeShell],
    });
    const repo: Repo = {
      id: repoId,
      kind: "git",
      name: "app",
      root: "/work/app",
      checkouts: [primary, worktree],
      createdAt: "now",
      lastOpenedAt: "now",
    };

    let state = openRepo(createWorkspaceState(), repo);
    state = selectSession(state, shell.id);
    expect(state).toMatchObject({ activeCheckoutId: primary.id, activeSessionId: shell.id });

    state = selectCheckout(state, worktree.id);
    expect(state).toMatchObject({ activeCheckoutId: worktree.id, activeSessionId: null });
    expect(getActiveCheckout(state)).toBe(worktree);

    state = selectSession(state, worktreeShell.id);
    expect(state).toMatchObject({ activeCheckoutId: worktree.id, activeSessionId: worktreeShell.id });
    expect(primary.sessions).toEqual([shell]);
    expect(worktree.sessions).toEqual([worktreeShell]);
  });

  it("resolves the selected terminal, and the newest one when the selection names nothing", () => {
    // The one rule the sidebar row and the main panel both ask, and the reason they cannot disagree:
    // closing the selected terminal clears `activeSessionId` and selecting another workdir clears it
    // too, and a reader that answered from the stored id alone showed nothing while the panel was
    // showing a terminal.
    const checkout = createCheckout({
      repoId: "repo:/work/app",
      path: "/work/app",
      canonicalPath: "/work/app",
      isPrimary: true,
      sessions: [
        {
          id: "session-one",
          type: "shell",
          checkoutId: "checkout:/work/app",
          name: "zsh",
          createdAt: "1",
          status: "active",
        },
        {
          id: "session-two",
          type: "shell",
          checkoutId: "checkout:/work/app",
          name: "zsh",
          createdAt: "2",
          status: "active",
        },
      ] satisfies Session[],
    });

    // The selection wins, whatever its position in the list.
    expect(resolveActiveSession(checkout, "session-one")?.id).toBe("session-one");
    // Nothing stored — the close, or the workdir switch: the newest, which is what the pane's last
    // view already was.
    expect(resolveActiveSession(checkout, null)?.id).toBe("session-two");
    // A stale id, or one belonging to a workdir that is no longer open, is nothing: the panel is
    // about to show the newest, so the row that marks it has to say so.
    expect(resolveActiveSession(checkout, "session:gone")?.id).toBe("session-two");
    // A workdir with no terminal, and no workdir at all, are both honestly nothing.
    const empty = createCheckout({
      repoId: "repo:/work/app",
      path: "/work/app",
      canonicalPath: "/work/app",
      isPrimary: true,
    });
    expect(resolveActiveSession(empty, null)).toBeNull();
    expect(resolveActiveSession(null, "session-one")).toBeNull();
  });

  it("names a session after what is in front of it, and after the name it was opened with", () => {
    const shell: Session = {
      id: "session-shell",
      type: "shell",
      checkoutId: "checkout:/work/app",
      name: "zsh",
      createdAt: "now",
      status: "active",
    };

    // The title a program sets is the name it wants to be known by; the program in front is the
    // next thing down, and the stored name is what is left when nothing is in front at all —
    // which is also what a terminal with no live status has to be named by.
    expect(sessionTitle(shell, { state: "running", terminalTitle: "OpenCode: review task" })).toBe(
      "OpenCode: review task",
    );
    expect(sessionTitle(shell, { state: "running", foregroundProcess: true, foregroundApp: "nvim" })).toBe("nvim");
    expect(sessionTitle(shell, { state: "running", foregroundProcess: false })).toBe("zsh");
    expect(sessionTitle(shell)).toBe("zsh");
  });

  it("leaves the crumb's own naming alone when the sidebar row changes what it says about a workdir", () => {
    // The two rules are siblings, not one rule: same question, same terminal, and they must not
    // disagree about what it is called — but they are not the same question. The crumb is the
    // window's own title, so it takes whatever the PTY last wrote. The row is a line in a list, so
    // it takes the session and nothing else: the worktree above it already says where the terminal
    // is, and repeating it made two sibling shells read identically.
    const shell: Session = {
      id: "session-shell",
      type: "shell",
      checkoutId: "checkout:/work/app/.worktrees/bug/13133933180-disable-adguard",
      name: "zsh",
      createdAt: "now",
      status: "active",
    };
    const prompt = { state: "running" as const, foregroundProcess: false };

    // The crumb: unchanged, whatever the PTY wrote last, agent prefix and all.
    expect(sessionTitle(shell, { state: "running", terminalTitle: "OC | Copy ids" })).toBe("OC | Copy ids");
    expect(sessionTitle(shell, { state: "running", terminalTitle: "marani@msi:~/Trabajo" })).toBe(
      "marani@msi:~/Trabajo",
    );
    expect(sessionTitle(shell, { state: "running", foregroundProcess: true, foregroundApp: "nvim" })).toBe("nvim");
    expect(sessionTitle(shell, prompt)).toBe("zsh");

    // The row: the session, and never a place. Both read `zsh` here while the crumb above reads the
    // prompt — that is the intended difference, not a drift between them.
    expect(sessionRowTitle(shell, prompt)).toBe("zsh");
    expect(sessionRowTitle(shell, { state: "running", terminalTitle: "OC | Copy ids" })).toBe("zsh");
  });

  it("names a sidebar row by what runs in it, never by the prompt a shell left in the title", () => {
    const shell: Session = {
      id: "session-shell",
      type: "shell",
      checkoutId: "checkout:/work/app",
      name: "zsh",
      createdAt: "now",
      status: "active",
    };
    const prompt = { state: "running" as const, foregroundProcess: false, terminalTitle: "marani@msi:~/Trabajo" };

    // The row names the session and nothing else: the program in front of the shell when there is one,
    // the session's own name when there is not. Never the directory, because the branch row above
    // already carries it and repeating it made two sibling shells read the same text.
    expect(sessionRowTitle(shell, { state: "running", foregroundProcess: true, foregroundApp: "pnpm" })).toBe("pnpm");
    // A program that writes its own window title names no session, and the row does not go mining
    // one out of it: the program in front of the shell is what a terminal is named by, read from the
    // OS rather than guessed out of a string the program chose to write. So an editor reads `zsh` —
    // the shell it runs in — until the OS says what is in front.
    expect(
      sessionRowTitle(shell, { state: "running", foregroundProcess: true, terminalTitle: "nvim — src/main.rs" }),
    ).toBe("zsh");
    // A shell's own prompt is never a row's name either, and neither is the directory it sits in.
    expect(sessionRowTitle(shell, prompt)).toBe("zsh");
    expect(sessionRowTitle(shell, undefined)).toBe("zsh");
    // A rename is the stored name, and it is therefore the row's name whenever nothing else is.
    expect(sessionRowTitle({ ...shell, name: "deploy" }, prompt)).toBe("deploy");

    // The session a terminal is showing is named by that session's own title, which the terminal's
    // TUI wrote and which belongs to this terminal rather than to the directory it sits in. A cut
    // title is still the session's name, and a home screen names no session, so that row falls
    // back to the program. Both answers need the runtime to confirm OpenCode is in front of the
    // shell, or the title is a leftover rather than a fact.
    const agent = { ...shell, id: "session-agent" };
    const opencode = { state: "running" as const, foregroundProcess: true, foregroundApp: "opencode" };
    expect(sessionRowTitle(agent, { ...opencode, terminalTitle: "OC | Copy ids into lists" })).toBe(
      "Copy ids into lists",
    );
    expect(sessionRowTitle(agent, { ...opencode, terminalTitle: "OC | Plan de implementación para la s…" })).toBe(
      "Plan de implementación para la s…",
    );
    expect(sessionRowTitle(agent, { ...opencode, terminalTitle: "OC | OpenCode" })).toBe("opencode");
  });

  it("never reads a leftover agent title once another program is in front", () => {
    // A PTY title outlives the process that wrote it. OpenCode sets `OC | …`, exits, and the shell
    // takes the terminal back without writing anything, so the status still carries the old string.
    // Reading it would name a terminal after an agent no longer running in it and draw that agent's
    // state on it, so the current foreground answer has to say OpenCode before the title is read.
    const stale = "OC | Copy ids into lists";
    expect(agentSessionTitle({ state: "running", foregroundProcess: false, terminalTitle: stale })).toBeNull();
    expect(
      agentSessionTitle({ state: "running", foregroundProcess: true, foregroundApp: "sleep", terminalTitle: stale }),
    ).toBeNull();
    expect(
      agentSessionTitle({ state: "running", foregroundProcess: true, foregroundApp: "nvim", terminalTitle: stale }),
    ).toBeNull();
    expect(agentSessionTitle({ state: "running", terminalTitle: stale })).toBeNull();
    // The same title with the program still in front is still read: the check is about now, not
    // about whether a string ever looked like one.
    expect(
      agentSessionTitle({ state: "running", foregroundProcess: true, foregroundApp: "opencode", terminalTitle: stale }),
    ).toBe("Copy ids into lists");

    // And the row agrees: a terminal running something else is named after that program, and one with
    // nothing in front falls back to the name it was opened with — which is where a rename lives, and
    // which is the last answer rather than the first.
    const renamed: Session = {
      id: "session-renamed",
      type: "shell",
      checkoutId: "checkout:/work/app",
      name: "deploy",
      createdAt: "now",
      status: "active",
    };
    expect(
      sessionRowTitle(renamed, {
        state: "running",
        foregroundProcess: true,
        foregroundApp: "sleep",
        terminalTitle: stale,
      }),
    ).toBe("sleep");
    expect(sessionRowTitle(renamed, { state: "running", foregroundProcess: false, terminalTitle: stale })).toBe(
      "deploy",
    );
  });

  it("reads a session title out of a terminal title only behind OpenCode's own prefix", () => {
    // What a terminal's title can be called a session: what that terminal's own program wrote, and
    // only while that program is the one in front of the shell. The prefix is the other half of the
    // test — a shell's prompt, an editor's window title and a title with no session behind it are
    // all terminal titles, and none of them is a session.
    const inOpenCode = { state: "running" as const, foregroundProcess: true, foregroundApp: "opencode" };
    const of = (terminalTitle: string | null) => agentSessionTitle({ ...inOpenCode, terminalTitle });
    expect(of("OC | Copy ids into lists")).toBe("Copy ids into lists");
    expect(of("OC | OpenCode")).toBeNull();
    expect(of("marani@msi:~/Trabajo")).toBeNull();
    expect(of("nvim — src/main.rs")).toBeNull();
    expect(of(null)).toBeNull();
    expect(agentSessionTitle(undefined)).toBeNull();
    // The separator belongs to the prefix and is not part of the name, and nothing is stripped
    // from the middle of one.
    expect(of("OC |   spaced   ")).toBe("spaced");
    expect(of("OC | OC | nested")).toBe("OC | nested");
  });

  it("gives a checkout the icon its row in the sidebar wears", () => {
    const git = gitRepo();
    const root = git.checkouts[0]!;
    const worktree = git.checkouts[1]!;
    const plain = createPlainRepo(openedFolder("/work/notes", "notes"), "2026-01-01T00:00:00Z");

    // A plain directory is a folder, a repo root is a Git one, and anything else in a repo is a
    // branch checked out in a directory of its own.
    expect(workdirIconKind(plain, plain.checkouts[0]!)).toBe("folder");
    expect(workdirIconKind(git, root)).toBe("git");
    expect(workdirIconKind(git, worktree)).toBe("worktree");
    // The titlebar's first crumb falls back to the folder name when there is no repo to ask.
    expect(workdirIconKind(null, root)).toBe("folder");
  });

  it("decides the missing and the home one before anything the repo says", () => {
    const git = gitRepo();
    const worktree = git.checkouts[1]!;

    // A directory that is gone is still the row it was, and the home one is still the place, so
    // neither of them can be answered further down the order.
    expect(workdirIconKind(git, { ...worktree, isMissing: true }, worktree.id)).toBe("missing");
    expect(workdirIconKind(git, { ...worktree, isMissing: true })).toBe("missing");
    expect(workdirIconKind(git, worktree, worktree.id)).toBe("home");
    expect(workdirIconKind(git, git.checkouts[0]!, git.checkouts[0]!.id)).toBe("home");
  });
});
