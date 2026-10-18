import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporaryRoot = mkdtempSync(join(tmpdir(), "muster-load-benchmark-"));
const child = spawn(
  "cargo",
  [
    "test",
    "--locked",
    "--offline",
    "--manifest-path",
    "src-tauri/Cargo.toml",
    "persistence::tests::bounded_database_load_metrics",
    "--",
    "--ignored",
    "--nocapture",
  ],
  {
    cwd: root,
    detached: true,
    env: { ...process.env, TMPDIR: temporaryRoot, TMP: temporaryRoot, TEMP: temporaryRoot },
    stdio: "inherit",
  },
);

let timedOut = false;
let forceKill;
const timeout = setTimeout(() => {
  timedOut = true;
  try {
    process.kill(-child.pid, "SIGTERM");
  } catch {}
  forceKill = setTimeout(() => {
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch {}
  }, 5_000);
}, 5 * 60 * 1_000);

let exitCode = 1;
try {
  const result = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => resolve({ code, signal }));
  });
  exitCode = result.code ?? 1;
} finally {
  clearTimeout(timeout);
  if (forceKill) clearTimeout(forceKill);
  if (timedOut && child.pid) {
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch {}
  }
  rmSync(temporaryRoot, { recursive: true, force: true });
}

if (timedOut) {
  console.error("Persistence load benchmark exceeded its five-minute limit");
  process.exitCode = 124;
} else {
  process.exitCode = exitCode;
}
