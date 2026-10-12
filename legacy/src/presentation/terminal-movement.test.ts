import { effectScope } from "vue";
import { describe, expect, it, vi } from "vitest";
import type { WorkspaceState } from "../domain/workspace";
import { useTerminalMovement } from "./terminal-movement-controller";

type Move = (
  sessionId: string,
  checkoutId: string,
  index: number,
  changeDirectory: boolean,
  selectTarget: boolean,
) => Promise<{ moved: boolean; directoryChange: "not-requested" | "written" | "not-written" } | undefined>;

function setup(moveSession: Move, shouldFollowWindow = false) {
  const workspace = {
    activeCheckoutId: "one",
    activeSessionId: null,
    repos: ["one", "two", "three"].map((id) => ({
      id: `repo:${id}`,
      kind: "git" as const,
      checkouts: [
        {
          id,
          isMissing: false,
          sessions: id === "one" ? [{ id: "terminal" }] : [],
        },
      ],
    })),
  } as unknown as WorkspaceState;
  const followMovedTerminal = vi.fn(async () => {});
  const scope = effectScope();
  const controller = scope.run(() =>
    useTerminalMovement({
      getWorkspace: () => workspace,
      getFollowMode: () => "agent",
      terminalShowsSession: (_checkout, terminal, agentSession) =>
        terminal === "terminal" && agentSession === "agent-session",
      shouldFollowWindow: () => shouldFollowWindow,
      moveSession,
      followMovedTerminal,
      onError: vi.fn(),
    }),
  )!;
  return { controller, followMovedTerminal, scope };
}

const relocation = (toCheckoutId: string, observedAt: number) => ({
  sessionId: "agent-session",
  fromCheckoutId: "one",
  toCheckoutId,
  observedAt,
});
const flush = async () => {
  for (let i = 0; i < 8; i += 1) await Promise.resolve();
};

describe("terminal movement controller", () => {
  it("clears a pending manual-directory timeout when its scope stops", async () => {
    vi.useFakeTimers();
    const move = vi.fn<Move>().mockResolvedValue({ moved: true, directoryChange: "written" });
    const { controller, scope } = setup(move);
    try {
      await controller.moveManually("terminal", "two", 0);
      expect(vi.getTimerCount()).toBe(1);
      scope.stop();
      expect(vi.getTimerCount()).toBe(0);
      vi.advanceTimersByTime(15_000);
      expect(move).toHaveBeenCalledTimes(1);
    } finally {
      scope.stop();
      vi.useRealTimers();
    }
  });

  it("does not start a scheduled move after scope disposal", async () => {
    const move = vi.fn<Move>().mockResolvedValue({ moved: true, directoryChange: "not-requested" });
    const { controller, scope } = setup(move);
    controller.onAgentRelocation(relocation("two", 100));
    scope.stop();
    await flush();
    expect(move).not.toHaveBeenCalled();
  });

  it("invalidates delayed focus when its terminal disappears", async () => {
    const move = vi.fn<Move>().mockResolvedValue({ moved: true, directoryChange: "not-requested" });
    const { controller, followMovedTerminal, scope } = setup(move, true);
    let isCurrent!: () => boolean;
    followMovedTerminal.mockImplementationOnce(async (...args: unknown[]) => {
      isCurrent = args[2] as () => boolean;
    });
    controller.onAgentRelocation(relocation("two", 100));
    await flush();
    expect(isCurrent()).toBe(true);
    controller.onStatus("terminal", null);
    expect(isCurrent()).toBe(false);
    scope.stop();
  });

  it("retries a failed observation when reoffered, but drops its in-flight duplicate", async () => {
    let rejectFirst!: (cause: Error) => void;
    const move = vi
      .fn<Move>()
      .mockImplementationOnce(
        () =>
          new Promise((_resolve, reject) => {
            rejectFirst = reject;
          }),
      )
      .mockResolvedValue({ moved: true, directoryChange: "not-requested" });
    const { controller, scope } = setup(move);
    const event = relocation("two", 100);

    controller.onAgentRelocation(event);
    await flush();
    controller.onAgentRelocation(event);
    controller.onAgentRelocation(event);
    rejectFirst(new Error("transient failure"));
    await flush();
    expect(move).toHaveBeenCalledTimes(1);

    controller.onAgentRelocation(event);
    await flush();
    expect(move).toHaveBeenCalledTimes(2);
    scope.stop();
  });

  it("keeps the newest queued observation after an in-flight move succeeds", async () => {
    let finishFirst!: (result: { moved: boolean; directoryChange: "not-requested" }) => void;
    const move = vi
      .fn<Move>()
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finishFirst = resolve;
          }),
      )
      .mockResolvedValue({ moved: true, directoryChange: "not-requested" });
    const { controller, scope } = setup(move);

    controller.onAgentRelocation(relocation("two", 100));
    await flush();
    controller.onAgentRelocation(relocation("three", 200));
    controller.onAgentRelocation(relocation("two", 150));
    finishFirst({ moved: true, directoryChange: "not-requested" });
    await flush();

    expect(move).toHaveBeenCalledTimes(2);
    expect(move.mock.calls[1]?.[1]).toBe("three");
    scope.stop();
  });

  it("retries when the pane reports not moved, then acknowledges success without following", async () => {
    const move = vi
      .fn<Move>()
      .mockResolvedValueOnce({ moved: false, directoryChange: "not-requested" })
      .mockResolvedValue({ moved: true, directoryChange: "not-requested" });
    const { controller, followMovedTerminal, scope } = setup(move);
    const event = relocation("two", 100);
    controller.onAgentRelocation(event);
    await flush();
    controller.onAgentRelocation(event);
    await flush();
    controller.onAgentRelocation(event);
    await flush();
    expect(move).toHaveBeenCalledTimes(2);
    expect(followMovedTerminal).not.toHaveBeenCalled();
    scope.stop();
  });

  it("does not acknowledge or follow a terminal that disappears during a move", async () => {
    let finishMove!: (result: { moved: boolean; directoryChange: "not-requested" }) => void;
    const move = vi.fn<Move>(
      () =>
        new Promise((resolve) => {
          finishMove = resolve;
        }),
    );
    const { controller, followMovedTerminal, scope } = setup(move, true);

    controller.onAgentRelocation(relocation("two", 100));
    await flush();
    controller.onStatus("terminal", null);
    finishMove({ moved: true, directoryChange: "not-requested" });
    await flush();

    expect(followMovedTerminal).not.toHaveBeenCalled();
    controller.onAgentRelocation(relocation("two", 100));
    expect(move).toHaveBeenCalledTimes(1);
    scope.stop();
  });

  it("suppresses delayed follow effects after scope disposal", async () => {
    let finishMove!: (result: { moved: boolean; directoryChange: "not-requested" }) => void;
    const move = vi.fn<Move>(
      () =>
        new Promise((resolve) => {
          finishMove = resolve;
        }),
    );
    const { controller, followMovedTerminal, scope } = setup(move, true);
    controller.onAgentRelocation(relocation("two", 100));
    await flush();
    controller.onAgentRelocation(relocation("three", 200));
    scope.stop();
    finishMove({ moved: true, directoryChange: "not-requested" });
    await flush();

    expect(move).toHaveBeenCalledTimes(1);
    expect(followMovedTerminal).not.toHaveBeenCalled();
    controller.onAgentRelocation(relocation("two", 300));
    expect(move).toHaveBeenCalledTimes(1);
  });
});
