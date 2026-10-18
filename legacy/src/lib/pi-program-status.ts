import type { IDisposable, Terminal } from "@xterm/xterm";

const OSC_PI_PROGRAM_STATUS = 7501;
const MAX_OSC_CHARS = 4096;
const MAX_MESSAGE_BYTES = 2048;
const APP_NAME = /^[A-Za-z0-9_.+-]{1,32}$/;
const BASE64 = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;

export type PiProgramStatusState = "idle" | "working" | "blocked" | "done" | "error" | "clear";
export type PiProgramStatusKind = "permission" | "question" | "auth";
export type PiProgramStatusEvent =
  | { kind: "query" }
  | { kind: "progress"; state: "working" | "clear" }
  | {
      kind: "status";
      state: PiProgramStatusState;
      app: string | null;
      statusKind: PiProgramStatusKind | null;
    };

/** Parse Pi's bounded OSC 7501 payload; unknown applications and malformed fields are ignored. */
export function parsePiProgramStatus(data: string): PiProgramStatusEvent | null {
  if (data.length > MAX_OSC_CHARS) return null;
  if (data === "?") return { kind: "query" };
  if (!data.startsWith("state=")) return null;

  const fields = new Map<string, string>();
  for (const field of data.split(":")) {
    const separator = field.indexOf("=");
    if (separator < 1) return null;
    const key = field.slice(0, separator);
    if (fields.has(key)) return null;
    fields.set(key, field.slice(separator + 1));
  }

  const state = fields.get("state");
  if (!state || !["idle", "working", "blocked", "done", "error", "clear"].includes(state)) return null;
  if ([...fields.keys()].some((key) => !["state", "app", "kind", "msg"].includes(key))) return null;

  const app = fields.get("app") ?? null;
  if (app !== null && (!APP_NAME.test(app) || app !== "pi")) return null;

  const statusKind = fields.get("kind") ?? null;
  if (statusKind !== null && (state !== "blocked" || !["permission", "question", "auth"].includes(statusKind))) {
    return null;
  }

  const message = fields.get("msg");
  if (message !== undefined && (!BASE64.test(message) || atob(message).length > MAX_MESSAGE_BYTES)) return null;

  return {
    kind: "status",
    state: state as PiProgramStatusState,
    app,
    statusKind: statusKind as PiProgramStatusKind | null,
  };
}

/** Parse only the indeterminate and clear progress states Pi emits in OSC 9;4. */
export function parsePiProgress(data: string): Extract<PiProgramStatusEvent, { kind: "progress" }> | null {
  if (data === "4;3") return { kind: "progress", state: "working" };
  if (data === "4;0") return { kind: "progress", state: "clear" };
  return null;
}

/** Register before PTY output is attached; xterm handles BEL and ST OSC terminators. */
export function registerPiProgramStatus(
  terminal: Terminal,
  onEvent: (event: PiProgramStatusEvent) => void,
): IDisposable {
  const status = terminal.parser.registerOscHandler(OSC_PI_PROGRAM_STATUS, (data) => {
    const event = parsePiProgramStatus(data);
    if (!event) return false;
    onEvent(event);
    return true;
  });
  const progress = terminal.parser.registerOscHandler(9, (data) => {
    const event = parsePiProgress(data);
    if (!event) return false;
    onEvent(event);
    return true;
  });
  return {
    dispose() {
      status.dispose();
      progress.dispose();
    },
  };
}

export const PI_PROGRAM_STATUS_REPLY = "\u001b]7501;?\u001b\\";
