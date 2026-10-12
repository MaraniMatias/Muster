# Legacy: the Tauri app

The Tauri 2 + Vue 3 application, **parked** at the last release (`v0.21.1`) while
the native Rust app in `../native` takes its place.

## It is not built, tested or released from here

- No CI job in `.github/workflows/` reaches into this directory.
- `legacy/.github/workflows/release.yml` is inert: GitHub only reads
  `.github/workflows` at the repository root. Nothing is published from this tree.
- Its RustSec audit is not run. The audit at the repository root covers the
  crates that are built, which are `../core` and `../native`.

Anything here that no longer works is expected. It is kept as a reference for the
behaviour the native app has to reproduce, not as something to maintain.

## What it is for

Reading the behavior the port has to match. The useful entry points:

| Path | What it defines |
| --- | --- |
| `src/components/Sidebar.vue`, `src/presentation/workspace.ts` | The repository / worktree / session tree and its states. |
| `src/components/DocumentPane.vue`, `src/lib/terminal-*.ts` | File editing, diffs, and the terminal behaviors (click-to-move, links, shell integration). |
| `src/components/ReviewComposer.vue`, `src/domain/review.ts` | Review notes: draft / sent / resolved, and what makes one outdated. |
| `src-tauri/src/services/` | Git, worktrees, files, review rounds and the agent integration. |
| `src-tauri/src/persistence/` | The SQLite schema and `SCHEMA_VERSION`. |

`../docs/native-rust-migration/interface-parity.md` maps each of these to its
native counterpart.

## What moved out of here

Two modules are **not** in this tree any more, because they became the shared
core rather than a copy:

- `src-tauri/src/terminal/` → [`../core/src/pty/`](../core/src/pty): process
  lifetime, bounded writes, group-safe shutdown. There is one implementation.
- `src-tauri/src/domain/workspace.rs` → [`../core/src/workspace.rs`](../core/src/workspace.rs).

This tree declares `muster-core` as a path dependency (`../../core`), so the
references still resolve; the tree is parked, not merged.

`src-tauri/vendor/glib-0.18.5` stays here with `scripts/patched-glib.mjs`, its
hash-pin. It was a security backport for the GTK stack that only the Tauri build
links; nothing compiles it any more.