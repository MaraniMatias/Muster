import { describe, expect, it, vi } from "vitest";
import type { IDisposable, Terminal } from "@xterm/xterm";
import { registerShellIntegration, type ShellIntegrationEvent } from "./terminal-shell-integration";

/**
 * A terminal whose only job is to hand back the OSC handler it was given, so the parsing can be
 * exercised without a DOM, a renderer or a PTY behind it. What is worth pinning is the payload
 * grammar the shell writes and this reads — the shell is the producer and cannot be mocked here, so
 * the payloads below are the ones the script in `services/terminal.rs` really writes.
 */
function stubTerminal() {
  const handlers = new Map<number, (data: string) => boolean>();
  let disposed = false;
  const terminal = {
    parser: {
      registerOscHandler(ident: number, callback: (data: string) => boolean): IDisposable {
        handlers.set(ident, callback);
        return {
          dispose: () => {
            disposed = true;
            handlers.delete(ident);
          },
        };
      },
    },
  };
  return {
    terminal: terminal as unknown as Terminal,
    /** Delivers what the terminal would have parsed out of the PTY's output. */
    send(data: string) {
      const handler = handlers.get(133);
      if (!handler) throw new Error("the OSC 133 handler was not registered");
      return handler(data);
    },
    get registered() {
      return handlers.has(133) && !disposed;
    },
  };
}

describe("shell integration", () => {
  it("reports the exit code of a command that finished", () => {
    const stub = stubTerminal();
    const events: ShellIntegrationEvent[] = [];
    registerShellIntegration(stub.terminal, (event) => events.push(event));

    // `false` is what a failed command leaves in `$?`, and the hook prints it verbatim.
    expect(stub.send("D;1")).toBe(true);
    expect(stub.send("D;0")).toBe(true);
    // 128+N is how a shell reports a signal-killed child, and it is still a non-zero failure.
    expect(stub.send("D;130")).toBe(true);
    expect(events).toEqual([
      { kind: "command-finished", exitCode: 1 },
      { kind: "command-finished", exitCode: 0 },
      { kind: "command-finished", exitCode: 130 },
    ]);
  });

  it("reports a command starting, so a previous failure stops being the answer", () => {
    const stub = stubTerminal();
    const events: ShellIntegrationEvent[] = [];
    registerShellIntegration(stub.terminal, (event) => events.push(event));

    expect(stub.send("D;1")).toBe(true);
    // `C` is OSC 133 command execution start; the shell hooks emit it just before executing a command.
    expect(stub.send("C")).toBe(true);
    expect(events).toEqual([{ kind: "command-finished", exitCode: 1 }, { kind: "command-started" }]);
  });

  it("reports command execution start, and the prompt ending where editing starts", () => {
    const stub = stubTerminal();
    const events: ShellIntegrationEvent[] = [];
    registerShellIntegration(stub.terminal, (event) => events.push(event));

    expect(stub.send("C")).toBe(true);
    expect(stub.send("B")).toBe(true);
    expect(events).toEqual([{ kind: "command-started" }, { kind: "input-started" }]);
  });

  it("reports command execution start and leaves the prompt start alone", () => {
    const stub = stubTerminal();
    const onEvent = vi.fn();
    registerShellIntegration(stub.terminal, onEvent);

    // `C` means command execution start and `B` the end of the prompt. `A` is the other end of that
    // prompt, and nothing here has a use for it, so it is left for whatever else wants it.
    expect(stub.send("A")).toBe(false);
    expect(stub.send("C;pnpm test")).toBe(true);
    expect(onEvent).toHaveBeenCalledWith({ kind: "command-started" });
  });

  it("leaves an exit code it cannot read to somebody else", () => {
    const stub = stubTerminal();
    const onEvent = vi.fn();
    registerShellIntegration(stub.terminal, onEvent);

    // `false` is the xterm.js answer for "not mine": it goes on to any other handler on this ident
    // rather than reporting a code nobody asked for.
    expect(stub.send("D")).toBe(false);
    expect(stub.send("D;")).toBe(false);
    expect(stub.send("D;not-a-number")).toBe(false);
    // A negative code is not something a shell produces, and `Number("-1")` would happily read it.
    expect(stub.send("D;-1")).toBe(false);
    // Another producer's marker on this ident, left for its handler.
    expect(stub.send("P;cwd")).toBe(false);
    expect(onEvent).not.toHaveBeenCalled();
  });

  it("takes a command marker in the fuller form, which carries the command text", () => {
    const stub = stubTerminal();
    const events: ShellIntegrationEvent[] = [];
    registerShellIntegration(stub.terminal, (event) => events.push(event));

    // `A;<command>` is the same event with the command line attached, and the text says nothing this needs.
    expect(stub.send("C;pnpm test")).toBe(true);
    expect(events).toEqual([{ kind: "command-started" }]);
  });

  it("takes the handler back when the registration is disposed", () => {
    const stub = stubTerminal();
    const onEvent = vi.fn();
    const registration = registerShellIntegration(stub.terminal, onEvent);

    stub.send("D;1");
    expect(onEvent).toHaveBeenCalledTimes(1);

    registration.dispose();
    // A disposed terminal must not keep answering: its parser is gone and a handler that outlived it
    // would be writing into a component that is no longer mounted.
    expect(stub.registered).toBe(false);
    expect(() => stub.send("D;1")).toThrow();
  });
});
