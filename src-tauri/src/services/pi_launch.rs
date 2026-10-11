//! Starting an interactive Pi in a terminal Muster opened, carrying a review as its first prompt.
//!
//! Pi has no server to send a message to (see [`crate::services::pi`]), so delivering a review to
//! it means the only route Pi has: start one, with the review already in front of it, in a terminal
//! the person is looking at afterwards. Everything here exists to make that safe.
//!
//! # The review is a file, never an argument and never a command
//!
//! A note is a person's text about a line of code. It contains quotes, backticks, `$(...)`,
//! newlines and, eventually, whatever a shell would have found interesting — so it is never
//! assembled into a command line and never reaches a shell. There is no shell here at all: the PTY
//! is handed `pi` directly, with its arguments as arguments, which is the only way a string
//! becomes bytes that a shell has to interpret.
//!
//! The review itself travels as an `@file`, which Pi documents as "include a text file or image in
//! the first prompt" and resolves with `path.resolve`, so an absolute path is taken as written.
//! That leaves one file on disk that a person's review notes pass through, which is why it is
//! written into this app's own folder in their home, private, and swept once it is older than any
//! Pi could still be reading it. Its *name* is generated: nothing a person or a note says ends up
//! in a path, so there is no note that can choose where it is written or be read from.
//!
//! # How this is known to be interactive, and not a batch run
//!
//! Verified against `earendil-works/pi` `v1.1.0`: `resolveAppMode` in `src/main.ts` returns
//! `print` whenever `--print` is set, `--mode` names another interface, **or either stdin or stdout
//! is not a TTY**, and `readPipedStdin` also downgrades an interactive run to `print` if stdin has
//! anything piped into it. So the TUI is reached only by giving Pi the PTY on both ends and
//! nothing on stdin — which is what a spawn into a terminal does. `--print`, `--mode` and piping
//! are therefore all absent by construction rather than by a flag that could be forgotten.
//!
//! The initial prompt is not typed into the PTY. Pi reads the `@file` itself during startup and
//! passes the result to the interactive mode as its first prompt, so there is no window in which
//! input written early would be lost to a terminal that was not listening yet.
//!
//! # The extension travels with the binary
//!
//! `pi -e <path>` loads an extension file, and `-e` paths load even where discovery is off, so the
//! bridge this app reports through is passed explicitly rather than hoped for. That path is written
//! out of [`BRIDGE_SOURCE`] into the app's own folder before the spawn: the copy that runs is the
//! one this build shipped, so the protocol version on the wire is the one both ends were built
//! for. Nothing is ever written into the user's own Pi configuration — see `docs/pi-integration.md`
//! for what a person who also starts Pi by hand installs, and why that is their decision.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use crate::domain::review::{is_review_round_marker, ROUND_MARKER_PREFIX};
use crate::services::files::atomic_write;

/// This build's copy of the bridge extension, from the same file Pi loads in development.
///
/// Compiled in rather than resolved at runtime out of the bundle: the path a packaged resource
/// lands at differs between a `.app`, an installed `.deb` and a development run, and a bridge that
/// silently failed to load would leave a Pi this app started looking like one it never saw. This
/// way there is one copy, and it cannot be the wrong one.
pub const BRIDGE_SOURCE: &str = include_str!("../../../src/pi/muster-bridge.ts");

/// The name Pi's own installer and npm package put on `PATH`.
const PROGRAM: &str = "pi";

/// Where this app keeps everything Pi needs on disk, under `~/.muster`.
const PI_DIR: &str = "pi";
const BRIDGE_FILE: &str = "muster-bridge.ts";
const PROMPTS_DIR: &str = "prompts";

/// How long a written prompt is kept when its round never confirmed.
///
/// A confirmed delivery removes its file at once (see [`remove_prompt`]), so what is left here is
/// a launch that was written and never read — a Pi that failed to start, or one whose bridge
/// never reported it. Pi reads the file during startup, which is seconds after the spawn, so a
/// day is far longer than any live launch could still be reading. Cleanup is lazy: the sweep
/// runs on a later Pi review launch, so the file can outlive this age if no further send happens.
const PROMPT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// The executable Pi is installed as, or `None` when this machine has none.
///
/// Resolved from `PATH` rather than run through a shell to ask it: this is a lookup, and a lookup
/// that runs a login shell to find out what `pi` means would already have given a shell control
/// over the question.
pub fn resolve_program() -> Option<PathBuf> {
    resolve_program_in(&std::env::var_os("PATH")?)
}

/// The same lookup against a `PATH` value, so the answer can be asked of one that is known rather
/// than of whatever this process happens to have.
fn resolve_program_in(path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .flat_map(|directory| {
            // `pi` is a Node program; on Windows npm puts a `.cmd` shim beside the real script, and
            // a spawn needs the one the shell would have run.
            ["", ".cmd", ".exe", ".bat"].map(|suffix| directory.join(format!("{PROGRAM}{suffix}")))
        })
        .find(|candidate| candidate.is_file())
}

/// The bridge file, written out for Pi to load, and the directory it lives in.
///
/// Rewritten only when the bytes differ, so an unchanged build does not touch a file another Pi
/// may have loaded, and a changed one cannot leave a stale protocol version behind.
pub fn bridge_path(app_dir: &Path) -> Result<PathBuf, String> {
    let directory = app_dir.join(PI_DIR);
    let path = directory.join(BRIDGE_FILE);
    let current = fs::read(&path).ok();
    if current.as_deref() != Some(BRIDGE_SOURCE.as_bytes()) {
        private_dir(&directory)?;
        atomic_write(&path, BRIDGE_SOURCE.as_bytes(), private_file())
            .map_err(|error| format!("the Pi bridge could not be written: {}", error.message))?;
    }
    Ok(path)
}

/// The file name a round's review is written to, derived from that round's own marker.
///
/// Derived rather than generated, because the round has to be able to find this file again
/// later: a delivery that the Pi confirmed can then be cleaned up, and one that never confirmed
/// can be left exactly where it is until the sweep reaches it. Nothing a note or a person wrote
/// reaches the name — it is the round id this build generated, with separators no filesystem
/// treats specially.
pub fn prompt_name(marker: &str) -> Option<String> {
    if !is_review_round_marker(marker) {
        return None;
    }
    // The prefix is dropped rather than carried: what is left is the round id this build
    // generated, and `:` is replaced because some systems read it as a drive letter.
    let id = marker
        .strip_prefix(&format!("{ROUND_MARKER_PREFIX}:"))
        .expect("a valid marker starts with the prefix");
    Some(format!("{}.md", id.replace([':', '/', '\\'], "_")))
}

/// Writes the review to a file of its own and answers the path to hand Pi.
///
/// The file is named after the round's marker so the round can be found with it afterwards, and
/// it is swept by age rather than by "everything else here": a second send can be starting while
/// this one is, and only age tells the two apart.
pub fn write_prompt(app_dir: &Path, marker: &str, prompt: &str) -> Result<PathBuf, String> {
    let name =
        prompt_name(marker).ok_or_else(|| "the review could not be named for Pi".to_string())?;
    let directory = app_dir.join(PI_DIR).join(PROMPTS_DIR);
    private_dir(&directory)?;
    sweep_prompts(&directory);
    let path = directory.join(name);
    atomic_write(&path, prompt.as_bytes(), private_file())
        .map_err(|error| format!("the review could not be written for Pi: {}", error.message))?;
    Ok(path)
}

/// Deletes the review a round was written for, once the Pi has been seen to have read it.
///
/// Only ever called for a round whose delivery was confirmed from a receipt. Pi reads the file
/// during startup and records the prompt before it introduces itself, so a receipt cannot exist
/// while the file is still wanted — and an unconfirmed round keeps its file, because the only
/// thing that can read it is a Pi that has not started yet.
///
/// A failure is not reported: there is nothing left to tell anybody, and the file is swept by
/// age regardless.
pub fn remove_prompt(app_dir: &Path, marker: &str) {
    let Some(name) = prompt_name(marker) else {
        return;
    };
    let _ = fs::remove_file(app_dir.join(PI_DIR).join(PROMPTS_DIR).join(name));
}

/// The argument list that starts an interactive Pi with one review in front of it.
///
/// `--` before the file reference is what makes the reference a file rather than an option or a
/// message: it ends Pi's option parsing, and after it a `@path` is read as a file argument. The
/// extension is named before that separator, because after it nothing is a flag.
pub fn arguments(bridge: &Path, prompt: &Path) -> Vec<String> {
    vec![
        "--extension".into(),
        bridge.to_string_lossy().into_owned(),
        "--".into(),
        format!("@{}", prompt.to_string_lossy()),
    ]
}

/// Deletes prompt files older than [`PROMPT_RETENTION`].
///
/// Age rather than "everything else in the folder", because a second send can be starting while
/// this one is: the other launch's file is seconds old, and only age tells the two apart.
fn sweep_prompts(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let expired = metadata
            .modified()
            .ok()
            .and_then(|written| now.duration_since(written).ok())
            .is_some_and(|age| age > PROMPT_RETENTION);
        if expired {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(unix)]
fn private_dir(directory: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
    // Only ever tightened: an existing folder keeps whatever mode it has, so this cannot undo a
    // choice somebody made about their own home directory.
    let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o700));
    Ok(())
}

#[cfg(not(unix))]
fn private_dir(directory: &Path) -> Result<(), String> {
    fs::create_dir_all(directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))
}

#[cfg(unix)]
fn private_file() -> fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    fs::Permissions::from_mode(0o600)
}

#[cfg(not(unix))]
fn private_file() -> fs::Permissions {
    fs::Permissions::default()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::{
        arguments, bridge_path, prompt_name, remove_prompt, resolve_program_in, write_prompt,
        BRIDGE_SOURCE,
    };

    /// A round this build would have written, and a second one, because a send opens a Pi rather
    /// than messaging a running one and each of those is its own round.
    const MARKER: &str = "muster-review:round:1762000000:4242:7";
    const OTHER_MARKER: &str = "muster-review:round:1762000100:4242:8";

    /// Whatever a note says, none of it may reach a path or an argument.
    ///
    /// The review is carried by a file this app names, and the only thing on the command line is
    /// that file's path, so a note cannot become an option, a second file reference, or anything
    /// a shell would read.
    #[test]
    fn a_review_reaches_pi_as_a_file_and_nothing_else() {
        let home = tempdir().unwrap();
        let bridge = bridge_path(home.path()).unwrap();
        let prompt = write_prompt(home.path(), MARKER, "review body").unwrap();
        let argv = arguments(&bridge, &prompt);

        assert_eq!(argv[0], "--extension");
        assert_eq!(argv[1], bridge.to_string_lossy());
        assert_eq!(
            argv[2], "--",
            "nothing after the separator ends Pi's option parsing, so the reference cannot be read as a flag"
        );
        assert_eq!(
            argv[3],
            format!("@{}", prompt.to_string_lossy()),
            "the review is named by its own path, not by its text"
        );
        assert_eq!(
            argv.iter()
                .filter(|argument| argument.starts_with('@'))
                .count(),
            1,
            "one review, one file reference"
        );
    }

    /// A note is arbitrary text. Quoting, expansion, command substitution and newlines are all
    /// ordinary characters in a file, which is the whole reason the file exists.
    #[test]
    fn shell_operators_in_a_review_cannot_escape_the_file_it_is_written_to() {
        let home = tempdir().unwrap();
        let hostile = "'; rm -rf ~ #\n$(curl evil)\n`whoami`\n&& echo done\n--extension /elsewhere";
        let prompt = write_prompt(home.path(), MARKER, hostile).unwrap();

        assert_eq!(fs_read(&prompt), hostile);
        // The path is the only thing on the command line, so it is the only place a note could
        // start choosing where it is written or read from. Nothing of the review may appear in it.
        let path = prompt.to_string_lossy();
        for fragment in ["rm", "curl", "whoami", "echo", ";", "$", "`", "--extension"] {
            assert!(
                !path.contains(fragment),
                "{fragment} reached the path: {path}"
            );
        }
        // The name is the round's own marker, so it is derived from something this build generated
        // rather than from anything a person or a note wrote.
        let name = prompt.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name, prompt_name(MARKER).expect("a marker has a name"));
        assert!(name.starts_with("round_1762000000_4242_7."), "{name}");
    }

    /// Two sends, two terminals, two files: nothing is shared and nothing is overwritten.
    #[test]
    fn every_launch_writes_its_own_prompt_file() {
        let home = tempdir().unwrap();
        let first = write_prompt(home.path(), MARKER, "first").unwrap();
        let second = write_prompt(home.path(), OTHER_MARKER, "second").unwrap();
        assert_ne!(first, second);
        assert_eq!(fs_read(&first), "first");
        assert_eq!(fs_read(&second), "second");
    }

    /// The file is found again by the round that wrote it, so a delivery can clean up after
    /// itself. Nothing else removes it: an unconfirmed round keeps its file, because a Pi that
    /// has not started yet is still the only thing that could read it.
    #[test]
    fn a_confirmed_round_removes_its_own_file_and_leaves_every_other() {
        let home = tempdir().unwrap();
        let confirmed = write_prompt(home.path(), MARKER, "confirmed").unwrap();
        let pending = write_prompt(home.path(), OTHER_MARKER, "pending").unwrap();

        remove_prompt(home.path(), MARKER);
        assert!(
            !confirmed.exists(),
            "a confirmed round left its review on disk"
        );
        assert!(
            pending.exists(),
            "an unconfirmed round lost the file Pi still needs"
        );

        // And removing something that was never written, or a name this build does not use, is
        // not an error: there is nothing left to report either way.
        remove_prompt(home.path(), "muster-review:not-a-round");
        remove_prompt(home.path(), "not-a-marker");
        remove_prompt(home.path(), OTHER_MARKER);
        assert!(!pending.exists());
    }

    /// A marker is what makes a file findable, so a string this build did not write has no name.
    #[test]
    fn only_a_marker_this_build_writes_has_a_file_name() {
        assert!(prompt_name(MARKER).is_some());
        for impostor in ["", "review", "muster-review:round:1:2", "../../etc/passwd"] {
            assert_eq!(prompt_name(impostor), None, "{impostor}");
        }
    }

    /// The bridge this build ships is the one written out, so the copy Pi loads cannot be a
    /// previous version speaking a protocol this app no longer speaks.
    #[test]
    fn the_written_bridge_is_the_one_this_build_ships() {
        let home = tempdir().unwrap();
        let path = bridge_path(home.path()).unwrap();
        assert_eq!(fs_read(&path), BRIDGE_SOURCE);
        // Rewritten identically rather than skipped: what matters is the bytes, and a second
        // launch must be able to leave a half-written file from a first one behind it.
        assert_eq!(fs_read(&bridge_path(home.path()).unwrap()), BRIDGE_SOURCE);
    }

    #[cfg(unix)]
    #[test]
    fn what_is_written_for_pi_is_private_to_the_person_who_owns_it() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempdir().unwrap();
        for path in [
            bridge_path(home.path()).unwrap(),
            write_prompt(home.path(), MARKER, "review").unwrap(),
        ] {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o600,
                "{} is readable by somebody else",
                path.display()
            );
        }
        let directory = home.path().join("pi").join("prompts");
        let mode = std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// A missing Pi is a launch failure the person is told about, not a terminal that opens and
    /// reports nothing. The lookup is asked of a `PATH` that is known rather than of this
    /// process's own, so the answer does not depend on what the machine running the test has
    /// installed.
    #[test]
    fn a_program_that_is_not_installed_is_reported_rather_than_guessed() {
        assert_eq!(resolve_program_in(std::ffi::OsStr::new("")), None);

        let directory = tempdir().unwrap();
        let installed = directory
            .path()
            .join(if cfg!(windows) { "pi.cmd" } else { "pi" });
        std::fs::write(&installed, "#!/bin/sh\n").unwrap();
        assert_eq!(
            resolve_program_in(directory.path().as_os_str()),
            Some(installed),
            "a Pi on PATH was not found"
        );
    }

    fn fs_read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }
}
