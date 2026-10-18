//! Hands a web link to the browser this machine has.
//!
//! The webview cannot follow one: it is not a browser, and a document's `[link](https://…)` is
//! not a thing the window can render. So the URL travels back over the bridge and leaves here,
//! where it is a web address and nothing else.
//!
//! What makes that safe is what this file refuses rather than what it runs. The opener is spawned
//! with the URL as its own argument and no shell in between, so there is nothing to inject into;
//! what a document can reach is a command line. A URL that is not `http` or `https` would be a
//! way to name something else entirely — `file:`, a custom scheme an installed app has registered,
//! anything the platform would rather open than draw — and a URL carrying whitespace or control
//! characters is one the shell's word splitting has no business keeping intact. Both are refused
//! here, and the scheme check is also what guarantees the argument can never start with a `-`, so
//! there is no flag for a document to smuggle through.
//!
//! A path is the other kind of question, and it is not the same one. A path does not arrive from a
//! document: it is resolved here, through the checkout it belongs to, before this is reached, so
//! what has to be refused was already refused where the checkout's own boundary is enforced. What
//! is left to check here is the argument itself, and the one that matters is that it is absolute:
//! a relative spelling can begin with `-` and be read as a flag of the opener rather than as a
//! file, and an absolute one cannot. There is no shell in between either way, and the exit status
//! is read either way, because `open` and `xdg-open` both answer a file they could not show by
//! exiting non-zero.

use std::{path::Path, process::Command};

/// Long enough for the deepest link anybody writes, short enough that a document cannot hand the
/// platform a megabyte of its own text.
const MAX_URL_CHARS: usize = 2_048;

/// The opener for this platform: what a desktop session already uses to show a web page.
#[cfg(target_os = "macos")]
const OPENER: &str = "open";
#[cfg(not(target_os = "macos"))]
const OPENER: &str = "xdg-open";

/// Whether this URL is a web address, and so the only thing this is willing to open.
///
/// An `http` or `https` URL only, with the scheme in front of the `://`: that is what makes the
/// argument unforgeable as anything but a page. The character pass afterwards is what keeps a
/// line break or a null out of the argument, and the length is a bound rather than a rule.
fn is_web_url(url: &str) -> bool {
    if url.len() > MAX_URL_CHARS {
        return false;
    }
    if !url
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
        && !url
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
    {
        return false;
    }
    url.chars()
        .all(|character| !character.is_control() && !character.is_whitespace())
}

/// Opens `url` in the browser, or says why it did not.
///
/// A refusal is a fact about the link rather than a failure of the machine, so it is an `Err` the
/// caller can show: a document naming something that is not a web page is something the person
/// reading it has to be told about, not something to swallow.
pub fn open_url(url: &str) -> Result<(), String> {
    open_url_with(OPENER, url)
}

/// The opening itself, with the opener as an argument so a test can run a process whose ending it
/// decides. Nothing else names a program: production goes through `open_url`, which is the one
/// place `OPENER` is read from.
fn open_url_with(opener: &str, url: &str) -> Result<(), String> {
    if !is_web_url(url) {
        return Err("only an http or https link can be opened in the browser".into());
    }
    run(opener, "the link", url)
}

/// Hands a file or a folder to the application this machine has for it.
///
/// `path` is the canonical path of something inside a registered checkout, already resolved by the
/// caller: this is the last step of that chain, not the part that decides what may be opened.
pub fn open_path(path: &Path) -> Result<(), String> {
    open_path_with(OPENER, path)
}

/// The opening of a path, with the opener as an argument so a test can run a process whose ending
/// it decides. Nothing here names a program: production goes through `open_path`.
fn open_path_with(opener: &str, path: &Path) -> Result<(), String> {
    // Absolute, which is what keeps the argument from being read as a flag of the opener, and
    // present, which is what keeps a process from being started for a name nothing is behind.
    // A `Path` on the platforms this runs on cannot hold a NUL, and a relative one is the only
    // spelling that could arrive here looking like a flag.
    if path.as_os_str().is_empty() {
        return Err("there is no path to open".into());
    }
    if !path.is_absolute() {
        return Err("only an absolute path can be opened".into());
    }
    if !path.exists() {
        return Err(format!("{} is not there to open", path.display()));
    }
    run(opener, "the file", &path.to_string_lossy())
}

/// Spawns the opener on one argument and waits for it, reporting what the process said.
///
/// Both entry points share this so a file and a link are read the same way: each caller's own
/// refusal is settled before this is reached, and everything from the spawn onwards is one reading
/// of what the platform answered.
fn run(opener: &str, subject: &str, argument: &str) -> Result<(), String> {
    let status = Command::new(opener)
        .arg(argument)
        .status()
        .map_err(|error| format!("{opener} could not open {subject}: {error}"))?;
    // Starting the opener is not the link having opened, which is the whole reason the status is
    // read at all: `open` and `xdg-open` answer a page they could not show by exiting non-zero --
    // no display, no handler registered, a profile another process holds, a sandbox refusing -- and
    // `.status()` on its own reports that as an open, so the person who clicked is told nothing
    // while no page was ever drawn.
    if status.success() {
        return Ok(());
    }
    // Spelled the way Git's own failures name an exit, so a machine that could not open and a Git
    // that could not answer read as the same kind of fact: about here, not about the link.
    Err(match status.code() {
        Some(code) => format!("{opener} did not open {subject} (exit code {code})"),
        // Killed by a signal, which is not an exit code and has none to quote.
        None => format!("{opener} did not open {subject} (terminated without an exit code)"),
    })
}

#[cfg(test)]
mod tests {
    use super::{is_web_url, open_path_with, open_url_with};
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::Path,
        thread,
        time::{Duration, Instant},
    };
    use tempfile::tempdir;

    /// A program on disk that runs `script`, so the opener is a real process whose ending the test
    /// has decided rather than this machine's browser.
    fn opener_in(directory: &Path, script: &str) -> String {
        let path = directory.join("opener");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("a program to run");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("it to be runnable");
        path.display().to_string()
    }

    /// `open_url_with` on a page, waiting out the one refusal that belongs to another thread.
    ///
    /// Writing a program and then running it cannot always be done back to back. A fork anywhere
    /// else in this binary carries the writing end of that file into the child, and a file still
    /// open for writing somewhere is one the kernel refuses to execute: `Text file busy`, which on
    /// Linux and on macOS is error 26 both. The tests below run in parallel with each other, so a
    /// fork here is a fork there, and the state is that fork's rather than this one's: it lasts as
    /// long as the fork does and nothing about the program changes while it is waited out, which is
    /// why this retries rather than spawning something else. A refusal that is not this one is
    /// returned as it is, because a page that would not open for any other reason is what the tests
    /// above this one are about.
    fn open_page(opener: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let outcome = open_url_with(opener, "https://example.com");
            let another_threads_fork = outcome
                .as_ref()
                .is_err_and(|message| message.ends_with("(os error 26)"));
            if !another_threads_fork || Instant::now() >= deadline {
                return outcome;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn web_addresses_are_the_only_ones_it_opens() {
        for url in [
            "https://example.com",
            "https://example.com/a/b?c=d#e",
            "http://127.0.0.1:1420/index.html",
            "HTTPS://EXAMPLE.COM",
        ] {
            assert!(is_web_url(url), "{url} is a web address");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        for url in [
            // A scheme that names a file, an app or the platform rather than a page.
            "file:///etc/passwd",
            "javascript:alert(1)",
            "mailto:someone@example.com",
            "vscode://file/tmp/x",
            // No scheme at all, which is a path and not a URL.
            "/etc/passwd",
            "example.com",
            // A flag smuggled in as the argument.
            "-a",
            // Carrying what a command line is split on.
            "https://example.com/a b",
            "https://example.com\na",
            "https://example.com/a\u{0}b",
            "",
        ] {
            assert!(!is_web_url(url), "{url} was not refused");
        }
    }

    #[test]
    fn a_link_a_document_wrote_does_not_become_an_argument_list() {
        // Long enough to be a bound rather than a rule, and it is the only reason the opener ever
        // waits on a child: everything above the bound is refused before a process is started.
        let url = format!("https://example.com/{}", "a".repeat(super::MAX_URL_CHARS));
        assert!(!is_web_url(&url));
    }

    /// What the process actually did is what this file is judged on, and it is invisible from the
    /// return type: an opener that runs and opens nothing used to answer `Ok(())` exactly as an
    /// opener that opened the page does. So each of these runs a real one.
    #[test]
    fn an_opener_that_succeeds_is_the_only_one_that_opened_the_link() {
        let temp = tempdir().expect("a directory for the opener");
        let opener = opener_in(temp.path(), "exit 0");

        assert_eq!(open_page(&opener), Ok(()));
    }

    #[test]
    fn an_opener_that_exits_non_zero_is_said_to_have_exited() {
        let temp = tempdir().expect("a directory for the opener");
        let opener = opener_in(temp.path(), "exit 17");

        // Named for its exit rather than folded into the URL refusal, because the two are not the
        // same fact: the link was a web page and the machine is what would not open it.
        assert_eq!(
            open_page(&opener).expect_err("exit 17 opened nothing"),
            format!("{opener} did not open the link (exit code 17)")
        );
    }

    #[test]
    fn an_opener_a_signal_ended_has_no_exit_code_to_be_given() {
        let temp = tempdir().expect("a directory for the opener");
        // A shell killing itself is the cheap portable way to get a status the kernel reported a
        // signal for: `code()` has nothing to answer, and the message must not print an empty
        // number or invent one.
        let opener = opener_in(temp.path(), "kill -TERM $$");

        assert_eq!(
            open_page(&opener).expect_err("a killed opener opened nothing"),
            format!("{opener} did not open the link (terminated without an exit code)")
        );
    }

    #[test]
    fn a_link_that_is_not_a_page_is_refused_before_a_process_is_started() {
        let temp = tempdir().expect("a directory for the opener");
        let ran = temp.path().join("ran");
        let opener = opener_in(temp.path(), &format!("touch {}", ran.display()));

        // The scheme check stays ahead of the spawn rather than behind the exit status: the
        // process would already have been handed the argument by the time an exit was read, and a
        // file that appeared is the only witness that it was not.
        assert_eq!(
            open_url_with(&opener, "file:///etc/passwd").expect_err("a file is not a page"),
            "only an http or https link can be opened in the browser"
        );
        assert!(
            !ran.exists(),
            "the opener ran for a link that is not a page"
        );
    }

    /// `open_path_with` on a file that is there, waiting out the one refusal that belongs to
    /// another thread, for the same reason `open_page` does.
    fn open_file(opener: &str, path: &Path) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let outcome = open_path_with(opener, path);
            let another_threads_fork = outcome
                .as_ref()
                .is_err_and(|message| message.ends_with("(os error 26)"));
            if !another_threads_fork || Instant::now() >= deadline {
                return outcome;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_file_that_is_there_is_opened_and_an_opener_that_exits_non_zero_is_said_to_have_exited() {
        let temp = tempdir().expect("a directory for the opener");
        let file = temp.path().join("notes.txt");
        fs::write(&file, "hello\n").expect("a file to open");

        assert_eq!(open_file(&opener_in(temp.path(), "exit 0"), &file), Ok(()));
        assert_eq!(
            open_file(&opener_in(temp.path(), "exit 17"), &file)
                .expect_err("exit 17 opened nothing"),
            format!(
                "{} did not open the file (exit code 17)",
                temp.path().join("opener").display()
            )
        );
    }

    #[test]
    fn a_path_that_cannot_be_a_flag_is_refused_before_a_process_is_started() {
        let temp = tempdir().expect("a directory for the opener");
        let ran = temp.path().join("ran");
        let opener = opener_in(temp.path(), &format!("touch {}", ran.display()));

        // The absolute check is the argument's safety, and it is the one that stays ahead of the
        // spawn: a relative spelling beginning with `-` would be read as a flag of the opener
        // rather than as a file, and a file appearing is the only witness that it was not.
        for refused in [Path::new("-a"), Path::new("relative/file.txt")] {
            assert_eq!(
                open_path_with(&opener, refused).expect_err("a relative path is not a file"),
                "only an absolute path can be opened"
            );
        }
        // An empty path is refused on its own terms: there is nothing there to name.
        assert_eq!(
            open_path_with(&opener, Path::new("")).expect_err("an empty path opened nothing"),
            "there is no path to open"
        );
        // And a name nothing is behind is refused on the same footing: starting a process to be
        // told by it that there is nothing there is one round trip spent saying nothing.
        assert!(open_path_with(&opener, Path::new("/nope/not-here"))
            .expect_err("an absent path opened nothing")
            .ends_with("is not there to open"));
        assert!(
            !ran.exists(),
            "the opener ran for a path it was never handed"
        );
    }

    #[test]
    fn a_folder_is_opened_the_way_a_file_is() {
        let temp = tempdir().expect("a directory for the opener");
        let folder = temp.path().join("src");
        fs::create_dir(&folder).expect("a folder to open");

        // A row of the tree can name a directory as easily as a file, and the platform answers a
        // folder by showing it rather than by refusing it, so nothing here asks which of the two it is.
        assert_eq!(
            open_file(&opener_in(temp.path(), "exit 0"), &folder),
            Ok(())
        );
    }
}
