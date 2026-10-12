/**
 * The thread Prettier runs on.
 *
 * It answers one question — format this text as that language — and nothing else, so everything it
 * needs arrives in the message and everything it has to say goes back in the reply. The formatter
 * itself and the table that chooses its parser live in `prettier-format.ts`, next to the side that
 * asks; only the wiring is here.
 */

import { failureAnswer, runFormat } from "./prettier-format";
import type { FormatAnswer, FormatCommand } from "./prettier-format";

// The window scope and the worker scope are both `self` and only one of them is this file's, so the
// two members this uses are named rather than the whole global being cast to `any`.
const scope = self as unknown as {
  addEventListener(type: string, listener: (event: MessageEvent<FormatCommand>) => void): void;
  postMessage(message: FormatAnswer): void;
};

scope.addEventListener("message", (event) => {
  const { id, request } = event.data;
  void runFormat(request).then(
    (text) => scope.postMessage({ id, text }),
    (error: unknown) => scope.postMessage(failureAnswer(id, error)),
  );
});
