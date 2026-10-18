import { Terminal } from "@xterm/headless";
import { describe, expect, it, vi } from "vitest";
import {
  parsePiProgramStatus,
  parsePiProgress,
  PI_PROGRAM_STATUS_REPLY,
  registerPiProgramStatus,
} from "./pi-program-status";
import { createPtyOutputWriter } from "./terminal-renderer";

describe("Pi OSC 7501 status", () => {
  it("accepts the query and replies with the same OSC body", () => {
    const registerOscHandler = vi.fn((ident: number, handler: (data: string) => boolean) => {
      if (ident === 7501) expect(handler("?")).toBe(true);
      return { dispose: vi.fn() };
    });
    const terminal = { parser: { registerOscHandler } } as never;
    const events: unknown[] = [];

    registerPiProgramStatus(terminal, (event) => events.push(event));

    expect(registerOscHandler).toHaveBeenCalledWith(7501, expect.any(Function));
    expect(registerOscHandler).toHaveBeenCalledWith(9, expect.any(Function));
    expect(events).toEqual([{ kind: "query" }]);
    expect(PI_PROGRAM_STATUS_REPLY).toBe("\u001b]7501;?\u001b\\");
  });

  it("parses the combined Pi handshake in OSC-before-device-attributes order across split writes", async () => {
    const terminal = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
    const events: string[] = [];
    const registration = registerPiProgramStatus(terminal as never, (event) => {
      expect(event).toEqual({ kind: "query" });
      events.push("status-query", "status-reply-queued");
    });
    terminal.onData((data) => {
      if (data.includes("\u001b[?1;2c")) events.push("device-attributes-reply");
    });
    const writer = createPtyOutputWriter(terminal, () => {});
    const write = async (data: string) => {
      writer.push(new TextEncoder().encode(data).buffer as ArrayBuffer);
      while (writer.pending > 0) await new Promise((resolve) => setTimeout(resolve, 0));
    };

    await write("\u001b[?u\u001b]7501;");
    expect(events).toEqual([]);
    await write("?\u001b\\\u001b[c");
    expect(events).toEqual(["status-query", "status-reply-queued", "device-attributes-reply"]);

    events.length = 0;
    await write("\u001b]7501;");
    await write("?\u0007\u001b[c");
    expect(events).toEqual(["status-query", "status-reply-queued", "device-attributes-reply"]);

    registration.dispose();
    events.length = 0;
    await write("\u001b]7501;?\u0007");
    expect(events).toEqual([]);
    writer.dispose();
    terminal.dispose();
  });

  it("parses Pi OSC 9;4 progress with split BEL and ST terminators", async () => {
    const terminal = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
    const events: unknown[] = [];
    const registration = registerPiProgramStatus(terminal as never, (event) => events.push(event));
    const writer = createPtyOutputWriter(terminal, () => {});
    const write = async (data: string) => {
      writer.push(new TextEncoder().encode(data).buffer as ArrayBuffer);
      while (writer.pending > 0) await new Promise((resolve) => setTimeout(resolve, 0));
    };

    await write("\u001b]9;4;3");
    expect(events).toEqual([]);
    await write("\u0007");
    expect(events).toEqual([{ kind: "progress", state: "working" }]);
    await write("\u001b]9;4;0");
    expect(events).toHaveLength(1);
    await write("\u001b\\");
    expect(events).toEqual([
      { kind: "progress", state: "working" },
      { kind: "progress", state: "clear" },
    ]);
    await write("\u001b]9;notification;message\u0007");
    expect(events).toHaveLength(2);
    expect(parsePiProgress("4;1;50")).toBeNull();

    registration.dispose();
    writer.dispose();
    terminal.dispose();
  });

  it("parses bounded Pi task states and accepts only known blocked kinds", () => {
    expect(parsePiProgramStatus("state=working:app=pi")).toMatchObject({ kind: "status", state: "working", app: "pi" });
    expect(parsePiProgramStatus("state=blocked:kind=question:app=pi")).toMatchObject({
      kind: "status",
      state: "blocked",
      statusKind: "question",
    });
    expect(parsePiProgramStatus("state=done:app=pi:msg=ZmluaXNoZWQ=")).toMatchObject({
      kind: "status",
      state: "done",
    });
    expect(parsePiProgramStatus("state=idle")).toMatchObject({ kind: "status", state: "idle" });
    expect(parsePiProgramStatus("state=clear:app=pi")).toMatchObject({ kind: "status", state: "clear" });
    expect(parsePiProgramStatus("state=blocked:kind=unknown:app=pi")).toBeNull();
    expect(parsePiProgramStatus("state=working:kind=question:app=pi")).toBeNull();
  });

  it("ignores unknown apps, malformed fields, oversized messages, and oversized reports", () => {
    expect(parsePiProgramStatus("state=working:app=opencode")).toBeNull();
    expect(parsePiProgramStatus("state=working:app=pi;anything")).toBeNull();
    expect(parsePiProgramStatus("state=working:app=pi:app=pi")).toBeNull();
    expect(parsePiProgramStatus("state=working:app=bad\nname")).toBeNull();
    expect(parsePiProgramStatus("state=working:app=pi:msg=" + "A".repeat(2732))).toBeNull();
    expect(parsePiProgramStatus("state=working:app=pi:msg=" + "A".repeat(4097))).toBeNull();
  });

  it("leaves unrelated OSC 9 and malformed OSC 7501 reports unhandled", () => {
    const callbacks = new Map<number, (data: string) => boolean>();
    const terminal = {
      parser: {
        registerOscHandler: (ident: number, handler: (data: string) => boolean) => {
          callbacks.set(ident, handler);
          return { dispose: vi.fn() };
        },
      },
    } as never;
    registerPiProgramStatus(terminal, vi.fn());
    expect(callbacks.get(7501)?.("state=working:app=other")).toBe(false);
    expect(callbacks.get(9)?.("notification;message")).toBe(false);
  });
});
