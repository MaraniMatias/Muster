// @vitest-environment happy-dom
import { mount } from "@vue/test-utils";
import { defineComponent, h, ref } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Repo, WorkspaceState } from "../domain/workspace";

const mocks = vi.hoisted(() => ({
  syncWorkspaceRepo: vi.fn(),
  handlers: new Map<string, (event: { payload: unknown }) => void>(),
  listen: vi.fn(),
}));

vi.mock("../lib/ipc", () => ({
  syncWorkspaceRepo: mocks.syncWorkspaceRepo,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: mocks.listen,
}));

import { useWorktreeSync } from "./worktree-sync";

function repo(id: string, ...checkoutIds: string[]): Repo {
  return {
    id,
    kind: "git",
    name: id,
    root: `/${id}`,
    checkouts: checkoutIds.map((checkoutId, index) => ({
      id: checkoutId,
      repoId: id,
      path: `/${checkoutId}`,
      canonicalPath: `/${checkoutId}`,
      isPrimary: index === 0,
      changedFiles: 0,
      isMissing: false,
      sessions: [],
    })),
    createdAt: "now",
    lastOpenedAt: "now",
  };
}

function workspace(repos: Repo[], activeCheckoutId = repos[0]?.checkouts[0]?.id ?? null): WorkspaceState {
  return {
    repos,
    archivedWorktrees: [],
    activeCheckoutId,
    activeSessionId: null,
  };
}

function host(getWorkspace: () => WorkspaceState, applyWorkspace: (next: WorkspaceState) => void, onError = vi.fn()) {
  return defineComponent({
    setup() {
      useWorktreeSync(getWorkspace, applyWorkspace, onError);
      return () => h("div");
    },
  });
}

/** Lets the listener registration and the calls it triggers run. */
async function settle() {
  for (let round = 0; round < 5; round += 1) await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function signal(repoIds: string[]) {
  mocks.handlers.get("git-worktrees-changed")?.({ payload: repoIds });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((finish) => {
    resolve = finish;
  });
  return { promise, resolve };
}

describe("useWorktreeSync", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.handlers.clear();
    mocks.listen.mockImplementation(async (name: string, handler: (event: { payload: unknown }) => void) => {
      mocks.handlers.set(name, handler);
      return () => mocks.handlers.delete(name);
    });
    mocks.syncWorkspaceRepo.mockResolvedValue(null);
  });

  it("updates the named repo without replacing another repo or the current selection", async () => {
    const current = ref(workspace([repo("repo:a", "checkout:a"), repo("repo:b", "checkout:b")], "checkout:b"));
    const applyWorkspace = vi.fn((next: WorkspaceState) => {
      current.value = next;
    });
    const wrapper = mount(host(() => current.value, applyWorkspace));
    await settle();
    mocks.syncWorkspaceRepo.mockResolvedValue(
      workspace([repo("repo:a", "checkout:a", "checkout:new"), repo("repo:b", "checkout:b")], "checkout:a"),
    );

    signal(["repo:a"]);
    await settle();

    expect(applyWorkspace).toHaveBeenCalledTimes(1);
    expect(current.value.repos.map((item) => item.id)).toEqual(["repo:a", "repo:b"]);
    expect(current.value.repos[0]?.checkouts.map((item) => item.id)).toEqual(["checkout:a", "checkout:new"]);
    expect(current.value.activeCheckoutId).toBe("checkout:b");
    wrapper.unmount();
  });

  it("reads again when the disk changes while the previous read is in flight", async () => {
    const current = ref(workspace([repo("repo:a", "checkout:a")]));
    const first = deferred<WorkspaceState | null>();
    mocks.syncWorkspaceRepo.mockReturnValueOnce(first.promise).mockResolvedValueOnce(null);
    const wrapper = mount(host(() => current.value, vi.fn()));
    await settle();

    signal(["repo:a"]);
    expect(mocks.syncWorkspaceRepo).toHaveBeenCalledTimes(1);
    signal(["repo:a"]);
    expect(mocks.syncWorkspaceRepo).toHaveBeenCalledTimes(1);

    first.resolve(null);
    await settle();

    expect(mocks.syncWorkspaceRepo).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });

  it("does not undo an archive made while the repository read was in flight", async () => {
    const current = ref(workspace([repo("repo:a", "checkout:a")]));
    const archived = {
      id: "checkout:archived",
      repoId: "repo:a",
      path: "/checkout:archived",
      branch: "archived",
    };
    const reply = deferred<WorkspaceState | null>();
    mocks.syncWorkspaceRepo.mockReturnValue(reply.promise);
    const applyWorkspace = vi.fn((next: WorkspaceState) => {
      current.value = next;
    });
    const wrapper = mount(host(() => current.value, applyWorkspace));
    await settle();
    signal(["repo:a"]);
    current.value = {
      ...current.value,
      archivedWorktrees: [archived],
    };
    reply.resolve(workspace([repo("repo:a", "checkout:a", "checkout:archived")]));
    await settle();

    expect(current.value.repos[0]?.checkouts.map((item) => item.id)).toEqual(["checkout:a"]);
    expect(current.value.archivedWorktrees).toEqual([archived]);
    wrapper.unmount();
  });

  it("applies nothing when the list did not move after all", async () => {
    const current = ref(workspace([repo("repo:a", "checkout:a")]));
    const applyWorkspace = vi.fn();
    const wrapper = mount(host(() => current.value, applyWorkspace));
    await settle();

    signal(["repo:a"]);
    await settle();

    expect(applyWorkspace).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("keeps the rows it has and says so when the read fails", async () => {
    const current = ref(workspace([repo("repo:a", "checkout:a")]));
    const applyWorkspace = vi.fn();
    const onError = vi.fn();
    const wrapper = mount(host(() => current.value, applyWorkspace, onError));
    await settle();
    mocks.syncWorkspaceRepo.mockRejectedValue(new Error("could not list Git worktrees"));

    signal(["repo:a"]);
    await settle();

    expect(onError).toHaveBeenCalledTimes(1);
    expect(applyWorkspace).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("does not apply a reply after its listener scope was disposed", async () => {
    const current = ref(workspace([repo("repo:a", "checkout:a")]));
    const reply = deferred<WorkspaceState | null>();
    mocks.syncWorkspaceRepo.mockReturnValue(reply.promise);
    const applyWorkspace = vi.fn();
    const wrapper = mount(host(() => current.value, applyWorkspace));
    await settle();

    signal(["repo:a"]);
    wrapper.unmount();
    reply.resolve(workspace([repo("repo:a", "checkout:new")]));
    await settle();

    expect(applyWorkspace).not.toHaveBeenCalled();
    expect(mocks.handlers.has("git-worktrees-changed")).toBe(false);
  });
});
