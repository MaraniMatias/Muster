// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";
import { trapDialogTab } from "./dialog-focus";

describe("trapDialogTab", () => {
  it("wraps Tab from the last control to the first and Shift+Tab back", () => {
    const dialog = document.createElement("section");
    dialog.tabIndex = -1;
    const first = document.createElement("button");
    const last = document.createElement("button");
    dialog.append(first, last);
    document.body.append(dialog);

    try {
      last.focus();
      const forward = new KeyboardEvent("keydown", { key: "Tab", cancelable: true });
      trapDialogTab(forward, dialog);
      expect(forward.defaultPrevented).toBe(true);
      expect(document.activeElement).toBe(first);

      first.focus();
      const backward = new KeyboardEvent("keydown", { key: "Tab", shiftKey: true, cancelable: true });
      trapDialogTab(backward, dialog);
      expect(backward.defaultPrevented).toBe(true);
      expect(document.activeElement).toBe(last);
    } finally {
      dialog.remove();
    }
  });

  it("keeps focus on the dialog when there are no enabled controls", () => {
    const dialog = document.createElement("section");
    dialog.tabIndex = -1;
    const disabled = document.createElement("button");
    disabled.disabled = true;
    dialog.append(disabled);
    document.body.append(dialog);

    try {
      const event = new KeyboardEvent("keydown", { key: "Tab", cancelable: true });
      trapDialogTab(event, dialog);
      expect(event.defaultPrevented).toBe(true);
      expect(document.activeElement).toBe(dialog);
    } finally {
      dialog.remove();
    }
  });
});
