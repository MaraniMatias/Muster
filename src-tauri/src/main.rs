#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::{Emitter, Manager};

mod commands;
mod config;
pub mod domain;
pub mod git;
#[cfg(target_os = "macos")]
mod menus;
mod persistence;
pub mod services;
mod terminal;

/// The level every log statement is kept at, in every build.
///
/// This is the only place the level is chosen, because neither `fern` nor `tauri-plugin-log` reads
/// `RUST_LOG` (checked in fern 0.7.1 and tauri-plugin-log 2.10.0). So a level that changed with the
/// build profile would mean a failure reported from a shipped build is logged under a filter nobody
/// debugged it under, with no way to raise it afterwards. Release is therefore exactly as verbose as
/// development, not quieter: this is the trade-off, and it buys a shipped failure being reproducible
/// without a rebuild.
///
/// It is `Debug` and not `Trace` because `Trace` is where the volume of this dependency tree is:
/// `notify` writes one line per filesystem event, and this app watches whole repositories with it,
/// and `tao` writes one every time it locks and unlocks a window's shared state. None of that
/// belongs in a file somebody attaches to a bug report. What is left at `Debug` is a handful of
/// call sites in the whole tree, and the ones Tauri has are the ones worth keeping: "web content
/// process terminated", a webview reload, an asset falling back to its index.
fn log_level() -> log::LevelFilter {
    log::LevelFilter::Debug
}

/// Bytes one rotated log file holds before the writer starts the next one.
///
/// The plugin's default is 40_000 bytes, which is about four hundred lines, and with the default
/// `KeepOne` the file those lines live in is deleted the moment the next one is written. A session
/// with a terminal in it crosses that in seconds, so the log a bug report needs is routinely gone
/// before anybody opens it. A megabyte is roughly ten thousand lines of what this app writes, and
/// still nothing next to the repository the app is pointed at.
fn log_max_file_size() -> u128 {
    1024 * 1024
}

/// Rotated log files kept beside the live one, so a failure that reproduced yesterday is still
/// there today.
///
/// `KeepOne` is the other default worth replacing: it deletes rather than renames, so the evidence
/// is gone the first time the file fills. `KeepAll` would keep it and grow without bound inside a
/// log directory a shipped app owns, which is its own bug. Three archives beside the live file is
/// about four megabytes and is what "yesterday" is worth here — but it is three *files*, not three
/// sessions: rotation is by size, so a quiet afternoon keeps days of history while one session with a
/// terminal and a line per agent request can fill all three and push the previous one out. The
/// evidence that survives is therefore the recent megabytes, not the recent sessions, and a bug
/// report is better attached before the next heavy run than after it.
fn log_rotated_files_kept() -> usize {
    3
}

/// Where the log is written: the log directory always, stdout while developing.
///
/// A packaged app has no console on its stdout — a release binary launched from Finder on macOS has
/// no terminal at all — so the `Stdout` target's lines go where nobody can read them, on every
/// launch, while still costing a write on every record. In development it is the other way round:
/// the app runs attached to a terminal, and that terminal is faster than opening the log directory.
/// This is the one part of the policy that depends on the build profile, and it only ever adds a
/// destination where somebody is already looking.
fn log_targets() -> Vec<tauri_plugin_log::Target> {
    let log_dir =
        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir { file_name: None });
    #[cfg(debug_assertions)]
    {
        vec![
            tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
            log_dir,
        ]
    }
    #[cfg(not(debug_assertions))]
    {
        vec![log_dir]
    }
}

/// The two setup steps that can stop the app before a window is usable, named the way the log and
/// the message read: a name of what was being done, not of what went wrong, so the cause carries
/// the reason and this carries the step.
const OPENING_THE_WORKSPACE_DATABASE: &str = "opening the workspace database";
const RECORDING_THE_HOME_CHECKOUT: &str = "recording the Home checkout";

/// One line for a startup failure, and the error the failure leaves through.
///
/// The plugin that writes the log is registered before `setup` runs, so this lands in the file a
/// bug report attaches rather than only on a stdout a packaged app does not have. It is written
/// here, at the step that failed, and the error is handed on unchanged afterwards: the refusal is
/// not the app's to swallow, and turning it into a warning is what made a refusal that ends the
/// launch invisible in the one file support asks for.
///
/// `cause` is what the failing step said, verbatim, and nothing is added to it: it already names
/// the schema versions the two builds speak, and repeating the database path or the user's
/// directories here would only put the same local layout in the log a second time.
fn startup_failure(step: &str, cause: &str) -> std::io::Error {
    let message = format!("Muster did not start: {step} failed: {cause}");
    log::error!("{message}");
    std::io::Error::other(message)
}

/// Registers the local MCP automation bridge. It only exists in debug builds
/// compiled with `--features dev-bridge`, so release builds never include it.
fn with_dev_plugins(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    #[cfg(all(debug_assertions, feature = "dev-bridge"))]
    {
        builder.plugin(tauri_plugin_mcp_bridge::init())
    }
    #[cfg(not(all(debug_assertions, feature = "dev-bridge")))]
    {
        builder
    }
}

/// Replaces the menu Tauri builds when it is given none, which on macOS is a File, a View, a
/// Window and a Help with nothing in it. It is replaced only on macOS: a menu set on Linux or
/// Windows draws a bar inside the window, and neither has one today.
fn with_menus(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    #[cfg(target_os = "macos")]
    {
        builder.menu(menus::build)
    }
    #[cfg(not(target_os = "macos"))]
    {
        builder
    }
}

fn main() {
    // The one plugin that has to come before every other, and before `setup`, because plugins run
    // in the order they are added. A second launch has to stop before it opens the database:
    // opening the database is what clears the stored terminal sessions, so two copies of the app
    // would each wipe the other's on the way in. What the second launch does instead is bring the
    // window that is already open back to the front.
    let builder =
        tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));
    let app = with_menus(with_dev_plugins(builder))
        // The policy is written out rather than left to `tauri_plugin_log`'s defaults, because
        // every one of those defaults is a third party's decision about a shipped app: a level of
        // whatever `fern` defaults to, a 40 KB file deleted on rotation, and stdout on a release
        // build that has no console attached to it. What the level and the two rotation numbers
        // are, and why, is on the constants above.
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log_level())
                // `ureq` is the one dependency in this tree that writes a line per unit of work
                // at `log_level()`: one per HTTP call to the OpenCode service, carrying the method,
                // the URL and the response's status and headers. That is the app's own traffic
                // written out one line at a time into the file people attach to bug reports. It
                // does not log bodies, and it redacts the headers it prints, but the URLs are not
                // wanted either way. `ureq_proto` is a second target and needs its own entry:
                // `fern` matches `level_for` on `::` boundaries and that crate is `ureq_proto`, not
                // `ureq::proto`, so without this line half of them come through anyway.
                .level_for("ureq", log::LevelFilter::Info)
                .level_for("ureq_proto", log::LevelFilter::Info)
                .max_file_size(log_max_file_size())
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(
                    log_rotated_files_kept(),
                ))
                .targets(log_targets())
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            use tauri::Manager;

            let data_dir = app.path().app_data_dir()?;
            let home = app.path().home_dir()?;
            let home_directory = services::workspace::HomeDirectory(std::fs::canonicalize(&home)?);
            let config_file = config::config_file(&home);
            std::fs::create_dir_all(&data_dir)?;
            // Both steps are logged on the way out rather than left to the panic the propagation
            // ends in: a database this build refuses to open, by name, is a question a person can
            // act on, and it has to reach the file support reads to be answered.
            let database = persistence::Database::open(data_dir.join("muster.sqlite3"))
                .map_err(|cause| startup_failure(OPENING_THE_WORKSPACE_DATABASE, &cause))?;
            database
                .set_home_checkout_id(crate::domain::workspace::checkout_id_for_path(
                    &home_directory.0.display().to_string(),
                ))
                .map_err(|cause| startup_failure(RECORDING_THE_HOME_CHECKOUT, &cause))?;
            if let Some(window) = app.get_webview_window("main") {
                if let Ok(Some(geometry)) = database.window_geometry() {
                    let geometry_is_visible = window.available_monitors().is_ok_and(|monitors| {
                        monitors.iter().any(|monitor| {
                            let monitor_position = monitor.position();
                            let monitor_size = monitor.size();
                            let left = i64::from(geometry.x).max(i64::from(monitor_position.x));
                            let top = i64::from(geometry.y).max(i64::from(monitor_position.y));
                            let right = (i64::from(geometry.x) + i64::from(geometry.width))
                                .min(i64::from(monitor_position.x) + i64::from(monitor_size.width));
                            let bottom = (i64::from(geometry.y) + i64::from(geometry.height)).min(
                                i64::from(monitor_position.y) + i64::from(monitor_size.height),
                            );
                            right - left >= 100 && bottom - top >= 100
                        })
                    });
                    if geometry_is_visible {
                        let _ = window
                            .set_size(tauri::PhysicalSize::new(geometry.width, geometry.height));
                        let _ = window
                            .set_position(tauri::PhysicalPosition::new(geometry.x, geometry.y));
                    }
                }
                if database.window_maximized().unwrap_or(false) {
                    let _ = window.maximize();
                }
                let tracked_window = window.clone();
                let window_database = database.clone();
                window.on_window_event(move |event| {
                    if !matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                        return;
                    }
                    let maximized = tracked_window.is_maximized().unwrap_or(false);
                    if !maximized {
                        if let (Ok(position), Ok(size)) =
                            (tracked_window.outer_position(), tracked_window.outer_size())
                        {
                            // The app is on its way out, so this is the only chance to keep the
                            // window where the user put it. Losing it silently costs them the
                            // placement on every launch after this one.
                            if let Err(error) =
                                window_database.set_window_geometry(persistence::WindowGeometry {
                                    x: position.x,
                                    y: position.y,
                                    width: size.width,
                                    height: size.height,
                                })
                            {
                                log::warn!("the window's size and place were not saved: {error}");
                            }
                        }
                    }
                    if let Err(error) = window_database.set_window_maximized(maximized) {
                        log::warn!("the window's maximized state was not saved: {error}");
                    }
                    // The window is not closed from here. The frontend is holding this request
                    // open to land its queued writes, and ending the app from under it would
                    // abandon the layout and the settings the user last chose. Closing the window
                    // is the frontend's own `close()` once those writes land, and the last window
                    // going away ends the app on every platform.
                });
            }
            app.manage(database);
            app.manage(home_directory);
            app.manage(config_file);
            app.manage(std::sync::Arc::new(terminal::TerminalBackend::default()));
            app.manage(std::sync::Arc::new(
                services::git::GitWatcherManager::default(),
            ));
            // The OpenCode service belongs to whoever started it, so nothing here starts or stops
            // one: `AgentService` connects to the service the user is already running and reports
            // itself disconnected while there is none.
            let agents = std::sync::Arc::new(services::agent::AgentService::new(home.clone()));
            let emitter = app.handle().clone();
            agents.set_event_sink(std::sync::Arc::new(
                move |event: services::agent::AgentEvent| {
                    // A dead window must not take the reader thread down with it.
                    let _ = emitter.emit("muster://agent-event", event);
                },
            ));
            app.manage(agents);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app::app_prepare_exit,
            commands::app::open_url,
            commands::app::frontend_diagnostic,
            commands::agent::agent_sessions,
            commands::agent::agent_candidate_sessions,
            commands::agent::agent_relocations,
            commands::agent::agent_agents,
            commands::agent::agent_session_create,
            commands::agent::agent_prompt,
            commands::agent::agent_stop,
            commands::folder::open_folder,
            commands::workspace::restore_workspace,
            commands::workspace::sync_workspace_repo,
            commands::workspace::register_folder,
            commands::workspace::list_recent_paths,
            commands::workspace::locate_missing_checkout,
            commands::workspace::close_missing_checkout,
            commands::workspace::close_checkout,
            commands::workspace::archive_checkout,
            commands::workspace::restore_archived_worktrees,
            commands::workspace::set_default_branch,
            commands::workspace::select_checkout,
            commands::workspace::select_session,
            commands::files::files_list,
            commands::files::file_read,
            commands::files::file_read_media,
            commands::files::file_probe,
            commands::files::file_open_externally,
            commands::files::file_write,
            commands::files::file_read_prettier_config,
            commands::files::review_export_markdown,
            commands::files::review_folder,
            commands::files::review_folder_clear,
            commands::files::file_read_markdown_image,
            commands::git::git_status,
            commands::git::git_diff_stats,
            commands::git::git_checkout_diff_stats,
            commands::git::git_diff,
            commands::git::git_diff_page,
            commands::git::git_viewed_files,
            commands::git::git_mark_viewed,
            commands::git::git_watch_repo,
            commands::git::git_unwatch_repo,
            commands::review::review_notes,
            commands::review::review_note_create,
            commands::review::review_note_update,
            commands::review::review_note_delete,
            commands::review::review_notes_mark_sent,
            commands::review::review_note_anchors_verify,
            commands::review::review_note_outdated_clear,
            commands::review::review_note_resolve,
            commands::review::review_rounds,
            commands::review::review_round_dispatch,
            commands::review::review_round_launch_pi,
            commands::review::review_round_settle_pi,
            commands::review::review_rounds_requeue,
            commands::review::review_round_flush,
            commands::review::review_round_reconcile,
            commands::review::review_round_ack,
            commands::worktree::worktree_defaults,
            commands::worktree::worktree_create,
            commands::worktree::worktree_removal_info,
            commands::worktree::worktree_remove,
            commands::terminal::terminal_create,
            commands::terminal::terminal_write,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_status,
            commands::terminal::terminal_close,
            commands::terminal::terminal_rename,
            commands::terminal::terminal_move,
            commands::terminal::terminal_layout_load,
            commands::terminal::terminal_layout_save,
            commands::ui_state::ui_layout_load,
            commands::ui_state::ui_layout_save,
            commands::ui_state::settings_load,
            commands::ui_state::settings_save,
            commands::ui_state::checkout_ui_state_load,
            commands::ui_state::checkout_ui_state_save,
            commands::ui_state::review_target_load,
            commands::ui_state::review_target_save
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Muster");
    app.run(|handle, event| {
        // Nothing below runs on its own: the loop ends with `std::process::exit`, so a `Drop` in
        // the managed state is never reached.
        //
        // A window close asks for the sweep itself, over the bridge, while the window is still
        // there to ask. This is the backstop for an exit that arrives without one, and it is not
        // the main path: an exit that does not come through the window is announced by
        // `ExitRequested`, and `Exit` is what is left when even that is skipped.
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            static STOPPED: std::sync::Once = std::sync::Once::new();
            STOPPED.call_once(|| {
                // The states are read rather than taken: an exit that arrives before `setup`
                // finished has neither of them managed, and panicking on the way out is worse
                // than leaving them be.
                let agents = handle.try_state::<std::sync::Arc<services::agent::AgentService>>();
                let terminal = handle.try_state::<std::sync::Arc<terminal::TerminalBackend>>();
                if let (Some(agents), Some(terminal)) = (agents, terminal) {
                    commands::app::stop_children(&agents, &terminal);
                }
            });
        }
    });
}

#[cfg(test)]
mod security_tests {
    use serde_json::Value;

    #[test]
    fn sec_05_webview_capabilities_are_domain_scoped_without_process_or_filesystem_privileges() {
        let capabilities: Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        let permissions = capabilities["permissions"].as_array().unwrap();
        let permissions = permissions
            .iter()
            .map(|permission| permission.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            permissions,
            [
                "core:default",
                "core:window:allow-close",
                // Tauri's `onCloseRequested` completes the allowed close by calling `destroy()`.
                "core:window:allow-destroy",
                // The title bar draws minimize on a window that has no frame of its own.
                "core:window:allow-minimize",
                "core:window:allow-start-dragging",
                // It draws the maximize control next to it, and the platform's own double-click
                // zoom reaches the window through the drag region, which is allowed above.
                "core:window:allow-toggle-maximize",
                "dialog:allow-open",
                "clipboard-manager:allow-write-text"
            ]
        );

        let cargo_manifest = include_str!("../Cargo.toml");
        assert!(!cargo_manifest.contains("tauri-plugin-shell"));
        assert!(!cargo_manifest.contains("tauri-plugin-fs"));
        let registered_commands = include_str!("main.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(!registered_commands.contains("shell::exec"));
        assert!(!registered_commands.contains("fs::read"));

        let config: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        for key in ["csp", "devCsp"] {
            let csp = config["app"]["security"][key].as_str().unwrap();
            assert!(!csp.contains("unsafe-eval"));
            assert!(!csp.contains("*"));
            assert!(csp.split(';').any(|d| d.trim() == "media-src blob:"));
            assert!(csp
                .split(';')
                .find(|d| d.trim().starts_with("img-src "))
                .unwrap()
                .split_whitespace()
                .any(|s| s == "blob:"));
        }

        // The whole Tauri API is reached through the `@tauri-apps/api` imports, so injecting it a
        // second way as a global on the window would be reachability nothing needs and anything
        // running in the document would have.
        assert!(
            !config["app"]
                .get("withGlobalTauri")
                .is_some_and(|enabled| enabled.as_bool() == Some(true)),
            "`withGlobalTauri` hands the webview a global copy of the API it does not use"
        );

        // The dev bridge is the one build that does need it, and it needs it completely: it drives
        // the webview through `window.__TAURI__`, so without the global the automation can still
        // read the DOM but can no longer reach the app. Dropping the global from the base config
        // without putting it back here is what broke it, and nothing in the app's own source says
        // so. It is merged in by the one script that cannot produce an installable build, which is
        // what keeps it out of every build a user runs.
        let dev_bridge: Value =
            serde_json::from_str(include_str!("../tauri.dev-bridge.conf.json")).unwrap();
        assert_eq!(
            dev_bridge["app"]["withGlobalTauri"],
            Value::Bool(true),
            "the dev bridge overlay is the only place the global comes back"
        );
        let scripts: Value = serde_json::from_str(include_str!("../../package.json")).unwrap();
        assert!(
            scripts["scripts"]["dev:app"]
                .as_str()
                .unwrap()
                .contains("tauri.dev-bridge.conf.json"),
            "the dev-bridge build is the one that gets the global"
        );
        assert!(
            !scripts["scripts"]["build:app"]
                .as_str()
                .unwrap()
                .contains("tauri.dev-bridge.conf.json"),
            "the build that ships must not get the global back"
        );
    }

    #[test]
    fn sec_05_2_the_only_write_the_webview_can_reach_is_contained_by_its_allowed_root() {
        // `sec_05` says what the webview may ask for. This says what it may change, which is the
        // question that only became one when the app started writing into the user's
        // repositories. It is numbered beside `sec_05` rather than on its own: `sec_06` is
        // already taken three times over by the ownership checks, and `sec_01_02` is the
        // precedent for a companion.
        //
        // Everything the webview can invoke is a domain command: the setup hook runs before any
        // of it and is not part of this surface.
        let registered_commands = include_str!("main.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        let handler = registered_commands
            .split("generate_handler![")
            .nth(1)
            .and_then(|block| block.split("])").next())
            .expect("the invoke handler is not a list of commands any more");
        for entry in handler
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            assert!(
                entry.starts_with("commands::"),
                "`{entry}` is reachable from the webview outside the domain commands"
            );
        }

        // And no command module reaches for the filesystem itself, outside its own tests: each one
        // names a `services::` entry instead, which is where containment lives. Read from disk
        // rather than listed here, so a module added later is covered without editing this test.
        let commands_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
        let mut modules: Vec<_> = std::fs::read_dir(&commands_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
            .collect();
        modules.sort();
        assert!(!modules.is_empty(), "no command module was found to check");
        for module in modules {
            let source = std::fs::read_to_string(&module).unwrap();
            let production = source.split("#[cfg(test)]").next().unwrap();
            for primitive in ["fs::", "File::", "OpenOptions", "Command::new"] {
                assert!(
                    !production.contains(primitive),
                    "{} reached for `{primitive}` instead of a service",
                    module.display()
                );
            }
        }

        // File writes name their origin and hand it to the service that resolves the path against
        // that origin's root. A `file_write` that grew a bare path, or that stopped going through
        // the service, is the escape.
        let write_command = include_str!("commands/files.rs")
            .split("#[tauri::command]")
            .find(|block| block.contains("pub async fn file_write("))
            .expect("file_write is not a command any more");
        assert!(write_command.contains("checkout_id: String"));
        assert!(write_command.contains("services::files::write"));

        // The checkout retains its guards, and the review root gets its own canonical containment
        // check rather than borrowing the checkout's.
        let file_service = include_str!("services/files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for guard in [
            "resolve_checkout_path",
            "parse_relative_path",
            "permissions.readonly()",
            "fs::set_permissions",
            "resolve_review_path",
            "canonical_path.starts_with(&canonical_root)",
        ] {
            assert!(
                file_service.contains(guard),
                "the write path stopped checking for `{guard}`"
            );
        }
    }

    /// The Linux window is the one the base config describes, without the frame.
    ///
    /// It has to be written out again because a platform config is merged as a patch and a
    /// patch replaces an array rather than merging into it, so `tauri.linux.conf.json` carries
    /// the whole window to turn one flag off. That is the one place the window can say two
    /// things at once, and a Linux build that quietly kept a stale width, minimum or colour
    /// would look like nothing at all from a macOS machine.
    #[test]
    fn sec_05_3_the_linux_window_is_the_base_window_without_its_frame() {
        let base: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let linux: Value = serde_json::from_str(include_str!("../tauri.linux.conf.json")).unwrap();
        let mut base_window = base["app"]["windows"][0].clone();
        let mut linux_window = linux["app"]["windows"][0].clone();

        assert_eq!(base_window["decorations"], Value::Bool(true));
        assert_eq!(linux_window["decorations"], Value::Bool(false));

        // The traffic lights are macOS's alone and are read on macOS alone, so the Linux
        // window does not carry them. Everything else about it is the window macOS gets.
        for key in ["decorations", "titleBarStyle", "hiddenTitle"] {
            base_window.as_object_mut().unwrap().remove(key);
            linux_window.as_object_mut().unwrap().remove(key);
        }
        assert_eq!(base_window, linux_window);
    }
}

#[cfg(test)]
mod startup_tests {
    /// The About panel carries the repository, and carries it as text macOS will draw.
    ///
    /// This one is a runtime assertion and not a grep over `menus.rs` on purpose. The first version
    /// of it looked for `credits: handle.config().bundle.homepage.clone()`, found it, passed, and
    /// shipped 0.3.1 with an About panel that named no repository: the codegen embedding the config
    /// into the binary hardcodes `bundle.homepage` to `None`, so the panel was handed `None` and
    /// muda left the credits out. A source match cannot tell a value that arrives from a value that
    /// is already `None`, so this builds a real handle and reads what the panel is given.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_about_panel_carries_the_repository() {
        use tauri::test::mock_app;

        use crate::menus;

        let app = mock_app();
        let about = menus::about_metadata(app.handle());

        assert_eq!(
            about.credits.as_deref(),
            Some("https://github.com/MaraniMatias/Muster"),
            "the About panel has no repository in it"
        );
        // The two lines above the credits. The mock runtime calls itself "test" and carries the
        // version of whatever it was built from, so only the shape is pinned here: a `None` in
        // either is what blanks those lines.
        assert!(about.name.is_some(), "the About panel has no name in it");
        assert!(
            about.version.is_some(),
            "the About panel has no version in it"
        );
    }

    /// Nothing on the panel is read off `config().bundle`.
    ///
    /// `ToTokens for BundleConfig` in tauri-utils builds the embedded config and hardcodes
    /// `homepage`, `publisher`, `copyright`, `category`, `targets` and `resources` to `None` or
    /// `default`, so every one of them reads as absent in a shipped app no matter what
    /// `tauri.conf.json` says. The test above would catch a panel that depends on one of those, so
    /// this only has to say where the value may not come from.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_about_panel_reads_no_field_the_config_codegen_drops() {
        let menus = include_str!("menus.rs");
        let code: String = menus
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !code.contains("config().bundle"),
            "the About panel is reading a field the config codegen does not embed"
        );
    }

    #[test]
    fn a_second_launch_is_stopped_before_the_database_is_opened() {
        // The single-instance plugin works by being registered first, and "first" is the whole of
        // it: the plugins run in the order they are added, and `setup` is where the database is
        // opened. Opening it is what clears the stored terminal sessions, so a second copy that got
        // that far would take the first copy's sessions with it and leave a set of terminals whose
        // PTYs belong to a process that is gone.
        let main = include_str!("main.rs");
        let registered = main.split("#[cfg(test)]").next().unwrap();
        let guard = registered
            .find("tauri_plugin_single_instance::init")
            .expect("the single-instance plugin is not registered at all");
        for later in [
            ".setup(",
            "tauri_plugin_log::Builder",
            "tauri_plugin_dialog::init",
            "tauri_plugin_clipboard_manager::init",
        ] {
            assert!(
                !registered[..guard].contains(later),
                "`{later}` is registered before the single-instance plugin, so a second launch \
                 reaches it before anything can turn it away"
            );
        }
    }

    /// The logging policy is written out in this file, not inherited from `tauri-plugin-log`.
    ///
    /// Every default in `Builder::new()` is a third party's decision about a shipped app, and none
    /// of them is visible in the source: the level is whatever `fern` defaults to, the log file is
    /// 40 KB and is deleted the moment the next one starts, and stdout is a target on a release
    /// build that has no console attached to it. So nothing in the app, and nothing in a review of
    /// it, says what the file a user attaches to a bug report actually holds. This says the three
    /// numbers are what they are and that the builder is the one given them, rather than functions
    /// standing on their own while the plugin keeps its defaults.
    ///
    /// None of the three can be read back out of a built logger: they are private on the plugin's
    /// builder, and the only function that hands a logger over (`split`) needs a real app handle
    /// and opens the log file to do it, which is not something a test should do in the log
    /// directory of whoever runs it. So this reads them where they are declared, and reads the
    /// registration itself from the source.
    #[test]
    fn the_logging_policy_is_written_out_and_not_inherited() {
        use crate::{log_level, log_max_file_size, log_rotated_files_kept};

        assert_eq!(
            log_level(),
            log::LevelFilter::Debug,
            "the level moved, and a failure reported from a shipped build is now logged under a \
             filter nobody debugged it under"
        );
        assert!(
            log_max_file_size() >= 1024 * 1024,
            "the log file is back under a megabyte, which is under four thousand lines of what \
             this app writes"
        );
        assert!(
            log_rotated_files_kept() >= 2,
            "rotation is deleting the previous session's log again, so a failure that reproduced \
             yesterday is gone before anybody reads it"
        );

        let main = include_str!("main.rs");
        let registered = main.split("#[cfg(test)]").next().unwrap();
        for given in [
            ".level(log_level())",
            ".max_file_size(log_max_file_size())",
            ".rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(",
            ".level_for(\"ureq\", log::LevelFilter::Info)",
            ".level_for(\"ureq_proto\", log::LevelFilter::Info)",
            ".targets(log_targets())",
        ] {
            assert!(
                registered.contains(given),
                "the log plugin stopped being given `{given}`, so that part of the policy is the \
                 library's default again"
            );
        }

        // And `stdout` is the one destination that may depend on the build, only in the direction
        // that adds a place where somebody is looking.
        let targets = registered
            .split("fn log_targets()")
            .nth(1)
            .and_then(|body| body.split("\n}").next())
            .expect("`log_targets` is not a function any more");
        assert!(
            targets.contains("TargetKind::LogDir"),
            "the log file is not a destination any more, so a release build has nowhere to write"
        );
        let (development, release) = targets
            .split_once("#[cfg(not(debug_assertions))]")
            .expect("a target list with no release branch");
        assert!(
            development.contains("#[cfg(debug_assertions)]"),
            "the target list does not depend on the build profile at all"
        );
        assert!(
            development.contains("TargetKind::Stdout"),
            "stdout is not a development destination any more, and the terminal the app runs under \
             is the fastest way to read it"
        );
        assert!(
            !release.contains("TargetKind::Stdout"),
            "a release build writes to stdout, which is nowhere a person running the app can read"
        );
    }

    /// A startup failure says what was being done and why, and leaves through an error that
    /// carries both.
    ///
    /// The refusal to open a database stamped with another schema is the one startup failure with
    /// a known answer, and it used to reach nobody: `setup` propagated it and the process ended in
    /// the abort that follows, so the log file support asks for never held the reason. The reason
    /// is written where the app can be read, once, on the way out.
    ///
    /// The error is returned rather than logged and dropped, and it carries the same text: a
    /// startup that is swallowed is a startup nobody is told about either.
    #[test]
    fn a_startup_failure_is_named_before_it_is_propagated() {
        use crate::{startup_failure, OPENING_THE_WORKSPACE_DATABASE};

        let refusal = "the workspace database is schema version 13 and this build speaks version \
                       14: update by deleting the workspace database and opening the app again";
        let error = startup_failure(OPENING_THE_WORKSPACE_DATABASE, refusal);

        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        let reported = error.to_string();
        assert!(
            reported.contains(OPENING_THE_WORKSPACE_DATABASE),
            "the failure does not name the step that failed: {reported}"
        );
        assert!(
            reported.contains(refusal),
            "the failure does not carry the cause it was given: {reported}"
        );

        // Both steps go through it. `Database::open` refusing the file is the case this exists for,
        // and a second step added to `setup` afterwards has to say why it failed the same way
        // instead of handing a bare `io::Error::other` to the abort.
        let main = include_str!("main.rs");
        let registered = main.split("#[cfg(test)]").next().unwrap();
        let setup = registered
            .split(".setup(|app| {")
            .nth(1)
            .and_then(|body| body.split("\n        .invoke_handler").next())
            .expect("`setup` is not a block any more");
        assert!(
            setup.contains("startup_failure(OPENING_THE_WORKSPACE_DATABASE, &cause)"),
            "the database is opened without logging why it could not be opened"
        );
        assert!(
            setup.contains("startup_failure(RECORDING_THE_HOME_CHECKOUT, &cause)"),
            "the initialization step after it fails without saying so in the log"
        );
        assert!(
            !setup.contains("std::io::Error::other"),
            "a startup failure is propagated without being written to the log first"
        );
    }

    /// Every terminal is stopped from the event loop, and this app's own event readers are detached,
    /// because there is no other way to stop them.
    ///
    /// `App::run` never returns: the loop ends and the process is handed to `std::process::exit`, so a
    /// `Drop` in the managed state is never reached. A `Drop` in the managed state was written for an
    /// exit that does not happen, which is how a shell outlived every single quit of the app.
    #[test]
    fn the_exit_hook_ends_what_the_app_started() {
        let main = include_str!("main.rs");
        let registered = main.split("#[cfg(test)]").next().unwrap();
        assert!(
            !registered.contains(".run(tauri::generate_context!())"),
            "`App::run` leaves through `std::process::exit`, so nothing is unwound and the \
             children are stopped by nothing at all"
        );
        // The window is what ends the app, so the window is what asks for the sweep. Neither exit
        // event reaches the app on that path, which is measured, not assumed: a marker written by
        // the hook is absent after the window closes while the process ends with status 0.
        assert!(
            registered.contains("commands::app::app_prepare_exit"),
            "the command the window asks for the sweep with is not registered"
        );
        assert!(
            registered.contains("tauri::RunEvent::ExitRequested"),
            "an exit that does not come through the window is announced by `ExitRequested`, and \
             nothing sweeps when only `Exit` is watched"
        );
        assert!(
            !registered.contains("exit(0)"),
            "ending the app while the close request is being handled takes the webview away from \
             the write the window is still waiting for, and skips the sweep entirely"
        );
        let command = include_str!("commands/app.rs");
        for call in ["agents.stop_all()", "terminal.shutdown()"] {
            assert!(command.contains(call), "the sweep stopped calling `{call}`");
        }
        // The states are read rather than taken: an exit that arrives before `setup` finished has
        // neither of them managed, and panicking on the way out is worse than leaving it be.
        assert!(
            registered.matches("try_state").count() == 2,
            "the backstop reads the managed state instead of unwrapping it"
        );
    }

    /// Nothing here starts or stops an OpenCode service, because the person using the app owns it.
    ///
    /// This is what makes the app usable alongside someone else's OpenCode already running: the service
    /// registers itself, this app connects to it, and every path out of the app leaves it alone. A
    /// sweep for servers to end, or an `exit(0)` from inside a close request, would end the service out
    /// from under whatever is still running it.
    #[test]
    fn the_app_never_starts_or_ends_an_opencode_service() {
        let main = include_str!("main.rs");
        let registered = main.split("#[cfg(test)]").next().unwrap();
        let command = include_str!("commands/app.rs");
        let bridge = include_str!("services/agent.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();

        for forbidden in ["\"serve\"", "reap_orphaned_servers", "orphan"] {
            assert!(
                !bridge.contains(forbidden),
                "a bridge no longer owns an OpenCode process, but it still refers to `{forbidden}`"
            );
        }
        assert!(
            !registered.contains("reap_orphaned_servers"),
            "the app must not search the process table for services to end"
        );
        // The service a person started outlives the app, so the sweep before exit only disconnects
        // this app's own reader and stops the terminals it started.
        assert!(
            command.contains("agents.stop_all()"),
            "the sweep is what closes this app's side of the connection"
        );
        assert!(
        !registered.contains("exit(0)"),
        "ending the app while a close request is being handled takes the webview away from the \
         write the window is still waiting for"
    );
    }

    /// The service registers itself under the user's home, and the app already resolved that once.
    ///
    /// Discovery reads the home it is given rather than the environment, so where the service is
    /// looked for is decided where the app resolves its other per-user paths.
    #[test]
    fn the_service_is_found_under_the_users_own_home() {
        let main = include_str!("main.rs");
        let registered = main.split("#[cfg(test)]").next().unwrap();
        assert!(
            registered.contains("AgentService::new(home.clone())"),
            "the service registration is looked for under a home the app did not resolve"
        );
    }
    #[test]
    fn the_menu_bar_is_the_app_menu_and_the_one_macos_requires() {
        // Tauri builds a menu of its own when the builder is given none, so the way to have a
        // small menu bar is not to have no menu code. It is to have menu code that says what
        // stays, and these are the two submenus that stay.
        let menus = include_str!("menus.rs");
        assert_eq!(
            menus.matches("Submenu::with_items(").count(),
            2,
            "the menu bar is not the app menu and Edit any more"
        );
        for item in [
            // The app menu, named after the product. macOS reads the first submenu as the
            // application menu, and an app that cannot be hidden, put away or quit from it is
            // not an app.
            "handle.package_info().name.clone()",
            "PredefinedMenuItem::about",
            "PredefinedMenuItem::services",
            "PredefinedMenuItem::hide(",
            "PredefinedMenuItem::hide_others",
            "PredefinedMenuItem::quit",
            // Edit, and every one of these is load-bearing rather than conventional. macOS never
            // tells the webview about `⌘C`, `⌘V`, `⌘X`, `⌘A` or `⌘Z`; the key equivalents on
            // these items send `copy:` and the rest down the responder chain into it. Drop `copy`
            // and the search field, the editor and the terminal silently stop copying, and the
            // right-click copy menu does not stand in for it.
            r#""Edit""#,
            "PredefinedMenuItem::undo",
            "PredefinedMenuItem::redo",
            "PredefinedMenuItem::cut",
            "PredefinedMenuItem::copy",
            "PredefinedMenuItem::paste",
            "PredefinedMenuItem::select_all",
        ] {
            assert!(
                menus.contains(item),
                "the menu bar stopped carrying `{item}`"
            );
        }

        // And the About panel names the repository. macOS draws the name, the version, the short
        // version, the copyright, an icon and the credits, and no `website`, so the credits are
        // the only line of text in there that can say where the source is. Dropped, the panel
        // says what the app is called and nothing about where to get it or read it.
        assert!(
            menus.contains(r#"credits: Some(env!("CARGO_PKG_HOMEPAGE").to_string())"#),
            "the About panel stopped naming the repository"
        );

        // And it is set, on macOS only. A menu set on Linux or Windows would draw a bar inside
        // the window, which is a second place for the app's menus to live.
        let main = include_str!("main.rs");
        let registered = main.split("#[cfg(test)]").next().unwrap();
        assert!(
            registered.contains("with_menus(with_dev_plugins(builder))"),
            "the menu is built and then never handed to the app"
        );
        let helper = registered
            .split("fn with_menus(")
            .nth(1)
            .and_then(|body| body.split("\n}").next())
            .expect("`with_menus` is not a function any more");
        assert!(
            helper.contains(r#"#[cfg(target_os = "macos")]"#),
            "the menu is set on every platform, and only macOS has a menu bar"
        );
    }

    #[test]
    fn the_window_opens_on_the_page_background_and_not_on_a_white_frame() {
        // A webview with no background color of its own paints white until the page covers it, and
        // wry only stops it doing so when one is set. So the window opens on the dark palette's
        // page background and the app opens on `--muster-bg-0`, and the difference is a frame the
        // user sees on every launch. Setting it is not enough on its own: the value lives in a
        // config file and the color it has to match lives in a stylesheet, and nothing makes the
        // two follow each other when the palette changes. This is where that is said out loud.
        //
        // One color for one window, so this can only be one of the two palettes: dark is the one
        // because it is what the `:root` block is, and a person whose preference is `light` sees
        // that frame for as long as the webview takes to paint the page over it.
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let page_background = config["app"]["windows"][0]["backgroundColor"]
            .as_str()
            .expect("the window has no `backgroundColor`, so it opens on a white frame")
            .to_lowercase();
        let stylesheet = include_str!("../../src/muster.css");
        assert!(
            stylesheet.contains(&format!("--muster-bg-0: {page_background};")),
            "the window opens on {page_background} but the page is painted on something else, so \
             the frame before the first paint is a different color than the app"
        );
    }
}
