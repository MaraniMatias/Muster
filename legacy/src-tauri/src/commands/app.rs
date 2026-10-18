use std::sync::Arc;

use serde::Deserialize;
use tauri::State;

use crate::{
    domain::ipc::{IpcError, IpcErrorCode},
    services::{agent::AgentService, opener},
    terminal::TerminalBackend,
};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendDiagnosticCategory {
    FrontendVueError,
    FrontendWindowError,
    FrontendUnhandledRejection,
    UiWritesDeadline,
    ExitSweepDeadline,
}

/// The class of the failure, as the frontend reads it off the error object. The words a real message
/// could have carried — a path, a URL, the document that was open — are what this enum replaces.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendDiagnosticKind {
    TypeError,
    ReferenceError,
    RangeError,
    SyntaxError,
    NetworkError,
    AbortError,
    SecurityError,
    NotSupportedError,
    Unknown,
}

/// Which phase of Vue raised a `frontend_vue_error`. A phase is worth naming and a component is
/// not, so this is all the "which component" a diagnostic gets: `unknown` for every other category.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendDiagnosticHook {
    Setup,
    Render,
    LifecycleHook,
    EventHandler,
    WatcherCallback,
    Unknown,
}

fn frontend_diagnostic_category_message(category: FrontendDiagnosticCategory) -> &'static str {
    match category {
        FrontendDiagnosticCategory::FrontendVueError => "frontend_vue_error",
        FrontendDiagnosticCategory::FrontendWindowError => "frontend_window_error",
        FrontendDiagnosticCategory::FrontendUnhandledRejection => "frontend_unhandled_rejection",
        FrontendDiagnosticCategory::UiWritesDeadline => "ui_writes_deadline",
        FrontendDiagnosticCategory::ExitSweepDeadline => "exit_sweep_deadline",
    }
}

fn frontend_diagnostic_kind_message(kind: FrontendDiagnosticKind) -> &'static str {
    match kind {
        FrontendDiagnosticKind::TypeError => "type_error",
        FrontendDiagnosticKind::ReferenceError => "reference_error",
        FrontendDiagnosticKind::RangeError => "range_error",
        FrontendDiagnosticKind::SyntaxError => "syntax_error",
        FrontendDiagnosticKind::NetworkError => "network_error",
        FrontendDiagnosticKind::AbortError => "abort_error",
        FrontendDiagnosticKind::SecurityError => "security_error",
        FrontendDiagnosticKind::NotSupportedError => "not_supported_error",
        FrontendDiagnosticKind::Unknown => "unknown",
    }
}

fn frontend_diagnostic_hook_message(hook: FrontendDiagnosticHook) -> &'static str {
    match hook {
        FrontendDiagnosticHook::Setup => "setup",
        FrontendDiagnosticHook::Render => "render",
        FrontendDiagnosticHook::LifecycleHook => "lifecycle_hook",
        FrontendDiagnosticHook::EventHandler => "event_handler",
        FrontendDiagnosticHook::WatcherCallback => "watcher_callback",
        FrontendDiagnosticHook::Unknown => "unknown",
    }
}

fn frontend_diagnostic_line(
    category: FrontendDiagnosticCategory,
    kind: FrontendDiagnosticKind,
    hook: FrontendDiagnosticHook,
    sequence: u64,
) -> String {
    format!(
        "frontend diagnostic category={} kind={} hook={} sequence={sequence}",
        frontend_diagnostic_category_message(category),
        frontend_diagnostic_kind_message(kind),
        frontend_diagnostic_hook_message(hook),
    )
}

/// Logs the fixed category, the fixed class of the failure, the fixed Vue phase and a per-run
/// counter; the frontend cannot send diagnostic text or payloads.
///
/// Every field is a closed enum that `serde` refuses anything outside of, and each one is rendered
/// by a `match` to a `&'static str` in this file, so the line is one of
/// `categories x kinds x hooks` followed by a number and nothing else. That is why the error message,
/// the stack, the component and its props, the paths and the URLs the error mentioned are read by
/// the frontend and never sent: a log file the user can forward has no business carrying any of it,
/// and a class of failure plus a phase is what is missing from a line that only said a failure.
#[tauri::command]
pub fn frontend_diagnostic(
    category: FrontendDiagnosticCategory,
    kind: FrontendDiagnosticKind,
    hook: FrontendDiagnosticHook,
    sequence: u64,
) {
    log::warn!(
        target: "muster::frontend_diagnostic",
        "{}",
        frontend_diagnostic_line(category, kind, hook, sequence)
    );
}

/// Releases every agent bridge and ends every terminal, which is what leaves neither behind.
///
/// Only the terminals are children of this app. The agent service is the other way round:
/// `services::opencode` finds one that is already running and connects to it, and the absence of
/// one leaves the app disconnected rather than leaving a server behind, so there is no server here
/// to end and none that would be ours to end. What the sweep releases is this app's side of that
/// conversation — the event readers and the sockets they hold, each signalled and then joined under
/// a deadline of its own so a stream that will not close cannot hold the sweep. The shells are the
/// other half and they really are children: a PTY left with no parent still holding its pane and its
/// port is a process nothing is left to reap.
pub fn stop_children(agents: &AgentService, terminal: &TerminalBackend) {
    agents.stop_all();
    terminal.shutdown();
}

/// Ends the processes the app started, before the window closes.
///
/// The window is what decides when the app ends, and it decides it in the frontend, so the sweep
/// is asked for over the bridge rather than taken from an exit event: `RunEvent::ExitRequested` and
/// `RunEvent::Exit` are both delivered once the runtime has decided the process is going, and on a
/// window close neither of them arrives at all. Measured on macOS, the window closes and the process
/// ends with status 0 while no exit event reaches the app, which is how every terminal outlived the
/// quit it belonged to until a later launch swept it up.
#[tauri::command]
pub async fn app_prepare_exit(
    agents: State<'_, Arc<AgentService>>,
    terminal: State<'_, Arc<TerminalBackend>>,
) -> Result<(), IpcError> {
    let agents = Arc::clone(agents.inner());
    let terminal = Arc::clone(terminal.inner());
    // The sweep waits on children, which takes longer than a bridge call is meant to hold a thread
    // for, and the window is still up: nothing here needs the event loop to keep turning.
    tauri::async_runtime::spawn_blocking(move || stop_children(&agents, &terminal))
        .await
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::OperationFailed,
                format!("the sweep before exit did not finish: {error}"),
            )
        })
}

/// Hands a web link to the browser this machine has.
///
/// A document can print a link and this window cannot follow one: the preview is not a browser,
/// and a webview is not one either. So the URL comes back over the bridge and leaves through the
/// service, which is where the decision about what may be opened lives.
///
/// The opener is a process and `.status()` waits for it, so the call goes to a blocking thread
/// rather than being spawned and forgotten: a browser that takes a moment to start is waited on
/// where waiting does not hold the event loop.
#[tauri::command]
pub async fn open_url(url: String) -> Result<(), IpcError> {
    tauri::async_runtime::spawn_blocking(move || opener::open_url(&url))
        .await
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::OperationFailed,
                format!("the browser could not be asked to open the link: {error}"),
            )
        })?
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATEGORIES: [FrontendDiagnosticCategory; 5] = [
        FrontendDiagnosticCategory::FrontendVueError,
        FrontendDiagnosticCategory::FrontendWindowError,
        FrontendDiagnosticCategory::FrontendUnhandledRejection,
        FrontendDiagnosticCategory::UiWritesDeadline,
        FrontendDiagnosticCategory::ExitSweepDeadline,
    ];
    const KINDS: [FrontendDiagnosticKind; 9] = [
        FrontendDiagnosticKind::TypeError,
        FrontendDiagnosticKind::ReferenceError,
        FrontendDiagnosticKind::RangeError,
        FrontendDiagnosticKind::SyntaxError,
        FrontendDiagnosticKind::NetworkError,
        FrontendDiagnosticKind::AbortError,
        FrontendDiagnosticKind::SecurityError,
        FrontendDiagnosticKind::NotSupportedError,
        FrontendDiagnosticKind::Unknown,
    ];
    const HOOKS: [FrontendDiagnosticHook; 6] = [
        FrontendDiagnosticHook::Setup,
        FrontendDiagnosticHook::Render,
        FrontendDiagnosticHook::LifecycleHook,
        FrontendDiagnosticHook::EventHandler,
        FrontendDiagnosticHook::WatcherCallback,
        FrontendDiagnosticHook::Unknown,
    ];

    #[test]
    fn diagnostic_message_contains_only_its_fixed_category() {
        let cases = [
            (
                FrontendDiagnosticCategory::FrontendVueError,
                "frontend_vue_error",
            ),
            (
                FrontendDiagnosticCategory::FrontendWindowError,
                "frontend_window_error",
            ),
            (
                FrontendDiagnosticCategory::FrontendUnhandledRejection,
                "frontend_unhandled_rejection",
            ),
            (
                FrontendDiagnosticCategory::UiWritesDeadline,
                "ui_writes_deadline",
            ),
            (
                FrontendDiagnosticCategory::ExitSweepDeadline,
                "exit_sweep_deadline",
            ),
        ];

        for (category, expected) in cases {
            assert_eq!(frontend_diagnostic_category_message(category), expected);
        }
    }

    #[test]
    fn diagnostic_line_is_the_only_thing_the_frontend_can_have_it_say() {
        // The whole vocabulary crossed: 5 x 9 x 6 lines, every one of them a name from the three
        // matches above and a number. There is no fourth field for the frontend to have said
        // something in, which is the whole of the privacy claim.
        for category in CATEGORIES {
            for kind in KINDS {
                for hook in HOOKS {
                    let expected = format!(
                        "frontend diagnostic category={} kind={} hook={} sequence=7",
                        frontend_diagnostic_category_message(category),
                        frontend_diagnostic_kind_message(kind),
                        frontend_diagnostic_hook_message(hook),
                    );
                    assert_eq!(frontend_diagnostic_line(category, kind, hook, 7), expected);
                }
            }
        }
    }

    #[test]
    fn diagnostic_line_carries_the_sequence_that_orders_reports() {
        // One category, one line per minute: the sequence is the only thing that tells two reports of
        // the same failure apart, so it has to be on the line and it has to differ between them.
        let window = FrontendDiagnosticCategory::FrontendWindowError;
        let network = FrontendDiagnosticKind::NetworkError;
        let first = frontend_diagnostic_line(window, network, FrontendDiagnosticHook::Unknown, 1);
        let second = frontend_diagnostic_line(window, network, FrontendDiagnosticHook::Unknown, 2);

        assert_eq!(
            first,
            "frontend diagnostic category=frontend_window_error kind=network_error hook=unknown sequence=1"
        );
        assert_ne!(first, second);
        assert!(second.ends_with("sequence=2"));
    }

    /// The four arguments as one deserialization, which is the shape they arrive in over the bridge.
    type DiagnosticArguments = (
        FrontendDiagnosticCategory,
        FrontendDiagnosticKind,
        FrontendDiagnosticHook,
        u64,
    );

    #[test]
    fn diagnostic_rejects_a_payload_outside_its_vocabularies() {
        assert!(
            serde_json::from_value::<DiagnosticArguments>(serde_json::json!([
                "frontend_vue_error",
                "type_error",
                "setup",
                1
            ]))
            .is_ok()
        );

        let refused = [
            // The privacy claim stated as a test: text in any field is refused before a log line
            // exists, rather than accepted and dropped where it would look as if it had been scrubbed.
            serde_json::json!(["frontend_vue_error", "private payload", "setup", 1]),
            serde_json::json!(["private payload", "type_error", "setup", 1]),
            serde_json::json!(["frontend_vue_error", "type_error", "private payload", 1]),
            // A word outside the vocabulary is as refused as a word that is text.
            serde_json::json!(["frontend_vue_error", "typeerror", "setup", 1]),
            serde_json::json!(["frontend_vue_error", "type_error", "Setup", 1]),
            // And the correlation is a number, not something a word can be written into.
            serde_json::json!(["frontend_vue_error", "type_error", "setup", "1"]),
            serde_json::json!(["frontend_vue_error", "type_error", "setup", -1]),
            serde_json::json!(["frontend_vue_error", "type_error", "setup", 1.5]),
        ];

        for payload in refused {
            assert!(serde_json::from_value::<DiagnosticArguments>(payload).is_err());
        }
    }

    #[test]
    fn diagnostic_category_rejects_unlisted_input() {
        assert!(
            serde_json::from_value::<FrontendDiagnosticCategory>(serde_json::json!(
                "private payload"
            ))
            .is_err()
        );
    }
}
