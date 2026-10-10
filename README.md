# Muster

Muster is a desktop workspace for people working with coding agents in Git worktrees. It keeps each repository and worktree’s files, diffs, and terminals together. Leave notes on specific diff lines, then send them to OpenCode or export them as Markdown for other agents. Muster doesn’t run agents; you start them in its terminals.

- **Platforms:** macOS (Apple Silicon) · Linux (amd64 / arm64)
- **License:** MIT

![Muster](./docs/muster.png)

## Why

When several agents work in parallel, each worktree ends up with its own terminals and changes to review. Muster groups them by checkout in one window, so you can try the code, review the diff, and send feedback without switching between directories and copying comments from a browser. A checkout is the repository root or one of its worktrees.

## What you can do

- Browse and edit files, and inspect Git changes and diffs.
- Open multiple terminals in each repository or worktree; worktrees created outside Muster are detected too.
- Leave notes on diff lines or ranges and track them as draft, sent, or resolved. A note is marked outdated when the code it refers to changes.
- Send review rounds to OpenCode or export notes as Markdown for any CLI agent.

## What it is not

- Not an IDE. The editor is for quick changes, not a replacement for your main editor.
- Not an LLM harness. Muster does not call models or manage how agents run.
- Not a GitHub or GitLab client. Review notes go to OpenCode or a Markdown export.
- Not just a terminal. It also groups worktrees, shows diffs, and keeps review notes.

You can use any CLI agent, or work without one. Only OpenCode has native session and review-delivery integration.

## Install

Download the [latest release](https://github.com/MaraniMatias/Muster/releases/latest).

- **macOS:** Open the DMG and move Muster to Applications. If macOS blocks the first launch, run:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Muster.app
  ```

- **Debian / Ubuntu:** `sudo apt install ./muster_<version>_amd64.deb` (or `_arm64.deb`).
- **Other Linux:** Make the AppImage executable and run it:

  ```sh
  chmod +x Muster_<version>_amd64.AppImage
  ./Muster_<version>_amd64.AppImage
  ```

For native OpenCode integration, install and run [OpenCode](https://opencode.ai). Other CLI agents work in Muster’s terminals; review notes can be exported as Markdown.

## Quick start

1. Open a Git repository.
2. Select a checkout or create a worktree.
3. Open a terminal in that checkout and start your agent.
4. Try the code in another terminal, then open **Changes** and add notes to the relevant diff lines.
5. Send notes to OpenCode or export them as Markdown, then get back to what you were working on.

## FAQ

### Muster won’t start

Check the startup log first:

```sh
case "$(uname -s)" in
  Darwin) LOG="$HOME/Library/Logs/dev.muster.workspace/Muster.log" ;;                       # macOS
  *)      LOG="${XDG_DATA_HOME:-$HOME/.local/share}/dev.muster.workspace/logs/Muster.log" ;; # Linux
esac
tail -n 60 "$LOG"
```

If it reports an unsupported database schema, Muster cannot migrate it. Quit Muster first, back up the database, then remove it; the next launch creates an empty workspace. This deletes registrations, review notes, rounds, and saved UI state, but not repository files. Settings and review exports are stored separately.

```sh
case "$(uname -s)" in
  Darwin) DB="$HOME/Library/Application Support/dev.muster.workspace/muster.sqlite3" ;;    # macOS
  *)      DB="${XDG_DATA_HOME:-$HOME/.local/share}/dev.muster.workspace/muster.sqlite3" ;; # Linux
esac
cp -v "$DB" "${DB%.sqlite3}.$(date +%s).sqlite3" &&
  rm -fv "$DB" "$DB"-wal "$DB"-shm "$DB"-journal
```

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

Workspace data and review notes stay local; Muster has no telemetry, and it never backs up or migrates your database — that is yours to do before any reset (see [Muster won’t start](#muster-wont-start)). Your agent controls what it sends to its model provider.

## Maintainers

[Release safety checklist](docs/release-safety.md)

## License

[MIT](LICENSE)
