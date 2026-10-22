//! Following the work a terminal is doing into another worktree.
//!
//! A terminal row is filed under a worktree. Sometimes the work it is doing moves to
//! another one, and the row should follow: a person looking at the panel is asking
//! "where is the thing I am working on", and the answer is the worktree, not the
//! directory a shell happened to be started in.
//!
//! What moves is the **row**. The process, its scrollback and its window stay exactly
//! where they are: nothing here writes to a terminal, feeds a directory to a shell, or
//! tells a program in front of the shell where to go. A program in front of the shell
//! is a program's own business, and a `cd` is the person's.
//!
//! Two things are followed, and they are separate questions.
//!
//! * **The shell's directory** says where a terminal is working, when the shell is
//!   what is in front. A terminal running an agent is not followed this way: the agent
//!   is in front of the shell, and the shell's directory says where the agent was
//!   started, not where it is working.
//! * **The agent's work** says where an agent session is working. A session's own
//!   directory is where it was started; the work it has done may be somewhere else,
//!   and only completed tool work says where.
//!
//! Three rules keep a move from being wrong:
//!
//! * **Exactly one terminal** may be showing the session. A session two terminals are
//!   showing names no terminal, because either of them could be the one that moved.
//! * **A move needs a destination that is a registered checkout.** The deepest
//!   registered root wins, a nested Git checkout this app does not know about is never
//!   folded into its parent, and two roots that are one directory are ambiguous.
//! * **A person who moved a terminal by hand wins.** A move the person made is not
//!   undone by an observation that is older than it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui_kit::{Context, Task};
use muster_core::opencode::{Client, ReadBudget, OBSERVATION_TIMEOUT};

use muster_core::opencode::observe::AgentSession;
use muster_core::opencode::relocate::{latest_work, CheckoutIndex};
use muster_core::settings::FollowWorktree;

use crate::app::TerminalWindow;

/// The registered checkouts, resolved once per pass.
///
/// The roots are canonicalized when the snapshot is taken, so a checkout that is
/// renamed under it is not a checkout that moved.
pub(crate) struct CheckoutRoots {
    index: CheckoutIndex,
    /// The checkout each root belongs to, deepest first.
    ids: Vec<(PathBuf, String)>,
}

impl CheckoutRoots {
    /// The checkouts this pass knows, from their roots.
    pub(crate) fn new(checkouts: &[(String, PathBuf)]) -> Self {
        let index = CheckoutIndex::new(checkouts);
        let mut ids: Vec<(PathBuf, String)> = checkouts
            .iter()
            .filter_map(|(id, path)| Some((path.canonicalize().ok()?, id.clone())))
            .collect();
        ids.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
        Self { index, ids }
    }

    /// The checkout a directory belongs to, when exactly one of them can be meant.
    ///
    /// `None` is the answer for a directory outside every registered checkout, for one
    /// inside a Git checkout this app does not know about, and for one that two
    /// registered roots could be: none of those is a worktree to move a row into.
    pub(crate) fn checkout_of(&self, directory: &Path) -> Option<&str> {
        let root = self
            .index
            .work_directory(std::slice::from_ref(&directory.to_path_buf()))?;
        self.ids
            .iter()
            .find(|(path, _)| path == &root)
            .map(|(_, id)| id.as_str())
    }
}

/// One terminal, as the follow logic sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FollowTerminal {
    /// The terminal's own id.
    pub(crate) id: String,
    /// The title the operating system gives the terminal.
    pub(crate) title: Option<String>,
    /// Whether an agent is what is in front of the shell.
    pub(crate) agent: bool,
    /// Where the shell is, as the operating system recorded it.
    pub(crate) shell_directory: Option<String>,
    pub(crate) checkout_id: String,
    pub(crate) generation: u64,
    pub(crate) foreground: Option<String>,
    pub(crate) foreground_running: bool,
}

/// What the follow logic remembers about one terminal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FollowState {
    /// When the person last moved this terminal by hand.
    ///
    /// A move the person made is not undone by an observation that is older than it.
    pub(crate) manual_at: Option<i64>,
    /// The shell's directory at the last pass.
    pub(crate) shell_directory: Option<String>,
    /// When that directory last changed.
    pub(crate) shell_changed_at: Option<i64>,
}

/// Keep the last verified targets, not just the cursor: no new work is not a return
/// to the session's starting directory. This is live window state, never stored JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionFollow {
    cursor: i64,
    targets: Option<Vec<PathBuf>>,
    origin: Option<PathBuf>,
    origin_changed_at: Option<i64>,
}

/// Cancelling the window's task also cancels further HTTP calls in its worker.
pub(crate) struct FollowTask {
    _task: Task<()>,
    budget: ReadBudget,
}

impl Drop for FollowTask {
    fn drop(&mut self) {
        self.budget.cancel();
    }
}

/// What one pass needs to know.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FollowInput {
    /// What is followed: neither, the shell's directory, the agent's work, or both.
    pub(crate) mode: FollowWorktree,
    /// The sessions the service lists.
    pub(crate) sessions: Vec<AgentSession>,
    /// The terminals, with what is in front of each shell.
    pub(crate) terminals: Vec<FollowTerminal>,
    /// The registered checkouts, with their canonical roots.
    pub(crate) checkouts: Vec<(String, PathBuf)>,
    /// What the last pass remembered.
    pub(crate) states: HashMap<String, FollowState>,
    /// When the completion cursor of each session was last read.
    pub(crate) cursors: HashMap<String, SessionFollow>,
    /// Work completed before following was enabled is not a new relocation.
    pub(crate) enabled_at: i64,
    /// Now, in epoch milliseconds.
    pub(crate) now: i64,
}

/// What one pass found.
pub(crate) struct FollowOutcome {
    /// What the pass remembered.
    pub(crate) states: HashMap<String, FollowState>,
    /// The completion cursor of each session.
    pub(crate) cursors: HashMap<String, SessionFollow>,
    /// The terminals whose row should move, and where to.
    pub(crate) moves: Vec<(String, String)>,
}

/// The work a session has done: when it finished, and where it happened.
pub(crate) type SessionWork = (i64, Vec<PathBuf>);

/// The pass, with the service read that attribution needs.
///
/// The read is bounded: only the sessions that are candidates are read, and only when
/// the service says they have changed. A service that cannot be read leaves the
/// session's work where it was, which is the conservative answer: the row stays where
/// the person put it.
pub(crate) fn run(
    input: FollowInput,
    read_work: &mut dyn FnMut(&str, i64, &Path) -> Option<SessionWork>,
) -> FollowOutcome {
    let mut states = input.states;
    let mut cursors = input.cursors;
    let mut moves = Vec::new();
    let roots = CheckoutRoots::new(&input.checkouts);

    if input.mode.follows_agent() {
        let showing = terminals_showing(&input.sessions, &input.terminals);
        for session in &input.sessions {
            // Exactly one terminal may be showing the session. Two of them means the
            // session names no terminal, and none of them means nothing to move.
            let Some(terminals) = showing.get(&session.id) else {
                continue;
            };
            let [terminal] = terminals.as_slice() else {
                continue;
            };
            let manual_at = states.entry(terminal.clone()).or_default().manual_at;
            let remembered = cursors
                .entry(session.id.clone())
                .or_insert_with(|| SessionFollow {
                    cursor: input.enabled_at,
                    origin: session.directory.clone(),
                    ..SessionFollow::default()
                });
            if remembered.origin != session.directory {
                // A genuinely changed service origin starts a new attribution boundary.
                remembered.origin = session.directory.clone();
                remembered.origin_changed_at = Some(input.now);
                remembered.targets = None;
                remembered.cursor = input.now;
            }
            if let Some(origin) = session
                .directory
                .as_deref()
                .filter(|path| path.is_absolute())
            {
                if let Some((completed, targets)) =
                    read_work(&session.id, remembered.cursor, origin)
                {
                    if completed > remembered.cursor {
                        remembered.cursor = completed;
                        remembered.targets = Some(targets);
                    }
                }
            }
            let changed_at = if remembered.targets.is_some() {
                remembered.cursor
            } else if let Some(changed_at) = remembered.origin_changed_at {
                changed_at
            } else {
                // First sight establishes the origin baseline. It is not a move
                // that happened while following was enabled.
                continue;
            };
            if manual_at.is_some_and(|manual| changed_at <= manual) {
                continue;
            }
            let work = remembered
                .targets
                .as_ref()
                .map(|targets| (remembered.cursor, targets.clone()));
            if let Some(checkout) = targets_checkout(&roots, remembered.origin.as_deref(), work) {
                moves.push((terminal.clone(), checkout));
            }
        }
    }

    if input.mode.follows_cd() {
        for terminal in &input.terminals {
            // An agent in front of the shell is the agent's business, not the shell's.
            if terminal.agent || terminal.foreground_running {
                continue;
            }
            let Some(directory) = terminal.shell_directory.as_deref() else {
                continue;
            };
            let state = states.entry(terminal.id.clone()).or_default();
            if state.shell_directory.as_deref() != Some(directory) {
                state.shell_directory = Some(directory.to_string());
                state.shell_changed_at = Some(input.now);
            }
            // A move the person made wins over a change that is older than it.
            if state.manual_at.is_some_and(|manual| {
                state
                    .shell_changed_at
                    .is_some_and(|changed| changed <= manual)
            }) {
                continue;
            }
            let Some(checkout) = roots.checkout_of(Path::new(directory)) else {
                continue;
            };
            moves.push((terminal.id.clone(), checkout.to_string()));
        }
    }

    FollowOutcome {
        states,
        cursors,
        moves,
    }
}

/// The terminal each session is showing, when exactly one of them shows it.
///
/// A terminal is matched by the title its own program wrote, and conservatively: a
/// title that names several sessions names none of them.
fn terminals_showing(
    sessions: &[AgentSession],
    terminals: &[FollowTerminal],
) -> HashMap<String, Vec<String>> {
    let mut showing: HashMap<String, Vec<String>> = HashMap::new();
    for terminal in terminals {
        if !terminal.agent {
            continue;
        }
        let Some(title) = terminal.title.as_deref() else {
            continue;
        };
        let Some(title) = muster_core::opencode::terminal_session_title(title) else {
            continue;
        };
        if let muster_core::opencode::MatchedSession::One(session) =
            muster_core::opencode::match_session_title(sessions, title)
        {
            showing
                .entry(session.id.clone())
                .or_default()
                .push(terminal.id.clone());
        }
    }
    showing
}

/// The checkout a session's work belongs to.
///
/// The work the session has done wins over the directory it was started in: a session
/// that is working in another worktree is working there, whatever directory it still
/// names. Work that cannot be attributed to exactly one checkout is not attributed at
/// all. Only a session with no newer tool targets falls back to its own directory.
fn targets_checkout(
    roots: &CheckoutRoots,
    origin: Option<&Path>,
    work: Option<(i64, Vec<PathBuf>)>,
) -> Option<String> {
    if let Some((_, targets)) = &work {
        if targets.is_empty() {
            return None;
        }
        let mut resolved: Option<String> = None;
        for target in targets {
            let checkout = roots.checkout_of(target)?;
            if resolved.as_deref().is_some_and(|known| known != checkout) {
                return None;
            }
            resolved = Some(checkout.to_string());
        }
        return resolved;
    }
    let origin = origin?;
    roots.checkout_of(origin).map(str::to_string)
}

/// The completed work of a session, as the service reports it.
///
/// This is the read the pass needs for attribution, and it is the only one: the
/// transcript is asked once per session that changed, and the answer is a list of
/// paths the completed tool calls touched.
pub(crate) fn completed_work(
    client: &muster_core::opencode::Client,
    session_id: &str,
    after: i64,
    origin: &Path,
) -> Option<SessionWork> {
    let messages = client.session_messages(session_id).ok()?;
    let array = messages.as_array()?;
    latest_work(array, after, origin)
}

type CheckoutIdentity = (String, Option<(u64, u64)>);

fn checkout_identities(checkouts: &[(String, PathBuf)]) -> Vec<CheckoutIdentity> {
    use std::os::unix::fs::MetadataExt;
    checkouts
        .iter()
        .map(|(id, path)| {
            (
                id.clone(),
                path.metadata()
                    .ok()
                    .map(|metadata| (metadata.dev(), metadata.ino())),
            )
        })
        .collect()
}

impl TerminalWindow {
    /// Runs the follow pass and moves the rows it found.
    ///
    /// The pass itself is off the window's thread: it reads the service for the work a
    /// session has done, and a service that takes its time must not hold the panel.
    pub(crate) fn run_follow_pass(&mut self, cx: &mut Context<Self>) {
        if self.closing || self.follow_worktree == FollowWorktree::Off || self.follow_task.is_some()
        {
            return;
        }
        let input = self.follow_input();
        let revision = self.follow_revision;
        let navigation_revision = self.navigation.revision;
        let identities = checkout_identities(&input.checkouts);
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute());
        let budget = ReadBudget::new(OBSERVATION_TIMEOUT);
        let read_budget = budget.clone();
        let work_input = input.clone();
        let worker = cx.background_executor().spawn(async move {
            let mut client: Option<Client> = None;
            let mut read = |session_id: &str, after: i64, origin: &Path| -> Option<SessionWork> {
                let home = home.as_ref()?;
                if client.is_none() {
                    client =
                        Some(Client::connect_service_with_budget(home, read_budget.clone()).ok()?);
                }
                completed_work(client.as_ref()?, session_id, after, origin)
            };
            run(work_input, &mut read)
        });
        self.retain_follow_pass(
            input,
            revision,
            navigation_revision,
            identities,
            budget,
            worker,
            cx,
        );
    }

    /// A preference change invalidates a delayed answer even after off -> on. Keep
    /// the in-flight task until it exits, so a new poll cannot overlap its worker.
    pub(crate) fn set_follow_worktree(&mut self, mode: FollowWorktree) {
        if self.follow_worktree == mode {
            return;
        }
        if let Some(task) = &self.follow_task {
            task.budget.cancel();
        }
        self.follow_revision = self.follow_revision.wrapping_add(1);
        self.follow_enabled_at = muster_core::opencode::now_millis();
        self.follow_cursors.clear();
        for state in self.follow_states.values_mut() {
            state.shell_directory = None;
            state.shell_changed_at = None;
        }
        self.follow_worktree = mode;
    }

    fn follow_input(&self) -> FollowInput {
        let mut terminals: Vec<FollowTerminal> = self
            .terminals
            .iter()
            .map(|(id, terminal)| {
                let status = terminal.shell.session.status();
                let program =
                    crate::navigation::foreground_program(status.foreground.app.as_deref());
                FollowTerminal {
                    id: id.clone(),
                    title: terminal.shell.session.title(),
                    agent: status.poll_error.is_none()
                        && status.foreground.running
                        && program == Some(crate::navigation::ForegroundProgram::OpenCode),
                    shell_directory: status
                        .poll_error
                        .is_none()
                        .then(|| status.working_directory.clone())
                        .flatten(),
                    checkout_id: terminal.checkout_id.clone(),
                    generation: terminal.shell.session.generation(),
                    foreground: status.foreground.app.clone(),
                    foreground_running: status.foreground.running || status.poll_error.is_some(),
                }
            })
            .collect();
        terminals.sort_by(|left, right| left.id.cmp(&right.id));
        let checkouts: Vec<(String, PathBuf)> = self
            .navigation
            .repos
            .iter()
            .flat_map(|repo| repo.checkouts.iter())
            .filter(|checkout| {
                !checkout.is_missing
                    && !self.navigation.archived.contains(&checkout.id)
                    && !self.removing_checkouts.contains(&checkout.id)
            })
            .map(|checkout| (checkout.id.clone(), PathBuf::from(&checkout.canonical_path)))
            .collect();
        FollowInput {
            mode: self.follow_worktree,
            sessions: if self.agent.error.is_none() {
                self.agent.sessions.clone()
            } else {
                Vec::new()
            },
            terminals,
            checkouts,
            states: self.follow_states.clone(),
            cursors: self.follow_cursors.clone(),
            enabled_at: self.follow_enabled_at,
            now: muster_core::opencode::now_millis(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn retain_follow_pass(
        &mut self,
        input: FollowInput,
        revision: u64,
        navigation_revision: u64,
        identities: Vec<CheckoutIdentity>,
        budget: ReadBudget,
        worker: Task<FollowOutcome>,
        cx: &mut Context<Self>,
    ) {
        let task = cx.spawn(async move |view, cx| {
            let outcome = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.follow_task = None;
                let mut current = app.follow_input();
                current.now = input.now;
                if app.closing
                    || app.follow_revision != revision
                    || app.navigation.revision != navigation_revision
                    || current != input
                    || checkout_identities(&current.checkouts) != identities
                {
                    return;
                }
                // Maps remained authoritative while the read was pending. Replace them
                // only after all the facts used by this result have been rechecked.
                app.follow_states = outcome.states;
                app.follow_cursors = outcome.cursors;
                for (terminal, checkout) in outcome.moves {
                    app.apply_follow_move(&terminal, &checkout, cx);
                }
            });
        });
        self.follow_task = Some(FollowTask {
            _task: task,
            budget,
        });
    }

    /// A terminal's row moves to another worktree.
    ///
    /// The row moves; the process, its scrollback and its window do not. Nothing here
    /// writes to the terminal or feeds a directory to a shell.
    fn apply_follow_move(&mut self, terminal: &str, checkout: &str, cx: &mut Context<Self>) {
        let Some(session) = self.navigation.session(terminal).cloned() else {
            return;
        };
        if session.checkout_id == checkout
            || !self.terminals.contains_key(terminal)
            || self.removing_checkouts.contains(checkout)
            || self.removing_checkouts.contains(&session.checkout_id)
        {
            return;
        }
        // What the person was looking at before the move. A person who had navigated
        // somewhere else keeps what they were looking at: the move is about a row, not
        // about where the window looks.
        let previous_checkout = self.navigation.active_checkout.clone();
        let previous = self.navigation.active_session.clone();
        let previous_revision = self.navigation.revision;
        let mut selected = self.navigation.selected_by_checkout.clone();
        selected.retain(|_, id| id != terminal);
        let visible = previous.as_deref() == Some(terminal)
            && self.open_document.is_none()
            && self.document_target.is_none()
            && self.open_diff.is_none();
        let index = self
            .navigation
            .checkout(checkout)
            .map(|checkout| checkout.sessions.len())
            .unwrap_or(0);
        if self
            .navigation
            .move_session(terminal, checkout, index)
            .is_err()
        {
            return;
        }
        if let Some(live) = self.terminals.get_mut(terminal) {
            live.checkout_id = checkout.into();
        }
        if visible {
            self.focus_pending = true;
        } else {
            // move_session selects its destination. Undo that navigation for a row
            // moving behind a document, diff, empty pane, or another terminal.
            self.navigation.active_checkout = previous_checkout;
            self.navigation.active_session = previous;
            self.navigation.revision = previous_revision;
            self.navigation.selected_by_checkout = selected;
        }
        self.persist();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui_kit::test::TestWindowExt;
    use gpui_kit::AppContext;
    use muster_core::opencode::observe::AgentSession;

    fn session(id: &str, title: &str, directory: &str, updated_at: i64) -> AgentSession {
        AgentSession {
            id: id.into(),
            title: title.into(),
            directory: Some(PathBuf::from(directory)),
            updated_at,
            ..AgentSession::default()
        }
    }

    fn terminal(id: &str, title: Option<&str>, agent: bool) -> FollowTerminal {
        FollowTerminal {
            id: id.into(),
            title: title.map(str::to_string),
            agent,
            shell_directory: None,
            ..FollowTerminal::default()
        }
    }

    fn roots() -> (tempfile::TempDir, Vec<(String, PathBuf)>) {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let checkouts = vec![
            ("checkout:first".to_string(), first.canonicalize().unwrap()),
            (
                "checkout:second".to_string(),
                second.canonicalize().unwrap(),
            ),
        ];
        (temp, checkouts)
    }

    #[test]
    fn a_session_one_terminal_is_showing_moves_that_terminal() {
        let (_temp, checkouts) = roots();
        let second = checkouts[1].1.clone();
        let sessions = vec![session(
            "ses_a",
            "work",
            second.to_string_lossy().as_ref(),
            100,
        )];
        let terminals = vec![terminal("t1", Some("OC | work"), true)];
        let outcome = run(
            FollowInput {
                mode: FollowWorktree::Agent,
                sessions,
                terminals,
                checkouts,
                states: HashMap::new(),
                cursors: HashMap::new(),
                now: 200,
                enabled_at: 0,
            },
            &mut |_, _, _| Some((100, vec![second.clone()])),
        );
        assert_eq!(
            outcome.moves,
            vec![("t1".to_string(), "checkout:second".to_string())]
        );
    }

    #[test]
    fn a_session_two_terminals_are_showing_moves_nothing() {
        let (_temp, checkouts) = roots();
        let second = checkouts[1].1.clone();
        let sessions = vec![session(
            "ses_a",
            "work",
            second.to_string_lossy().as_ref(),
            100,
        )];
        let terminals = vec![
            terminal("t1", Some("OC | work"), true),
            terminal("t2", Some("OC | work"), true),
        ];
        let outcome = run(
            FollowInput {
                mode: FollowWorktree::Agent,
                sessions,
                terminals,
                checkouts,
                states: HashMap::new(),
                cursors: HashMap::new(),
                now: 200,
                enabled_at: 0,
            },
            &mut |_, _, _| None,
        );
        assert!(outcome.moves.is_empty());
    }

    #[test]
    fn a_move_the_person_made_wins_over_an_older_observation() {
        let (_temp, checkouts) = roots();
        let second = checkouts[1].1.clone();
        let sessions = vec![session(
            "ses_a",
            "work",
            second.to_string_lossy().as_ref(),
            100,
        )];
        let terminals = vec![terminal("t1", Some("OC | work"), true)];
        let mut states = HashMap::new();
        states.insert(
            "t1".to_string(),
            FollowState {
                manual_at: Some(150),
                ..FollowState::default()
            },
        );
        let outcome = run(
            FollowInput {
                mode: FollowWorktree::Agent,
                sessions,
                terminals,
                checkouts,
                states,
                cursors: HashMap::new(),
                now: 200,
                enabled_at: 0,
            },
            &mut |_, _, _| None,
        );
        assert!(
            outcome.moves.is_empty(),
            "the observation is older than the move"
        );
    }

    #[test]
    fn a_directory_outside_every_registered_checkout_moves_nothing() {
        let (_temp, checkouts) = roots();
        let sessions = vec![session("ses_a", "work", "/tmp/elsewhere", 100)];
        let terminals = vec![terminal("t1", Some("OC | work"), true)];
        let outcome = run(
            FollowInput {
                mode: FollowWorktree::Agent,
                sessions,
                terminals,
                checkouts,
                states: HashMap::new(),
                cursors: HashMap::new(),
                now: 200,
                enabled_at: 0,
            },
            &mut |_, _, _| None,
        );
        assert!(outcome.moves.is_empty());
    }

    #[test]
    fn the_shells_directory_is_followed_only_when_no_agent_is_in_front() {
        let (_temp, checkouts) = roots();
        let second = checkouts[1].1.clone();
        let mut plain = terminal("t1", None, false);
        plain.shell_directory = Some(second.to_string_lossy().into_owned());
        let mut agent = terminal("t2", Some("OC | work"), true);
        agent.shell_directory = Some(second.to_string_lossy().into_owned());
        let outcome = run(
            FollowInput {
                mode: FollowWorktree::Cd,
                sessions: Vec::new(),
                terminals: vec![plain, agent],
                checkouts,
                states: HashMap::new(),
                cursors: HashMap::new(),
                now: 200,
                enabled_at: 0,
            },
            &mut |_, _, _| None,
        );
        assert_eq!(outcome.moves.len(), 1);
        assert_eq!(outcome.moves[0].0, "t1");
    }

    #[test]
    fn a_follow_is_off_when_the_setting_says_so() {
        let (_temp, checkouts) = roots();
        let sessions = vec![session("ses_a", "work", "/tmp/second", 100)];
        let terminals = vec![terminal("t1", Some("OC | work"), true)];
        let outcome = run(
            FollowInput {
                mode: FollowWorktree::Off,
                sessions,
                terminals,
                checkouts,
                states: HashMap::new(),
                cursors: HashMap::new(),
                now: 200,
                enabled_at: 0,
            },
            &mut |_, _, _| None,
        );
        assert!(outcome.moves.is_empty());
    }

    #[test]
    fn no_new_tool_work_keeps_the_effective_target_and_does_not_bounce_to_origin() {
        let (_temp, checkouts) = roots();
        let mut input = FollowInput {
            mode: FollowWorktree::Agent,
            sessions: vec![session(
                "ses_a",
                "work",
                checkouts[0].1.to_str().unwrap(),
                100,
            )],
            terminals: vec![terminal("t1", Some("OC | work"), true)],
            checkouts: checkouts.clone(),
            ..FollowInput::default()
        };
        let first = run(input.clone(), &mut |_, after, origin| {
            assert_eq!(after, 0);
            assert_eq!(origin, checkouts[0].1);
            Some((100, vec![checkouts[1].1.clone()]))
        });
        assert_eq!(first.moves, vec![("t1".into(), "checkout:second".into())]);
        input.cursors = first.cursors;
        input.states = first.states;
        let second = run(input.clone(), &mut |_, after, _| {
            assert_eq!(after, 100);
            None
        });
        assert_eq!(
            second.moves, first.moves,
            "a new poll is not a return to origin"
        );

        input.states.entry("t1".into()).or_default().manual_at = Some(150);
        input.sessions[0].updated_at = 200;
        let after_manual = run(input, &mut |_, _, _| None);
        assert!(
            after_manual.moves.is_empty(),
            "a later status timestamp is not new work"
        );
    }

    #[test]
    fn enabling_follow_baselines_the_cursor_and_uses_the_actual_session_origin() {
        let (_temp, checkouts) = roots();
        let origin = checkouts[0].1.clone();
        let input = FollowInput {
            mode: FollowWorktree::Agent,
            sessions: vec![session("ses_a", "work", origin.to_str().unwrap(), 100)],
            terminals: vec![terminal("t1", Some("OC | work"), true)],
            checkouts,
            enabled_at: 200,
            ..FollowInput::default()
        };
        let outcome = run(input, &mut |_, after, actual_origin| {
            assert_eq!(after, 200);
            assert_eq!(actual_origin, origin);
            let messages = vec![serde_json::json!({
                "type": "assistant", "content": [{
                    "type": "tool", "name": "edit", "time": {"completed": 100},
                    "state": {"status": "completed", "input": {"path": "../second/file"}}
                }]
            })];
            latest_work(&messages, after, actual_origin)
        });
        assert!(
            outcome.moves.is_empty(),
            "baseline observations are not relocations"
        );
        assert_eq!(outcome.cursors["ses_a"].cursor, 200);
    }

    struct WindowFixture {
        _directory: tempfile::TempDir,
        handle: gpui_kit::AnyWindowHandle,
        view: gpui_kit::Entity<TerminalWindow>,
        shell: crate::terminal::TerminalSession,
        source: String,
        target: String,
    }

    fn open_window(cx: &mut gpui_kit::TestAppContext) -> WindowFixture {
        use crate::workspace::{Repo, Session, SessionStatus as SavedSessionStatus, SessionType};
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let (directory, checkouts) = roots();
        let first = Repo::plain(&checkouts[0].1, "first").unwrap();
        let second = Repo::plain(&checkouts[1].1, "second").unwrap();
        let source = first.checkouts[0].id.clone();
        let target = second.checkouts[0].id.clone();
        let backend = Arc::new(crate::pty::TerminalBackend::default());
        let mut config = crate::terminal::TerminalSessionConfig::new(
            format!("follow-{}", source),
            checkouts[0].1.clone(),
            80,
            24,
            Some(PathBuf::from("/bin/sh")),
        );
        config.args = Some(vec!["-c".into(), "set -m; cat; :".into()]);
        config.status_interval = Duration::from_millis(10);
        let shell = crate::terminal::TerminalSession::spawn(backend.clone(), config).unwrap();
        // The child starts once the size it will keep is known: a session with
        // no view is sized here.
        shell.resize(80, 24).expect("the session takes its size");
        let deadline = Instant::now() + Duration::from_secs(5);
        while shell.status().foreground.app.as_deref() != Some("cat") {
            assert!(
                Instant::now() < deadline,
                "the fixture's foreground process starts"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });
        let (handle, view) = cx.update(|cx| {
            gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| {
                    let mut app =
                        TerminalWindow::with_session(shell.clone(), backend.clone(), window, cx);
                    app.navigation.merge(
                        muster_core::navigation::OpenedDirectory {
                            repo: first,
                            checkout_id: source.clone(),
                        },
                        true,
                    );
                    app.navigation.merge(
                        muster_core::navigation::OpenedDirectory {
                            repo: second,
                            checkout_id: target.clone(),
                        },
                        false,
                    );
                    app.navigation.add_session(
                        Session {
                            id: shell.id().into(),
                            checkout_id: source.clone(),
                            name: "sh".into(),
                            created_at: "test".into(),
                            session_type: SessionType::Shell,
                            status: SavedSessionStatus::Active,
                        },
                        Some(app.navigation.revision),
                    );
                    app.terminals.get_mut(shell.id()).unwrap().checkout_id = source.clone();
                    app.set_follow_worktree(FollowWorktree::Agent);
                    app
                })
            })
            .unwrap()
        });
        for _ in 0..3 {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
        }
        WindowFixture {
            _directory: directory,
            handle,
            view,
            shell,
            source,
            target,
        }
    }

    fn close_window(fixture: WindowFixture, cx: &mut gpui_kit::TestAppContext) {
        cx.update_window(fixture.handle, |_, window, _| window.remove_window())
            .unwrap();
        fixture.shell.close().wait().unwrap();
        cx.run_until_parked();
    }

    /// Delay exactly the production completion path without a real service or a
    /// sleeping HTTP thread. The worker result represents an already computed move.
    fn delayed_move(fixture: &WindowFixture, cx: &mut gpui_kit::TestAppContext) {
        use std::time::Duration;
        fixture.view.update(cx, |app, cx| {
            let input = app.follow_input();
            let states = input.states.clone();
            let cursors = input.cursors.clone();
            let moves = vec![(fixture.shell.id().into(), fixture.target.clone())];
            let executor = cx.background_executor().clone();
            let worker = executor.clone().spawn(async move {
                executor.timer(Duration::from_secs(1)).await;
                FollowOutcome {
                    states,
                    cursors,
                    moves,
                }
            });
            let identities = checkout_identities(&input.checkouts);
            app.retain_follow_pass(
                input,
                app.follow_revision,
                app.navigation.revision,
                identities,
                ReadBudget::new(OBSERVATION_TIMEOUT),
                worker,
                cx,
            );
        });
        cx.run_until_parked();
    }

    fn finish_delay(cx: &mut gpui_kit::TestAppContext) {
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
    }

    #[gpui_kit::test]
    fn an_unchanged_delayed_pass_reaches_the_production_row_move(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let fixture = open_window(cx);
        delayed_move(&fixture, cx);
        finish_delay(cx);
        fixture.view.read_with(cx, |app, _| {
            assert!(app.follow_task.is_none());
            assert_eq!(
                app.terminals[fixture.shell.id()].checkout_id,
                fixture.target
            );
            assert_eq!(
                app.navigation
                    .session(fixture.shell.id())
                    .unwrap()
                    .checkout_id,
                fixture.target
            );
        });
        close_window(fixture, cx);
    }

    #[gpui_kit::test]
    fn a_checkout_reserved_during_a_pass_invalidates_the_result(cx: &mut gpui_kit::TestAppContext) {
        let fixture = open_window(cx);
        delayed_move(&fixture, cx);
        fixture.view.update(cx, |app, _| {
            app.removing_checkouts.insert(fixture.target.clone());
        });
        finish_delay(cx);
        fixture.view.read_with(cx, |app, _| {
            assert_eq!(
                app.terminals[fixture.shell.id()].checkout_id,
                fixture.source
            );
        });
        close_window(fixture, cx);
    }

    #[gpui_kit::test]
    fn a_delayed_pass_keeps_authoritative_maps_and_never_overwrites_a_manual_move(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let fixture = open_window(cx);
        fixture.view.update(cx, |app, _| {
            app.follow_states
                .entry(fixture.shell.id().into())
                .or_default()
                .manual_at = Some(5);
        });
        delayed_move(&fixture, cx);
        cx.update_window(fixture.handle, |_, window, cx| {
            fixture.view.update(cx, |app, cx| {
                assert_eq!(app.follow_states[fixture.shell.id()].manual_at, Some(5));
                let revision = app.follow_revision;
                app.move_terminal(fixture.shell.id(), &fixture.source, 0, window, cx);
                assert!(app.follow_revision > revision);
                app.run_follow_pass(cx);
                assert!(
                    app.follow_task.is_some(),
                    "a second poll cannot start another pass"
                );
            });
        })
        .unwrap();
        finish_delay(cx);
        fixture.view.read_with(cx, |app, _| {
            assert!(app.follow_task.is_none());
            assert_eq!(
                app.navigation
                    .session(fixture.shell.id())
                    .unwrap()
                    .checkout_id,
                fixture.source
            );
            assert_ne!(app.follow_states[fixture.shell.id()].manual_at, Some(5));
        });
        close_window(fixture, cx);
    }

    #[gpui_kit::test]
    fn disabling_and_reenabling_follow_invalidates_a_delayed_pass(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let fixture = open_window(cx);
        delayed_move(&fixture, cx);
        fixture.view.update(cx, |app, _| {
            app.set_follow_worktree(FollowWorktree::Off);
            app.set_follow_worktree(FollowWorktree::Agent);
        });
        finish_delay(cx);
        fixture.view.read_with(cx, |app, _| {
            assert_eq!(
                app.navigation
                    .session(fixture.shell.id())
                    .unwrap()
                    .checkout_id,
                fixture.source
            );
            assert!(app.follow_cursors.is_empty());
        });
        close_window(fixture, cx);
    }

    #[gpui_kit::test]
    fn a_second_terminal_arriving_during_a_pass_invalidates_its_matching(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let fixture = open_window(cx);
        delayed_move(&fixture, cx);
        fixture.view.update(cx, |app, _| {
            let live = &app.terminals[fixture.shell.id()];
            let second = crate::app::LiveTerminal {
                checkout_id: live.checkout_id.clone(),
                view: live.view.clone(),
                shell: live.shell.clone(),
                prompt: None,
            };
            app.terminals.insert("second-row".into(), second);
        });
        finish_delay(cx);
        fixture.view.read_with(cx, |app, _| {
            assert_eq!(
                app.navigation
                    .session(fixture.shell.id())
                    .unwrap()
                    .checkout_id,
                fixture.source
            );
        });
        close_window(fixture, cx);
    }

    #[gpui_kit::test]
    fn an_automatic_row_move_preserves_a_document_or_empty_pane_and_updates_live_ownership(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let fixture = open_window(cx);
        fixture.view.update(cx, |app, cx| {
            app.navigation.active_session = None;
            app.document_target = Some((fixture.source.clone(), "draft.rs".into()));
            let before = (
                app.navigation.active_checkout.clone(),
                app.navigation.active_session.clone(),
                app.navigation.revision,
                app.document_target.clone(),
            );
            let shell = app.terminals[fixture.shell.id()].shell.clone();
            app.apply_follow_move(fixture.shell.id(), &fixture.target, cx);
            assert_eq!(
                (
                    app.navigation.active_checkout.clone(),
                    app.navigation.active_session.clone(),
                    app.navigation.revision,
                    app.document_target.clone()
                ),
                before
            );
            assert_eq!(
                app.terminals[fixture.shell.id()].checkout_id,
                fixture.target
            );
            assert_eq!(
                app.navigation
                    .session(fixture.shell.id())
                    .unwrap()
                    .checkout_id,
                fixture.target
            );
            assert!(std::sync::Arc::ptr_eq(
                &shell,
                &app.terminals[fixture.shell.id()].shell
            ));
            assert!(app
                .navigation
                .selected_by_checkout
                .values()
                .all(|id| id != fixture.shell.id()));
        });
        close_window(fixture, cx);
    }

    #[gpui_kit::test]
    fn a_document_over_the_moving_terminal_keeps_its_checkout_and_navigation(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let fixture = open_window(cx);
        fixture.view.update(cx, |app, cx| {
            app.document_target = Some((fixture.source.clone(), "draft.rs".into()));
            let before = (
                app.navigation.active_checkout.clone(),
                app.navigation.active_session.clone(),
                app.navigation.revision,
                app.document_target.clone(),
            );
            app.apply_follow_move(fixture.shell.id(), &fixture.target, cx);
            assert_eq!(
                (
                    app.navigation.active_checkout.clone(),
                    app.navigation.active_session.clone(),
                    app.navigation.revision,
                    app.document_target.clone()
                ),
                before
            );
            assert_eq!(
                app.terminals[fixture.shell.id()].checkout_id,
                fixture.target
            );
        });
        close_window(fixture, cx);
    }

    #[gpui_kit::test]
    fn a_visible_terminal_follows_its_row_but_a_reserved_checkout_never_receives_it(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let fixture = open_window(cx);
        fixture.view.update(cx, |app, cx| {
            app.removing_checkouts.insert(fixture.target.clone());
            app.apply_follow_move(fixture.shell.id(), &fixture.target, cx);
            assert_eq!(
                app.terminals[fixture.shell.id()].checkout_id,
                fixture.source
            );
            app.removing_checkouts.clear();
            app.apply_follow_move(fixture.shell.id(), &fixture.target, cx);
            assert_eq!(
                app.navigation.active_checkout.as_deref(),
                Some(fixture.target.as_str())
            );
            assert_eq!(
                app.navigation.active_session.as_deref(),
                Some(fixture.shell.id())
            );
            assert_eq!(
                app.terminals[fixture.shell.id()].checkout_id,
                fixture.target
            );
        });
        close_window(fixture, cx);
    }

    #[test]
    fn the_deepest_registered_root_wins_and_a_nested_unknown_one_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let nested = root.join("nested");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        let checkouts = vec![("checkout:root".to_string(), root.canonicalize().unwrap())];
        let roots = CheckoutRoots::new(&checkouts);
        assert_eq!(
            roots.checkout_of(&root),
            Some("checkout:root"),
            "a directory inside the registered root belongs to it"
        );
        assert_eq!(
            roots.checkout_of(&nested),
            None,
            "a nested Git checkout this app does not know about is not folded into its parent"
        );
    }
}
