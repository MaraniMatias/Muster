import { ref } from "vue";

/**
 * What the pane is reading, all of it through getters because the pane is what changes its mind
 * about it: a file opens, a grammar is chosen, and the view changes without the text moving.
 */
export interface SourceHighlightPane {
  /** The text on screen, which is both what the grammar is asked about and what its answer is
   *  checked against: a reading taken over text that has since moved is worth nothing. */
  content: () => string;
  /** The grammar this file is read with, or null when nothing is loaded for it. */
  language: () => string | null;
  /** The document on screen, which is what an answer is checked to belong to. */
  fileIdentity: () => string | null;
  /** Whether the pane is drawing a Markdown preview, which owns its own fences and reads nothing. */
  markdownPreview: () => boolean;
}

/**
 * The source lines of the file on screen, and the reading of them that is on its way.
 *
 * A grammar is a dynamic import and reading a file is a pass over all of it, neither of which can be
 * awaited before the file is drawn: the reader opened a file, not a highlighted one. So the reading
 * is asked for on its own and lands whenever it lands, and what it is checked against is the text it
 * was asked about rather than the file it was asked for, because those are the two that can disagree
 * while the grammar is still loading.
 */
export function useSourceHighlight(pane: SourceHighlightPane) {
  const highlightedLines = ref<readonly string[] | null>(null);
  const highlightedSource = ref<string | null>(null);
  const highlighting = ref(false);
  /**
   * Bumped by every reading asked for and by every reading given up on, so the answer of one is
   * recognisable as spent once a newer one has been asked for or the file stopped being drawn.
   */
  let highlightGeneration = 0;

  /**
   * Gives up on the reading on screen without necessarily clearing what it already painted, which is
   * what a view change does: the Code view takes the Shiki lines away with it, and a view that is
   * about to be asked again should not flicker on the way.
   */
  function invalidateHighlight(clear = true) {
    highlightGeneration += 1;
    highlighting.value = false;
    if (clear) {
      highlightedLines.value = null;
      highlightedSource.value = null;
    }
  }

  function startHighlight(fileIdentity: string, source: string) {
    // Nothing to highlight without a document on screen, which is the same question the pane's path
    // asks: an identity exists for exactly the paths that do.
    if (pane.fileIdentity() === null) return;
    const language = pane.language();
    const request = ++highlightGeneration;
    highlightedLines.value = null;
    highlightedSource.value = null;
    highlighting.value = language !== null;
    if (language === null) return;

    void import("../lib/source-highlighter")
      .then(({ highlightSourceAs }) => highlightSourceAs(language, source))
      .then((lines) => {
        if (
          request !== highlightGeneration ||
          pane.fileIdentity() !== fileIdentity ||
          pane.content() !== source ||
          pane.markdownPreview()
        )
          return;
        highlightedLines.value = lines;
        highlightedSource.value = lines === null ? null : source;
        highlighting.value = false;
      })
      .catch(() => {
        if (request === highlightGeneration && pane.fileIdentity() === fileIdentity && pane.content() === source) {
          highlighting.value = false;
        }
      });
  }

  return { highlightedLines, highlightedSource, highlighting, startHighlight, invalidateHighlight };
}
