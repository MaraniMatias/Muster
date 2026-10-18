// @vitest-environment happy-dom
import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Checkout, Repo, WorkspaceState } from "../domain/workspace";
import WorktreeDialog from "./WorktreeDialog.vue";

const ipc = vi.hoisted(() => ({
  archiveCheckout: vi.fn(),
  createWorktree: vi.fn(),
  getWorktreeDefaults: vi.fn(),
  getWorktreeRemovalInfo: vi.fn(),
  removeWorktree: vi.fn(),
  setDefaultBranch: vi.fn(),
}));

vi.mock("../lib/ipc", () => ipc);

const checkout: Checkout = {
  id: "checkout:primary",
  repoId: "repo:test",
  path: "/test",
  canonicalPath: "/test",
  isPrimary: true,
  branch: "trunk",
  changedFiles: 0,
  isMissing: false,
  sessions: [],
};

const repo: Repo = {
  id: "repo:test",
  kind: "git",
  name: "test",
  root: "/test",
  defaultBranch: "trunk",
  checkouts: [checkout],
  createdAt: "now",
  lastOpenedAt: "now",
};

const workspace: WorkspaceState = { repos: [repo], activeCheckoutId: checkout.id, activeSessionId: null };

describe("WorktreeDialog", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("creates from main under the fixed repo-local worktree directory", async () => {
    ipc.getWorktreeDefaults.mockResolvedValue({ location: "/test/.worktrees", defaultBranch: "main" });
    ipc.createWorktree.mockResolvedValue({ workspace, checkoutId: "checkout:new" });
    const wrapper = mount(WorktreeDialog, {
      props: { open: true, mode: "create", repo, checkout },
    });
    await flushPromises();

    expect(wrapper.findAll("input")).toHaveLength(2);
    expect((wrapper.findAll("input")[1].element as HTMLInputElement).value).toBe("feature/new-task");
    expect(wrapper.text()).toContain("Starting point: main");
    expect(wrapper.text()).toContain("Worktrees are created under /test/.worktrees");

    const maliciousName = "../../$(touch should-not-run); *";
    await wrapper.get("input[autofocus]").setValue(maliciousName);
    await wrapper.findAll("input")[1].setValue("feature/safe");
    await wrapper.get("form").trigger("submit");
    await flushPromises();

    expect(ipc.createWorktree).toHaveBeenCalledWith(checkout.id, maliciousName, "feature/safe", "/test/.worktrees");
    expect(wrapper.emitted("workspaceUpdated")).toEqual([[workspace]]);
    expect(wrapper.emitted("requestShell")).toBeUndefined();
    expect(wrapper.text()).toContain("Starting point: main");
  });

  it("shows an actionable error and blocks creation when main is missing", async () => {
    const prompt = vi.spyOn(window, "prompt");
    ipc.getWorktreeDefaults.mockRejectedValue({
      code: "default_branch_unknown",
      message: "Git branch 'main' does not exist; create or fetch it before creating a worktree",
    });
    const wrapper = mount(WorktreeDialog, {
      props: { open: true, mode: "create", repo, checkout },
    });
    await flushPromises();

    expect(wrapper.get('[role="alert"]').text()).toContain("Git branch 'main' does not exist");
    expect(wrapper.get('button[type="submit"]').attributes("disabled")).toBeDefined();
    expect(prompt).not.toHaveBeenCalled();
    prompt.mockRestore();
  });

  it("shows dirty files and active sessions, defaults to keeping the branch, and requires confirmation", async () => {
    ipc.getWorktreeRemovalInfo.mockResolvedValue({
      checkoutId: "checkout:feature",
      isPrimary: false,
      isMissing: false,
      branch: "feature",
      dirtyFiles: ["uncommitted.txt"],
      unmergedCommits: 2,
      activeSessions: [{ id: "session:1", type: "shell", name: "zsh · feature" }],
      activeAgentSessions: [],
    });
    ipc.removeWorktree.mockResolvedValue({ workspace });
    const worktree = { ...checkout, id: "checkout:feature", isPrimary: false, branch: "feature" };
    const wrapper = mount(WorktreeDialog, {
      props: { open: true, mode: "remove", repo, checkout: worktree },
    });
    await flushPromises();

    expect(wrapper.text()).toContain("uncommitted.txt");
    expect(wrapper.text()).toContain("zsh · feature");
    // Branch deletion is opt-in; without checking this box, removal keeps it.
    const deleteBranch = wrapper
      .findAll('input[type="checkbox"]')
      .find((input) => input.element.closest("label")?.textContent?.includes("Delete branch"));
    expect((deleteBranch?.element as HTMLInputElement | undefined)?.checked).toBe(false);
    expect(wrapper.text()).toContain("Delete branch and its unmerged commits");
    const removeButton = wrapper.findAll("button").find((button) => button.text() === "Delete");
    expect(removeButton?.attributes("disabled")).toBeDefined();

    await wrapper.findAll('input[type="checkbox"]')[0].setValue(true);
    await wrapper.findAll('input[type="checkbox"]')[1].setValue(true);
    await wrapper
      .findAll("button")
      .find((button) => button.text() === "Delete")
      ?.trigger("click");
    await flushPromises();

    expect(ipc.removeWorktree).toHaveBeenCalledWith(
      "checkout:feature",
      true,
      ["uncommitted.txt"],
      ["session:1"],
      "feature",
      2,
      false,
    );
    expect(wrapper.emitted("workspaceUpdated")).toEqual([[workspace]]);
  });

  it("deletes the branch only when its checkbox is selected", async () => {
    ipc.getWorktreeRemovalInfo.mockResolvedValue({
      checkoutId: "checkout:feature",
      isPrimary: false,
      isMissing: false,
      branch: "feature",
      dirtyFiles: [],
      unmergedCommits: 0,
      activeSessions: [],
      activeAgentSessions: [],
    });
    ipc.removeWorktree.mockResolvedValue({ workspace });
    const worktree = { ...checkout, id: "checkout:feature", isPrimary: false, branch: "feature" };
    const wrapper = mount(WorktreeDialog, {
      props: { open: true, mode: "remove", repo, checkout: worktree },
    });
    await flushPromises();

    const deleteBranch = wrapper
      .findAll('input[type="checkbox"]')
      .find((input) => input.element.closest("label")?.textContent?.includes("Delete branch"));
    expect((deleteBranch?.element as HTMLInputElement | undefined)?.checked).toBe(false);
    await deleteBranch?.setValue(true);
    await wrapper
      .findAll("button")
      .find((button) => button.text() === "Delete")
      ?.trigger("click");
    await flushPromises();

    expect(ipc.removeWorktree).toHaveBeenCalledWith("checkout:feature", false, [], [], "feature", 0, true);
  });

  it("offers a shell for commit or stash and blocks removal while an agent is active", async () => {
    ipc.getWorktreeRemovalInfo.mockResolvedValue({
      checkoutId: "checkout:feature",
      isPrimary: false,
      isMissing: false,
      branch: "feature",
      dirtyFiles: ["draft.txt"],
      unmergedCommits: 0,
      activeSessions: [],
      activeAgentSessions: [{ id: "agent:1", type: "agent", name: "Agent 1" }],
    });
    const worktree = { ...checkout, id: "checkout:feature", isPrimary: false, branch: "feature" };
    const wrapper = mount(WorktreeDialog, {
      props: { open: true, mode: "remove", repo, checkout: worktree },
    });
    await flushPromises();

    expect(wrapper.text()).toContain("Agent 1");
    expect(
      wrapper
        .findAll("button")
        .find((button) => button.text() === "Delete")
        ?.attributes("disabled"),
    ).toBeDefined();
    await wrapper.get("button.underline").trigger("click");

    expect(wrapper.emitted("requestShell")).toEqual([["checkout:feature"]]);
    expect(ipc.removeWorktree).not.toHaveBeenCalled();
  });

  it("offers both answers to one question, and Delete is the one painted in the failure colour", async () => {
    ipc.getWorktreeRemovalInfo.mockResolvedValue({
      checkoutId: "checkout:feature",
      isPrimary: false,
      isMissing: false,
      branch: "feature",
      dirtyFiles: [],
      unmergedCommits: 0,
      activeSessions: [],
      activeAgentSessions: [],
    });
    const worktree = { ...checkout, id: "checkout:feature", isPrimary: false, branch: "feature" };
    const wrapper = mount(WorktreeDialog, {
      props: { open: true, mode: "remove", repo, checkout: worktree },
    });
    await flushPromises();

    // The two answers are named before either is offered, because they differ in what they
    // leave behind and an icon cannot say that.
    expect(wrapper.text()).toContain("keeps every file");
    expect(wrapper.text()).toContain("removes its directory from disk");
    const del = wrapper.findAll("button").find((button) => button.text() === "Delete");
    expect(del?.classes()).toContain("muster-button-danger");
    expect(wrapper.findAll("button").map((button) => button.text())).toContain("Archive");
    expect(wrapper.findAll("button").map((button) => button.text())).toContain("Cancel");
    expect(
      wrapper
        .findAll("button")
        .slice(-3)
        .map((button) => button.text()),
    ).toEqual(["Archive", "Cancel", "Delete"]);
    // Focus opens on the answer that does nothing, so a stray Enter cannot delete a worktree.
    expect(wrapper.get("button[autofocus]").text()).toBe("Cancel");

    document.body.append(wrapper.element);
    const first = wrapper.get('button[aria-label="Close"]');
    const lastButton = wrapper.findAll("button").find((button) => button.text() === "Delete")!;
    (lastButton.element as HTMLButtonElement).focus();
    await lastButton.trigger("keydown", { key: "Tab" });
    expect(document.activeElement).toBe(first.element);

    (first.element as HTMLButtonElement).focus();
    await first.trigger("keydown", { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(lastButton.element);
    wrapper.unmount();
  });

  it("archives without the confirmations Delete needs, and keeps the dialog open when it fails", async () => {
    ipc.getWorktreeRemovalInfo.mockResolvedValue({
      checkoutId: "checkout:feature",
      isPrimary: false,
      isMissing: false,
      branch: "feature",
      dirtyFiles: ["uncommitted.txt"],
      unmergedCommits: 0,
      activeSessions: [],
      activeAgentSessions: [],
    });
    const worktree = { ...checkout, id: "checkout:feature", isPrimary: false, branch: "feature" };
    const wrapper = mount(WorktreeDialog, {
      props: { open: true, mode: "remove", repo, checkout: worktree },
    });
    await flushPromises();

    // Nothing on disk moves, so an uncommitted file is no reason to ask twice.
    ipc.archiveCheckout.mockResolvedValue(workspace);
    await wrapper
      .findAll("button")
      .find((button) => button.text() === "Archive")
      ?.trigger("click");
    await flushPromises();

    expect(ipc.archiveCheckout).toHaveBeenCalledWith("checkout:feature");
    expect(ipc.removeWorktree).not.toHaveBeenCalled();
    expect(wrapper.emitted("workspaceUpdated")).toEqual([[workspace]]);
    expect(wrapper.emitted("close")).toEqual([[]]);

    ipc.archiveCheckout.mockRejectedValue(new Error("close active terminal sessions before archiving"));
    const retry = mount(WorktreeDialog, {
      props: { open: true, mode: "remove", repo, checkout: worktree },
    });
    await flushPromises();
    await retry
      .findAll("button")
      .find((button) => button.text() === "Archive")
      ?.trigger("click");
    await flushPromises();

    // The dialog is the only thing between the user and a failed command, so it stays up with
    // the failure in it rather than closing on top of the reason.
    expect(retry.get('[role="alert"]').text()).toContain("close active terminal sessions");
    expect(retry.emitted("close")).toBeUndefined();
  });

  it("refuses to archive while a session runs in the worktree, and says so", async () => {
    ipc.getWorktreeRemovalInfo.mockResolvedValue({
      checkoutId: "checkout:feature",
      isPrimary: false,
      isMissing: false,
      branch: "feature",
      dirtyFiles: [],
      unmergedCommits: 0,
      activeSessions: [{ id: "session:1", type: "shell", name: "zsh · feature" }],
      activeAgentSessions: [],
    });
    const worktree = { ...checkout, id: "checkout:feature", isPrimary: false, branch: "feature" };
    const wrapper = mount(WorktreeDialog, {
      props: { open: true, mode: "remove", repo, checkout: worktree },
    });
    await flushPromises();

    const archive = wrapper.findAll("button").find((button) => button.text() === "Archive");
    expect(archive?.attributes("disabled")).toBeDefined();
    expect(wrapper.text()).toContain("Archive stays unavailable while a session runs here");
    await archive?.trigger("click");

    expect(ipc.archiveCheckout).not.toHaveBeenCalled();
  });
});
