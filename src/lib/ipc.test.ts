import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import type { OpenedFolder } from "../domain/folder";
import { isIpcError } from "../domain/ipc";
import {
  ackReviewRound,
  clearReviewNoteOutdated,
  closeCheckout,
  closeMissingCheckout,
  closeTerminal,
  archiveCheckout,
  restoreArchivedWorktrees,
  createAgentSession,
  createReviewNote,
  createTerminal,
  deleteReviewNote,
  dispatchReviewRound,
  listAgentSessions,
  listReviewNotes,
  listReviewRounds,
  reconcileReviewRound,
  requeueReviewRounds,
  resolveReviewNote,
  exportReviewMarkdown,
  getReviewFolder,
  clearReviewFolder,
  loadReviewTarget,
  stopAgent,
  updateReviewNote,
  verifyReviewNoteAnchors,
  getGitCheckoutDiffStats,
  getGitDiffPage,
  getGitDiffStats,
  loadTerminalLayout,
  openExternalFile,
  openExternalUrl,
  openFolder,
  probeCheckoutFile,
  readCheckoutFile,
  readCheckoutMedia,
  saveReviewTarget,
  resizeTerminal,
  saveTerminalLayout,
  writeCheckoutFile,
  writeTerminal,
} from "./ipc";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("openFolder IPC client", () => {
  beforeEach(() => vi.clearAllMocks());

  it("sends the selected path to the narrow folder command", async () => {
    const folder: OpenedFolder = { path: "/tmp/test workspace", name: "test workspace" };
    vi.mocked(invoke).mockResolvedValue(folder);

    await expect(openFolder(folder.path)).resolves.toEqual(folder);
    expect(invoke).toHaveBeenCalledWith("open_folder", { path: folder.path });
  });

  it("recognizes only errors using the typed IPC error contract", () => {
    expect(isIpcError({ code: "folder_missing", message: "Folder not found" })).toBe(true);
    expect(isIpcError({ code: "terminal_ownership_mismatch", message: "Wrong checkout" })).toBe(true);
    expect(isIpcError({ code: "arbitrary", message: "No" })).toBe(false);
  });
});

describe("web link IPC client", () => {
  beforeEach(() => vi.clearAllMocks());

  it("sends the link to the one command that opens it, and sends nothing else", async () => {
    // The whole surface: a document's link travels back as a URL and leaves through the command
    // that decides what may be opened. There is no shell and no filesystem behind it, so there is
    // nothing else the webview could ask for.
    vi.mocked(invoke).mockResolvedValue(undefined);

    await openExternalUrl("https://example.com/guide");

    expect(invoke).toHaveBeenCalledExactlyOnceWith("open_url", { url: "https://example.com/guide" });
  });
});

describe("external file IPC client", () => {
  beforeEach(() => vi.clearAllMocks());

  it("sends the checkout and the tree's own spelling of the path, and nothing else", async () => {
    // A checkout-relative name, because that is what the tree lists and what the other end
    // resolves against the checkout it belongs to. An absolute path never crosses here.
    vi.mocked(invoke).mockResolvedValue(undefined);

    await openExternalFile("checkout:repo", "src/lib/ipc.ts");

    expect(invoke).toHaveBeenCalledExactlyOnceWith("file_open_externally", {
      checkoutId: "checkout:repo",
      path: "src/lib/ipc.ts",
    });
  });

  it("hands back a refusal rather than swallowing it", async () => {
    // The caller shows it: a tree row that did not open is otherwise indistinguishable from one
    // that did, and the person who pressed the key is the only one who can tell.
    vi.mocked(invoke).mockRejectedValueOnce({ code: "path_outside_checkout", message: "outside" });

    await expect(openExternalFile("checkout:repo", "../secrets")).rejects.toBeTruthy();
  });
});

describe("file probe IPC client", () => {
  beforeEach(() => vi.clearAllMocks());

  it("sends the directory the terminal is in, because a bare name is relative to it", async () => {
    // `ls` prints `LICENSE` with no directory on it, and the only thing that says which one it
    // means is where the shell is. A terminal that has not reported one sends `null` rather than
    // leaving the argument out, because "the terminal has not said" and "there was nothing to
    // say" are the same absence.
    vi.mocked(invoke).mockResolvedValue({ path: "src/lib/LICENSE" });

    await expect(probeCheckoutFile("checkout:repo", "LICENSE", "/work/feature/src/lib")).resolves.toEqual({
      path: "src/lib/LICENSE",
    });
    expect(invoke).toHaveBeenCalledExactlyOnceWith("file_probe", {
      checkoutId: "checkout:repo",
      path: "LICENSE",
      workingDirectory: "/work/feature/src/lib",
    });

    vi.mocked(invoke).mockClear();
    vi.mocked(invoke).mockResolvedValue(null);

    await probeCheckoutFile("checkout:repo", "LICENSE");

    expect(invoke).toHaveBeenCalledExactlyOnceWith("file_probe", {
      checkoutId: "checkout:repo",
      path: "LICENSE",
      workingDirectory: null,
    });
  });
});

describe("workspace IPC client", () => {
  beforeEach(() => vi.clearAllMocks());

  it("takes a workdir off the panel through the command that takes only its id", async () => {
    await closeCheckout("checkout:/work/feature");

    expect(invoke).toHaveBeenNthCalledWith(1, "close_checkout", { checkoutId: "checkout:/work/feature" });
  });

  it("closes a missing checkout through the command that takes only its id", async () => {
    await closeMissingCheckout("checkout:/work/gone");

    expect(invoke).toHaveBeenNthCalledWith(1, "close_missing_checkout", { checkoutId: "checkout:/work/gone" });
  });

  it("archives by checkout and restores by repository, which is the row each hangs on", async () => {
    await archiveCheckout("checkout:/work/feature");
    await restoreArchivedWorktrees("repo:/work");

    expect(invoke).toHaveBeenNthCalledWith(1, "archive_checkout", { checkoutId: "checkout:/work/feature" });
    expect(invoke).toHaveBeenNthCalledWith(2, "restore_archived_worktrees", { repoId: "repo:/work" });
  });

  it("sends the expected content with a checkout-scoped file write", async () => {
    await writeCheckoutFile("checkout:one", "src/app.ts", "new", "old", "checkout");

    expect(invoke).toHaveBeenCalledWith("file_write", {
      checkoutId: "checkout:one",
      path: "src/app.ts",
      content: "new",
      expectedContent: "old",
      origin: "checkout",
    });
  });

  it("threads file origin and review export metadata through IPC", async () => {
    await readCheckoutFile("checkout:one", "notes.md", "review-workdir");
    await exportReviewMarkdown("checkout:one", "2026-03-14", "2026-03-14-1532", "# Review");
    await loadReviewTarget("checkout:one");
    await saveReviewTarget("checkout:one", "opencode");

    expect(invoke).toHaveBeenNthCalledWith(1, "file_read", {
      checkoutId: "checkout:one",
      path: "notes.md",
      origin: "review-workdir",
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "review_export_markdown", {
      checkoutId: "checkout:one",
      date: "2026-03-14",
      timestamp: "2026-03-14-1532",
      markdown: "# Review",
    });
    expect(invoke).toHaveBeenNthCalledWith(3, "review_target_load", { checkoutId: "checkout:one" });
    expect(invoke).toHaveBeenNthCalledWith(4, "review_target_save", {
      checkoutId: "checkout:one",
      target: "opencode",
    });
  });

  /// The folder is asked for the mode the row is showing, and for the saved one when the caller has
  /// no mode to show: `null` is the saved preference, not a third folder.
  it("asks for the review folder of one checkout, in the mode being shown", async () => {
    await getReviewFolder("checkout:one", "default");
    await getReviewFolder("checkout:one", "workdir");
    await getReviewFolder("checkout:one");

    expect(invoke).toHaveBeenNthCalledWith(1, "review_folder", {
      checkoutId: "checkout:one",
      storage: "default",
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "review_folder", {
      checkoutId: "checkout:one",
      storage: "workdir",
    });
    expect(invoke).toHaveBeenNthCalledWith(3, "review_folder", {
      checkoutId: "checkout:one",
      storage: null,
    });
  });

  it("asks to clear the review folder of one checkout", async () => {
    await clearReviewFolder("checkout:one", "/Users/dev/.muster/tmp/reviews");

    expect(invoke).toHaveBeenCalledWith("review_folder_clear", {
      checkoutId: "checkout:one",
      storage: "default",
      confirmedPath: "/Users/dev/.muster/tmp/reviews",
    });
  });
});

describe("media IPC client", () => {
  it.each(["buffer", "array"])("preserves binary bytes from a %s response", async (kind) => {
    const bytes = new Uint8Array([...new TextEncoder().encode("image/png\n"), 0, 128, 255]);
    vi.mocked(invoke).mockResolvedValue(kind === "buffer" ? bytes.buffer : Array.from(bytes));
    const blob = await readCheckoutMedia("checkout:one", "image.png");
    expect(blob.type).toBe("image/png");
    // The payload is a view of the response handed straight to the Blob: one copy, and the bytes
    // after the header exactly.
    expect(blob.size).toBe(3);
    expect(new Uint8Array(await blob.arrayBuffer())).toEqual(new Uint8Array([0, 128, 255]));
    expect(invoke).toHaveBeenLastCalledWith("file_read_media", { checkoutId: "checkout:one", path: "image.png" });
  });
  it.each(["buffer", "array"])("refuses an untrusted or unbounded MIME from a %s response", async (kind) => {
    for (const header of ["image/svg+xml\n", "text/html\n", "image/png".repeat(10)]) {
      const bytes = new TextEncoder().encode(header);
      vi.mocked(invoke).mockResolvedValue(kind === "buffer" ? bytes.buffer : Array.from(bytes));
      await expect(readCheckoutMedia("checkout:one", "file.png")).rejects.toThrow("Invalid media");
    }
  });
});

describe("terminal IPC client", () => {
  beforeEach(() => vi.clearAllMocks());

  it("uses only the scoped terminal domain commands and sends binary input as bytes", async () => {
    const channel = { onmessage: null } as never;
    const bytes = new Uint8Array([0, 195, 169, 255]);
    vi.mocked(invoke).mockResolvedValue(undefined);

    await createTerminal("checkout:/work/repo", 96, 30, channel);
    await writeTerminal("checkout:one", "session:one", bytes);
    await resizeTerminal("checkout:one", "session:one", 97, 31);
    await closeTerminal("checkout:one", "session:one");

    expect(invoke).toHaveBeenNthCalledWith(1, "terminal_create", {
      request: {
        checkoutId: "checkout:/work/repo",
        cols: 96,
        rows: 30,
      },
      onOutput: channel,
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "terminal_write", {
      checkoutId: "checkout:one",
      sessionId: "session:one",
      bytes: [0, 195, 169, 255],
    });
    expect(invoke).toHaveBeenNthCalledWith(3, "terminal_resize", {
      checkoutId: "checkout:one",
      sessionId: "session:one",
      cols: 97,
      rows: 31,
    });
    expect(invoke).toHaveBeenNthCalledWith(4, "terminal_close", {
      checkoutId: "checkout:one",
      sessionId: "session:one",
    });
  });

  it("loads and saves layout through checkout-scoped typed commands", async () => {
    const layout = {
      activeTabId: "tab-one",
      sessionOrder: ["session-one"],
      tabs: [{ id: "tab-one", root: { kind: "session" as const, sessionId: "session-one" } }],
    };
    vi.mocked(invoke).mockResolvedValueOnce(layout).mockResolvedValueOnce(undefined);

    await expect(loadTerminalLayout("checkout:one")).resolves.toEqual(layout);
    await saveTerminalLayout("checkout:one", layout);

    expect(invoke).toHaveBeenNthCalledWith(1, "terminal_layout_load", { checkoutId: "checkout:one" });
    expect(invoke).toHaveBeenNthCalledWith(2, "terminal_layout_save", { checkoutId: "checkout:one", layout });
  });
});

describe("Git review IPC client", () => {
  beforeEach(() => vi.clearAllMocks());

  it("uses the checkout-scoped diff page command", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({ path: "src/file.ts", lines: [] });

    await getGitDiffPage("checkout:one", "src/file.ts", 320, 32);

    expect(invoke).toHaveBeenNthCalledWith(1, "git_diff_page", {
      checkoutId: "checkout:one",
      path: "src/file.ts",
      offset: 320,
      limit: 32,
    });
  });

  it("asks for diff counts with a checkout id and never a ref or a path", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce({ "checkout:one": { additions: 3, deletions: 1 } })
      .mockResolvedValueOnce([]);

    await getGitCheckoutDiffStats();
    await getGitDiffStats("checkout:one");

    // The all-checkouts call carries no argument at all: the base ref is resolved in Rust.
    expect(invoke).toHaveBeenNthCalledWith(1, "git_checkout_diff_stats");
    expect(invoke).toHaveBeenNthCalledWith(2, "git_diff_stats", { checkoutId: "checkout:one" });
  });
});

describe("Review note IPC client", () => {
  beforeEach(() => vi.clearAllMocks());

  it("sends checkout-scoped review note commands", async () => {
    vi.mocked(invoke).mockResolvedValue(undefined);

    await listReviewNotes("checkout:one");
    await createReviewNote({
      checkoutId: "checkout:one",
      path: "src/foo.js",
      side: "new",
      lineStart: 10,
      lineEnd: 12,
      content: "revisit this calculation",
      code: "const result = a + b;",
    });
    await updateReviewNote("checkout:one", "note:1", "edited");
    await deleteReviewNote("checkout:one", "note:1");

    expect(invoke).toHaveBeenNthCalledWith(1, "review_notes", { checkoutId: "checkout:one" });
    expect(invoke).toHaveBeenNthCalledWith(2, "review_note_create", {
      request: {
        checkoutId: "checkout:one",
        path: "src/foo.js",
        side: "new",
        lineStart: 10,
        lineEnd: 12,
        content: "revisit this calculation",
        code: "const result = a + b;",
      },
    });
    expect(invoke).toHaveBeenNthCalledWith(3, "review_note_update", {
      checkoutId: "checkout:one",
      id: "note:1",
      content: "edited",
    });
    expect(invoke).toHaveBeenNthCalledWith(4, "review_note_delete", {
      checkoutId: "checkout:one",
      id: "note:1",
    });
  });

  it("sends checkout-scoped agent commands", async () => {
    vi.mocked(invoke).mockResolvedValue(undefined);

    await listAgentSessions("checkout:one");
    await createAgentSession("checkout:one", "Review 2026-09-26");
    await stopAgent("checkout:one");

    expect(invoke).toHaveBeenNthCalledWith(1, "agent_sessions", { checkoutId: "checkout:one" });
    expect(invoke).toHaveBeenNthCalledWith(2, "agent_session_create", {
      checkoutId: "checkout:one",
      title: "Review 2026-09-26",
    });
    expect(invoke).toHaveBeenNthCalledWith(3, "agent_stop", { checkoutId: "checkout:one" });
  });

  it("sends review round commands, which record before they send", async () => {
    vi.mocked(invoke).mockResolvedValue(undefined);

    await listReviewRounds("checkout:one");
    await dispatchReviewRound({
      checkoutId: "checkout:one",
      sessionId: "ses_one",
      ids: ["note:1"],
      markdown: "# Code Review",
      queue: false,
    });
    await requeueReviewRounds("checkout:one");
    await reconcileReviewRound("checkout:one", "round:1");
    await ackReviewRound("checkout:one", "round:1");
    await resolveReviewNote("checkout:one", "note:1");

    expect(invoke).toHaveBeenNthCalledWith(1, "review_rounds", { checkoutId: "checkout:one" });
    expect(invoke).toHaveBeenNthCalledWith(2, "review_round_dispatch", {
      request: {
        checkoutId: "checkout:one",
        sessionId: "ses_one",
        ids: ["note:1"],
        markdown: "# Code Review",
        queue: false,
      },
    });
    expect(invoke).toHaveBeenNthCalledWith(3, "review_rounds_requeue", { checkoutId: "checkout:one" });
    expect(invoke).toHaveBeenNthCalledWith(4, "review_round_reconcile", {
      checkoutId: "checkout:one",
      roundId: "round:1",
    });
    expect(invoke).toHaveBeenNthCalledWith(5, "review_round_ack", {
      checkoutId: "checkout:one",
      roundId: "round:1",
    });
    expect(invoke).toHaveBeenNthCalledWith(6, "review_note_resolve", {
      checkoutId: "checkout:one",
      id: "note:1",
    });
  });

  it("sends anchor checks and clears the outdated mark", async () => {
    vi.mocked(invoke).mockResolvedValue(undefined);

    await verifyReviewNoteAnchors("checkout:one", "src/foo.js", [
      { id: "note:1", currentCode: "const result = a - b;" },
    ]);
    await clearReviewNoteOutdated("checkout:one", "note:1");

    expect(invoke).toHaveBeenNthCalledWith(1, "review_note_anchors_verify", {
      request: {
        checkoutId: "checkout:one",
        path: "src/foo.js",
        checks: [{ id: "note:1", currentCode: "const result = a - b;" }],
      },
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "review_note_outdated_clear", {
      checkoutId: "checkout:one",
      id: "note:1",
    });
  });
});
