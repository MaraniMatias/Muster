// @vitest-environment happy-dom
import { flushPromises, mount } from "@vue/test-utils";
import { computed, defineComponent, h, ref } from "vue";
import type { Ref } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GitStatus } from "../domain/git";
import type { Checkout, Repo } from "../domain/workspace";

const mocks = vi.hoisted(() => ({
  getGitStatus: vi.fn(),
  listen: vi.fn(),
  unlisten: vi.fn(),
  handlers: new Map<string, (event: { payload: string[] }) => void>(),
}));

vi.mock("../lib/ipc", () => ({
  getGitStatus: mocks.getGitStatus,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));

import { useActiveGitSnapshot } from "./active-git-snapshot";

function checkout(id: string): Checkout {
  return {
    id,
    repoId: "repo:test",
    path: `/${id}`,
    canonicalPath: `/${id}`,
    isPrimary: true,
    changedFiles: 0,
    isMissing: false,
    sessions: [],
  };
}

function repo(defaultBranch = "trunk"): Repo {
  return {
    id: "repo:test",
    kind: "git",
    name: "test",
    root: "/test",
    defaultBranch,
    checkouts: [],
    createdAt: "now",
    lastOpenedAt: "now",
  };
}

function status(branch: string): GitStatus {
  return { branch, defaultBranch: "trunk", aheadCount: 0, files: [] };
}

function host(
  checkoutRef: Ref<Checkout | null>,
  repoRef: Ref<Repo | null>,
  onDefaultBranchUnknown: () => void = () => undefined,
  unwatchedRepos: Ref<ReadonlySet<string>> = ref(new Set()),
) {
  return defineComponent({
    setup() {
      const snapshot = useActiveGitSnapshot(
        computed(() => checkoutRef.value),
        computed(() => repoRef.value),
        onDefaultBranchUnknown,
        unwatchedRepos,
      );
      return () => h("div", [snapshot.status?.branch ?? "no status", snapshot.changesWatchError].join("|"));
    },
  });
}

describe("useActiveGitSnapshot", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    mocks.handlers.clear();
    mocks.listen.mockImplementation(async (name: string, handler: (event: { payload: string[] }) => void) => {
      mocks.handlers.set(name, handler);
      return mocks.unlisten;
    });
    mocks.getGitStatus.mockImplementation(async (id: string) => status(id));
  });

  it("owns one listener for the selected checkout and releases it on selection change/unmount", async () => {
    const activeCheckout = ref<Checkout | null>(checkout("first"));
    const activeRepo = ref<Repo | null>(repo());
    const wrapper = mount(host(activeCheckout, activeRepo));
    await flushPromises();

    expect(mocks.listen).toHaveBeenCalledTimes(1);
    expect(mocks.getGitStatus).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).toContain("first");

    // A commit in one worktree moves the merge base its siblings count against, so the signal
    // names every checkout it speaks for and this one reads only its own.
    mocks.handlers.get("git-status-changed")?.({ payload: ["other", "third"] });
    await flushPromises();
    expect(mocks.getGitStatus).toHaveBeenCalledTimes(1);

    mocks.handlers.get("git-status-changed")?.({ payload: ["other", "first"] });
    await flushPromises();
    expect(mocks.getGitStatus).toHaveBeenCalledTimes(2);

    activeCheckout.value = checkout("second");
    await flushPromises();
    expect(mocks.listen).toHaveBeenCalledTimes(2);
    expect(mocks.unlisten).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).toContain("second");

    wrapper.unmount();
    await flushPromises();
    expect(mocks.unlisten).toHaveBeenCalledTimes(2);
  });

  it("ignores a status response that finishes after switching checkout", async () => {
    const activeCheckout = ref<Checkout | null>(checkout("first"));
    const activeRepo = ref<Repo | null>(repo());
    let resolveFirst!: (value: GitStatus) => void;
    mocks.getGitStatus.mockImplementation((id: string) =>
      id === "first" ? new Promise<GitStatus>((resolve) => (resolveFirst = resolve)) : Promise.resolve(status(id)),
    );
    const wrapper = mount(host(activeCheckout, activeRepo));
    await flushPromises();

    activeCheckout.value = checkout("second");
    await flushPromises();
    expect(wrapper.text()).toContain("second");

    resolveFirst(status("stale-first"));
    await flushPromises();
    expect(wrapper.text()).toContain("second");
    expect(mocks.getGitStatus).toHaveBeenCalledTimes(2);
    expect(mocks.getGitStatus).toHaveBeenLastCalledWith("second");
    wrapper.unmount();
  });

  it("says so where the numbers are read when the repository is not being watched", async () => {
    const unwatched = ref<ReadonlySet<string>>(new Set());
    const activeCheckout = ref<Checkout | null>(checkout("first"));
    const activeRepo = ref<Repo | null>(repo());
    const wrapper = mount(host(activeCheckout, activeRepo, () => undefined, unwatched));
    await flushPromises();
    expect(wrapper.text()).not.toContain("not being watched");

    // Nothing can tell this panel that anything changed, so its rows would sit on numbers
    // nothing can refresh. The reason belongs next to them rather than in a toast that expires.
    unwatched.value = new Set(["repo:test"]);
    await flushPromises();
    expect(wrapper.text()).toContain("not being watched");

    // Another repository failing to start says nothing about the one on screen.
    unwatched.value = new Set(["repo:other"]);
    await flushPromises();
    expect(wrapper.text()).not.toContain("not being watched");
    wrapper.unmount();
  });

  it("requests default-branch recovery once per checkout configuration", async () => {
    const activeCheckout = ref<Checkout | null>(checkout("first"));
    const activeRepo = ref<Repo | null>(repo());
    const onDefaultBranchUnknown = vi.fn();
    mocks.getGitStatus.mockRejectedValue({ code: "default_branch_unknown", message: "Choose a default branch" });
    const wrapper = mount(host(activeCheckout, activeRepo, onDefaultBranchUnknown));
    await flushPromises();
    expect(onDefaultBranchUnknown).toHaveBeenCalledTimes(1);

    mocks.handlers.get("git-status-changed")?.({ payload: ["first"] });
    await flushPromises();
    expect(onDefaultBranchUnknown).toHaveBeenCalledTimes(1);

    activeRepo.value = repo("main");
    await flushPromises();
    expect(onDefaultBranchUnknown).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });
});
