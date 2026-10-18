// @vitest-environment happy-dom
import { beforeEach, describe, expect, it } from "vitest";
import { installContextMenu } from "./context-menu";

/** The chrome a right-click lands on when it misses, built the way the app builds it. */
function shell(inner: string): HTMLElement {
  document.body.innerHTML = `<div class="app-shell">${inner}</div>`;
  return document.querySelector(".app-shell") as HTMLElement;
}

/** A right-click on this target, and whether the webview's menu was taken off it. */
function denied(target: EventTarget | null): boolean {
  installContextMenu(document);
  const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
  target?.dispatchEvent(event);
  return event.defaultPrevented;
}

describe("Context menu", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  it("denies the webview menu on the chrome, which is where Reload is reachable", () => {
    const row = shell(`<div class="workdir-item"><span class="workdir-name">main</span></div>`);

    expect(denied(row.querySelector(".workdir-name"))).toBe(true);
  });

  // The surfaces that used to be granted the menu. A process watching the mouse draws a second
  // menu over this one, so a grant here is a double menu, and the Edit submenu carries the same
  // four commands behind ⌘C and ⌘V.
  it("denies it on the surfaces text is selected from as well", () => {
    shell(`
      <div class="code-editor-host"><div class="cm-content"><span class="cm-line">x</span></div></div>
      <div class="terminal-host"><div class="xterm"><div class="xterm-rows">y</div></div></div>
      <div class="source-read">z</div>
      <div class="markdown-preview">w</div>
      <div class="diff-files">v</div>
      <input />
      <textarea></textarea>
    `);

    for (const selector of [
      ".cm-line",
      ".xterm-rows",
      ".source-read",
      ".markdown-preview",
      ".diff-files",
      "input",
      "textarea",
    ]) {
      expect(denied(document.querySelector(selector)), selector).toBe(true);
    }
  });

  it("reads a right-click on a child of one of those surfaces, not only on the surface itself", () => {
    shell(`<div class="markdown-preview"><p><em>deep</em></p></div>`);

    expect(denied(document.querySelector("em"))).toBe(true);
  });

  it("takes the menu even from a library that handles the event on its way up", () => {
    const row = shell(`<div class="source-read"></div>`);
    row.addEventListener("contextmenu", (event) => event.stopPropagation());

    expect(denied(row)).toBe(true);
  });
});
