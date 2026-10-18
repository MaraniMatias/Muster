/**
 * Where a document's metadata block is, if it has one.
 *
 * The two places that care are the preview, which drops the block because a page of metadata is not
 * the document, and the source view, which keeps it and reads it as the YAML it is. They have to
 * agree on where the block ends, so the rule lives here rather than being written twice.
 */

/** The line that opens a metadata block, and the one that closes it. */
export const FRONT_MATTER_FENCE = "---";
/** YAML lets a document end with this instead, and a file that writes one means it. */
const FRONT_MATTER_END = "...";

/** Whether a line opens a metadata block, with the indentation one may carry around it. */
export function opensFrontMatter(line: string): boolean {
  return line.trim() === FRONT_MATTER_FENCE;
}

/** Whether a line closes the block, which is either fence a document may be closed with. */
function closesFrontMatter(line: string): boolean {
  const text = line.trim();
  return text === FRONT_MATTER_FENCE || text === FRONT_MATTER_END;
}

/**
 * How many lines the metadata block takes at the top of the document, or 0 when it does not open
 * with one.
 *
 * A document that merely *starts* with a rule is not metadata: a rule needs a line of its own and
 * the block needs a second fence to close, and a file that opens with one and never closes it is
 * written as prose.
 */
export function frontMatterLineCount(source: string): number {
  const lines = source.split(/\r?\n/);
  if (!opensFrontMatter(lines[0] ?? "")) return 0;
  const closing = lines.findIndex((line, index) => index > 0 && closesFrontMatter(line));
  return closing < 0 ? 0 : closing + 1;
}

/** The YAML between a document's opening and closing metadata fences, or null when there is none. */
export function frontMatterContent(source: string): string | null {
  const count = frontMatterLineCount(source);
  if (count === 0) return null;
  return source
    .split(/\r?\n/)
    .slice(1, count - 1)
    .join("\n");
}

/** The document without its metadata block, which is what a preview of it renders. */
export function withoutFrontMatter(source: string): string {
  const count = frontMatterLineCount(source);
  if (count === 0) return source;
  return source.split(/\r?\n/).slice(count).join("\n");
}
