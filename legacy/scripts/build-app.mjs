import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { homedir } from "node:os";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const ENCODED_SEPARATOR = "\x1f";
export const PINNED_RUST_TOOLCHAIN = "1.97.1";

export function rustFlagsWithPathRemap(
  env,
  root,
  cargoHome = resolve(root, env.CARGO_HOME || join(homedir(), ".cargo")),
) {
  const remaps = [`--remap-path-prefix=${resolve(root)}=.`, `--remap-path-prefix=${resolve(cargoHome)}=.cargo`];
  const existing =
    env.CARGO_ENCODED_RUSTFLAGS !== undefined
      ? env.CARGO_ENCODED_RUSTFLAGS
        ? env.CARGO_ENCODED_RUSTFLAGS.split(ENCODED_SEPARATOR)
        : []
      : (env.RUSTFLAGS ?? "").trim().split(/\s+/).filter(Boolean);

  return {
    ...env,
    CARGO_ENCODED_RUSTFLAGS: [...existing, ...remaps].join(ENCODED_SEPARATOR),
  };
}

export function tauriBuildArgs(args) {
  const forwarded = args[0] === "--" ? args.slice(1) : args;
  return ["exec", "tauri", "build", ...forwarded, "--", "--locked"];
}

export function pinnedBuildArgs(args) {
  return ["run", PINNED_RUST_TOOLCHAIN, "pnpm", ...tauriBuildArgs(args)];
}

function rustupOutput(env, binary, args) {
  return spawnSync("rustup", ["run", PINNED_RUST_TOOLCHAIN, binary, ...args], {
    cwd: ROOT,
    env: { ...env, RUSTUP_TOOLCHAIN: PINNED_RUST_TOOLCHAIN },
    encoding: "utf8",
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const env = { ...process.env, RUSTUP_TOOLCHAIN: PINNED_RUST_TOOLCHAIN };
  const rustc = rustupOutput(env, "rustc", ["--version", "--verbose"]);
  const cargo = rustupOutput(env, "cargo", ["--version"]);
  const valid =
    !rustc.error &&
    rustc.status === 0 &&
    rustc.stdout.startsWith(`rustc ${PINNED_RUST_TOOLCHAIN} `) &&
    !cargo.error &&
    cargo.status === 0 &&
    cargo.stdout.startsWith(`cargo ${PINNED_RUST_TOOLCHAIN} `);

  if (!valid) {
    console.error(`Production app builds require rustup with Rust ${PINNED_RUST_TOOLCHAIN} (rustc and cargo).`);
    process.exitCode = 1;
  } else {
    console.log(`Pinned build toolchain:\n${rustc.stdout.trim()}\n${cargo.stdout.trim()}`);
    const result = spawnSync("rustup", pinnedBuildArgs(process.argv.slice(2)), {
      cwd: ROOT,
      env: rustFlagsWithPathRemap(env, ROOT),
      stdio: "inherit",
    });

    if (result.error) {
      console.error(`Could not start the pinned Rust ${PINNED_RUST_TOOLCHAIN} production build.`);
      process.exitCode = 1;
    } else {
      process.exitCode = result.status ?? 1;
    }
  }
}
