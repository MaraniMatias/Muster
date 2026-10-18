/** A run of lines, counted from 1 the way an editor and Git both count them. */
export interface LineRange {
  start: number;
  end: number;
}

/** `@@ -<old side> +<new side> @@`, keeping the new side's first line and nothing else. */
const HUNK_HEADER = /^@@ -\d+(?:,\d+)? \+(\d+)/;

/**
 * The lines a unified diff patch says the new side of a file has added.
 *
 * These are the new side's own line numbers, so they mean the same thing to a file on disk as they
 * do to the copy an editor is holding. That is the whole reason this is read from the patch rather
 * than from the hunks the backend already returns: those count lines of the *rendered diff*, which
 * is a different list, and a line number out of that one says nothing about the file.
 *
 * The hunk header's new-side count is not the answer, and taking it for one would mark the context
 * around a change as changed too: `@@ -1,5 +1,6 @@` describes a six-line window, not one changed
 * line. So the body is walked and the new side is counted along with it, which is also what settles
 * the two cases a header alone cannot. A hunk that only removes lines never advances the new side,
 * so it marks nothing — there is no line in the new file for it to have changed. An insertion
 * written `@@ -3,0 +4,1 @@` names line 4 of the new file, one past the old file's last line, and
 * that is the line the body then marks.
 *
 * The body is read the way the backend reads it (`parse_diff_display_line`), because the two have to
 * count the same file the same way. Only a space opens a context line, as Git writes them.
 *
 * Ranges come back merged and in order. Git's own hunks do not overlap, but the builder that draws
 * these into a document is owed ascending positions it can rely on, and merging here is cheaper
 * than trusting the shape of every patch that ever reaches it.
 */
export function changedLineRanges(patch: string): LineRange[] {
  const ranges: LineRange[] = [];
  let newLine = 0;
  let inHunk = false;
  for (const line of patch.split("\n")) {
    const header = HUNK_HEADER.exec(line);
    if (header) {
      newLine = Number(header[1]);
      inHunk = true;
    } else if (!inHunk) {
      continue;
    } else if (line.startsWith("+")) {
      ranges.push({ start: newLine, end: newLine });
      newLine += 1;
    } else if (line.startsWith(" ")) {
      newLine += 1;
    } else if (line.startsWith("-") || line.startsWith("\\")) {
      // A removed line is not a line of the new file, so the new side does not move past it. The
      // "no newline at end of file" marker belongs to the line above it and claims none either.
    } else {
      // Anything else is not part of a hunk: a second file's diff, or a line this reader does not
      // understand. Counting on past it would put every mark after it on the wrong line.
      inHunk = false;
    }
  }
  return mergeRanges(ranges);
}

function mergeRanges(ranges: LineRange[]): LineRange[] {
  const merged: LineRange[] = [];
  for (const range of ranges) {
    const previous = merged.at(-1);
    // Touching runs are one run: two marks either side of a shared line are one changed block, and
    // a builder handed the same position twice would refuse the set outright.
    if (previous && range.start <= previous.end + 1) previous.end = Math.max(previous.end, range.end);
    else merged.push({ ...range });
  }
  return merged;
}
