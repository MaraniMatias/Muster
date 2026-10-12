# Muster

A desktop workspace for coding agents in Git worktrees, being rebuilt as a native
Rust application. **This repository is mid-migration: the shipped app is parked
under [`legacy/`](legacy), and the root is the new native app.**

## Layout

| Path | What it is |
| --- | --- |
| [`core/`](core) | `muster-core`: the PTY backend and the workspace shapes its callers exchange. No window, no toolkit, nothing to keep in sync with a UI. |
| [`native/`](native) | `muster-native`: the app. GPUI Kit window, VT grid, keyboard and lifecycle. |
| [`legacy/`](legacy) | The Tauri 2 + Vue 3 app, frozen at `v0.21.1`. Not built, not tested, not released from here. |
| [`docs/native-rust-migration/`](docs/native-rust-migration) | Why the port, and what the terminal milestone has to satisfy. |

`core/` is a real dependency, not a shared folder: `native/` and the parked Tauri
tree both compile the same PTY implementation, so process lifetime, bounded
writes and group-safe shutdown exist once.

## Build and run

Requires the Rust toolchain in [`rust-toolchain.toml`](rust-toolchain.toml) and,
for the security audit only, Node 22.

```sh
cargo run -p muster-native                 # a window and a shell in this directory
cargo run -p muster-native -- /some/dir    # a window and a shell in that one
cargo build --release -p muster-native     # target/release/muster-native

cargo test --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all --check
node scripts/audit-rust.mjs                # RustSec over the locked graph
```

## State

The native app is a **terminal**. The workspace UI the shipped Tauri app has --
repositories and worktrees in a sidebar, diffs, review notes on diff lines,
delivery to an agent -- is not ported yet. That is the next milestone, and
[`docs/native-rust-migration/interface-parity.md`](docs/native-rust-migration/interface-parity.md)
is the specification for it.

`legacy/` keeps the last shipped version (v0.21.1) for reference and for its
users until the native app covers it.

## License

[MIT](LICENSE)