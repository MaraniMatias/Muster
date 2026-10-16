# Muster

A desktop workspace for coding agents in Git worktrees, being rebuilt as a native
Rust application. **This repository is mid-migration: the shipped app is parked
under [`legacy/`](legacy), and the root is the new native app.**

## Layout

| Path | What it is |
| --- | --- |
| [`core/`](core) | `muster-core`: the PTY backend and the workspace shapes its callers exchange. No window, no toolkit, nothing to keep in sync with a UI. |
| [`native/`](native) | `muster`: the app. GPUI Kit window, VT grid, keyboard and lifecycle. |
| [`legacy/`](legacy) | The Tauri 2 + Vue 3 app, frozen at `v0.21.1`. Not built, not tested, not released from here. |
| [`docs/native-rust-migration/`](docs/native-rust-migration) | Why the port, and what the terminal milestone has to satisfy. |

`core/` is a real dependency, not a shared folder: `native/` and the parked Tauri
tree both compile the same PTY implementation, so process lifetime, bounded
writes and group-safe shutdown exist once.

## Build and run

Requires the Rust toolchain in [`rust-toolchain.toml`](rust-toolchain.toml) and,
for the security audit only, Node 22.

```sh
rustup run 1.97.1 cargo run --locked -p muster
rustup run 1.97.1 cargo run --locked -p muster -- /some/dir
rustup run 1.97.1 cargo build --locked --release -p muster

rustup run 1.97.1 cargo test --locked --workspace
rustup run 1.97.1 cargo clippy --locked --workspace --all-targets -- -D warnings
rustup run 1.97.1 cargo fmt --all --check
node scripts/audit-rust.mjs                # RustSec over the locked graph

scripts/perf-native.sh                      # the terminal hot-path cost table
```

`MUSTER_LOG=debug cargo run -p muster` prints what the terminal refuses
and what it could not do. Without a sink installed every one of those records
would be discarded, so the app installs one before anything that can log.

## State

The native app is a **terminal with the workspace panel around it**:
repositories and worktrees in a sidebar, live terminals grouped under them,
line counts counted against Git, a searchable worktree picker in the title bar,
worktree creation and archive-or-delete dialogs, drag to reorder or move a
terminal between worktrees, and a window state file.

**Not ported yet:** review notes on diff lines, delivery to an agent, live
preferences, OSC 8 links, file activation and terminal search. Those are the next milestone, and
[`docs/native-rust-migration/interface-parity.md`](docs/native-rust-migration/interface-parity.md)
is the specification for them.

[`Terminal implementation status`](docs/native-rust-migration/ghostty-reference.md)
records the implemented behavior, remaining parity work, and measured costs (§12).
The app uses one dark appearance. Native state lives in
`~/.muster/native-workspace.json`, independently of Legacy SQLite; a single-instance
lock protects it and shutdown flushes the latest accepted navigation snapshot.
Git discovery/counting stays active at a slower cadence in hidden windows.

`legacy/` keeps the last shipped version (v0.21.1) for reference and for its
users until the native app covers it.

## License

[MIT](LICENSE)
