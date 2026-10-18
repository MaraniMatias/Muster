import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { PINNED_RUST_TOOLCHAIN, pinnedBuildArgs, rustFlagsWithPathRemap, tauriBuildArgs } from "./build-app.mjs";

test("Rust path remapping preserves existing flags and paths with spaces", () => {
  const env = rustFlagsWithPathRemap(
    { RUSTFLAGS: "--cfg existing --check-cfg=cfg(test)" },
    "/work tree/Muster",
    "/cargo home/.cargo",
  );

  assert.equal(
    env.CARGO_ENCODED_RUSTFLAGS,
    [
      "--cfg",
      "existing",
      "--check-cfg=cfg(test)",
      "--remap-path-prefix=/work tree/Muster=.",
      "--remap-path-prefix=/cargo home/.cargo=.cargo",
    ].join("\x1f"),
  );
  assert.equal(env.RUSTFLAGS, "--cfg existing --check-cfg=cfg(test)");
});

test("encoded Rust flags retain precedence and all existing arguments", () => {
  const env = rustFlagsWithPathRemap(
    { CARGO_ENCODED_RUSTFLAGS: ["--cfg", "existing value"].join("\x1f"), RUSTFLAGS: "--cfg ignored" },
    "/work tree/Muster",
    "/cargo home/.cargo",
  );

  assert.equal(
    env.CARGO_ENCODED_RUSTFLAGS,
    [
      "--cfg",
      "existing value",
      "--remap-path-prefix=/work tree/Muster=.",
      "--remap-path-prefix=/cargo home/.cargo=.cargo",
    ].join("\x1f"),
  );
  assert.equal(env.RUSTFLAGS, "--cfg ignored");
});

test("Tauri options stay before the Cargo --locked separator", () => {
  assert.deepEqual(tauriBuildArgs(["--", "--target", "aarch64-apple-darwin", "--bundles", "dmg"]), [
    "exec",
    "tauri",
    "build",
    "--target",
    "aarch64-apple-darwin",
    "--bundles",
    "dmg",
    "--",
    "--locked",
  ]);
});

test("production wrapper invokes and pins both Cargo and rustc", () => {
  assert.match(
    readFileSync(new URL("../rust-toolchain.toml", import.meta.url), "utf8"),
    new RegExp(`channel = "${PINNED_RUST_TOOLCHAIN}"`),
  );
  assert.deepEqual(pinnedBuildArgs(["--", "--bundles", "app"]), [
    "run",
    PINNED_RUST_TOOLCHAIN,
    "pnpm",
    "exec",
    "tauri",
    "build",
    "--bundles",
    "app",
    "--",
    "--locked",
  ]);
});
