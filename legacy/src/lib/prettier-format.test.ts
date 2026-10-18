import { beforeEach, describe, expect, it, vi } from "vitest";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { resolveConfig } from "prettier";
import { failureAnswer, formatSource, formattingParser, runFormat, resolveFormatOptions } from "./prettier-format";
import type { FormatAnswer, FormatCommand } from "./prettier-format";

/**
 * Stands in for the worker: the same answer, on this thread.
 *
 * Neither this environment nor the component one has a `Worker`, and what is under test is the
 * round trip and not the thread. The reply waits a microtask because a real worker answers on
 * another task, and the code must not come to depend on the answer arriving first.
 */
class InlinePrettierWorker {
  private listener: ((event: { data: FormatAnswer }) => void) | null = null;
  private failures: ((event: { message: string }) => void)[] = [];

  /** Whether the next worker created fails the way one whose script will not load does. */
  static failOnStart = false;

  addEventListener(
    type: string,
    listener: ((event: { data: FormatAnswer }) => void) | ((event: { message: string }) => void),
  ): void {
    if (type === "message") this.listener = listener as (event: { data: FormatAnswer }) => void;
    else this.failures.push(listener as (event: { message: string }) => void);
  }

  removeEventListener(): void {}

  terminate(): void {}

  /** Stands in for a worker that cannot load: it fires `error` once and then answers nothing. */
  reportStartFailure(message: string): void {
    InlinePrettierWorker.failOnStart = false;
    for (const failure of this.failures) failure({ message });
  }

  postMessage(command: FormatCommand): void {
    void Promise.resolve().then(async () => {
      const answer = await runFormat(command.request).then(
        (text): FormatAnswer => ({ id: command.id, text }),
        (error: unknown): FormatAnswer => failureAnswer(command.id, error),
      );
      this.listener?.({ data: answer });
    });
  }
}

describe("formattingParser", () => {
  it("names the parser for every grammar Prettier can read", () => {
    expect(formattingParser("typescript")).toBe("typescript");
    expect(formattingParser("tsx")).toBe("typescript");
    expect(formattingParser("javascript")).toBe("babel");
    expect(formattingParser("jsonc")).toBe("jsonc");
    expect(formattingParser("scss")).toBe("scss");
    expect(formattingParser("vue")).toBe("vue");
    expect(formattingParser("markdown")).toBe("markdown");
  });

  it("has no parser for the grammars Prettier does not read", () => {
    for (const language of ["rust", "python", "go", "java", "c", "cpp", "php", "lua", "shellscript"]) {
      expect(formattingParser(language)).toBeNull();
    }
    // The app's own name for "no grammar at all" is a grammar Prettier has never heard of.
    expect(formattingParser("plaintext")).toBeNull();
    expect(formattingParser(null)).toBeNull();
    expect(formattingParser("xml")).toBe("xml");
  });
});

describe("runFormat", () => {
  it("rewrites every grammar the toolbar offers that Prettier can read", async () => {
    const cases = [
      { language: "typescript", path: "a.ts", content: "const   x:number=1" },
      { language: "tsx", path: "a.tsx", content: "const   X=()=><b   />" },
      { language: "javascript", path: "a.js", content: "const   x=1" },
      { language: "json", path: "a.json", content: '{"a":   1}' },
      { language: "jsonc", path: "a.jsonc", content: '{ // note\n"a":   1}' },
      { language: "json5", path: "a.json5", content: "{a:   1,}" },
      { language: "css", path: "a.css", content: "a{color:red}" },
      { language: "scss", path: "a.scss", content: "a{&:hover{color:red}}" },
      { language: "less", path: "a.less", content: "a{color:red}" },
      { language: "html", path: "a.html", content: "<div   id='a'>x</div>" },
      { language: "vue", path: "a.vue", content: "<template><div   id='a'>x</div></template>" },
      { language: "markdown", path: "a.md", content: "#    Title" },
      { language: "yaml", path: "a.yaml", content: "a:    1" },
      { language: "graphql", path: "a.graphql", content: "query   A{__typename}" },
    ];
    for (const { language, path, content } of cases) {
      const formatted = await runFormat({ content, language, path });
      expect(formatted, path).not.toBe(content);
      expect(formatted, path).toContain("\n");
    }
  });

  it("lays Markdown front matter out with the YAML rules rather than leaving it as it was", async () => {
    const formatted = await runFormat({
      content: "---\ntitle:   A title\n---\n\n# Heading\n",
      language: "markdown",
      path: "a.md",
    });
    expect(formatted).toBe("---\ntitle: A title\n---\n\n# Heading\n");
  });

  it("formats the script and the styles of a Vue component, not only its template", async () => {
    const formatted = await runFormat({
      content: "<template><p>x</p></template>\n<script>\nconst   x=1\n</script>\n",
      language: "vue",
      path: "a.vue",
    });
    expect(formatted).toContain("const x = 1;");
  });

  it("applies the options the checkout's own config carried", async () => {
    const source = `const description = "${"x".repeat(40)}";`;
    const wide = await runFormat({
      content: source,
      language: "typescript",
      path: "a.ts",
      options: { printWidth: 120 },
    });
    const narrow = await runFormat({
      content: source,
      language: "typescript",
      path: "a.ts",
      options: { printWidth: 40 },
    });
    expect(wide.split("\n")).toHaveLength(2);
    expect(narrow.split("\n").length).toBeGreaterThan(2);
  });

  it("refuses a grammar Prettier has no parser for rather than formatting it as something else", async () => {
    await expect(runFormat({ content: "fn main() {}", language: "rust", path: "a.rs" })).rejects.toThrow(
      "Prettier does not format rust.",
    );
    await expect(runFormat({ content: "x", language: null, path: "a.txt" })).rejects.toThrow(
      "Prettier does not format this file.",
    );
  });

  it("passes a parse error on whole, with the place Prettier put it", async () => {
    const failure = await runFormat({ content: "const = ;", language: "typescript", path: "a.ts" }).then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(SyntaxError);
    expect((failure as Error).message).toContain("Variable declaration expected. (1:7)");
    // The line and column travel on the error itself, so anything holding it whole can point at
    // the character rather than repeat a code frame the reader has to count through.
    expect((failure as { loc?: { start: { line: number; column: number } } }).loc?.start).toEqual({
      line: 1,
      column: 7,
    });
  });
});

describe("config overrides", () => {
  it("matches Prettier basename, braces, classes, Unicode, exclusions and ordered nested config overrides", async () => {
    const root = await mkdtemp(join(tmpdir(), "muster-prettier-"));
    const options = {
      printWidth: 120,
      overrides: [
        { files: "*.ts", options: { semi: false } },
        { files: "src/*.{ts,tsx}", options: { tabWidth: 3 } },
        { files: "src/[ab]?.ts", options: { printWidth: 40 } },
        { files: ["**/é*.ts", "**/你*.ts"], options: { singleQuote: true } },
        { files: "*.ts", excludeFiles: ["src/a1.ts", "hidden.ts"], options: { printWidth: 60 } },
        { files: "src/**", options: { tabWidth: 7 } },
      ],
    };
    try {
      const config = join(root, ".prettierrc");
      await writeFile(config, JSON.stringify(options));
      for (const path of [
        "src/app.ts",
        "src/a1.ts",
        "src/ab.ts",
        "src/école.ts",
        "src/你好.ts",
        "src/app.tsx",
        "src/deep/a.ts",
        ".hidden.ts",
        "hidden.ts",
        "other/app.ts",
      ]) {
        const baseline = await resolveConfig(join(root, path), { config, useCache: false });
        expect(
          resolveFormatOptions({
            path: "packages/app/" + path,
            configPath: path,
            language: "typescript",
            content: "",
            options,
          }),
          path,
        ).toEqual(baseline);
      }
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});

describe("safe SVG formatting", () => {
  it("preserves meaningful text, CDATA, XML case and namespaces even with an unsafe config", async () => {
    const source = `<svg   xmlns="http://www.w3.org/2000/svg"><text xml:space="preserve">  a   b\n c  </text><text>a<tspan>  b  </tspan> c</text><style><![CDATA[  .a { content: "a   b"; }  ]]></style><linearGradient id="g"/><use href="#g"/></svg>`;
    const result = await runFormat({
      content: source,
      language: "xml",
      path: "icon.svg",
      options: { xmlWhitespaceSensitivity: "ignore", parser: "html", plugins: [] },
    });
    expect(result).not.toBe(source);
    for (const text of [
      "  a   b\n c  ",
      "a<tspan>  b  </tspan> c",
      `<![CDATA[  .a { content: "a   b"; }  ]]>`,
      "linearGradient",
      `xmlns="http://www.w3.org/2000/svg"`,
    ])
      expect(result).toContain(text);
    expect(await runFormat({ content: result, language: "xml", path: "icon.svg" })).toBe(result);
  });
  it("reports malformed XML rather than repairing it as HTML", async () => {
    await expect(runFormat({ content: "<svg><text></svg>", language: "xml", path: "icon.svg" })).rejects.toThrow();
  });
});

describe("failureAnswer", () => {
  it("keeps the line that names the place and drops the code frame under it", () => {
    const answer = failureAnswer(7, new SyntaxError("Unexpected token. (2:3)\n> 2 | x\n    |     ^"));
    expect(answer).toEqual({ id: 7, message: "Unexpected token. (2:3)" });
  });

  it("keeps a message that has no frame at all as it is", () => {
    expect(failureAnswer(1, new Error("nothing to strip"))).toEqual({ id: 1, message: "nothing to strip" });
    expect(failureAnswer(1, "a bare string")).toEqual({ id: 1, message: "a bare string" });
  });
});

describe("formatSource", () => {
  beforeEach(() => {
    InlinePrettierWorker.failOnStart = false;
    vi.stubGlobal("Worker", InlinePrettierWorker);
  });

  // Ahead of the formatting case on purpose, and not for tidiness: the worker is kept between
  // calls, so once anything has formatted there is one to reuse, and this assertion about not
  // starting another would hold for that reason rather than for its own.
  it("refuses a grammar with no parser without ever starting a worker", async () => {
    const started = vi.fn();
    vi.stubGlobal(
      "Worker",
      class extends InlinePrettierWorker {
        constructor() {
          super();
          started();
        }
      },
    );
    await expect(formatSource({ content: "fn main() {}", language: "rust", path: "a.rs" })).rejects.toThrow(
      "Prettier does not format rust.",
    );
    expect(started).not.toHaveBeenCalled();
  });

  it("answers with what the worker formatted", async () => {
    await expect(formatSource({ content: "const   x=1", language: "typescript", path: "a.ts" })).resolves.toBe(
      "const x = 1;\n",
    );
  });

  it("fails the waiting request when the worker cannot start, and starts over on the next click", async () => {
    // A worker whose script will not load never answers, so a request left waiting on it would hang
    // the Format button forever. It has to be failed here, and the dead thread has to be dropped so
    // the next click is a fresh one rather than another wait on nothing.
    let started = 0;
    class FailingWorker extends InlinePrettierWorker {
      constructor() {
        super();
        started += 1;
        if (InlinePrettierWorker.failOnStart) {
          queueMicrotask(() => this.reportStartFailure("the worker script would not load"));
        }
      }
    }
    vi.stubGlobal("Worker", FailingWorker);
    InlinePrettierWorker.failOnStart = true;
    // The worker is kept between clicks on purpose, and that also means the module above is holding
    // one from an earlier test. A fresh module is the only way to see this test create its first.
    vi.resetModules();
    const { formatSource: format } = await import("./prettier-format");

    const pending = [
      format({ content: "const   x=1", language: "typescript", path: "a.ts" }),
      format({ content: "const   y=2", language: "typescript", path: "b.ts" }),
    ];
    await Promise.all(
      pending.map((request) =>
        expect(request).rejects.toThrow("Prettier could not start: the worker script would not load"),
      ),
    );
    await expect(format({ content: "const   x=1", language: "typescript", path: "a.ts" })).resolves.toBe(
      "const x = 1;\n",
    );
    // Two threads for two clicks: the failed one was terminated and not kept.
    expect(started).toBe(2);
  });

  it("answers a failure with the line Prettier named and not its whole message", async () => {
    await expect(formatSource({ content: "const = ;", language: "typescript", path: "a.ts" })).rejects.toThrow(
      "Variable declaration expected. (1:7)",
    );
  });
});
