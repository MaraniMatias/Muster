export interface TerminalRenderer {
  write(data: Uint8Array, callback?: () => void): void;
}

/**
 * How often the reader is told how much of the output it sent has been parsed.
 *
 * The answer changes on every chunk, and a session under load changes it thousands of times a
 * second. Reporting it in steps is what makes that cheap, and it is safe in both directions: a
 * reader stopped for its window being behind is already a whole mark behind, so parsing one step
 * always crosses a report boundary, and a report a step late costs the reader at most a step of
 * progress — it lets the reader go while the window is between one and two steps under the low
 * mark, never under the high one it was stopped at.
 *
 * The two marks it is being stopped at are the reader's and not this file's: they live with the
 * gate in `src-tauri/src/terminal/mod.rs`, because that is the only thing that compares against
 * them and nothing here ever does. A step is an eighth of that high mark and a half of the low
 * one, which is what makes the two sentences above true.
 */
export const PTY_OUTPUT_REPORT_STEP_BYTES = 256 * 1024;

export interface PtyOutputWriter {
  /** Bytes received that the renderer has not parsed yet. */
  readonly pending: number;
  push(buffer: ArrayBuffer): void;
  dispose(): void;
}

/**
 * Feeds PTY output to xterm in as few writes as it will take, and says how far behind it is.
 *
 * The coalescing is the obvious half. A burst arrives as chunks of up to 64 KiB, xterm parses each
 * write on its own schedule, and handing it a thousand chunks queues a thousand parse tasks behind
 * a keyboard that answers none of them. What has arrived while one write is being parsed is joined
 * into the next one, so a burst is parsed in order and one write at a time rather than all at once.
 *
 * The half that is not obvious is `parsed`, and it is the reason this is not a call to `write`. The
 * reader counts what it hands over from the moment it hands it over, so what it needs from here is
 * the same number counted from this side: the difference between the two is everything still owed,
 * including the chunks that have not arrived yet. A count of what this writer happens to be holding
 * would report an empty queue for a burst that is still in the channel, and the reader would let go
 * of a gate it had every reason to keep shut.
 *
 * Nothing is dropped to make room: a byte that arrived is a byte that gets written, in the order it
 * arrived, which is the only property that makes stopping the reader safe at all.
 */
export function createPtyOutputWriter(
  renderer: TerminalRenderer,
  reportParsed: (parsed: number) => void,
): PtyOutputWriter {
  let queued: Uint8Array[] = [];
  let parsing = false;
  let received = 0;
  let parsed = 0;
  let bucket = 0;
  let lastReported = -1;
  let disposed = false;

  function syncPending() {
    // Bucketed on what this writer still holds rather than on the total it reports, because the
    // queue has to be heard draining as well as filling: a report is owed on both edges of a step.
    // And skipped when the total has not moved, because two reports of the same number are one
    // event with the same answer in it — which is also what makes the number safe to report
    // repeatedly, since a reader that takes the latest of them cannot be rewound by one.
    const next = Math.floor((received - parsed) / PTY_OUTPUT_REPORT_STEP_BYTES);
    if (next === bucket || parsed === lastReported) return;
    bucket = next;
    lastReported = parsed;
    reportParsed(parsed);
  }

  function flush() {
    if (disposed || parsing || queued.length === 0) return;
    const chunks = queued;
    queued = [];
    let size = 0;
    for (const chunk of chunks) size += chunk.length;
    const joined = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) {
      joined.set(chunk, offset);
      offset += chunk.length;
    }
    parsing = true;
    renderer.write(joined, () => {
      parsing = false;
      parsed += size;
      if (disposed) return;
      // Reported before the next write, so the reader hears the queue draining even when the
      // renderer has nothing left to do about it — which is the only moment it can act.
      syncPending();
      flush();
    });
  }

  return {
    get pending() {
      return received - parsed;
    },
    push(buffer) {
      if (disposed) return;
      // Counted on the way in rather than on the way to the renderer, so a chunk that is still
      // waiting its turn is pending too: that is exactly the work the reader is being stopped for.
      const bytes = new Uint8Array(buffer);
      queued.push(bytes);
      received += bytes.length;
      syncPending();
      flush();
    },
    dispose() {
      disposed = true;
      queued = [];
    },
  };
}
