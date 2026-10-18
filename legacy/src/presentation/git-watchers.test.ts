// @vitest-environment happy-dom
import { mount } from "@vue/test-utils";
import { defineComponent, h, ref } from "vue";
import type { Ref } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Checkout, Repo } from "../domain/workspace";

const mocks = vi.hoisted(() => ({
  watchGitRepo: vi.fn(),
  unwatchGitRepo: vi.fn(),
  failureListener: undefined as
    ((event: { payload: { repoId: string; checkoutIds: string[]; registrationId: string } }) => void) | undefined,
  listenFailure: null as unknown,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (_event: string, listener: typeof mocks.failureListener) => {
    mocks.failureListener = listener;
    return mocks.listenFailure ? Promise.reject(mocks.listenFailure) : Promise.resolve(() => undefined);
  },
}));

vi.mock("../lib/ipc", () => ({
  watchGitRepo: mocks.watchGitRepo,
  unwatchGitRepo: mocks.unwatchGitRepo,
}));

import { useGitWatchers } from "./git-watchers";

function checkout(id: string, isMissing = false): Checkout {
  return {
    id,
    repoId: "repo:a",
    path: `/${id}`,
    canonicalPath: `/${id}`,
    isPrimary: true,
    changedFiles: 0,
    isMissing,
    sessions: [],
  };
}

function repo(id: string, checkouts: Checkout[], kind: Repo["kind"] = "git"): Repo {
  return {
    id,
    kind,
    name: id,
    root: `/${id}`,
    defaultBranch: "trunk",
    checkouts,
    createdAt: "now",
    lastOpenedAt: "now",
  };
}

function host(repos: Ref<Repo[]>) {
  return defineComponent({
    setup() {
      const unwatched = useGitWatchers(repos);
      return () => h("div", [...unwatched].join(","));
    },
  });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

/** Lets the watcher promises and the Vue watcher callback both run. */
async function settle() {
  for (let round = 0; round < 5; round += 1) await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("useGitWatchers", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.failureListener = undefined;
    mocks.listenFailure = null;
    mocks.watchGitRepo.mockResolvedValue(undefined);
    mocks.unwatchGitRepo.mockResolvedValue(undefined);
  });

  it("watches every git repository once, not once per checkout and not once per edit", async () => {
    const repos = ref([repo("repo:a", [checkout("one"), checkout("two")]), repo("repo:b", [checkout("three")])]);
    const wrapper = mount(host(repos));
    await settle();

    // The sidebar names every checkout at once, so one watcher per repository covers all of
    // them, and a workspace edit that adds a terminal asks for nothing new.
    expect(mocks.watchGitRepo.mock.calls.map(([id]) => id)).toEqual(["repo:a", "repo:b"]);

    repos.value = [repos.value[0], repo("repo:b", [checkout("three"), checkout("four")])];
    await settle();

    expect(mocks.watchGitRepo.mock.calls.map(([id]) => id)).toEqual(["repo:a", "repo:b", "repo:b"]);
    wrapper.unmount();
  });

  it("never watches a plain folder or a repository whose every worktree is gone", async () => {
    const repos = ref([repo("folder", [checkout("one")], "plain")]);
    const wrapper = mount(host(repos));
    await settle();
    expect(mocks.watchGitRepo).not.toHaveBeenCalled();

    repos.value = [repo("gone", [checkout("one", true)])];
    await settle();
    expect(mocks.watchGitRepo).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("releases a repository when its last worktree is archived and picks it up again", async () => {
    const repos = ref([repo("repo:a", [checkout("one")])]);
    const wrapper = mount(host(repos));
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(1);

    repos.value = [repo("repo:a", [])];
    await settle();
    expect(mocks.unwatchGitRepo).toHaveBeenCalledWith("repo:a");

    repos.value = [repo("repo:a", [checkout("one")])];
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });

  it("re-registers the latest worktree plan after a watch already in flight settles", async () => {
    const pending = deferred<void>();
    mocks.watchGitRepo.mockImplementation(() => pending.promise);
    const repos = ref([repo("repo:a", [checkout("one")])]);
    const wrapper = mount(host(repos));
    await settle();

    // Several edits arrive while the first plan is still being registered. They collapse to the
    // latest one: starting multiple watchers would waste resources, but forgetting the change
    // would leave the new worktree unwatched.
    repos.value = [repo("repo:a", [checkout("one"), checkout("two")])];
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(1);

    pending.resolve(undefined);
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(2);

    repos.value = [repo("repo:a", [checkout("one"), checkout("two"), checkout("three")])];
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(3);
    wrapper.unmount();
  });

  it("unwatches after a repository leaves while its watcher is starting", async () => {
    const pending = deferred<void>();
    mocks.watchGitRepo.mockImplementation(() => pending.promise);
    const repos = ref([repo("repo:a", [checkout("one")])]);
    const wrapper = mount(host(repos));
    await settle();

    repos.value = [];
    await settle();
    pending.resolve(undefined);
    await settle();

    expect(mocks.unwatchGitRepo).toHaveBeenCalledWith("repo:a");
    wrapper.unmount();
  });

  it("does not let a delayed unwatch remove a repository that rejoined while starting", async () => {
    const pending = deferred<void>();
    mocks.watchGitRepo.mockImplementationOnce(() => pending.promise).mockResolvedValue(undefined);
    const repos = ref([repo("repo:a", [checkout("one")])]);
    const wrapper = mount(host(repos));
    await settle();

    repos.value = [];
    await settle();
    repos.value = [repo("repo:a", [checkout("one"), checkout("two")])];
    await settle();
    pending.resolve(undefined);
    await settle();

    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(2);
    expect(mocks.unwatchGitRepo).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("asks again after a repository fails to start, so a later change can succeed", async () => {
    mocks.watchGitRepo.mockRejectedValueOnce(new Error("could not start repository watcher"));
    const repos = ref([repo("repo:a", [checkout("one")])]);
    const wrapper = mount(host(repos));
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).toContain("repo:a");

    repos.value = [repo("repo:a", [checkout("one")])];
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).toContain("repo:a");

    repos.value = [repo("repo:a", [checkout("one"), checkout("two")])];
    await settle();

    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(2);
    expect(wrapper.text()).not.toContain("repo:a");
    wrapper.unmount();
  });

  it("does not install a database plan that diverged during an ABA registration", async () => {
    const request = deferred<void>();
    let expectedCheckoutIds: string[] = [];
    mocks.watchGitRepo.mockImplementationOnce((_repoId, _registrationId, expected) => {
      expectedCheckoutIds = expected;
      return request.promise;
    });
    const repos = ref([repo("repo:a", [checkout("one")])]);
    const wrapper = mount(host(repos));
    await settle();

    repos.value = [repo("repo:a", [checkout("one"), checkout("two")])];
    await settle();
    repos.value = [repo("repo:a", [checkout("one")])];
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(1);
    expect(expectedCheckoutIds).toEqual(["one"]);

    // Backend observed B while this request expected A; its plan matcher rejects before install.
    request.reject(new Error("repository checkout plan changed"));
    await settle();
    expect(wrapper.text()).toContain("repo:a");
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(1);

    repos.value = [repo("repo:a", [checkout("one"), checkout("two")])];
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(2);
    expect(wrapper.text()).not.toContain("repo:a");
    wrapper.unmount();
  });

  it("marks a runtime-failed watcher unwatched and retries only after the plan changes", async () => {
    const repos = ref([repo("repo:a", [checkout("one")])]);
    const wrapper = mount(host(repos));
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(1);

    const registrationId = mocks.watchGitRepo.mock.calls[0]?.[1];
    mocks.failureListener?.({ payload: { repoId: "repo:a", checkoutIds: ["one"], registrationId } });
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).toContain("repo:a");

    repos.value = [repo("repo:a", [checkout("one")])];
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).toContain("repo:a");

    repos.value = [repo("repo:a", [checkout("one"), checkout("two")])];
    await settle();
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(2);
    expect(wrapper.text()).not.toContain("repo:a");
    wrapper.unmount();
  });

  it("accepts the current failure token before the watch command resolves", async () => {
    const pending = deferred<void>();
    mocks.watchGitRepo.mockImplementation(() => pending.promise);
    const repos = ref([repo("repo:a", [checkout("one")])]);
    const wrapper = mount(host(repos));
    await settle();
    const registrationId = mocks.watchGitRepo.mock.calls[0]?.[1];

    mocks.failureListener?.({
      payload: { repoId: "repo:a", checkoutIds: ["one"], registrationId },
    });
    await settle();
    expect(wrapper.text()).toContain("repo:a");

    pending.resolve(undefined);
    await settle();
    expect(wrapper.text()).toContain("repo:a");
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it("ignores a queued ABA failure from an older registration", async () => {
    const repos = ref([repo("repo:a", [checkout("one")])]);
    const wrapper = mount(host(repos));
    await settle();
    const oldA = mocks.watchGitRepo.mock.calls[0]?.[1];

    repos.value = [repo("repo:a", [checkout("one"), checkout("two")])];
    await settle();
    const b = mocks.watchGitRepo.mock.calls[1]?.[1];

    repos.value = [repo("repo:a", [checkout("one")])];
    await settle();
    const newA = mocks.watchGitRepo.mock.calls[2]?.[1];
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(3);
    expect(newA).not.toBe(oldA);
    expect(b).not.toBe(newA);

    mocks.failureListener?.({
      payload: { repoId: "repo:a", checkoutIds: ["one"], registrationId: oldA },
    });
    await settle();
    expect(wrapper.text()).not.toContain("repo:a");

    mocks.failureListener?.({
      payload: { repoId: "repo:a", checkoutIds: ["one"], registrationId: newA },
    });
    await settle();
    expect(wrapper.text()).toContain("repo:a");
    expect(mocks.watchGitRepo).toHaveBeenCalledTimes(3);
    wrapper.unmount();
  });

  it("fails closed when the watcher-error listener cannot be installed", async () => {
    mocks.listenFailure = new Error("listener unavailable");
    const logError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const repos = ref([repo("repo:a", [checkout("one")])]);
    const wrapper = mount(host(repos));
    await settle();

    expect(mocks.watchGitRepo).not.toHaveBeenCalled();
    expect(wrapper.text()).toContain("repo:a");
    expect(logError).toHaveBeenCalled();

    repos.value = [repo("repo:b", [checkout("two")])];
    await settle();
    expect(mocks.watchGitRepo).not.toHaveBeenCalled();
    expect(wrapper.text()).toContain("repo:b");
    expect(wrapper.text()).not.toContain("repo:a");
    wrapper.unmount();
    logError.mockRestore();
  });
});
