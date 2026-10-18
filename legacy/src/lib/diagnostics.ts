import { invoke } from "@tauri-apps/api/core";
import type { App } from "vue";

export const DIAGNOSTIC_CATEGORIES = [
  "frontend_vue_error",
  "frontend_window_error",
  "frontend_unhandled_rejection",
  "ui_writes_deadline",
  "exit_sweep_deadline",
] as const;

/**
 * The class of the failure, said as a word rather than as the message that carried it.
 *
 * A `frontend_window_error` on its own says that something failed, which is the one thing the line
 * already said; this is the cause. The vocabulary is closed because the line is a log file a user
 * can hand to someone, and an error message can hold a path, a URL or a fragment of what was on
 * screen. Reading the message to pick the word is fine, because the message itself stops here.
 */
export const DIAGNOSTIC_KINDS = [
  "type_error",
  "reference_error",
  "range_error",
  "syntax_error",
  "network_error",
  "abort_error",
  "security_error",
  "not_supported_error",
  "unknown",
] as const;

/**
 * Which phase of Vue was running, and the only form of "which component" that costs nothing:
 * a phase is named, and nothing the user owns is.
 *
 * Only `frontend_vue_error` has one. The other categories are reported as `unknown` rather than
 * left out, so every line has the same shape and a reader never has to know which fields to expect.
 */
export const DIAGNOSTIC_HOOKS = [
  "setup",
  "render",
  "lifecycle_hook",
  "event_handler",
  "watcher_callback",
  "unknown",
] as const;

export type DiagnosticCategory = (typeof DIAGNOSTIC_CATEGORIES)[number];
export type DiagnosticKind = (typeof DIAGNOSTIC_KINDS)[number];
export type DiagnosticHook = (typeof DIAGNOSTIC_HOOKS)[number];

/**
 * The whole of what a report carries. Four closed fields, one of them a counter: nothing here can
 * hold text the frontend chose, which is what lets Rust deserialize it into enums and refuse the
 * rest instead of logging it.
 */
export type DiagnosticReport = {
  category: DiagnosticCategory;
  kind: DiagnosticKind;
  hook: DiagnosticHook;
  /** Counts the reports this run actually let through, so two of them can be told apart and put in order. */
  sequence: number;
};

export type DiagnosticReporter = (category: DiagnosticCategory, cause?: unknown, vueInfo?: string) => Promise<void>;

const REPORT_INTERVAL_MS = 60_000;
const ERROR_DEDUPLICATION_MS = 1_000;

const KIND_BY_ERROR_NAME: Readonly<Record<string, DiagnosticKind>> = {
  TypeError: "type_error",
  ReferenceError: "reference_error",
  RangeError: "range_error",
  SyntaxError: "syntax_error",
  NetworkError: "network_error",
  AbortError: "abort_error",
  SecurityError: "security_error",
  NotSupportedError: "not_supported_error",
};

/**
 * A `fetch` that never reached the server still rejects with a `TypeError`, and the platform says so
 * in the only place it can: a fixed phrase in the message. A `TypeError` carrying one of these is a
 * network failure, and the phrase decides which word is sent.
 */
const NETWORK_FAILURE_PHRASES = ["fetch", "network", "load failed", "failed to load"];

/** Vue's phrases for a phase, dev builds only, scanned in the order that keeps the later ones from shadowing the earlier. */
const VUE_HOOK_BY_PHRASE: ReadonlyArray<readonly [string, DiagnosticHook]> = [
  ["setup", "setup"],
  ["watcher", "watcher_callback"],
  ["event handler", "event_handler"],
  // "renderTracked hook" and "renderTriggered hook" carry both words, and they are hooks.
  ["hook", "lifecycle_hook"],
  ["render", "render"],
];

const VUE_RUNTIME_ERROR_URL = /^https:\/\/vuejs\.org\/error-reference\/#runtime-(\d+|[a-z]+)$/;

function classifyDiagnosticCause(cause: unknown): DiagnosticKind {
  // A cause is whatever was thrown, so reading a property off it can throw as well. This runs inside
  // an error handler: a throw here would be the next failure reported, hiding the one being reported.
  try {
    if (typeof cause !== "object" || cause === null) return "unknown";
    const { name, message } = cause as { name?: unknown; message?: unknown };
    if (typeof name !== "string") return "unknown";
    if (name === "TypeError") {
      const text = typeof message === "string" ? message.toLowerCase() : "";
      return NETWORK_FAILURE_PHRASES.some((phrase) => text.includes(phrase)) ? "network_error" : "type_error";
    }
    return KIND_BY_ERROR_NAME[name] ?? "unknown";
  } catch {
    return "unknown";
  }
}

/**
 * Reads a release build's `info`, which is `https://vuejs.org/error-reference/#runtime-<code>` and
 * not a phrase. The codes are Vue's, matched against this closed table rather than parsed for
 * meaning: a code this build does not know, and a URL that is not that URL, are both `unknown`.
 */
function vueDiagnosticHookFromCode(code: string): DiagnosticHook {
  if (code === "0") return "setup";
  if (code === "1") return "render";
  // The watcher's getter, its callback and its cleanup all belong to the same watch.
  if (["2", "3", "4"].includes(code)) return "watcher_callback";
  // The listener on the element and the handler named by a template are the same thing to a reader.
  if (["5", "6"].includes(code)) return "event_handler";
  // 7-9 are the vnode, directive and transition hooks; a letter code is a lifecycle hook, which is
  // how Vue spells those. 10 and 11 are the app-level handlers, which are not a phase of anything.
  return ["7", "8", "9"].includes(code) || /^[a-z]+$/.test(code) ? "lifecycle_hook" : "unknown";
}

function vueDiagnosticHook(info: string | undefined): DiagnosticHook {
  if (info === undefined) return "unknown";
  const code = VUE_RUNTIME_ERROR_URL.exec(info)?.[1];
  if (code !== undefined) return vueDiagnosticHookFromCode(code);
  for (const [phrase, hook] of VUE_HOOK_BY_PHRASE) {
    if (info.includes(phrase)) return hook;
  }
  return "unknown";
}

export function createDiagnosticReporter(
  invokeDiagnostic: (report: DiagnosticReport) => Promise<unknown>,
  now: () => number = () => performance.now(),
): DiagnosticReporter {
  const lastReported = new Map<DiagnosticCategory, number>();
  const seenErrors = new WeakMap<object, number>();
  // Counted per run and only past the gates below, so the numbers in the log are dense: `n` is the
  // line before it, and a number that never appears is a report that never left this process rather
  // than one that reached the log and went missing.
  let sequence = 0;

  return (category, cause, vueInfo) => {
    const timestamp = now();
    if ((typeof cause === "object" && cause !== null) || typeof cause === "function") {
      const lastSeen = seenErrors.get(cause);
      if (lastSeen !== undefined && timestamp - lastSeen < ERROR_DEDUPLICATION_MS) return Promise.resolve();
      seenErrors.set(cause, timestamp);
    }

    const last = lastReported.get(category);
    if (last !== undefined && timestamp - last < REPORT_INTERVAL_MS) return Promise.resolve();
    lastReported.set(category, timestamp);

    const report: DiagnosticReport = {
      category,
      kind: classifyDiagnosticCause(cause),
      hook: category === "frontend_vue_error" ? vueDiagnosticHook(vueInfo) : "unknown",
      sequence: ++sequence,
    };

    return Promise.resolve()
      .then(() => invokeDiagnostic(report))
      .then(
        () => undefined,
        () => undefined,
      );
  };
}

export const reportFrontendDiagnostic = createDiagnosticReporter(({ category, kind, hook, sequence }) =>
  invoke<void>("frontend_diagnostic", { category, kind, hook, sequence }),
);

export function installFrontendErrorHandlers(app: App, report = reportFrontendDiagnostic): () => void {
  app.config.errorHandler = (error, _instance, info) => {
    if (import.meta.env.DEV) console.error("[Vue error]", error, info);
    void report("frontend_vue_error", error, info);
  };

  const onError = (event: ErrorEvent) => {
    void report("frontend_window_error", event.error);
  };
  const onUnhandledRejection = (event: PromiseRejectionEvent) => {
    void report("frontend_unhandled_rejection", event.reason);
  };
  window.addEventListener("error", onError);
  window.addEventListener("unhandledrejection", onUnhandledRejection);

  return () => {
    window.removeEventListener("error", onError);
    window.removeEventListener("unhandledrejection", onUnhandledRejection);
  };
}
