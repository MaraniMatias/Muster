/**
 * Replay deterministic real-PTY captures through xterm. Regenerate with:
 * MUSTER_CAPTURE_FIXTURES="$PWD/src/lib/__fixtures__/terminal-shell-integration" cargo test \
 *   --manifest-path src-tauri/Cargo.toml captures_a_real_session -- --ignored
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { describe, expect, it } from "vitest";
import headless from "@xterm/headless";

const { Terminal } = headless;
const CAPTURES = join(dirname(fileURLToPath(import.meta.url)), "__fixtures__", "terminal-shell-integration");
const SESSIONS = ["zsh-login", "zsh-bare", "bash-login"];

function capture(name: string): Uint8Array {
  return new Uint8Array(readFileSync(join(CAPTURES, `${name}.bin`)));
}

async function renderedRows(bytes: Uint8Array): Promise<string[]> {
  const terminal = new Terminal({ cols: 80, rows: 24, allowProposedApi: true, scrollback: 500 });
  await new Promise<void>((resolve) => terminal.write(bytes, resolve));
  const buffer = terminal.buffer.active;
  const rows: string[] = [];
  for (let y = 0; y < buffer.length; y++) {
    rows.push(buffer.getLine(y)?.translateToString(true) ?? "");
  }
  terminal.dispose();
  return rows;
}

describe("native shell integration startup", () => {
  it("never echoes setup input or clears startup output", async () => {
    for (const name of SESSIONS) {
      const bytes = capture(name);
      const stream = new TextDecoder().decode(bytes);
      expect(stream, name).not.toMatch(/hook\.(zsh|bash)|bootstrap\.bash|source /);
      expect(stream, name).not.toContain("\x1b[2J");
      expect(stream, name).not.toContain("\x1b[3J");
      const rows = await renderedRows(bytes);
      expect(
        rows.findIndex((row) => row.trim().length > 0),
        name,
      ).toBe(0);
      expect(rows.join("\n"), name).not.toMatch(/hook\.(zsh|bash)/);
    }
  });

  it("retains the user's login startup banner", async () => {
    for (const name of ["zsh-login", "bash-login"]) {
      const rows = await renderedRows(capture(name));
      expect(rows[0], name).toBe("USER STARTUP BANNER");
      expect(
        rows.filter((row) => row.includes("USER STARTUP BANNER")),
        name,
      ).toHaveLength(1);
    }
    expect((await renderedRows(capture("zsh-bare"))).join("\n")).not.toContain("USER STARTUP BANNER");
  });

  it("reports real command starts and failures without reporting startup failures", () => {
    for (const name of SESSIONS) {
      const stream = new TextDecoder().decode(capture(name));
      const statuses = stream
        .split("\x1b]133;D;")
        .slice(1)
        .map((part) => Number(part.split("\x07")[0]));
      expect(statuses, name).toEqual([0, 1]);
      // The Bash login fixture is macOS Bash 3.2: exit codes work, but PS0 does not.
      if (name === "bash-login") expect(stream, name).not.toContain("\x1b]133;C\x07");
      else expect(stream, name).toContain("\x1b]133;C\x07");
    }
  });

  it("renders the failing command and the user's next prompt", async () => {
    for (const name of SESSIONS) {
      const rows = (await renderedRows(capture(name))).filter((row) => row.trim().length > 0);
      expect(rows.join("\n"), name).toContain("muster-test> false");
      expect(rows.at(-1)?.trimEnd(), name).toBe("muster-test>");
    }
  });
});
