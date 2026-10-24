//! What OpenCode says about the task in front of it, and which of it may be believed.
//!
//! OpenCode's TUI reports through OSC 7501, the same escape sequence Pi uses,
//! and it says one thing Pi does not: the session the report is about. The
//! report is written into the terminal that produced it, so it says what *this*
//! terminal is doing -- which is the one thing the service cannot say, because
//! the service has no way to know which of its sessions a given terminal is
//! showing.
//!
//! The second half of this file is the rule about believing any of it. A
//! terminal's output is not OpenCode's output: another program in front of the
//! shell can print whatever it likes, and a report that arrives from a program
//! the operating system does not name as OpenCode is not a report about
//! OpenCode. So a report is believed only while the OS says OpenCode is what is
//! in front of the shell, and it is forgotten the moment that stops being true,
//! or the moment the shell runs something else.

/// The program OpenCode is installed as.
const PROGRAM: &str = "opencode";

/// Longest OSC 7501 payload read, in characters.
///
/// A report is a handful of fields; a payload longer than this is not one.
const MAX_OSC_CHARS: usize = 4096;

/// What OpenCode says about the task in front of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenCodeState {
    /// Nothing is happening.
    Idle,
    /// A turn is running.
    Working,
    /// The turn stopped to ask for something.
    Blocked,
    /// The report is over: the session it was about is gone.
    Clear,
}

/// One report from OpenCode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCodeReport {
    /// The state it named.
    pub state: OpenCodeState,
    /// The session it is about, when it named one.
    ///
    /// A session that has just been created is reported under a name the
    /// service does not know yet, so this is a name to look up rather than a
    /// claim about a session.
    pub session: Option<String>,
}

/// Parses one OSC 7501 payload as OpenCode's report.
///
/// The payload is `key=value` pairs separated by colons. Only the three fields
/// this app reads are looked at: a title and a message carry whatever the
/// program felt like saying, colons included, so what follows them is left
/// alone rather than parsed.
///
/// A payload that names another program, an unknown state, or the same field
/// twice is refused whole: half a report is not a report.
pub fn parse_opencode_status(data: &str) -> Option<OpenCodeReport> {
    if data.chars().count() > MAX_OSC_CHARS {
        return None;
    }
    let mut state = None;
    let mut session = None;
    for field in data.split(':') {
        let Some((key, value)) = field.split_once('=') else {
            // A value that carried a colon: the rest of the payload is that
            // value, and this app does not read it.
            break;
        };
        match key {
            "state" => {
                if state.is_some() {
                    return None;
                }
                state = Some(match value {
                    "idle" => OpenCodeState::Idle,
                    "working" => OpenCodeState::Working,
                    "blocked" => OpenCodeState::Blocked,
                    "clear" => OpenCodeState::Clear,
                    _ => return None,
                });
            }
            "id" => {
                if session.is_some() {
                    return None;
                }
                session = Some(value.to_string());
            }
            "app" if !value.eq_ignore_ascii_case(PROGRAM) => {
                // A program that names itself as something else is not
                // OpenCode, whatever else the payload says.
                return None;
            }
            // The rest is OpenCode's to say, and this app does not read it.
            _ => {}
        }
    }
    Some(OpenCodeReport {
        state: state?,
        session,
    })
}

/// What OpenCode has said, and whether it may be believed.
///
/// The state and the belief that it belongs to OpenCode are separate things: the
/// second one is the operating system's to answer, and it answers it by naming
/// what is in front of the shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenCodeTracker {
    /// The last report OpenCode made.
    report: Option<OpenCodeReport>,
    /// A command has started since the OS last said what was in front of the
    /// shell, so a report that arrives now belongs to a process nobody has
    /// confirmed yet.
    unconfirmed: bool,
}

impl OpenCodeTracker {
    /// A report OpenCode made.
    pub fn reported(&mut self, report: OpenCodeReport) {
        // A report that says the session is over says nothing else: there is no
        // turn to draw and no session to name.
        self.report = match report.state {
            OpenCodeState::Clear => None,
            _ => Some(report),
        };
    }

    /// A command started, so everything OpenCode said was about the one before it.
    pub fn command_started(&mut self) {
        self.report = None;
        self.unconfirmed = true;
    }

    /// A command finished, so everything OpenCode said was about it.
    pub fn command_finished(&mut self) {
        self.report = None;
    }

    /// The operating system said what is in front of the shell.
    ///
    /// Every read of the OS clears the doubt a command start raised, and a read
    /// that does not name OpenCode forgets everything OpenCode said: the report
    /// belonged to a process that is no longer there, and a row that still wore
    /// it would be claiming a turn nobody is running.
    pub fn confirmed(&mut self, foreground_is_opencode: bool) {
        self.unconfirmed = false;
        if !foreground_is_opencode {
            self.report = None;
        }
    }

    /// The report to draw, if there is one.
    pub fn report(&self, foreground_is_opencode: bool) -> Option<&OpenCodeReport> {
        if !foreground_is_opencode || self.unconfirmed {
            return None;
        }
        self.report.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_opencode_status, OpenCodeReport, OpenCodeState, OpenCodeTracker};

    #[test]
    fn a_report_names_a_state_and_the_session_it_is_about() {
        let report = parse_opencode_status(
            "state=working:id=ses_e912b168dffeXW3oWEfvXIw4U0:app=opencode:title=TmV3IHNlc3Npb24=",
        )
        .unwrap();
        assert_eq!(report.state, OpenCodeState::Working);
        assert_eq!(
            report.session.as_deref(),
            Some("ses_e912b168dffeXW3oWEfvXIw4U0")
        );

        // The report of a terminal that is at the home screen: a state and
        // nothing else.
        let report = parse_opencode_status("state=idle:app=opencode").unwrap();
        assert_eq!(report.state, OpenCodeState::Idle);
        assert_eq!(report.session, None);
    }

    #[test]
    fn a_report_that_names_another_program_is_refused() {
        assert_eq!(parse_opencode_status("state=working:app=pi"), None);
        assert_eq!(parse_opencode_status("state=working:app=codex"), None);
    }

    #[test]
    fn a_malformed_report_is_refused_whole() {
        assert_eq!(parse_opencode_status(""), None);
        assert_eq!(parse_opencode_status("hello"), None);
        assert_eq!(parse_opencode_status("state=typing"), None);
        assert_eq!(parse_opencode_status("state=working:state=done"), None);
        assert_eq!(parse_opencode_status("id=ses_a:app=opencode"), None);
        assert_eq!(parse_opencode_status(&"x".repeat(4097)), None);
    }

    #[test]
    fn a_title_and_a_message_are_left_alone() {
        // A title is base64 and a message is whatever the program said, so a
        // message with a colon in it does not break the report.
        let report = parse_opencode_status(
            "state=blocked:kind=permission:id=ses_a:app=opencode:title=TmV3IHNlc3Npb24=:message=Run ls?: yes",
        )
        .unwrap();
        assert_eq!(report.state, OpenCodeState::Blocked);
        assert_eq!(report.session.as_deref(), Some("ses_a"));
    }

    #[test]
    fn a_report_is_believed_only_while_opencode_is_in_front() {
        let mut tracker = OpenCodeTracker::default();
        tracker.reported(OpenCodeReport {
            state: OpenCodeState::Working,
            session: Some("ses_a".into()),
        });
        assert_eq!(
            tracker.report(true).map(|report| report.state),
            Some(OpenCodeState::Working)
        );
        assert_eq!(tracker.report(false), None);
    }

    #[test]
    fn a_command_start_forgets_what_was_said_before_it() {
        let mut tracker = OpenCodeTracker::default();
        tracker.reported(OpenCodeReport {
            state: OpenCodeState::Working,
            session: None,
        });
        tracker.command_started();
        assert_eq!(tracker.report(true), None);
        // The OS names what is in front again, and the doubt is gone.
        tracker.confirmed(true);
        assert_eq!(tracker.report(true), None);
    }

    #[test]
    fn a_report_that_the_session_is_over_says_nothing() {
        let mut tracker = OpenCodeTracker::default();
        tracker.reported(OpenCodeReport {
            state: OpenCodeState::Clear,
            session: Some("ses_a".into()),
        });
        assert_eq!(tracker.report(true), None);
    }
}
