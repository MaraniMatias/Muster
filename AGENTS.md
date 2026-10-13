# AGENTS.md

Muster is being rebuilt as a native Rust desktop application. The root is a Cargo
workspace of two crates; the Tauri + Vue app it replaces is parked in `legacy/`.

## Layout

```text
Cargo.toml        workspace: core + native, and the [patch.crates-io] table
core/             muster-core   PTY + workspace shapes + Git discovery. No window, no toolkit.
native/           muster-native the app: GPUI Kit, VT grid, keyboard, sidebar, lifecycle
legacy/           the Tauri 2 + Vue 3 app, frozen at v0.21.1. Not built here.
scripts/          the RustSec audit and the vendored-crate hash pins
docs/             the migration plan, and what the port has to satisfy
```

## Development and verification

- The toolchain is pinned in `rust-toolchain.toml`. Use that `cargo`, not another
  one that happens to be earlier in `PATH`: `rustup run 1.97.1 cargo ...`.
- `cargo test --workspace` covers both crates. `cargo clippy --locked --workspace
  --all-targets -- -D warnings` and `cargo fmt --all --check` are both clean and
  are expected to stay that way.
- `core` holds 70 tests and `native` holds 176. The PTY tests are `core`'s: they
  came from the Tauri tree with the code, so a change in the number is a change
  in what the crate owns.
- A focused test: `cargo test --locked -p muster-core pty::`.
- The native app uses GPUI's `test-support` feature for headless UI coverage;
  `native/src/app.rs` uses it for the window, focus and close-path cases,
  `native/src/sidebar_tests.rs` for the sidebar rows, the worktree dialog and
  the directory picker, and `native/src/titlebar_tests.rs` for the title bar:
  its crumbs, its menus and the window it belongs to.
- The native workspace lives in `~/.muster/native-workspace.json`, written by
  `core::navigation`. It is **not** the Legacy SQLite database and nothing reads
  the old one. It holds roots, archived checkout ids, the active checkout and
  the sidebar width. Live sessions and terminal layouts are never written, so a
  restart begins with no terminals rather than reviving processes that are gone.
- Linux builds need the Vulkan, X11 and XKB development packages. See the
  `linux-build` job in `.github/workflows/ci.yml` for the current list.

## The vendored parser

`native/vendor/alacritty_terminal-0.26.0` is a security backport, not vendored
noise. crates.io 0.26.0 is the latest published release and panics on a remotely
triggerable input, and upstream has published no corrected release to move to.
`native/vendor/alacritty_terminal-0.26.0/PATCHES.md` records the defect and the
one-line fix; `scripts/patched-alacritty.mjs` hash-checks the whole tree, so an
edit there breaks the audit gate.

The `[patch.crates-io]` table lives in the **root** `Cargo.toml`, not in
`native/Cargo.toml`: only the workspace root's patch table is read.

## Security

`scripts/audit-rust.mjs` reads the locked graph of `core/Cargo.toml` and
`native/Cargo.toml`, refuses any path package it cannot hash-pin, and queries
OSV. `node --test scripts/audit-rust.test.mjs` covers it. A new path dependency
that is not a workspace member will fail the audit closed, which is the intent.

## Adding to the port

- A module that no window needs belongs in `core`, even if it came out of
  `legacy/src-tauri/src/services/`. That tree is `pub(crate)`-shaped and its
  types move as one dependency, not as copies.
- A module that draws, takes a key event, or owns a widget belongs in `native`.
  The VT parser and the grid are here on purpose: nothing else parses terminal
  output, so a crate for them would have one caller.
- `legacy/` is reference material. Reading it is expected; editing it is not, and
  nothing verifies it any more.

## Versions are v0._._ — all of them

Every version this project ships is `v0.<minor>.<patch>`. The major is `0` and
stays `0` until someone says otherwise in a commit that also does the migration
work described below. A `v1.0.0` is not a version bump; it is the release where
the schema rule below has to already be in place, so it is its own deliberate
act, not a consequence of a `--bump`.

Consequences while the version is `v0.*.*`:

- A breaking change to a stored shape, an IPC contract, a preference key or a file
  format is made in place, and the version moves a minor. Nothing waits for a
  deprecation window, because nothing has to read what an older build wrote.
- There is no "deprecated but still supported" surface, no feature flag guarding a
  rewrite, and no compatibility layer held "one release" for anyone. The old path
  goes in the same commit as the new one.

## Database schema: no migrations before 1.0

`legacy/src-tauri/src/persistence/mod.rs` carries one schema and no ladder.
`create_schema` writes the whole schema into a file that has none and refuses a
file stamped with any other `SCHEMA_VERSION`; a user upgrades by deleting the
database and starting an empty workspace. That is deliberate, and while the
version is `v0.*.*` it stays deliberate.

While the version is `v0.*.*`:

- **Never write a migration.** No `ALTER TABLE`, no version ladder, no back-compat
  shim. Add the column to `create_schema`, bump `SCHEMA_VERSION`, ship it. Every
  existing database is refused by name and starts empty.
- **Never keep legacy code.** No deprecated variant kept readable, no
  `#[serde(default)]` on a field nothing writes yet, no branch for a shape the
  current build cannot produce. Delete the old path with the change.
- A field that exists only to read what an older build wrote is a migration by
  another name. Leave it out and fix the one place that wrote it.

The rule expires at 1.0. The first `v1.0.0` is where the ladder has to start: from
that version someone's work lives in that database and cannot be refused.