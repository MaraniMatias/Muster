// @vitest-environment happy-dom
import { describe, expect, it, vi } from "vitest";
import {
  createSourceCache,
  highlightCodeBlock,
  highlightSource,
  highlightSourceAs,
  languageForFenceInfo,
  loadSourceHighlighter,
  sanitizeHighlightedHtml,
} from "./source-highlighter";
import { PLAIN_TEXT } from "./source-languages";

describe("source highlighter", () => {
  it("renders in the stylesheet's tokens rather than in a palette of its own", async () => {
    // One render, resolved by the browser against whichever palette is in effect. A hex here is a
    // palette of its own: it is what would stop a file from repainting when the theme changes.
    const lines = await highlightSource("src/tokens.ts", "const answer: number = 42;\n// note");

    expect(lines?.join("")).toContain("var(--muster-syntax-token-keyword)");
    expect(lines?.join("")).not.toMatch(/#[0-9a-f]{6}\b/i);
  });

  it("highlights known source and reuses the path/content cache entry", async () => {
    const first = highlightSource("src/cache.ts", "const answer: number = 42;\nreturn answer;");
    const second = highlightSource("src/cache.ts", "const answer: number = 42;\nreturn answer;");

    expect(second).toBe(first);
    const lines = await first;
    expect(lines).toHaveLength(2);
    expect(lines?.[0]).toContain('class="line"');
    expect(lines?.[0]).toContain("color:");
  });

  it("reads the same source as whichever grammar it is told to, and caches one per grammar", async () => {
    const source = "answer = 42";
    const python = highlightSourceAs("python", source);
    expect(highlightSourceAs("python", source)).toBe(python);
    const lines = await python;
    expect(lines?.[0]).toContain('class="line"');

    // The same text under another grammar is another render, which is the whole point of forcing
    // one: a cache keyed by anything but the grammar would hand back the first render.
    const shell = highlightSourceAs("shellscript", source);
    expect(shell).not.toBe(python);
    expect(await shell).not.toEqual(lines);
  });

  it("reads a grammar the reader chose over one the extension never gave", async () => {
    // This is what the toolbar's forced grammar reaches: a `.txt` is plain on its own.
    expect(await highlightSource("notes.txt", "answer = 42")).toBeNull();
    expect(await highlightSourceAs("python", "answer = 42")).toHaveLength(1);
  });

  it("colors the config dotfiles that carry no extension, and keeps a negation out of a comment", async () => {
    // `gitignore` has no grammar of its own in Shiki, so it renders as `ini` under a separate name.
    // If the indirection ever drops that name, this is the file that goes back to plain.
    const lines = await highlightSource(".gitignore", "# deps\nnode_modules\n!.env.example");
    expect(lines).toHaveLength(3);
    expect(lines?.[0]).toContain("var(--muster-syntax-token-comment)");
    // `!.env.example` re-includes a pattern, so it must not be painted as the `#` comment above.
    expect(lines?.[2]).not.toContain("var(--muster-syntax-token-comment)");

    const env = await highlightSource(".env.local", "# local\nNODE_ENV=development");
    expect(env).toHaveLength(2);
    expect(env?.[1]).toContain("color:");

    expect(await highlightSource(".prettierrc", '{ "semi": true }')).toHaveLength(1);
  });

  it("colors a shell startup file, whose name is its whole extension", async () => {
    // `.zshrc` ends in `zshrc`, not in a suffix the extension table registered, so this is the file
    // that goes back to plain if the name table loses an entry.
    const lines = await highlightSource(".zshrc", "# prompt\nexport EDITOR=nvim");
    expect(lines).toHaveLength(2);
    expect(lines?.[0]).toContain("var(--muster-syntax-token-comment)");
    expect(lines?.[1]).toContain("color:");
  });

  it("gives a name that asks for no grammar no highlighting, and no grammar a module path", async () => {
    for (const name of [
      "",
      "not-a-language",
      "../../etc/passwd",
      "shiki/langs/typescript.mjs",
      "javascript:alert(1)",
      "<img src=x onerror=alert(1)>",
    ]) {
      expect(await highlightSourceAs(name, "answer = 42")).toBeNull();
    }
    // Shiki ships no plaintext language, so the name that means "do not highlight" is one the
    // allowlist does not know, and the caller's own text is what stays on screen.
    expect(await highlightSourceAs(PLAIN_TEXT, "answer = 42")).toBeNull();
  });

  it("keeps only sanitized Shiki line markup", () => {
    const lines = sanitizeHighlightedHtml(
      '<pre><code><span class="line" onclick="alert(3)"><span style="color:#FF7B72">const</span><img src="x" onerror="alert(1)"><script>alert(2)</script></span></code></pre>',
    );

    expect(lines).toHaveLength(1);
    expect(lines[0]).toContain("const");
    expect(lines[0]).not.toMatch(/<\/?(?:pre|code|img|script)\b/i);
    expect(lines[0]).not.toMatch(/on(?:click|error)=/i);
  });

  it("resolves a fence info string through the allowlist only", () => {
    const language = languageForFenceInfo("ts");
    expect(language).toBeDefined();
    expect(languageForFenceInfo("BASH")).toBe(languageForFenceInfo("bash"));
    expect(languageForFenceInfo('ts title="app.ts"')?.name).toBe(language?.name);

    for (const hostile of [
      "",
      "   ",
      "not-a-language",
      "../../etc/passwd",
      "shiki/langs/typescript.mjs",
      "javascript:alert(1)",
      "<img src=x onerror=alert(1)>",
    ]) {
      expect(languageForFenceInfo(hostile)).toBeUndefined();
    }
  });

  it("returns the colored body of a block without the pre Shiki wraps it in", async () => {
    const language = languageForFenceInfo("typescript");
    const first = highlightCodeBlock(language!, "const answer: number = 42;");
    const second = highlightCodeBlock(language!, "const answer: number = 42;");

    expect(second).toBe(first);
    const html = await first;
    expect(html).toContain('<span class="line">');
    expect(html).toContain("color:");
    expect(html).not.toMatch(/<\/?(?:pre|code)\b/i);
  });

  it("keeps a hostile code body as escaped text, never as markup", async () => {
    const language = languageForFenceInfo("html");
    const body = '<span onclick="alert(1)">x</span><span style="background:url(javascript:alert(2))">y</span>';
    const html = await highlightCodeBlock(language!, body);
    const root = new DOMParser().parseFromString(`<div>${html}</div>`, "text/html").body.firstElementChild;

    // Shiki tokenizes the body as text, so the only way it could become live markup is if the
    // fragment carried anything but a class and a color.
    for (const element of Array.from(root?.querySelectorAll("*") ?? [])) {
      expect(element.tagName).toBe("SPAN");
      for (const attribute of Array.from(element.attributes)) {
        expect(["class", "style"]).toContain(attribute.name);
        expect(attribute.value).not.toMatch(/(?:url|expression|javascript)\s*\(|[<>]/i);
      }
    }
    // The body survived as the exact text it was, which is what "escaped, not parsed" means.
    expect(root?.textContent).toBe(body);
  });

  it("spends the rendered sources within a budget of UTF-16 code units, oldest file first", async () => {
    // Every entry here is a whole file, which is what makes a count of entries the wrong bound: a
    // code-unit budget is what says the same thing about thirty small files and one large one.
    const cache = createSourceCache(6000);
    const render = (source: string) => Promise.resolve([`<span class="line">${source}</span>`]);
    const source = (id: number) => `${id}`.padEnd(1000, ".");

    const first = cache.set("ini", source(1), render(source(1)));
    const second = cache.set("python", source(2), render(source(2)));
    expect(cache.retainedCodeUnits()).toBeLessThanOrEqual(6000);

    const third = cache.set("ini", source(3), render(source(3)));
    const fourth = cache.set("python", source(4), render(source(4)));
    await Promise.all([first, second, third, fourth]);

    // Each entry costs its own text plus the markup it rendered into, so four of them do not fit in a
    // budget of two and the two oldest go, whichever language they belong to.
    expect(cache.retainedCodeUnits()).toBeLessThanOrEqual(6000);
    expect(cache.get("ini", source(1))).toBeUndefined();
    expect(cache.get("python", source(2))).toBeUndefined();
    expect(cache.get("ini", source(3))).toBe(third);
    expect(cache.get("python", source(4))).toBe(fourth);

    // A file too big for the whole budget is still handed over; it is the remembering that is given up.
    const huge = "x".repeat(9000);
    expect(cache.get("ini", huge)).toBeUndefined();
    expect(await cache.set("ini", huge, render(huge))).toHaveLength(1);
    expect(cache.get("ini", huge)).toBeUndefined();
    expect(cache.retainedCodeUnits()).toBeLessThanOrEqual(6000);
  });

  it("counts ASCII, Latin, CJK and emoji as UTF-16 code units", async () => {
    const sources = ["A", "é", "漢", "🙂"];
    expect(sources.map((source) => source.length)).toEqual([1, 1, 1, 2]);

    for (const source of sources) {
      const rendered = "<span>" + source + "</span>";
      const codeUnits = source.length + rendered.length;
      const cache = createSourceCache(codeUnits);

      await cache.set("text", source, Promise.resolve([rendered]));

      expect(cache.retainedCodeUnits()).toBe(codeUnits);
      expect(cache.get("text", source)).toBeDefined();
    }
  });

  it("counts the file it holds once, not a second copy of it inside a key", async () => {
    const cache = createSourceCache(1024 * 1024);
    // Newlines and quotes are what a composed key escapes, doubling the units it holds of a file.
    const source = Array.from({ length: 40 }, (_, index) => `let x${index} = "${index}";`).join("\n");
    const markup = `<span class="line">${source}</span>`;

    await cache.set("typescript", source, Promise.resolve([markup]));

    expect(cache.retainedCodeUnits()).toBe(source.length + markup.length);
  });

  it("asks for a grammar again after its load failed, and forgets the failed render", async () => {
    // `rust` is not read anywhere else in this file, so this is the first load of it. A chunk that
    // failed to arrive is worth one more try: the language stays unusable for the whole session if
    // the rejection is what the highlighter map keeps.
    let attempts = 0;
    vi.doMock("shiki/langs/rust.mjs", async () => {
      attempts += 1;
      if (attempts === 1) throw new Error("chunk load failed");
      return await vi.importActual<typeof import("shiki/langs/rust.mjs")>("shiki/langs/rust.mjs");
    });

    const source = 'fn main() {\n    println!("hi");\n}';
    // Vitest wraps whatever a mock factory throws, so the message is its own. What is being asserted
    // is that the load failed at all, and that the second ask does not fail the same way.
    await expect(highlightSourceAs("rust", source)).rejects.toThrow();

    // The second ask loads the grammar: the first one's rejection is not what the map answers with.
    await expect(loadSourceHighlighter("rust")).resolves.toBeDefined();
    expect(attempts).toBe(2);

    // And the failed render was not cached either, so this file gets the retry instead of the error.
    const lines = await highlightSourceAs("rust", source);
    expect(lines).toHaveLength(3);
    expect(lines?.[0]).toContain('class="line"');
    expect(lines?.[0]).toContain("var(--muster-syntax-token-keyword)");
  });

  it("keeps the sanitizer a barrier against script, handlers and url(javascript:)", () => {
    const lines = sanitizeHighlightedHtml(
      [
        '<pre><code class="language-html">',
        '<span class="line" onclick="alert(1)" onmouseover="alert(2)">',
        "<script>alert(3)</script>",
        '<span style="color:#FF7B72;background:url(javascript:alert(4))">const</span>',
        '<span style="width:expression(alert(5))">x</span>',
        '<a href="javascript:alert(6)">link</a>',
        "<img src=x onerror=alert(7)>",
        '<iframe srcdoc="&lt;script&gt;alert(8)&lt;/script&gt;"></iframe>',
        "</span></code></pre>",
      ].join(""),
    );

    expect(lines).toHaveLength(1);
    expect(lines[0]).toContain("const");
    // Nothing that can run, and nothing that says it could: the whole of the payload is gone.
    expect(lines[0]).not.toMatch(/<\/?(?:script|img|a|iframe|code|pre)\b/i);
    expect(lines[0]).not.toMatch(/\son[a-z]+\s*=/i);
    expect(lines[0]).not.toMatch(/(?:url|expression|javascript)\s*[(:]/i);
    expect(lines[0]).not.toContain("alert");
    expect(lines[0]).not.toContain("#FF7B72");
  });
});
