/**
 * Whether the program behind a PTY can be told which key was pressed, modifier and all.
 *
 * Shift+Enter is a key of its own, and a terminal that speaks for itself says so: the kitty keyboard
 * protocol encodes it as `ESC [ 13 ; 2 u`, where 13 is the key and 2 is one bit above the base
 * value, which is the shift. Everything else gets `\r`, because that escape is text to anything that
 * has not asked for it — a shell that received it would print the sequence instead of running the
 * line. xterm.js 6 implements no keyboard protocol at all, and its key mapping turns Return into
 * `\r` without ever looking at the shift, so this window has to do the part xterm will not: read
 * what the program announced about its own keyboard, answer it when it asks, and only then answer a
 * modified key with an encoding it can read.
 *
 * What a program announces is a set of flags, and the one that matters here is the first: pushed as
 * `ESC [ > flags u` (popped and restored with `ESC [ < u`, and set the older way with
 * `ESC [ = flags ; mode u`), it turns the disambiguation on. A question (`ESC [ ? u`) is answered
 * with the flags in force, and is deliberately not treated as agreement: a program that has decided
 * to read keys this way says so by pushing, not by asking.
 */

/** The flag that turns every ambiguous escape code into a key of its own. */
const DISAMBIGUATE_FLAG = 0b1;

/** What a terminal supports before any program has asked for anything: the legacy encodings. */
const INITIAL_FLAGS = 0;

/**
 * Enough of the previous chunk to hold an announcement the channel boundary cut in half. The longest
 * form the protocol has is a pair of five-digit flags, and a chunk of output ends wherever it ends.
 */
const ANNOUNCEMENT_TAIL = 32;

/** The escape, as a code rather than as a character: written out, it is a control character. */
const ESCAPE = String.fromCharCode(0x1b);

/**
 * A push of the flags a program wants, a pop, a question, or an answer to one — and, for the older
 * form, the mode that says how the flags are to be applied.
 */
const ANNOUNCEMENT = new RegExp(`${ESCAPE}\\[([<>?=])(\\d*)(?:[;: ](\\d+))*u`, "g");

/** How the older form applies what it is given: add to the flags in force, or take away from them. */
const ADD = 2;
const REMOVE = 3;

export interface KeyboardProtocol {
  /** Whether a modified key can be sent as a key of its own rather than as the bare one it looks like. */
  readonly csiU: boolean;
  /**
   * Reads one chunk of the program's output and keeps what it announced about its keyboard. What
   * goes to the screen belongs to the parser that draws it, and `answer` is how a question is put
   * back to the program that asked it.
   */
  read(output: ArrayBuffer): void;
}

export function watchKeyboardProtocol(answer: (data: string) => void): KeyboardProtocol {
  let flags = INITIAL_FLAGS;
  const pushed: number[] = [];
  // The tail is the part of the last chunk that could still be half an announcement, and it is only
  // ever content no match reached: a chunk of output ends wherever it ends.
  let tail = "";
  return {
    get csiU() {
      return (flags & DISAMBIGUATE_FLAG) !== 0;
    },
    read(output: ArrayBuffer) {
      // Latin-1 rather than UTF-8: an announcement is ASCII, and decoding would only put the bytes
      // of a character cut in half inside the window. One that ends mid character is harmless here
      // in a way that one ending mid announcement is not.
      const chunk = tail + Array.from(new Uint8Array(output), (byte) => String.fromCharCode(byte)).join("");
      // The carry is only ever content a match did not reach, so the whole of the chunk is read
      // again and nothing is read twice: a question carried whole would otherwise be answered by
      // every chunk that follows it.
      let lastRead = 0;
      for (const match of chunk.matchAll(ANNOUNCEMENT)) {
        const [, form, announced, mode] = match;
        if (form === "<") {
          flags = pushed.pop() ?? INITIAL_FLAGS;
        } else if (form === "?") {
          if (announced === "") answer(`${ESCAPE}[?${flags}u`);
        } else if (form === "=") {
          const asked = Number(announced);
          const how = Number(mode);
          flags = how === ADD ? flags | asked : how === REMOVE ? flags & ~asked : asked;
        } else {
          pushed.push(flags);
          flags = Number(announced);
        }
        lastRead = match.index + match[0].length;
      }
      const carried = chunk.length - lastRead;
      tail =
        carried > ANNOUNCEMENT_TAIL ? chunk.slice(lastRead + (carried - ANNOUNCEMENT_TAIL)) : chunk.slice(lastRead);
    },
  };
}
