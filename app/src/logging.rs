//! Where this crate's log records actually go.
//!
//! `log` is a facade. Until something implements [`Log`] and is installed,
//! every `warn!` the terminal and the window emit is discarded on the floor —
//! and nothing installed one, so a refused paste, a grid that would not resize
//! and a font that would not load were all silent. A refusal the window now
//! shows in its banner is still worth a record, because the banner is gone
//! after the person dismisses it and the log is not.
//!
//! The level is `MUSTER_LOG`, defaulting to `info`. There is no module
//! filtering here on purpose: this is a debugging aid for a desktop app that
//! writes to stderr, not a service with a dozen subsystems to sort apart.

use std::io::Write;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// The sink every record goes to.
struct Stderr;

impl Log for Stderr {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "[{:<5} {}] {}\n",
            level_name(record.level()),
            record.target(),
            record.args()
        );
        // One write, so two records cannot interleave mid-line, and a failed
        // write is not worth a second warning about the warning channel.
        let _ = std::io::stderr().write_all(line.as_bytes());
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Error => "ERROR",
        Level::Warn => "WARN",
        Level::Info => "INFO",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    }
}

/// Install the sink, once, at whatever `MUSTER_LOG` asks for.
///
/// Returns the requested level. A process that already had a logger keeps it:
/// this is a floor, not a takeover, because a host embedding this crate may
/// have installed its own.
pub fn install() -> LevelFilter {
    let level = std::env::var("MUSTER_LOG")
        .ok()
        .as_deref()
        .and_then(parse_level)
        .unwrap_or(LevelFilter::Info);
    if log::set_boxed_logger(Box::new(Stderr)).is_ok() {
        log::set_max_level(level);
    }
    level
}

/// The level `MUSTER_LOG` names, or `None` when it names nothing usable.
///
/// Case and surrounding space do not matter, because it is typed by a person
/// into a terminal. Unrecognised values use the default level.
fn parse_level(text: &str) -> Option<LevelFilter> {
    match text.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_is_named_by_the_word_a_person_would_type() {
        for (text, level) in [
            ("off", LevelFilter::Off),
            ("NONE", LevelFilter::Off),
            ("error", LevelFilter::Error),
            (" Warn ", LevelFilter::Warn),
            ("warning", LevelFilter::Warn),
            ("info", LevelFilter::Info),
            ("debug", LevelFilter::Debug),
            ("TRACE", LevelFilter::Trace),
        ] {
            assert_eq!(parse_level(text), Some(level), "{text:?}");
        }
    }

    #[test]
    fn a_level_it_cannot_read_is_not_a_reason_to_log_everything() {
        for text in ["", "verbose", "9", "warn "] {
            assert!(
                parse_level(text).is_none() || parse_level(text) == Some(LevelFilter::Warn),
                "{text:?} was read as a level"
            );
        }
        assert_eq!(parse_level("verbose"), None);
        assert_eq!(parse_level(""), None);
    }
}
