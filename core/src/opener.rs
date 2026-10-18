//! Hands a file or a link to the application this machine already has for it.
//!
//! Two kinds of question arrive here, and they are not the same one. A *path*
//! does not come from a document: it is resolved through the checkout it belongs
//! to before it is reached, in [`crate::files::external_path`], so what had to be
//! refused was refused where the checkout's own boundary is enforced. What is
//! left to check here is the argument itself.
//!
//! A *url* is the other kind, and it is the dangerous one: it can come out of a
//! file a person is reading. The opener is spawned with it as its own argument
//! and no shell in between, so there is nothing to inject into; what a document
//! can reach is a command line. Only `http` and `https` are accepted, with the
//! scheme in front of the `://`, which is what makes the argument unforgeable as
//! anything but a page — a `file:` url, a custom scheme an installed application
//! has registered, or anything else the platform would rather open than draw is
//! not a page. No control character and no whitespace survives either, and the
//! scheme check is what also guarantees the argument can never begin with a `-`,
//! so there is no flag for a document to smuggle through.
//!
//! The exit status is read rather than ignored: `open` and `xdg-open` both
//! answer a thing they could not show by exiting non-zero — no display, no
//! handler registered, a profile another process holds, a sandbox refusing — and
//! `.status()` on its own reports that as an open, so the person who clicked is
//! told nothing while nothing was ever shown. That refusal is returned, not
//! swallowed: a row that stayed where it was looks exactly like one that opened
//! something.

use std::{path::Path, process::Command};

/// Long enough for the deepest link anybody writes, short enough that a document
/// cannot hand the platform a megabyte of its own text.
const MAX_URL_CHARS: usize = 2_048;

/// The opener for this platform: what a desktop session already uses to show a
/// file or a web page.
#[cfg(target_os = "macos")]
const OPENER: &str = "open";
#[cfg(not(target_os = "macos"))]
const OPENER: &str = "xdg-open";

/// Whether this is a web address, and so the only thing this is willing to open.
///
/// An `http` or `https` url only, with the scheme in front of the `://`. The
/// character pass afterwards is what keeps a line break or a null out of the
/// argument, and the length is a bound rather than a rule.
fn is_web_url(url: &str) -> bool {
    if url.len() > MAX_URL_CHARS {
        return false;
    }
    let scheme = |prefix: &str| {
        url.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    };
    if !scheme("https://") && !scheme("http://") {
        return false;
    }
    url.chars()
        .all(|character| !character.is_control() && !character.is_whitespace())
}

/// Open a url in the browser, or say why it did not.
///
/// A refusal is a fact about the link rather than a failure of the machine, so
/// it is an `Err` the caller can show: a document naming something that is not a
/// web page is something the person reading it has to be told about, not
/// something to swallow.
pub fn open_url(url: &str) -> Result<(), String> {
    open_url_with(OPENER, url)
}

/// Hand a file or a folder to the application this machine has for it.
///
/// `path` is the canonical path of something inside a registered checkout,
/// already resolved by [`crate::files::external_path`]: this is the last step of
/// that chain, not the part that decides what may be opened.
pub fn open_path(path: &Path) -> Result<(), String> {
    open_path_with(OPENER, path)
}

/// The opening of a url, with the opener as an argument so a test can run a
/// process whose ending it decides. Nothing else names a program.
fn open_url_with(opener: &str, url: &str) -> Result<(), String> {
    if !is_web_url(url) {
        return Err("only an http or https link can be opened in the browser".into());
    }
    run(opener, "the link", url)
}

/// The opening of a path, with the opener as an argument so a test can run a
/// process whose ending it decides. Nothing else names a program.
fn open_path_with(opener: &str, path: &Path) -> Result<(), String> {
    // Absolute, which is what keeps the argument from being read as a flag of
    // the opener, and present, which is what keeps a process from being started
    // for a name nothing is behind. A relative spelling could arrive here
    // looking like a flag.
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

/// Spawns the opener on one argument and waits for it, reporting what the
/// process said.
///
/// Both entry points share this so a file and a link are read the same way: each
/// caller's own refusal is settled before this is reached, and everything from
/// the spawn onwards is one reading of what the platform answered.
fn run(opener: &str, subject: &str, argument: &str) -> Result<(), String> {
    let status = Command::new(opener)
        .arg(argument)
        .status()
        .map_err(|error| format!("{opener} could not open {subject}: {error}"))?;
    if status.success() {
        return Ok(());
    }
    Err(match status.code() {
        Some(code) => format!("{opener} did not open {subject} (exit code {code})"),
        // Killed by a signal, which is not an exit code and has none to quote.
        None => format!("{opener} did not open {subject} (terminated without an exit code)"),
    })
}

#[cfg(test)]
mod tests {
    use super::{is_web_url, open_path_with, open_url_with, MAX_URL_CHARS};
    use std::{fs, path::Path};

    #[test]
    fn only_an_http_or_https_link_is_a_thing_this_will_open() {
        for url in [
            "https://example.com",
            "http://example.com/a?b=c#d",
            "HTTPS://EXAMPLE.COM",
        ] {
            assert!(is_web_url(url), "{url} is a page");
        }
        for url in [
            // The schemes that would name something else entirely.
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>",
            "vscode://file/tmp/a.rs",
            // The scheme missing, so what is left is not a page.
            "example.com",
            "https:/example.com",
            " https://example.com",
            "https://example.com/a b",
            "https://example.com/a\nb",
            // Long enough that the bound, not the shape, is what refuses it.
            &format!("https://example.com/{}", "a".repeat(MAX_URL_CHARS)),
        ] {
            assert!(!is_web_url(url), "{url} is not a page");
        }
    }

    /// A fake opener, so what is under test is the reading of an exit and not
    /// whether this machine has a browser: `/usr/bin/true` succeeds,
    /// `/usr/bin/false` exits 1, and a name nothing answers for does not run.
    fn outcome(opener: &str, result: &Result<(), String>) -> String {
        let said = match result {
            Ok(()) => "opened".to_owned(),
            Err(error) => error.clone(),
        };
        said.replace(opener, "<opener>")
    }

    #[test]
    fn a_link_is_read_by_what_the_process_answered() {
        assert_eq!(
            outcome(
                "/usr/bin/true",
                &open_url_with("/usr/bin/true", "https://example.com")
            ),
            "opened"
        );
        assert_eq!(
            outcome(
                "/usr/bin/false",
                &open_url_with("/usr/bin/false", "https://example.com")
            ),
            "<opener> did not open the link (exit code 1)"
        );
        assert_eq!(
            outcome(
                "/usr/bin/true",
                &open_url_with("/usr/bin/true", "file:///etc/passwd")
            ),
            "only an http or https link can be opened in the browser"
        );
        assert!(
            open_url_with("/nonexistent-opener", "https://example.com")
                .expect_err("a program that cannot be started is not an open")
                .contains("could not open"),
            "a missing opener is said, not swallowed"
        );
    }

    #[test]
    fn a_file_is_opened_by_path_and_refuses_what_would_be_read_as_a_flag() {
        let file = tempfile::NamedTempFile::new().expect("a file to open");
        assert_eq!(
            outcome(
                "/usr/bin/true",
                &open_path_with("/usr/bin/true", file.path())
            ),
            "opened"
        );
        // Relative: a spelling that could begin with `-` and be read as a flag.
        assert_eq!(
            open_path_with("/usr/bin/true", Path::new("relative.txt")).expect_err("relative"),
            "only an absolute path can be opened"
        );
        assert_eq!(
            open_path_with("/usr/bin/true", Path::new("")).expect_err("empty"),
            "there is no path to open"
        );
        assert!(
            open_path_with("/usr/bin/true", Path::new("/definitely/not/here.txt"))
                .expect_err("absent")
                .ends_with("is not there to open"),
            "a path nothing is behind is refused before a process is started"
        );
        // The opener is handed one argument and no shell, so a name carrying
        // spaces, quotes or a semicolon is one argument rather than a command
        // line. This is the file that would be that argument.
        let directory = tempfile::tempdir().expect("a directory");
        let spaced = directory.path().join("two words.txt");
        fs::write(&spaced, "x").expect("a file with a space in its name");
        assert_eq!(
            outcome("/usr/bin/true", &open_path_with("/usr/bin/true", &spaced)),
            "opened"
        );
    }
}
