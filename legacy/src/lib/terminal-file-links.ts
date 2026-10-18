/**
 * Paths in the terminal that open in the preview on ctrl+click, and web links that open in the
 * browser on the same key.
 *
 * xterm.js has a link provider for exactly this: it asks, once per hovered line, what that line
 * offers, and underlines whatever comes back. But it has no way to know whether a path names a
 * file, and it draws the underline the moment the mouse arrives. Both are wanted here, so the
 * provider answers asynchronously: it asks the backend whether each candidate is a file this
 * checkout holds and the preview can draw, and only hands xterm the ones that are. A path that
 * does not exist is never underlined, which is the whole behaviour.
 *
 * The backend is told where the terminal's shell is, because that is what a bare name means: an
 * `ls` prints `README.md` and `LICENSE` with no directory on them, and they name the files where
 * the shell is sitting. Every relative name is therefore read from there, which is why a listing
 * opens at the root of a checkout and below it alike.
 *
 * A URL is the other half of the same feature and needs none of that: it names a page rather
 * than a file, so there is nothing to confirm and nothing to ask the disk about. It is offered
 * from the text itself and can be underlined on the same tick the mouse arrives. `https` or
 * `http` only, which is the pair the opener on the other end opens and everything else it
 * refuses — a terminal that prints `file:///…` or `vscode://…` gets no underline rather than a
 * link that opens the wrong thing.
 *
 * The cost is one IPC per hovered line rather than per mouse move, because xterm asks per line
 * and stops asking until the line changes. The answers are cached because a terminal prints the
 * same path over and over, and the cache is what keeps a slow disk from making the underline
 * stutter as the mouse travels down a build log.
 *
 * The underline is xterm's own and nothing here paints one. xterm underlines the whole range a
 * link covers — every row it spans, not only the one under the pointer — in the terminal's own
 * foreground, and it does it from the same event a link's `hover` is called on. Registering a
 * decoration to recolour the path instead fires `onDecorationRegistered`, which makes xterm clear
 * and repaint the screen, and that repaint drops the underline it had just drawn: a hovered path
 * came out recoloured and not underlined. So a path is one link over its whole range and the
 * renderer draws it.
 *
 * Activation is deliberately narrow: xterm activates a link on any click, so `activate` opens
 * nothing unless ctrl or cmd is held. A plain click on a path or a link stays what it was: a click
 * that selects nothing, which the selection copy already ignores. Terminal output is not the
 * user's own text, so nothing in a build log can open a browser on its own.
 */
import type { IDisposable, ILink, ILinkProvider, Terminal } from "@xterm/xterm";
import { probeCheckoutFile } from "./ipc";
import { cellRuns, readLogicalLine, type LogicalLine } from "./terminal-buffer-line";
import { terminalPathsIn, terminalUrlsIn } from "./terminal-paths";

/**
 * How long a probe's answer is trusted.
 *
 * A build writes the files it is about to mention, so an answer cached for the rest of the
 * session would be wrong for exactly the paths a terminal is most useful on. Long enough that
 * moving the mouse along one line does not re-ask, short enough that a file created while the
 * terminal sits there becomes clickable without a restart.
 */
const PROBE_TTL_MS = 5_000;

/**
 * How many answers are kept.
 *
 * The TTL alone does not bound this: a terminal that prints a thousand distinct paths keeps a
 * thousand entries until each one expires, and the whole map dies with the panel anyway. Oldest
 * goes first, which for a terminal means the paths of the last build rather than the current one.
 */
const MAX_CACHED_PATHS = 256;

export interface FileLinkOptions {
  /**
   * The checkout the terminal belongs to, which is the only tree a path may name.
   *
   * Read through a getter because a terminal keeps its process and this registration when it is
   * moved to another worktree: the tree a path may name is a property of the terminal's current
   * worktree, not of the moment it was opened.
   */
  readonly checkoutId: string;
  /** Cached shell directory used only when no fresh directory reader is provided. */
  readonly workingDirectory?: string;
  /**
   * Reads the authoritative shell directory for a path batch and again on activation. Returning
   * `null` means status failed or the OS could not report a directory, so the provider fails closed.
   */
  refreshWorkingDirectory?: (checkoutId: string) => Promise<string | null>;
  /** Opens a confirmed file in the preview. */
  open: (path: string) => void;
  /**
   * Opens a web address in the browser, which is the only place one can be opened.
   *
   * Refused on the other end too: this is only handed something `terminalUrlsIn` already called a
   * page, so nothing here can name a file or an app, whatever the terminal printed.
   */
  openUrl: (url: string) => void;
}

interface CachedProbe {
  path: string | null;
  at: number;
}

/**
 * One link over the whole run, however many rows it wraps onto, or `null` when it covers no cell.
 *
 * xterm draws the underline itself and does it for the entire range — every row a link spans is
 * underlined, not only the one holding the pointer — and it draws it in the terminal's own
 * foreground, so the run reads as underlined rather than recoloured. A link per row would ask
 * xterm for the same underline N times and get N half-paths.
 *
 * Nothing is registered to draw here on purpose: `registerDecoration` fires
 * `onDecorationRegistered`, which makes xterm clear and repaint the whole screen, and that repaint
 * drops the underline it had just drawn for the hovered link. Painting the accent from a hover
 * handler is what made a hovered path look un-underlined.
 *
 * `open` is the narrow key for both kinds of link: xterm activates on any click, and a plain click
 * has to stay a click that selects nothing.
 */
function linkOver(
  line: LogicalLine,
  span: { start: number; end: number },
  text: string,
  open: (text: string) => void,
): ILink | null {
  const runs = cellRuns(line.cells, span.start, span.end);
  const first = runs[0];
  const last = runs[runs.length - 1];
  if (!first || !last) return null;
  return {
    // xterm's link ranges are 1-based and the end is the last cell, not the one past it.
    range: { start: { x: first.from + 1, y: first.row + 1 }, end: { x: last.to, y: last.row + 1 } },
    text,
    decorations: { pointerCursor: true, underline: true },
    activate(event, linkText) {
      if (!event.ctrlKey && !event.metaKey) return;
      open(linkText);
    },
  };
}

/**
 * Puts the paths and the web links in the terminal on screen where ctrl+click opens them.
 *
 * Returns the disposable that takes them back, because a terminal outlives most of what is
 * configured around it: a checkout closing, a panel hiding, a session being replaced.
 */
export function registerFilePathLinks(terminal: Terminal, options: FileLinkOptions): IDisposable {
  const cache = new Map<string, CachedProbe>();
  // One probe per path however many lines are waiting on it: the same path is usually the same
  // path, and asking twice for one answer is one IPC too many on the line the mouse crosses most.
  // Keyed by checkout and directory as well as name, because the same name can resolve differently
  // in either tree.
  const inFlight = new Map<string, Promise<string | null>>();
  let disposed = false;
  let latestLookup = 0;

  const probe = (checkoutId: string, raw: string, workingDirectory?: string): Promise<string | null> => {
    const key = `${checkoutId}\0${workingDirectory ?? ""}\0${raw}`;
    const cached = cache.get(key);
    if (cached && Date.now() - cached.at < PROBE_TTL_MS) return Promise.resolve(cached.path);
    const pending = inFlight.get(key);
    if (pending) return pending;

    const request = probeCheckoutFile(checkoutId, raw, workingDirectory)
      .then((probed) => probed?.path ?? null)
      .catch(() => {
        // A failed probe is a path this cannot open, which is the same answer a missing file gets
        // and the only one a hover can act on. The error belongs to the read the click would do.
        return null;
      })
      .then((path) => {
        inFlight.delete(key);
        // A terminal that is gone does not need the answer, and a map that outlives it would keep
        // every path the panel ever saw.
        if (disposed) return path;
        if (cache.size >= MAX_CACHED_PATHS) {
          const oldest = cache.keys().next();
          if (!oldest.done) cache.delete(oldest.value);
        }
        cache.set(key, { path, at: Date.now() });
        return path;
      });
    inFlight.set(key, request);
    return request;
  };

  const provider: ILinkProvider = {
    provideLinks(bufferLineNumber, callback) {
      const lookup = ++latestLookup;
      const buffer = terminal.buffer.active;
      // xterm's rows are 1-based and `getLine` is 0-based, so the row it hands over is not the row
      // it can be read from. Reading it as given underlines the line below the one under the mouse.
      const line = readLogicalLine(buffer, bufferLineNumber);
      // A URL is its own evidence, so these are settled before anything is asked of the disk and
      // they are the whole answer on a line like the one `curl` prints.
      const urlLinks = terminalUrlsIn(line.text)
        .map((candidate) => linkOver(line, candidate, candidate.url, options.openUrl))
        .filter((link): link is ILink => link !== null);
      const candidates = terminalPathsIn(line.text);
      if (candidates.length === 0) {
        callback(urlLinks.length > 0 ? urlLinks : undefined);
        return;
      }

      // Relative candidates share one authoritative directory read; absolute paths need no CWD.
      void (async () => {
        const checkoutId = options.checkoutId;
        const refreshWorkingDirectory = options.refreshWorkingDirectory;
        const hasRelativeCandidates = candidates.some(({ path }) => !path.startsWith("/"));
        let workingDirectory = options.workingDirectory;
        if (refreshWorkingDirectory && hasRelativeCandidates) {
          try {
            workingDirectory = (await refreshWorkingDirectory(checkoutId)) ?? undefined;
          } catch {
            // A failed fresh read cannot safely fall back to the last polled directory.
            workingDirectory = undefined;
          }
        }
        const current = () => !disposed && lookup === latestLookup && options.checkoutId === checkoutId;
        if (!current()) return;
        // Absolute paths do not depend on the shell directory; relative paths fail closed when a
        // fresh status could not report one. All candidates still share this one status read.
        const probed = await Promise.all(
          candidates.map((candidate) => {
            const absolute = candidate.path.startsWith("/");
            if (refreshWorkingDirectory && !absolute && workingDirectory === undefined) return null;
            return probe(checkoutId, candidate.path, absolute ? undefined : workingDirectory);
          }),
        );
        if (!current()) return;
        const links: ILink[] = [...urlLinks];
        for (const [index, candidate] of candidates.entries()) {
          const opened = probed[index];
          // No answer means no link: the path did not survive the check that it is a file this
          // checkout holds, and xterm is not told about it at all, so it cannot underline it.
          if (!opened) continue;
          const openIfCurrent = (path: string) => {
            if (!current()) return;
            if (!refreshWorkingDirectory || candidate.path.startsWith("/")) {
              options.open(path);
              return;
            }
            // The shell may `cd` between hover and click. Do not open a path resolved against the
            // old directory; failed status reads also fail closed.
            void refreshWorkingDirectory(checkoutId)
              .then((directory) => {
                if (current() && directory === workingDirectory) options.open(path);
              })
              .catch(() => undefined);
          };
          const link = linkOver(line, candidate, opened, openIfCurrent);
          if (link) links.push(link);
        }
        callback(links.length > 0 ? links : undefined);
      })();
    },
  };

  const registration = terminal.registerLinkProvider(provider);
  return {
    dispose: () => {
      disposed = true;
      registration.dispose();
    },
  };
}
