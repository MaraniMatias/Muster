/** Keep keyboard focus inside an open modal dialog when Tab reaches either end. */
export function trapDialogTab(event: KeyboardEvent, dialog: HTMLElement | null) {
  if (event.key !== "Tab" || !dialog) return;

  const focusable = Array.from(
    dialog.querySelectorAll<HTMLElement>(
      "button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex]",
    ),
  ).filter((element) => element.tabIndex >= 0);

  if (!focusable.length) {
    event.preventDefault();
    dialog.focus();
    return;
  }

  const first = focusable[0]!;
  const last = focusable[focusable.length - 1]!;
  const active = document.activeElement;
  if (event.shiftKey && (active === first || !dialog.contains(active))) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && (active === last || !dialog.contains(active))) {
    event.preventDefault();
    first.focus();
  }
}

/** Close a modal on Escape and keep keyboard focus inside it on Tab. */
export function handleDialogKeydown(event: KeyboardEvent, onEscape: () => void) {
  if (event.key === "Escape") {
    event.stopPropagation();
    event.preventDefault();
    onEscape();
    return;
  }
  trapDialogTab(event, event.currentTarget as HTMLElement);
}
