# Muster

Muster is a desktop workspace for people working with coding agents in Git worktrees. It keeps each repository and worktree’s files, diffs, and terminals together. Leave notes on specific diff lines, then send them to OpenCode, start a fresh Pi review session, or export them as Markdown. Muster does not run models; other CLI agents can be started in its terminals.

> **Native app:** releases target macOS (Apple Silicon) and Ubuntu 24.04
> (x86_64).

- **Native release targets:** macOS (Apple Silicon) · Ubuntu 24.04 (x86_64)
- **License:** MIT


## Why

When several agents work in parallel, each worktree ends up with its own terminals and changes to review. Muster groups them by checkout in one window, so you can try the code, review the diff, and send feedback without switching between directories and copying comments from a browser. A checkout is the repository root or one of its worktrees.

## What you can do

- Browse and edit files, and inspect Git changes and diffs.
- Open multiple terminals in each repository or worktree; worktrees created outside Muster are detected too.
- Leave notes on diff lines or ranges and track them as draft, sent, or resolved. A note is marked outdated when the code it refers to changes.
- Send review rounds to an existing OpenCode session, start a new Pi terminal with selected notes as its first prompt, or export notes as Markdown for any CLI agent.

## What it is not

- Not an IDE. The editor is for quick changes, not a replacement for your main editor.
- Not an LLM harness. Muster does not call models or manage how agents run.
- Not a GitHub or GitLab client. Review notes go to OpenCode, a fresh Pi terminal, or a Markdown export.
- Not just a terminal. It also groups worktrees, shows diffs, and keeps review notes.

You can use any CLI agent, or work without one. OpenCode has native session and in-session delivery integration; Pi has native session observation and receives each review in a newly opened interactive terminal.

## Install

The current candidate is **v1.0.0-rc.12**:
[`Muster_1.0.0-rc.12_arm64.zip`](https://github.com/MaraniMatias/Muster/releases/download/v1.0.0-rc.12/Muster_1.0.0-rc.12_arm64.zip) for macOS on Apple Silicon,
[`Muster_1.0.0-rc.12_x86_64.zip`](https://github.com/MaraniMatias/Muster/releases/download/v1.0.0-rc.12/Muster_1.0.0-rc.12_x86_64.zip) for macOS on Intel,
[`muster_1.0.0-rc.12_amd64.deb`](https://github.com/MaraniMatias/Muster/releases/download/v1.0.0-rc.12/muster_1.0.0-rc.12_amd64.deb) for Ubuntu on x86_64,
[`muster_1.0.0-rc.12_arm64.deb`](https://github.com/MaraniMatias/Muster/releases/download/v1.0.0-rc.12/muster_1.0.0-rc.12_arm64.deb) for Ubuntu on arm64.

It is a pre-release, so GitHub's `Latest` badge still selects the frozen Legacy
release. Every candidate is on the [Releases](https://github.com/MaraniMatias/Muster/releases)
page. Candidates before v1.0.0-rc.11 may carry only a macOS archive and one
Linux package; every candidate from v1.0.0-rc.11 on carries all four.

- **macOS 15+ (Apple Silicon):** Unzip `Muster_<version>_arm64.zip` and move `Muster.app` to Applications. The bundle is ad-hoc signed, not notarized; assess the source and build before bypassing Gatekeeper quarantine.
- **macOS 15+ (Intel):** Unzip `Muster_<version>_x86_64.zip` and move `Muster.app` to Applications. Same bundle, same ad-hoc signature.
- **Ubuntu 24.04 x86_64:** install the package with `sudo apt install ./muster_<version>_amd64.deb`. Download filenames use `-rc.3`; the package's internal Debian version uses `~rc.3` for correct upgrade ordering.
- **Ubuntu 24.04 arm64:** the same command with `muster_<version>_arm64.deb`. AppImage is not provided by the native release path.
- **Other Linux:** No native package is currently provided.

For native OpenCode integration, install and run [OpenCode](https://opencode.ai). For Pi review delivery, install [Pi](https://github.com/earendil-works/pi) v1.1.0; each send starts a new interactive terminal. Other CLI agents work in Muster’s terminals, and notes can be exported as Markdown.

## Quick start

1. Open a Git repository.
2. Select a checkout or create a worktree.
3. Open a terminal in that checkout and start your agent.
4. Try the code in another terminal, then open **Changes** and add notes to the relevant diff lines.
5. Send notes to OpenCode or Pi, or export them as Markdown, then get back to what you were working on.

## FAQ

### Muster won’t start

The native app logs to stderr, not Legacy's log file. Quit any running instance,
then start the binary from a terminal to see the error:

```sh
# Linux
MUSTER_LOG=debug Muster
# macOS, when installed in Applications
MUSTER_LOG=debug /Applications/Muster.app/Contents/MacOS/Muster
```

Native workspace and review state are JSON files under `~/.muster`; preferences are in `~/.muster/settings.json`. A malformed or unsupported workspace/review file is refused rather than migrated or overwritten. Back up the named file before removing it; deleting it starts that store empty. Repository files are not affected.

### Why does macOS say the app is damaged or can’t be opened?

Gatekeeper marks a downloaded app with a quarantine attribute, so the launch is blocked before Muster can start. Clear it from the app you moved to Applications:

```sh
xattr -dr com.apple.quarantine /Applications/Muster.app
```

If Muster still does not start, read the [startup log](#muster-wont-start).

### Can the terminal display images?

Yes. Muster supports direct Kitty graphics transfers (PNG, RGB and RGBA, including
zlib compression), placements and Unicode image placeholders used by terminal
applications. OpenCode can detect it with its graphics probe. New terminals set
`PI_IMAGE_PROTOCOL=kitty` for Pi when that variable is not already configured;
explicit user overrides are preserved. Restart an existing Pi session after upgrading.

Images stay in memory, not workspace state. Transfers are limited to 16 MiB of
base64, decoded images to 32 MiB / 8192 pixels per edge, and each screen buffer to
64 MiB, 64 images and 256 placements. Unsupported or oversized requests are refused.
Unicode placeholders follow text reflow; classic placements are cleared on resize
and the terminal application must redraw them. Sixel, iTerm2 images, file/shared-memory
transfers, animation and tmux passthrough are not implemented.

### Do terminals survive a restart?

No. Muster restores your workspace and UI state, but stops terminal processes when it closes and clears their sessions on startup.

### What happens when a folder is missing?

Missing folders stay registered; their notes remain available if the path returns. A moved folder is not reattached automatically: opening the new path creates a separate registration, and the two histories do not merge.

### What does closing a missing checkout do?

It deletes that checkout’s registration and attached notes, rounds, and UI state. No file on disk is touched, and opening the folder again brings the checkout back. Removing a repository from the panel does the same for every checkout under it.

### What is the difference between Archive and Delete?

**Archive** takes a worktree off the panel and keeps every file, so the repository row can restore it. **Delete** removes the worktree’s directory from disk.

> **Delete is the only action here that removes files.** It takes the worktree’s notes, rounds, and UI state with it, so export what you need first.

## Privacy and local data

Workspace data, review rounds and preferences are stored locally under `~/.muster`; review exports use the selected export location. Muster has no telemetry. Native workspace and review state are not migrated from Legacy SQLite. Back up files before manually resetting them.

## Maintainers

[Release safety checklist](docs/release-safety.md)

## Layout

| Path | What it is |
| --- | --- |
| [`core/`](core) | `muster-core`: the PTY backend and the workspace shapes its callers exchange. No window, no toolkit, nothing to keep in sync with a UI. |
| [`app/`](app) | `muster`: the app. GPUI Kit window, VT grid, keyboard and lifecycle. Its source is `app/src/`, and its icon and fonts are `app/assets/`. |
| [`docs/release-safety.md`](docs/release-safety.md) | The release checklist, and what the native release path promises. |

`core/` is a real dependency, not a shared folder: the app and the parked Tauri
tree both compile the same PTY implementation, so process lifetime, bounded
writes and group-safe shutdown exist once.

## Build and run

Requires the Rust toolchain in [`rust-toolchain.toml`](rust-toolchain.toml) and,
for the security audit only, Node 22. Linux builds also require Vulkan, X11,
xcb, XKB, freetype and fontconfig development packages; Ubuntu package names
and the native Linux release process are documented in [release safety](docs/release-safety.md).

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

### Scripts

Every script in `scripts/` belongs to this app's release path.

| Script | What it does | When you run it |
| --- | --- | --- |
| `release.sh` | `check` / `build` / `release`: formatting, clippy, tests, audit, then the optimized build and the macOS bundle; `candidate <version>` cuts and publishes a release candidate, `publish <version>` publishes a prepared one | The entry point for every local build. `release` is macOS-only, `candidate` and `publish` need `gh` and publish |
| `bundle-macos.sh` | Assembles `dist/Muster.app` with the icon, name and version | Called by `release.sh release` and by CI |
| `package-linux-deb.sh` | Builds `dist/muster_<version>_<arch>.deb` for amd64 and arm64, deriving runtime dependencies from the ELF | On Ubuntu, after `release.sh build` |
| `debian-version.sh` | Converts a Cargo version to its Debian form (`1.0.0-rc.3` becomes `1.0.0~rc.3`) | Internal to packaging |
| `linux-package-smoke.sh` | Installs the `.deb` in a clean Ubuntu container and launches it under Xvfb | Internal to CI; run it directly to reproduce the CI check |
| `linux-window-smoke.sh` | The window and liveness half of that check | Internal to `linux-package-smoke.sh` |
| `finalize-native-release.mjs` | Checks the built macOS and Linux payloads of every target together before publication | Internal to the release workflow |
| `audit-rust.mjs` | RustSec audit of the locked graph over OSV, refusing unpinned path packages | `release.sh check` and CI |
| `perf-native.sh` | Runs the ignored terminal hot-path benchmarks | Development only |
| `generate-native-file-icons.mjs` | Regenerates `app/src/file_icons/data.rs` from Legacy's generated module | Only when a file icon changes; never hand-edit the output |
| `patched-alacritty.mjs`, `patched-gitignore.mjs` | Hash-pin the vendored crates so the audit can trust them | Internal to `audit-rust.mjs` |
| `audit-rust.test.mjs`, `release.test.mjs`, `linux-release.test.mjs` | Tests for the audit, the release commands and the packaging/publish gate | Run by `release.sh check` and CI |

The full release procedure is in [release safety](docs/release-safety.md).

## State

The native app is a **terminal with the workspace panel around it**:
repositories and worktrees in a sidebar, live terminals grouped under them,
line counts counted against Git, a searchable worktree picker in the title bar,
worktree creation and archive-or-delete dialogs, drag to reorder or move a
terminal between worktrees, and a window state file.

The native app includes review notes and agent delivery/export, editable
preferences, file activation and terminal search.

The app uses one dark appearance. Native state lives in
`~/.muster/native-workspace.json`, independently of the old SQLite database; a single-instance
lock protects it and shutdown flushes the latest accepted navigation snapshot.
Git discovery/counting stays active at a slower cadence in hidden windows.

## License

[MIT](LICENSE)