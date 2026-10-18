// @vitest-environment happy-dom
import { createApp } from "vue";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  createDiagnosticReporter,
  DIAGNOSTIC_CATEGORIES,
  DIAGNOSTIC_HOOKS,
  DIAGNOSTIC_KINDS,
  installFrontendErrorHandlers,
} from "./diagnostics";
import type { DiagnosticCategory, DiagnosticReport } from "./diagnostics";

const testRoot = { render: () => null };

/** A mock typed as the command it stands in for, so what it was handed is what the assertions read. */
const recorder = () => vi.fn<(report: DiagnosticReport) => Promise<unknown>>(async () => undefined);

/** One reporter per report, because the throttle is per reporter and these cases each need a line. */
async function reported(category: DiagnosticCategory, cause?: unknown, vueInfo?: string): Promise<DiagnosticReport> {
  const invokeDiagnostic = recorder();
  await createDiagnosticReporter(invokeDiagnostic)(category, cause, vueInfo);
  return invokeDiagnostic.mock.calls[0]![0];
}

describe("frontend diagnostics", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
    vi.restoreAllMocks();
  });

  it("sends only whitelisted categories and throttles each category", async () => {
    expect(DIAGNOSTIC_CATEGORIES).toEqual([
      "frontend_vue_error",
      "frontend_window_error",
      "frontend_unhandled_rejection",
      "ui_writes_deadline",
      "exit_sweep_deadline",
    ]);

    let now = 1_000;
    const invokeDiagnostic = vi.fn(async ({ category, kind, hook, sequence }: DiagnosticReport) => {
      expect(DIAGNOSTIC_CATEGORIES).toContain(category);
      expect(DIAGNOSTIC_KINDS).toContain(kind);
      expect(DIAGNOSTIC_HOOKS).toContain(hook);
      expect(sequence).toBeGreaterThan(0);
    });
    const report = createDiagnosticReporter(invokeDiagnostic, () => now);
    const privateError = new Error("private message must not cross the bridge");

    await Promise.all([report("frontend_vue_error", privateError), report("frontend_vue_error", privateError)]);
    await report("frontend_window_error", new Error("a different private error"));
    now += 60_000;
    await report("frontend_vue_error", privateError);

    expect(invokeDiagnostic.mock.calls.map(([entry]) => entry.category)).toEqual([
      "frontend_vue_error",
      "frontend_window_error",
      "frontend_vue_error",
    ]);
    expect(invokeDiagnostic.mock.calls[0]).toHaveLength(1);
  });

  it("carries no text: the whole report is a category, a kind, a phase and a counter", async () => {
    const invokeDiagnostic = recorder();
    const report = createDiagnosticReporter(invokeDiagnostic);
    const privateError = new TypeError("private message with /Users/someone/notes.md and a token");

    await report("frontend_vue_error", privateError, "setup function");
    await report("frontend_window_error", new TypeError("a second private message"));
    await report("frontend_unhandled_rejection", { path: "/Users/someone/notes.md", token: "sk-secret" });

    const sent = JSON.stringify(invokeDiagnostic.mock.calls);
    for (const leak of ["private message", "a second private", "Users", "notes.md", "sk-secret", "sk-"]) {
      expect(sent).not.toContain(leak);
    }
    expect(invokeDiagnostic.mock.calls.map(([entry]) => entry)).toEqual([
      { category: "frontend_vue_error", kind: "type_error", hook: "setup", sequence: 1 },
      { category: "frontend_window_error", kind: "type_error", hook: "unknown", sequence: 2 },
      { category: "frontend_unhandled_rejection", kind: "unknown", hook: "unknown", sequence: 3 },
    ]);
  });

  it("says the class of a failure in a word and not in its message", async () => {
    const cases: Array<[unknown, string]> = [
      [new TypeError("x is not a function"), "type_error"],
      [new TypeError("Failed to fetch"), "network_error"],
      [new TypeError("Load failed"), "network_error"],
      [new TypeError("NetworkError when attempting to fetch resource"), "network_error"],
      [{ name: "NetworkError" }, "network_error"],
      [new ReferenceError("private is not defined"), "reference_error"],
      [new RangeError("out of range"), "range_error"],
      [new SyntaxError("unexpected token"), "syntax_error"],
      [new DOMException("aborted", "AbortError"), "abort_error"],
      [new DOMException("denied", "SecurityError"), "security_error"],
      [new DOMException("nope", "NotSupportedError"), "not_supported_error"],
      [new Error("a plain failure"), "unknown"],
      [new EvalError("evaluated"), "unknown"],
      ["a rejection reason that is text", "unknown"],
      [42, "unknown"],
      [null, "unknown"],
      [undefined, "unknown"],
      [{ code: 20 }, "unknown"],
      // Reading a property off whatever was thrown can throw too, and this runs inside the handler
      // that is reporting the failure in the first place.
      [
        {
          get name(): string {
            throw new Error("property refused");
          },
        },
        "unknown",
      ],
    ];

    for (const [cause, expected] of cases) {
      expect((await reported("frontend_window_error", cause)).kind).toBe(expected);
    }
  });

  it("says which phase of Vue failed, in a dev phrase and in a release code", async () => {
    const cases: Array<[string | undefined, string]> = [
      ["setup function", "setup"],
      ["render function", "render"],
      ["watcher getter", "watcher_callback"],
      ["watcher callback", "watcher_callback"],
      ["watcher cleanup function", "watcher_callback"],
      ["component event handler", "event_handler"],
      ["native event handler", "event_handler"],
      ["beforeUnmount hook", "lifecycle_hook"],
      ["renderTracked hook", "lifecycle_hook"],
      ["serverPrefetch hook", "lifecycle_hook"],
      ["something Vue has not said yet", "unknown"],
      [undefined, "unknown"],
      // A release build gets a docs URL where a dev build gets a phrase, and the phase has to
      // survive that or the only builds that report it are the ones nobody reads.
      ["https://vuejs.org/error-reference/#runtime-0", "setup"],
      ["https://vuejs.org/error-reference/#runtime-1", "render"],
      ["https://vuejs.org/error-reference/#runtime-3", "watcher_callback"],
      ["https://vuejs.org/error-reference/#runtime-6", "event_handler"],
      ["https://vuejs.org/error-reference/#runtime-9", "lifecycle_hook"],
      ["https://vuejs.org/error-reference/#runtime-bu", "lifecycle_hook"],
      ["https://vuejs.org/error-reference/#runtime-10", "unknown"],
      ["https://vuejs.org/error-reference/#runtime-99", "unknown"],
      // Only that URL is a code. Anything else with digits in it is not Vue talking.
      ["https://example.invalid/#runtime-0", "unknown"],
      ["https://vuejs.org/error-reference/#runtime-0/../0", "unknown"],
    ];

    for (const [info, expected] of cases) {
      expect((await reported("frontend_vue_error", new Error("private"), info)).hook).toBe(expected);
    }
  });

  it("leaves the Vue phase out of a category that has no Vue phase", async () => {
    const context = await reported("frontend_unhandled_rejection", new Error("private"), "setup function");

    expect(context.hook).toBe("unknown");
  });

  it("counts only the reports it let through, so the numbers in the log are dense", async () => {
    let now = 1_000;
    const invokeDiagnostic = recorder();
    const report = createDiagnosticReporter(invokeDiagnostic, () => now);
    const first = new Error("private");
    const second = new Error("another private");

    // Three attempts: two are throttled away — one by the deduplication, one by the category — and
    // neither gets a number, because no line is ever going to carry it.
    await report("frontend_window_error", first);
    await report("frontend_window_error", first);
    await report("frontend_window_error", second);
    now += 60_000;
    await report("frontend_window_error", second);

    expect(invokeDiagnostic.mock.calls.map(([entry]) => entry.sequence)).toEqual([1, 2]);
    expect(invokeDiagnostic.mock.calls.map(([entry]) => entry)).toEqual([
      { category: "frontend_window_error", kind: "unknown", hook: "unknown", sequence: 1 },
      { category: "frontend_window_error", kind: "unknown", hook: "unknown", sequence: 2 },
    ]);
  });

  it("logs Vue errors in development without sending their payload to Rust", async () => {
    vi.stubEnv("DEV", true);
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    const invokeDiagnostic = recorder();
    const app = createApp(testRoot);
    const dispose = installFrontendErrorHandlers(app, createDiagnosticReporter(invokeDiagnostic));
    const error = new TypeError("private Vue error");

    app.config.errorHandler?.(error, null, "setup");
    await Promise.resolve();
    dispose();

    expect(consoleError).toHaveBeenCalledWith("[Vue error]", error, "setup");
    expect(invokeDiagnostic.mock.calls).toEqual([
      [{ category: "frontend_vue_error", kind: "type_error", hook: "setup", sequence: 1 }],
    ]);
  });

  it("does not echo Vue errors in release", async () => {
    vi.stubEnv("DEV", false);
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    const invokeDiagnostic = recorder();
    const app = createApp(testRoot);
    const dispose = installFrontendErrorHandlers(app, createDiagnosticReporter(invokeDiagnostic));
    const error = new TypeError("private Vue error");

    app.config.errorHandler?.(error, null, "setup function");
    await Promise.resolve();
    dispose();

    expect(consoleError).not.toHaveBeenCalled();
    expect(invokeDiagnostic.mock.calls).toEqual([
      [{ category: "frontend_vue_error", kind: "type_error", hook: "setup", sequence: 1 }],
    ]);
  });

  it("reports Vue and window failures without suppressing defaults or duplicating one error", async () => {
    vi.stubEnv("DEV", false);
    const reportedCategories: DiagnosticCategory[] = [];
    const report = createDiagnosticReporter(async (entry) => {
      reportedCategories.push(entry.category);
    });
    const app = createApp(testRoot);
    const dispose = installFrontendErrorHandlers(app, report);
    const sharedError = new Error("private stack must not be logged");

    app.config.errorHandler?.(sharedError, null, "setup");
    const duplicate = new ErrorEvent("error", { cancelable: true, error: sharedError });
    expect(window.dispatchEvent(duplicate)).toBe(true);
    expect(duplicate.defaultPrevented).toBe(false);

    const windowError = new ErrorEvent("error", { cancelable: true, error: new Error("private") });
    expect(window.dispatchEvent(windowError)).toBe(true);
    const rejection = new Event("unhandledrejection", { cancelable: true }) as PromiseRejectionEvent;
    Object.defineProperty(rejection, "reason", { value: "private rejection" });
    expect(window.dispatchEvent(rejection)).toBe(true);
    expect(rejection.defaultPrevented).toBe(false);

    await Promise.resolve();
    dispose();
    expect(reportedCategories).toEqual(["frontend_vue_error", "frontend_window_error", "frontend_unhandled_rejection"]);
  });

  it("does not echo an invoke failure", async () => {
    const report = createDiagnosticReporter(async () => {
      throw new Error("private invoke failure");
    });

    await expect(report("frontend_vue_error")).resolves.toBeUndefined();
  });
});
