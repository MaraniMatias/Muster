/**
 * The shapes a name in the panel can be drawn at, widest first.
 *
 * Nothing here cuts a string. A candidate is a shorter *whole* thing — one segment less of a path,
 * the detail word not drawn at all — and the ellipsis that takes the last few characters belongs to
 * CSS and runs only once not even the narrowest candidate fits. A row whose halves are allowed to
 * shrink together is a row that reads as two truncated things at once, which is what the panel's
 * two flex items did before the panel weighed them: `webapp` lost two characters while a path that
 * had not been shortened at all was still there taking its room.
 *
 * Every rung is a shape the label can be *drawn* at, so nothing here is a measurement: what fits is
 * answered by the panel, which is the only side that knows how wide a row is.
 */

/** How much of a path a header draws before the path has to start giving segments up. */
const PATH_SEGMENTS = 2;

/**
 * A path in the forms a header can draw it, widest first, with nothing at the end.
 *
 * The empty string is a rung and not an absence: once the panel is too narrow to say where a repo
 * lives, the header says the repo's name and nothing else. A prefix of `…` only ever appears where
 * a segment really was dropped — a path of one or two segments is drawn whole — because an ellipsis
 * in front of a path that is not elided is a claim the row cannot back up.
 */
export function pathSteps(path: string): string[] {
  const segments = path.split(/[\\/]/).filter(Boolean);
  if (segments.length === 0) return [""];
  const steps = [segments.length <= PATH_SEGMENTS ? path : `…/${segments.slice(-PATH_SEGMENTS).join("/")}`];
  if (segments.length > 1) steps.push(`…/${segments.at(-1)}`);
  steps.push("");
  return steps;
}

/**
 * A name in the forms a row can draw it, widest first, one segment given up per step.
 *
 * A branch is a path with a namespace at the front of it (`feature/sidebar/weigh-a-label`), so it
 * gives up its leading segments the way a path does: what is left is the half that names the work,
 * and the namespace is in the row's own tooltip. Unlike a path, the name is drawn whole to begin
 * with — it is the half of the row that says what the row is — and a name with no segments to drop
 * is a list of one, where the ellipsis is then the only thing that can shorten it.
 */
export function nameSteps(name: string): string[] {
  const segments = name.split("/").filter(Boolean);
  if (segments.length <= 1) return [name];
  return Array.from({ length: segments.length }, (_, step) =>
    step === 0 ? name : `…/${segments.slice(-(segments.length - step)).join("/")}`,
  );
}

/**
 * The widest candidate that fits the room, and the narrowest one when not even it does.
 *
 * Clamping to the last rung rather than to nothing is what keeps the tail: a name too long to be
 * drawn whole is still drawn as the shortest whole thing it has, so the ellipsis cuts the front of
 * that rather than the middle of the whole.
 */
export function widestThatFits(widths: number[], room: number): number {
  for (let step = 0; step < widths.length; step += 1) {
    if (widths[step] <= room) return step;
  }
  return widths.length - 1;
}
