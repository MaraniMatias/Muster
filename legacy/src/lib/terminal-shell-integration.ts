/**
 * Whether the last command in a shell worked, as reported by the shell itself.
 *
 * The backend knows whether a *shell* exited and with what code, and that is the only failure it
 * can see: a command that fails the way people actually fail — `clang: error: no input files`,
 * `zsh: command not found: sd` — prints to the PTY and leaves the shell right where it was, so the
 * process never exits and there is no exit code to read. The failing command is a grandchild of the
 * process Muster spawns, which `waitpid` cannot reach. The shell's own `$?` is the only source of
 * truth, and a shell only reports it if it is asked to.
 *
 * The asking is done once per session by `services/terminal.rs`, which writes OSC 133 markers to the
 * PTY: `D;<code>` after every command and `C` immediately before command execution. This reads them
 * back out of the byte stream and says what they mean. Nothing here touches the backend: a command's
 * exit code is a fact about the shell, not about the session's process, which is the same reasoning
 * that keeps `terminalTitle` in the component rather than in the IPC contract.
 *
 * `B` is the end of the prompt, which is where the shell's line editor starts: it rides at the
 * tail of `PS1` rather than being printed from the prompt hook, because a prompt is the last thing
 * a shell prints before the next line is typed. `C` is the command-start marker emitted from zsh's
 * `preexec_functions` and bash's `PS0`. See `services/terminal.rs` and its shell-level ordering
 * tests.
 *
 * No Vue, so the parsing is testable on its own: what is worth pinning here is the payload grammar,
 * which the shell decides and this code only reads.
 */
import type { IDisposable, Terminal } from "@xterm/xterm";

/**
 * The OSC ident the shell integration markers arrive on.
 *
 * 133 is the ident every shell-integration script uses, and xterm.js ships no handler for it, so
 * registering one here contends with nothing the terminal already does with those bytes.
 */
const OSC_SHELL_INTEGRATION = 133;

/** What the shell last said about the command in front of it. */
export type ShellIntegrationEvent =
  /** A command finished; `exitCode` is its `$?`, so zero is success and anything else is a failure. */
  | { kind: "command-finished"; exitCode: number }
  /**
   * A command started, which means whatever failed before it is no longer what the user is watching.
   *
   * From the OSC 133 `C` command-execution marker; see the note at the top of this file.
   */
  | { kind: "command-started" }
  /**
   * The prompt ended and the shell is reading a line again.
   *
   * This is the half of OSC 133's `A`/`B` pair that is worth having, and it is worth having for one
   * reason: everything before it is a prompt or output, which is text nobody is editing. Nothing is
   * said about *where* it landed, and that is deliberate — whoever wants the cell has to read it
   * off the terminal from inside this handler, because by the time the call returns the prompt has
   * finished printing and the cursor has moved.
   */
  | { kind: "input-started" };

/**
 * Reads the shell's exit codes out of the terminal's output.
 *
 * Returns the disposable that takes it back, because a terminal outlives most of what is registered
 * around it: a panel hiding, a session being replaced.
 */
export function registerShellIntegration(
  terminal: Terminal,
  onEvent: (event: ShellIntegrationEvent) => void,
): IDisposable {
  return terminal.parser.registerOscHandler(OSC_SHELL_INTEGRATION, (data) => {
    // `D;<code>`, `C` and `B` are the markers the hook writes. Anything else on this ident belongs
    // to some other producer, so it is reported as unhandled rather than guessed at.
    const [marker, code] = data.split(";");
    // `C` may carry the command text; the text says nothing this needs.
    if (marker === "C") {
      onEvent({ kind: "command-started" });
      return true;
    }
    if (marker === "B") {
      onEvent({ kind: "input-started" });
      return true;
    }
    if (marker !== "D" || !/^\d+$/.test(code)) return false;
    onEvent({ kind: "command-finished", exitCode: Number(code) });
    return true;
  });
}
