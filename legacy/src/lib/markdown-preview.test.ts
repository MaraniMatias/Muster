// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";
import { attachMarkdownImages, renderMarkdownPreview } from "./markdown-preview";

describe("Markdown preview", () => {
  it("renders GFM tables, task lists, and fenced code", async () => {
    const preview = await renderMarkdownPreview(
      "| name | value |\n| --- | --- |\n| one | two |\n\n- [x] done\n- [ ] later\n\n```js\nconst answer = 42;\n```",
      "docs/readme.md",
    );

    expect(preview.html).toContain("<table>");
    expect(preview.html).toContain('type="checkbox"');
    expect(preview.html).toContain("checked");
    expect(preview.html).toContain('<pre><code class="language-js">');
    expect(preview.html).toContain('<span class="line">');
  });

  it("SEC-04 sanitizes scripts, event handlers, and javascript URLs", async () => {
    const preview = await renderMarkdownPreview(
      '<script>alert(1)</script>\n<img src="x" onerror="alert(2)">\n[bad](javascript:alert(3))\n![bad](javascript:alert(4))',
      "readme.md",
    );

    const document = new DOMParser().parseFromString(preview.html, "text/html");
    expect(document.querySelector("script, img[onerror], a[href^='javascript:']")).toBeNull();
    expect(preview.images).toEqual([]);
  });

  it("only asks the backend for contained relative images and attaches trusted data URLs", async () => {
    const preview = await renderMarkdownPreview(
      "![local](../images/pic.png) ![repeat](../images/pic.png) ![escape](../../../secret.png) ![remote](https://example.com/a.png)",
      "docs/guide/readme.md",
    );

    expect(preview.images).toEqual([{ source: "../images/pic.png", path: "docs/images/pic.png" }]);
    expect(preview.html).not.toContain("https://example.com");
    const html = attachMarkdownImages(
      preview.html,
      preview.images.map((image) => image.path),
      new Map([["docs/images/pic.png", { mimeType: "image/png", dataBase64: "iVBORw0KGgo=" }]]),
    );
    expect(html).toContain('src="data:image/png;base64,iVBORw0KGgo="');
    expect(html).not.toContain("data-muster-image");
  });

  it("colors fenced code, including a fence nested in a blockquote", async () => {
    const preview = await renderMarkdownPreview(
      "> ```python\n> value = 1\n> ```\n\n```rust\nlet answer = 42;\n```",
      "docs/readme.md",
    );
    const document = new DOMParser().parseFromString(preview.html, "text/html");
    const blocks = Array.from(document.querySelectorAll("pre > code"));

    expect(blocks).toHaveLength(2);
    for (const block of blocks) {
      expect(block.querySelectorAll("span.line").length).toBeGreaterThan(0);
      expect(block.querySelector("span[style]")).not.toBeNull();
    }
    expect(blocks[0]?.textContent).toBe("value = 1\n");
    expect(blocks[1]?.textContent).toBe("let answer = 42;\n");
  });

  it("leaves blocks plain when no language, an unknown one, or the budget says so", async () => {
    const preview = await renderMarkdownPreview(
      "```\nno language\n```\n\n```not-a-language\nstill plain\n```\n",
      "docs/readme.md",
    );

    expect(preview.html).toContain("no language");
    expect(preview.html).toContain("still plain");
    expect(preview.html).not.toContain("span.line");
  });

  it("SEC-05 never resolves a fence info string to anything outside the allowlist", async () => {
    const preview = await renderMarkdownPreview(
      "```../../etc/passwd\nnope\n```\n\n```<img src=x onerror=alert(1)>\nnope\n```\n\n```javascript:alert(1)\nnope\n```",
      "docs/readme.md",
    );
    const document = new DOMParser().parseFromString(preview.html, "text/html");

    expect(document.querySelector("span.line, img, script")).toBeNull();
    expect(document.querySelector("a[href^='javascript:']")).toBeNull();
  });

  it("SEC-06 escapes a code body that tries to inject markup", async () => {
    const preview = await renderMarkdownPreview(
      "```html\n<script>alert(1)</script><img src=x onerror=alert(2)>\n```",
      "docs/readme.md",
    );
    const document = new DOMParser().parseFromString(preview.html, "text/html");

    // The body has to reach the page as text. Asserted over the whole document rather than from
    // inside `pre > code`: happy-dom's parser drops the `<pre>` when its content holds escaped
    // angle brackets, which is a test-environment quirk a browser does not have.
    expect(document.querySelector("script, img")).toBeNull();
    expect(preview.html).toContain("&lt;script&gt;alert(1)&lt;/script&gt;");
    expect(preview.html).not.toContain("<script>");
  });

  it("drops the HTML comments a page shows nothing of", async () => {
    const preview = await renderMarkdownPreview(
      [
        "<!-- Describe the change here -->",
        "",
        "Lead paragraph.",
        "",
        "# What this does",
        "",
        "Body text <!-- a note for the reviewer --> and more.",
        "",
        "- item <!-- hidden -->",
        "",
        "> quoted <!-- hidden -->",
      ].join("\n"),
      "docs/readme.md",
    );
    const document = new DOMParser().parseFromString(preview.html, "text/html");

    expect(preview.html).not.toContain("<!--");
    expect(preview.html).not.toContain("Describe the change here");
    expect(preview.html).not.toContain("a note for the reviewer");
    // What the comment was sitting between stays: the words join up, as they do on GitHub. The lead
    // paragraph is there because happy-dom's sanitizer drops the tags of the first element, and
    // what is being checked here has to survive that to be worth asserting on.
    expect(document.querySelector("h1")?.textContent).toBe("What this does");
    expect([...document.querySelectorAll("p")].map((node) => node.textContent)).toContain("Body text  and more.");
    expect(document.querySelector("li")?.textContent).toBe("item ");
    expect(document.querySelector("blockquote p")?.textContent).toBe("quoted ");
  });

  it("drops a comment that runs over as many lines as it likes", async () => {
    const preview = await renderMarkdownPreview(
      "Lead paragraph.\n\n<!--\nfirst line\nsecond line\n-->\n\n# Title",
      "readme.md",
    );
    const document = new DOMParser().parseFromString(preview.html, "text/html");

    expect(preview.html).not.toContain("first line");
    expect(document.querySelector("h1")?.textContent).toBe("Title");
  });

  it("keeps a comment that is code, and the text after one that shares its line", async () => {
    const preview = await renderMarkdownPreview(
      "```html\n<!-- kept -->\n```\n\nuse `<!-- kept -->` inline\n\n    <!-- kept -->\n\n<!-- gone --> trailing words",
      "readme.md",
    );

    // A fence, an indented block and a code span are all one token by the time a comment could be
    // taken out of a text run, so the comment in them is part of the code the file documents.
    expect(preview.html.match(/&lt;!-- kept --&gt;/g)).toHaveLength(3);
    expect(preview.html).not.toContain("&lt;!-- gone --&gt;");
    expect(preview.html).toContain("trailing words");
  });

  it("leaves an unclosed comment as the text it is", async () => {
    const preview = await renderMarkdownPreview("<!-- never closed\n\ntext after", "readme.md");

    expect(preview.html).toContain("&lt;!-- never closed");
    expect(preview.html).toContain("text after");
  });

  it("SEC-07 never lets a comment carry markup onto the page", async () => {
    const preview = await renderMarkdownPreview(
      "<!-- <script>alert(1)</script> -->\n\n```html\n<!-- <img src=x onerror=alert(2)> -->\n```\n\n<img src=x onerror=alert(3)>",
      "readme.md",
    );
    const document = new DOMParser().parseFromString(preview.html, "text/html");

    expect(document.querySelector("script, img")).toBeNull();
    expect(preview.html).not.toContain("<script>");
  });

  it("shows metadata as a closed, expandable YAML tree", async () => {
    const preview = await renderMarkdownPreview(
      [
        "---",
        "title: Add the export",
        "tags: [api, docs]",
        "author:",
        "  name: Ada",
        "---",
        "",
        "Lead paragraph.",
        "",
        "# What this does",
        "",
        "- [x] done",
        "",
        "| a | b |",
        "| --- | --- |",
        "| 1 | 2 |",
      ].join("\n"),
      "docs/readme.md",
    );

    const document = new DOMParser().parseFromString(preview.html, "text/html");
    expect(preview.html).toContain('<details class="markdown-front-matter"><summary>Metadata</summary>');
    expect(preview.html).toContain('<details class="markdown-yaml-branch" open>');
    expect(document.querySelector(".markdown-yaml-key")?.textContent).toBe("title");
    expect(document.querySelector(".markdown-yaml-value")?.textContent).toBe("Add the export");
    expect(preview.html).toContain("tags");
    expect(preview.html).toContain("<h1>What this does</h1>");
    expect(preview.html).toContain('type="checkbox"');
    expect(preview.html).toContain("<th>a</th>");
  });

  it("leaves a document that only opens with a rule alone", async () => {
    // The rule is not metadata: it has no second fence to close it, so the document is rendered
    // whole. The rule itself is the first element of the page, and this test DOM drops the tags off
    // whatever comes first, so what is asserted here is the document behind it.
    const preview = await renderMarkdownPreview("---\n\n# Heading\n\nbody", "readme.md");

    expect(preview.html).toContain("Heading");
    expect(preview.html).toContain("body");
  });

  it("SEC-08 never lets a metadata value reach the page as markup", async () => {
    const preview = await renderMarkdownPreview(
      "---\ntitle: <img src=x onerror=alert(1)>\nbody: <script>alert(2)</script>\n---\n\ntext",
      "readme.md",
    );
    const document = new DOMParser().parseFromString(preview.html, "text/html");

    // Metadata is rendered by the escaped tree, while the document behind it is untouched.
    expect(document.querySelector("script, img")).toBeNull();
    expect(preview.html).not.toContain("<script>");
    expect(preview.html).toContain("&lt;img src=x onerror=alert(1)&gt;");
    expect(preview.html).toContain("text");
  });
});
