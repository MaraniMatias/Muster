//! What Pi says about the task in front of it, and which of it may be believed.
//!
//! Pi reports through two escape sequences, and they are not the same claim.
//! OSC 7501 is Pi's own: it names a state, and it is the only thing that can
//! say *which* state. OSC 9;4 is the progress indicator every terminal
//! understands, and it can only say that something is running. So an explicit
//! state wins over the progress, and the progress is what is left when Pi has
//! said nothing more precise.
//!
//! The second half of this file is the rule about believing any of it. A
//! terminal's output is not Pi's output: another program in front of the shell
//! can print whatever it likes, and a report that arrives from a program the
//! operating system does not name as Pi is not a report about Pi. So a report
//! is believed only while the OS says Pi is what is in front of the shell,
//! and it is forgotten the moment that stops being true, or the moment the
//! shell runs something else.

/// The reply that says the host understands program status.
///
/// The same question back is the answer: Pi asks it once, and a host that
/// answers with the same sequence has said it will listen.
pub const PROGRAM_STATUS_REPLY: &str = "\u{1b}]7501;?\u{1b}\\";

/// Longest OSC 7501 payload read, in characters.
///
/// A status is a handful of fields; a payload longer than this is not one.
pub const MAX_OSC_CHARS: usize = 4096;

/// Longest decoded message read, in bytes.
const MAX_MESSAGE_BYTES: usize = 2048;

/// The fields a status may carry.
const FIELDS: [&str; 4] = ["state", "app", "kind", "msg"];

/// The states a status may name.
const STATES: [&str; 6] = ["idle", "working", "blocked", "done", "error", "clear"];

/// The kinds a blocked status may name.
const KINDS: [&str; 3] = ["permission", "question", "auth"];

/// What Pi says about the task in front of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiTaskState {
    /// Nothing is in front of it.
    Idle,
    /// A turn is running.
    Working,
    /// A turn stopped to ask something.
    ///
    /// Parsed and kept, and drawn as nothing: the panel has no shape for a
    /// question this program asked, and inventing one would be a claim about
    /// the agent that the agent did not make.
    Blocked,
    /// The last turn finished.
    Done,
    /// The last turn failed.
    Error,
}

/// One OSC 7501 payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiProgramStatus {
    /// Pi is asking whether the host understands program status.
    Query,
    /// Pi reported a state.
    State(PiTaskState),
}

/// The indeterminate progress Pi reports when it has nothing more precise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiProgress {
    /// Something is running.
    Working,
    /// Nothing is running.
    Clear,
}

/// Parses one OSC 7501 payload.
///
/// `data` is what followed `7501;`. A payload that is too long, that names an
/// unknown field, that names a field twice, or that carries a malformed
/// message is refused whole: half a status is not a status, and reading
/// `state=working` off the front of a broken payload would report a state
/// nobody sent.
pub fn parse_program_status(data: &str) -> Option<PiProgramStatus> {
    if data.chars().count() > MAX_OSC_CHARS {
        return None;
    }
    if data == "?" {
        return Some(PiProgramStatus::Query);
    }
    if !data.starts_with("state=") {
        return None;
    }
    let mut fields: Vec<(&str, &str)> = Vec::new();
    for field in data.split(':') {
        let (key, value) = field.split_once('=')?;
        if key.is_empty() || fields.iter().any(|(seen, _)| *seen == key) {
            return None;
        }
        fields.push((key, value));
    }
    let state = fields
        .iter()
        .find(|(key, _)| *key == "state")
        .map(|(_, value)| *value)?;
    if !STATES.contains(&state) {
        return None;
    }
    if fields.iter().any(|(key, _)| !FIELDS.contains(key)) {
        return None;
    }
    // A program that names itself is naming itself: a status from anything
    // other than Pi is not a status about Pi.
    if let Some((_, app)) = fields.iter().find(|(key, _)| *key == "app") {
        if !muster_core::pi::is_pi(app) {
            return None;
        }
    }
    // A kind is what a blocked turn is waiting for, and it means nothing
    // without the block.
    if let Some((_, kind)) = fields.iter().find(|(key, _)| *key == "kind") {
        if state != "blocked" || !KINDS.contains(kind) {
            return None;
        }
    }
    // The message is read for its shape and not for its content: it is a
    // blob Pi attaches, and the only thing this needs to know about it is
    // that it is a blob of a size that can be held.
    if let Some((_, message)) = fields.iter().find(|(key, _)| *key == "msg") {
        let length = base64_length(message)?;
        if length > MAX_MESSAGE_BYTES {
            return None;
        }
    }
    Some(PiProgramStatus::State(match state {
        "working" => PiTaskState::Working,
        "blocked" => PiTaskState::Blocked,
        "done" => PiTaskState::Done,
        "error" => PiTaskState::Error,
        _ => PiTaskState::Idle,
    }))
}

/// Parses the progress half of OSC 9;4.
///
/// Only the two values that mean something are read: `4;3` is a task running
/// and `4;0` is a task finished. Every other progress value is somebody else's
/// convention and is left alone.
pub fn parse_progress(data: &str) -> Option<PiProgress> {
    match data {
        "4;3" => Some(PiProgress::Working),
        "4;0" => Some(PiProgress::Clear),
        _ => None,
    }
}

/// The length of a base64 blob, in bytes, or `None` when it is not one.
///
/// The shape is checked rather than the bytes decoded, because the length is
/// the only thing that is read: a message this host has no use for is checked
/// so that a payload carrying one cannot be a payload that was never meant to
/// be read at all.
fn base64_length(message: &str) -> Option<usize> {
    let bytes = message.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let padding = bytes.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2 {
        return None;
    }
    let body = &bytes[..bytes.len() - padding];
    if !body
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'+' || *byte == b'/')
    {
        return None;
    }
    // A padded blob carries two bytes after the last full group and one after
    // a group of three; anything else is a blob that was cut short.
    match padding {
        0 if body.len().is_multiple_of(4) => Some(body.len() / 4 * 3),
        1 if body.len() % 4 == 3 => Some(body.len() / 4 * 3 + 2),
        2 if body.len() % 4 == 2 => Some(body.len() / 4 * 3 + 1),
        _ => None,
    }
}

/// What Pi has said, and whether it may be believed.
///
/// The three pieces of state here are separate on purpose: an explicit state
/// and the progress indicator are different claims, and the belief that a
/// report belongs to Pi is a third thing again -- the last one is the
/// operating system's to answer, and it answers it by naming what is in front
/// of the shell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PiTracker {
    /// The last state Pi reported.
    state: Option<PiTaskState>,
    /// Whether Pi has reported a state at all.
    ///
    /// Separate from the state itself, because "Pi said it is idle" and "Pi
    /// has not said anything" are different facts, and the first one is what
    /// stops the progress indicator from being read as a state.
    reported: bool,
    /// The last progress report: something is running, or is not.
    progress_working: bool,
    /// A command has started since the OS last said what was in front of the
    /// shell, so a report that arrives now belongs to a process nobody has
    /// confirmed yet.
    unconfirmed: bool,
}

impl PiTracker {
    /// A state Pi reported.
    pub fn report(&mut self, state: PiTaskState) {
        self.state = Some(state);
        self.reported = true;
    }

    /// A progress report.
    pub fn progress(&mut self, working: bool) {
        self.progress_working = working;
    }

    /// A command started, so everything Pi said was about the one before it.
    pub fn command_started(&mut self) {
        self.clear();
        self.unconfirmed = true;
    }

    /// A command finished, so everything Pi said was about it.
    pub fn command_finished(&mut self) {
        self.clear();
    }

    /// The operating system said what is in front of the shell.
    ///
    /// Every read of the OS clears the doubt a command start raised, and a
    /// read that does not name Pi forgets everything Pi said: the report
    /// belonged to a process that is no longer there, and a row that still
    /// wore it would be claiming a task nobody is doing.
    pub fn confirmed(&mut self, foreground_is_pi: bool) {
        self.unconfirmed = false;
        if !foreground_is_pi {
            self.clear();
        }
    }

    /// The state to draw, if there is one.
    ///
    /// An explicit state wins over the progress indicator, and an explicit
    /// "idle" wins over both: Pi said there is nothing in front of it, and the
    /// progress indicator is not a more precise claim than that.
    pub fn task(&self, foreground_is_pi: bool) -> Option<PiTaskState> {
        if !foreground_is_pi || self.unconfirmed {
            return None;
        }
        if self.reported {
            return match self.state {
                Some(PiTaskState::Idle) | None => None,
                Some(state) => Some(state),
            };
        }
        self.progress_working.then_some(PiTaskState::Working)
    }

    /// Forgets everything, which is what a new process in front of the shell
    /// means.
    fn clear(&mut self) {
        self.state = None;
        self.reported = false;
        self.progress_working = false;
    }
}

/// The words a Pi row says beside its name.
///
/// The three words are the ones the panel has always used for a task: what is
/// happening, what happened, and that it is over. A blocked turn says nothing,
/// because the panel has no shape for a question this program asked.
pub fn pi_task_note(state: Option<PiTaskState>) -> &'static str {
    match state {
        Some(PiTaskState::Working) => "working",
        Some(PiTaskState::Error) => "last task failed",
        Some(PiTaskState::Done) => "task complete",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        base64_length, parse_program_status, parse_progress, PiProgramStatus, PiProgress,
        PiTaskState, PiTracker,
    };

    #[test]
    fn a_query_is_a_question_and_a_state_is_a_state() {
        assert_eq!(parse_program_status("?"), Some(PiProgramStatus::Query));
        assert_eq!(
            parse_program_status("state=working"),
            Some(PiProgramStatus::State(PiTaskState::Working))
        );
        assert_eq!(
            parse_program_status("state=blocked:kind=permission"),
            Some(PiProgramStatus::State(PiTaskState::Blocked))
        );
        assert_eq!(
            parse_program_status("state=done:app=pi"),
            Some(PiProgramStatus::State(PiTaskState::Done))
        );
        assert_eq!(
            parse_program_status("state=clear"),
            Some(PiProgramStatus::State(PiTaskState::Idle))
        );
    }

    #[test]
    fn a_malformed_payload_is_refused_whole() {
        // Not a status at all.
        assert_eq!(parse_program_status(""), None);
        assert_eq!(parse_program_status("hello"), None);
        assert_eq!(parse_program_status("state=typing"), None);
        // A field that is not one of the four.
        assert_eq!(parse_program_status("state=working:colour=red"), None);
        // The same field twice.
        assert_eq!(parse_program_status("state=working:state=done"), None);
        // A field with no name.
        assert_eq!(parse_program_status("state=working:=x"), None);
        // A kind without a block, and a kind that is not one of the three.
        assert_eq!(parse_program_status("state=working:kind=question"), None);
        assert_eq!(parse_program_status("state=blocked:kind=elsewhere"), None);
        // A program that names itself as something else.
        assert_eq!(parse_program_status("state=working:app=opencode"), None);
        // A message that is not base64.
        assert_eq!(parse_program_status("state=working:msg=not base64!"), None);
        // A payload past the bound.
        assert_eq!(parse_program_status(&"x".repeat(4097)), None);
    }

    #[test]
    fn a_message_is_read_for_its_shape_and_its_size() {
        assert_eq!(base64_length(""), Some(0));
        assert_eq!(base64_length("YWJjZA=="), Some(4));
        assert_eq!(base64_length("YWJjZGU="), Some(5));
        assert_eq!(base64_length("YWJjZGVm"), Some(6));
        assert_eq!(base64_length("YWJjZ"), None);
        assert_eq!(base64_length("YWJjZA="), None);
        assert_eq!(base64_length("YW=J"), None);
        assert_eq!(base64_length("****"), None);
        // A message that decodes to more than the bound is refused.
        let long = "A".repeat(4 * 1024);
        assert_eq!(base64_length(&long), Some(3072));
        assert_eq!(
            parse_program_status(&format!("state=working:msg={long}")),
            None
        );
    }

    #[test]
    fn only_the_two_progress_values_that_mean_something_are_read() {
        assert_eq!(parse_progress("4;3"), Some(PiProgress::Working));
        assert_eq!(parse_progress("4;0"), Some(PiProgress::Clear));
        assert_eq!(parse_progress("4;1"), None);
        assert_eq!(parse_progress("4;2"), None);
        assert_eq!(parse_progress(""), None);
    }

    #[test]
    fn a_report_from_a_process_that_is_not_pi_is_not_believed() {
        let mut tracker = PiTracker::default();
        tracker.report(PiTaskState::Working);
        assert_eq!(tracker.task(true), Some(PiTaskState::Working));
        assert_eq!(tracker.task(false), None);
    }

    #[test]
    fn an_explicit_state_wins_over_the_progress_indicator() {
        let mut tracker = PiTracker::default();
        tracker.progress(true);
        assert_eq!(tracker.task(true), Some(PiTaskState::Working));

        tracker.report(PiTaskState::Error);
        assert_eq!(tracker.task(true), Some(PiTaskState::Error));

        // An explicit "idle" is a state too, and it ends the progress.
        tracker.report(PiTaskState::Idle);
        assert_eq!(tracker.task(true), None);
    }

    #[test]
    fn a_command_start_forgets_what_was_said_and_doubts_what_comes_next() {
        let mut tracker = PiTracker::default();
        tracker.report(PiTaskState::Working);
        tracker.command_started();
        assert_eq!(tracker.task(true), None);

        // A report that arrives before the OS has confirmed the new process is
        // not believed yet.
        tracker.report(PiTaskState::Working);
        assert_eq!(tracker.task(true), None);

        // The confirmation is what makes it true.
        tracker.confirmed(true);
        assert_eq!(tracker.task(true), Some(PiTaskState::Working));
    }

    #[test]
    fn a_foreground_that_is_not_pi_forgets_everything_pi_said() {
        let mut tracker = PiTracker::default();
        tracker.report(PiTaskState::Done);
        tracker.confirmed(false);
        assert_eq!(tracker.task(false), None);
        assert_eq!(tracker.task(true), None);
    }
}
