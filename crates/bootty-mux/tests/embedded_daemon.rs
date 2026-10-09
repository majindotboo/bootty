#![cfg(unix)]

use std::{
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt as _,
    process::{Child, Command, Stdio},
    sync::{OnceLock, mpsc},
    thread,
};

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_config::ApplicationIdentity;
use bootty_mux::rmux::{
    RemoteRmuxRequest, RmuxBackend, endpoint_path_for, run_remote_rmux_command,
};
use bootty_mux::{MuxBackendKind, MuxBindingConfig};
use bootty_mux::{
    backend::{MuxBackend as _, PaneCapture, PaneInput, PaneText},
    command::{MuxCommand, MuxDirection, MuxSplitDirection},
    provider::MuxBackendRegistry,
    snapshot::{MuxSessionTag, new_session_identity},
    terminal::ActiveTerminal,
};
use bootty_terminal::geometry::TerminalGeometry;
use bootty_terminal::terminal_engine::TerminalEngine;
use bootty_terminal::{frame_source::TerminalFrameSource, terminal_session::TerminalSessionConfig};
use tokio::runtime::Builder;

const SCENARIO_ENV: &str = "BOOTTY_RMUX_EMBEDDED_SCENARIO";
const SCENARIO_CHILD_TEST: &str = "embedded_rmux_scenario_child";
const REMOTE_STREAM_PAYLOAD_ENV: &str = "BOOTTY_RMUX_REMOTE_STREAM_PAYLOAD";
const REMOTE_STREAM_CHILD_TEST: &str = "embedded_rmux_remote_pane_stream_child";
const ISOLATED_PATH: &str = "/usr/bin:/bin";
const POSIX_SHELL: &str = "/bin/sh";
/// What a prepared pane prints back. No scenario prints it for another reason.
const PANE_READY: &str = "BOOTTY_PANE_READY";
const PANE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const PANE_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

fn start_embedded_rmux_daemon_for_tests() -> Result<()> {
    static STARTED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| {
            let socket = endpoint_path_for(ApplicationIdentity::Production)
                .map_err(|error| error.to_string())?;
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            thread::spawn(move || {
                let started_tx = ready_tx.clone();
                let result = (|| -> Result<()> {
                    let runtime = Builder::new_multi_thread().enable_all().build()?;
                    runtime.block_on(async {
                        let daemon =
                            rmux_server::ServerDaemon::new(rmux_server::DaemonConfig::new(socket))
                                .bind()
                                .await?;
                        let _ = started_tx.send(Ok(()));
                        daemon.wait().await
                    })?;
                    Ok(())
                })();
                if let Err(error) = result {
                    let _ = ready_tx.send(Err(error.to_string()));
                }
            });
            ready_rx.recv().map_err(|error| error.to_string())?
        })
        .clone()
        .map_err(anyhow::Error::msg)
}

/// Every scenario below drives bootty's public rmux behaviors through the SDK
/// only. Each gets its own `#[test]`, so a failure names the behavior that
/// broke and the test runner spends the wall clock of the slowest scenario
/// rather than the sum of all of them.
///
/// The scenario itself runs in a child process: the pane environment (`PATH`,
/// `SHELL`, `RMUX_TMPDIR`) is process-global, and a child is the only way to
/// set it without racing every other thread in the runner.
macro_rules! embedded_scenarios {
    ($($name:ident),+ $(,)?) => {
        $(
            #[test]
            fn $name() -> Result<()> {
                run_embedded_scenario(stringify!($name))
            }
        )+

        fn dispatch_embedded_scenario(name: &str) -> Result<()> {
            match name {
                $(stringify!($name) => scenario::$name(),)+
                unknown => anyhow::bail!("unknown embedded rmux scenario: {unknown}"),
            }
        }
    };
}

embedded_scenarios!(
    recovery_keeps_pending_cursor_escape,
    session_lifecycle,
    saved_session_topology,
    direct_respawn_keeps_pane_identity_and_literal_argv,
    hidden_restored_window_checkpoint_keeps_styled_history,
    explicit_create_runs_argv_and_never_reuses_a_name,
    a_hidden_session_takes_pane_io_through_the_backend,
    pane_navigation_and_zoom,
    terminal_requests,
    kitty_keyboard_protocol_reports_command_alt_key,
    terminal_queries_do_not_leak_into_the_shell,
    rmux_does_not_claim_the_system_tmux_server,
    kitty_keyboard_protocol_pop_restores_legacy_ctrl_c,
    multi_pane_window_resize_keeps_pane_targets_live,
    visible_windows_keep_independent_resize_requests,
    remote_window_resize_request_reaches_the_rmux_owner,
    remote_pane_stream_rebase_publishes_a_frame,
    closing_session_with_pending_resize_is_quiet,
    closed_pane_accepts_teardown_updates,
    shell_exit_is_quiet,
    bounded_live_output,
    kitty_images_reach_terminal_frames,
    closing_reader_during_large_output_keeps_other_reader_live,
    large_restore_progress,
);

/// The child-process entry point. A no-op in the runner's own process.
#[test]
fn embedded_rmux_scenario_child() -> Result<()> {
    if let Some(endpoint) = std::env::var_os("BOOTTY_RMUX_PIPE_ENDPOINT") {
        return bootty_mux::rmux::run_pipe_helper(endpoint.into());
    }
    let Some(scenario) = std::env::var_os(SCENARIO_ENV) else {
        return Ok(());
    };
    dispatch_embedded_scenario(&scenario.to_string_lossy())
}

#[test]
fn embedded_rmux_remote_pane_stream_child() -> Result<()> {
    let Some(payload) = std::env::var_os(REMOTE_STREAM_PAYLOAD_ENV) else {
        return Ok(());
    };
    run_remote_rmux_command(&payload.to_string_lossy())?;
    Ok(())
}

mod scenario {
    use super::*;
    use pretty_assertions::{assert_eq, assert_ne};

    pub fn recovery_keeps_pending_cursor_escape() -> Result<()> {
        for (pending, continuation) in [
            ("\\033", "[3;1H"),
            ("\\033[3;", "1H"),
            ("\\033]10;rgb:aa", "aa/bbbb/cccc\\033\\\\\\033[3;1H"),
        ] {
            let (mut backend, registry, session_id, window_id, pane) =
                create_embedded_session(unscoped_tag())?;
            let mut original = open_terminal(std::sync::Arc::clone(&registry), &pane, &window_id)?;
            prepare_pane(&mut original)?;
            original.write_input(format!("printf '\\033[?1049h\\033[>5u\\033[2J\\033[1;1HOld compaction\\033[3;1HCursor Images\\033[1;1H{pending}'; read go; printf '{continuation}\\033[2K\\033[1;1H\\033[2KUpdated transcript'; read hold\r").as_bytes())?;
            wait_for_terminal_text(&mut original, "Cursor Images")?;
            let mut restored = open_terminal(registry, &pane, &window_id)?;
            wait_for_terminal_text(&mut restored, "Cursor Images")?;
            original.write_input(b"\r")?;
            wait_for_terminal_text(&mut restored, "Updated transcript")?;
            wait_for_terminal_text(&mut original, "Updated transcript")?;
            let frame = restored.extract_frame()?;
            let reference = original.extract_frame()?;
            let text = frame.text.iter().collect::<String>();
            ditch_session(&mut backend, &session_id)?;
            anyhow::ensure!(
                !text.contains("Cursor Images"),
                "recovery left stale text: {text:?}"
            );
            assert_eq!(frame.text, reference.text);
            assert_eq!(frame.colors.foreground, reference.colors.foreground);
            assert_eq!(frame.colors.background, reference.colors.background);
        }
        Ok(())
    }

    /// An explicit create hands its argv to the first pane untouched and refuses a taken name
    /// without re-stamping the session that has it.
    pub fn explicit_create_runs_argv_and_never_reuses_a_name() -> Result<()> {
        start_embedded_rmux_daemon_for_tests()?;
        let directory = std::env::var_os("RMUX_TMPDIR").context("isolated scenario directory")?;
        let output = std::path::Path::new(&directory).join("argv");
        let arguments = ["a;", "multi\nline 'single' \"double\" $HOME", "", "-x"]
            .map(str::to_owned)
            .to_vec();
        let argv = [
            POSIX_SHELL.to_owned(),
            "-c".to_owned(),
            "printf '%s\\0' \"$@\" > \"$0\"; exec sleep 600".to_owned(),
            output.to_string_lossy().into_owned(),
        ]
        .into_iter()
        .chain(arguments.iter().cloned())
        .collect();
        let session_id = format!("bootty-mux-argv-{}", std::process::id());
        let tag = unscoped_tag();
        let mut backend = RmuxBackend::new();
        backend.execute(MuxCommand::CreateProjectSession {
            session_id: session_id.clone(),
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            tag: tag.clone(),
            argv: Some(argv),
        })?;

        let fields = |text: &str| -> Vec<String> {
            text.strip_suffix('\0')
                .unwrap_or(text)
                .split('\0')
                .map(str::to_owned)
                .collect()
        };
        let read = || std::fs::read_to_string(&output).unwrap_or_default();
        let started = std::time::Instant::now();
        while started.elapsed() < PANE_TIMEOUT {
            let text = read();
            if text.ends_with('\0') && fields(&text).len() == arguments.len() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(fields(&read()), arguments);

        let duplicate = backend.execute(MuxCommand::CreateProjectSession {
            session_id: session_id.clone(),
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            tag: unscoped_tag(),
            argv: Some(Vec::new()),
        });
        anyhow::ensure!(duplicate.is_err(), "a taken name must be refused");
        let tags = backend
            .snapshot()?
            .sessions
            .into_iter()
            .filter(|session| session.id == session_id)
            .map(|session| session.tag)
            .collect::<Vec<_>>();
        assert_eq!(tags, [tag], "the existing session keeps its stamp");
        ditch_session(&mut backend, &session_id)
    }

    /// A detached session nothing ever attached takes input and capture through the backend by
    /// its pane id, locally and through the remote daemon's entry point. A paste is bracketed
    /// because the program asked for it, with each LF sent as CR, like tmux's `paste-buffer -p`.
    pub fn a_hidden_session_takes_pane_io_through_the_backend() -> Result<()> {
        start_embedded_rmux_daemon_for_tests()?;
        let directory = std::env::var_os("RMUX_TMPDIR").context("isolated scenario directory")?;
        let received = std::path::Path::new(&directory).join("received");
        let session_id = format!("bootty-mux-hidden-{}", std::process::id());
        let mut backend = RmuxBackend::new();
        backend.execute(MuxCommand::CreateProjectSession {
            session_id: session_id.clone(),
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            tag: unscoped_tag(),
            argv: Some(vec![
                POSIX_SHELL.to_owned(),
                "-c".to_owned(),
                "printf '\\033[?2004hhidden-ready'; exec cat > \"$0\"".to_owned(),
                received.to_string_lossy().into_owned(),
            ]),
        })?;
        let pane = backend
            .snapshot()?
            .sessions
            .into_iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.windows.first()?.panes.first()?.pane_id.clone())
            .context("the hidden session's pane")?;
        let screen = PaneCapture {
            history: false,
            max_lines: 100,
            ansi: false,
        };
        let started = std::time::Instant::now();
        let ready = loop {
            let capture = backend.capture_pane(&pane, screen)?;
            if capture.text.contains("hidden-ready") || started.elapsed() > PANE_TIMEOUT {
                break capture;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        };
        anyhow::ensure!(ready.text.contains("hidden-ready"), "{ready:?}");
        assert_eq!((ready.captured_lines, ready.omitted_lines), (24, 0));
        let last_row = backend.capture_pane(
            &pane,
            PaneCapture {
                max_lines: 1,
                ..screen
            },
        )?;
        assert_eq!((last_row.captured_lines, last_row.omitted_lines), (1, 23));

        for input in [
            PaneInput::Paste("one\ntwo".to_owned()),
            PaneInput::Write(b" three".to_vec()),
            PaneInput::Submit,
        ] {
            backend.send_pane_input(&pane, &input)?;
        }
        let expected = "\u{1b}[200~one\ntwo\u{1b}[201~ three\n";
        let started = std::time::Instant::now();
        while std::fs::read_to_string(&received).unwrap_or_default() != expected
            && started.elapsed() < PANE_TIMEOUT
        {
            thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(std::fs::read_to_string(&received)?, expected);

        // The remote daemon reads the same request from stdin and runs it with this backend.
        let request = bootty_mux::remote_space::PaneRequest::Capture {
            pane,
            capture: screen,
        };
        let mut child = Command::new(std::env::current_exe()?)
            .args(["--exact", REMOTE_STREAM_CHILD_TEST, "--nocapture"])
            .env(REMOTE_STREAM_PAYLOAD_ENV, RemoteRmuxRequest::Pane.encode()?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        std::io::Write::write_all(
            &mut child.stdin.take().context("remote request stdin")?,
            &serde_json::to_vec(&request)?,
        )?;
        let output = child.wait_with_output()?;
        anyhow::ensure!(output.status.success(), "remote pane request failed");
        let remote = String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| serde_json::from_str::<PaneText>(line).ok())
            .context("the remote daemon answered the capture")?;
        anyhow::ensure!(remote.text.contains("hidden-ready"), "{remote:?}");
        ditch_session(&mut backend, &session_id)
    }

    pub fn session_lifecycle() -> Result<()> {
        let tag = MuxSessionTag {
            identity: Some(new_session_identity()),
            space: Some("space-under-test".to_owned()),
        };
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(tag.clone())?;

        let snapshot = backend.snapshot()?;
        let session = snapshot
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .context("created rmux session")?;
        assert_eq!(session.tag, tag, "rmux reports the tag bootty stamped");

        let mut terminal = open_terminal(std::sync::Arc::clone(&registry), &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        terminal.write_input(b"printf 'BOOTTY_RMUX_FRAME\\n'\r")?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_FRAME")?;

        let mut second_terminal = open_terminal(registry, &pane, &window_id)?;
        wait_for_terminal_text(&mut second_terminal, "BOOTTY_RMUX_FRAME")?;

        drop(terminal);
        second_terminal.write_input(b"printf 'BOOTTY_RMUX_SECOND_READER\\n'\r")?;
        wait_for_terminal_text(&mut second_terminal, "BOOTTY_RMUX_SECOND_READER")?;

        let endpoint = endpoint_path_for(ApplicationIdentity::Production)?;
        let rmux_root = endpoint.parent().context("embedded rmux endpoint parent")?;
        let spool_files = std::fs::read_dir(rmux_root)?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().starts_with("bootty-rmux-output-"))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            spool_files.is_empty(),
            "live rmux output created unbounded disk spools: {spool_files:?}"
        );

        backend.execute(MuxCommand::DitchSession {
            session_id: session_id.clone(),
        })?;
        let snapshot = backend.snapshot()?;
        anyhow::ensure!(
            !snapshot
                .sessions
                .iter()
                .any(|session| session.id == session_id)
        );
        Ok(())
    }

    pub fn direct_respawn_keeps_pane_identity_and_literal_argv() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let directory = assert_fs::TempDir::new()?;
        let cwd = directory.path().join("#{pane_current_path}; literal");
        std::fs::create_dir(&cwd)?;
        let cwd = cwd.canonicalize()?;
        let output = directory.path().join("resumed-argv");
        let before = backend.snapshot()?;
        let literals = [
            "a;",
            "$(literal)",
            "multi\nline 'single' \"double\" $HOME",
            "",
            "-x",
            "last;",
        ];
        let mut argv = vec![POSIX_SHELL.to_owned(), "-c".to_owned(),
            "printf '%s\\0' \"$PWD\" \"$@\" > \"$0\"; printf 'DIRECT_RESUME_READY\\n'; exec /bin/cat".to_owned(),
            output.to_string_lossy().into_owned()];
        argv.extend(literals.map(str::to_owned));
        backend.respawn_pane_command(
            pane.pane_id.as_deref().context("pane")?,
            &argv,
            Some(&cwd.to_string_lossy()),
        )?;
        let mut terminal = open_terminal_with_window_config(
            std::sync::Arc::clone(&registry),
            std::slice::from_ref(&pane),
            &pane,
            &window_id,
            TerminalSessionConfig {
                max_scrollback: 1024,
                restored_history: Some(std::sync::Arc::from(
                    "\x1b[38;2;210;80;170mSAVED_AGENT_HISTORY\x1b[0m\n",
                )),
                ..Default::default()
            },
        )?;
        wait_for_terminal_text(&mut terminal, "DIRECT_RESUME_READY")?;
        let expected = std::iter::once(cwd.to_string_lossy().into_owned())
            .chain(literals.map(str::to_owned))
            .collect::<Vec<_>>()
            .join("\0")
            + "\0";
        assert_eq!(std::fs::read_to_string(output)?, expected);
        let after = backend.snapshot()?;
        let shape = |snapshot: &bootty_mux::snapshot::MuxSnapshot| {
            snapshot
                .sessions
                .iter()
                .map(|session| {
                    (
                        session.id.clone(),
                        session
                            .windows
                            .iter()
                            .map(|window| {
                                (
                                    window.id.clone(),
                                    window
                                        .panes
                                        .iter()
                                        .map(|pane| pane.pane_id.clone())
                                        .collect::<Vec<_>>(),
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(&before), shape(&after));
        check_direct_resume_history(&mut terminal)?;
        drop(terminal);
        check_late_resume_history(registry, &pane, &window_id)?;
        ditch_session(&mut backend, &session_id)
    }

    fn check_direct_resume_history(terminal: &mut ActiveTerminal) -> Result<()> {
        for cols in [96, 80] {
            terminal.resize_native_layout_window(cols, 24)?;
            let marker = format!("AFTER_RESIZE_{cols}");
            terminal.write_input(format!("{marker}\n").as_bytes())?;
            wait_for_terminal_text(terminal, &marker)?;
            let captured = terminal
                .capture(bootty_terminal::terminal_capture::CaptureOptions {
                    scope: bootty_terminal::terminal_capture::CaptureScope::History,
                    ..Default::default()
                })?
                .receive("history after resize")?
                .map_err(anyhow::Error::msg)?;
            assert_eq!(captured.text.matches("SAVED_AGENT_HISTORY").count(), 1);
            anyhow::ensure!(captured.text.contains("DIRECT_RESUME_READY"));
        }
        terminal.write_input(b"AFTER_DIRECT_RESUME\n")?;
        wait_for_terminal_text(terminal, "AFTER_DIRECT_RESUME")?;
        let capture = terminal
            .capture(bootty_terminal::terminal_capture::CaptureOptions {
                scope: bootty_terminal::terminal_capture::CaptureScope::History,
                format: bootty_terminal::terminal_capture::CaptureFormat::Ansi,
                ..Default::default()
            })?
            .receive("retained styled history")?
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            capture.text.contains("SAVED_AGENT_HISTORY"),
            "respawn lost restored history: {:?}",
            capture.text
        );
        anyhow::ensure!(
            capture.text.contains("38;2;210;80;170"),
            "respawn lost saved colors"
        );
        anyhow::ensure!(
            capture.text.find("SAVED_AGENT_HISTORY") < capture.text.find("DIRECT_RESUME_READY"),
            "saved history must precede fresh output"
        );
        Ok(())
    }

    fn check_late_resume_history(
        registry: std::sync::Arc<MuxBackendRegistry>,
        pane: &bootty_mux::snapshot::MuxPaneAnchor,
        window_id: &str,
    ) -> Result<()> {
        // Normal panes can receive saved history after their first frame; resize retains it.
        let mut terminal = open_terminal_with_window_config(
            registry,
            std::slice::from_ref(pane),
            pane,
            window_id,
            TerminalSessionConfig {
                max_scrollback: 1024,
                ..Default::default()
            },
        )?;
        wait_for_terminal_text(&mut terminal, "DIRECT_RESUME_READY")?;
        terminal.restore_history("\x1b[38;2;210;80;170mLATE_SAVED_HISTORY\x1b[0m\n")?;
        terminal.resize_native_layout_window(96, 24)?;
        terminal.write_input(b"AFTER_LATE_RESTORE\n")?;
        wait_for_terminal_text(&mut terminal, "AFTER_LATE_RESTORE")?;
        let capture = terminal
            .capture(bootty_terminal::terminal_capture::CaptureOptions {
                scope: bootty_terminal::terminal_capture::CaptureScope::History,
                format: bootty_terminal::terminal_capture::CaptureFormat::Ansi,
                ..Default::default()
            })?
            .receive("late restored history")?
            .map_err(anyhow::Error::msg)?;
        assert_eq!(capture.text.matches("LATE_SAVED_HISTORY").count(), 1);
        anyhow::ensure!(capture.text.contains("38;2;210;80;170"));
        anyhow::ensure!(capture.text.contains("DIRECT_RESUME_READY"));
        drop(terminal);
        Ok(())
    }

    pub fn hidden_restored_window_checkpoint_keeps_styled_history() -> Result<()> {
        use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig};
        use bootty_mux::repository::WorkspaceRepository;
        start_embedded_rmux_daemon_for_tests()?;
        let directory = assert_fs::TempDir::new()?;
        let mut config = BoottyConfig {
            config_path: directory.path().join("config.toml"),
            ..Default::default()
        };
        config.multiplexer.backend = MultiplexerBackendConfig::Rmux;
        let (mut workspace, scope) = restore_hidden_workspace(&config, directory.path())?;
        let binding = workspace.binding(scope).context("restored Binding")?;
        let generation = binding.mux().binding_generation();
        let session = binding
            .session_attachment("saved-logical")
            .context("restored task")?
            .clone();
        assert_eq!(session.windows.len(), 2);
        let hidden = session
            .windows
            .first()
            .context("hidden first window")?
            .panes
            .first()
            .and_then(|pane| pane.pane_id.clone())
            .context("hidden pane")?;
        anyhow::ensure!(
            workspace.space_terminal_runtime(scope, &hidden).is_none(),
            "must exercise capture before hidden reader admission"
        );
        let backend = RmuxBackend::new();
        backend.send_pane_input(
            &hidden,
            &PaneInput::Write(b"printf '%s%s\\n' 'FRESH_' 'HIDDEN_OUTPUT'\r".to_vec()),
        )?;
        let started = std::time::Instant::now();
        while !backend
            .capture_pane(
                &hidden,
                PaneCapture {
                    history: true,
                    ansi: true,
                    max_lines: 10000,
                },
            )?
            .text
            .contains("FRESH_HIDDEN_OUTPUT")
        {
            anyhow::ensure!(
                started.elapsed() < PANE_TIMEOUT,
                "hidden shell produced no output"
            );
            thread::yield_now();
        }
        let captures = capture_hidden_restored_panes(&workspace, scope, &session, &hidden)?;
        let receipt = workspace
            .prepare_session_checkpoint(scope, "saved-logical", generation, 2)?
            .save(captures)?;
        anyhow::ensure!(workspace.publish_session_checkpoint(receipt)?);
        let (_, reloaded) = WorkspaceRepository::open(&config.config_path)?;
        let persisted = reloaded
            .spaces()
            .first()
            .context("reloaded Space")?
            .binding()
            .sessions()
            .get("saved-logical")
            .and_then(|session| session.terminal_snapshot.as_ref())
            .context("reloaded checkpoint")?;
        assert_eq!(persisted.windows.len(), 2);
        let text = &persisted
            .windows
            .first()
            .context("persisted first window")?
            .panes
            .first()
            .context("persisted hidden pane")?
            .text;
        anyhow::ensure!(
            text.contains("HIDDEN_COLD_HISTORY_a") && text.contains("FRESH_HIDDEN_OUTPUT")
        );
        let mut backend = RmuxBackend::new();
        ditch_session(&mut backend, &session.id)
    }

    fn restore_hidden_workspace(
        config: &bootty_config::config::BoottyConfig,
        cwd: &std::path::Path,
    ) -> Result<(
        bootty_mux::workspace::WorkspaceRuntime,
        bootty_mux::controller::SpaceId,
    )> {
        use bootty_config::config::AppearanceVariant;
        use bootty_mux::{
            controller::CommandSelection,
            executor,
            repository::WorkspaceRepository,
            session_membership::{SessionMembership, SessionState, WorkspaceSession},
            workspace::WorkspaceRuntime,
        };
        let (mut repository, spaces) = WorkspaceRepository::open(&config.config_path)?;
        let scope = spaces.spaces().first().context("test Space")?.id();
        let mut saved = saved_terminal_snapshot("absent-source", &cwd.to_string_lossy());
        for pane in saved
            .windows
            .iter_mut()
            .flat_map(|window| &mut window.panes)
        {
            use std::fmt::Write as _;
            let mut text = String::new();
            for index in 0..1500 {
                writeln!(text, "\x1b[38;2;210;80;170mretained row {index:04}\x1b[0m")?;
            }
            writeln!(
                text,
                "\x1b[38;2;210;80;170mHIDDEN_COLD_HISTORY_{}\x1b[0m",
                pane.id
            )?;
            pane.text = text;
        }
        repository.commit_binding_state(
            scope,
            &SessionMembership::from_sessions(vec![WorkspaceSession {
                identity: saved.session_id.clone(),
                backend_name: "absent-source".into(),
                display_name: "Cold history".into(),
                explicit: true,
                cwd: cwd.to_string_lossy().into_owned(),
                state: SessionState::default(),
                terminal_snapshot: Some(std::sync::Arc::new(saved)),
            }]),
        )?;
        let repaint: bootty_mux::RepaintHandle = std::sync::Arc::new(|| {});
        let registry = std::sync::Arc::new(MuxBackendRegistry::desktop()?);
        let mut workspace = WorkspaceRuntime::open(
            config,
            "main",
            registry,
            AppearanceVariant::Dark,
            std::sync::Arc::clone(&repaint),
        )?;
        let (command, membership) = workspace
            .begin_session_reopen(scope, "saved-logical")
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let pending = executor::submit_authoritative_command_for_scope(
            &mut workspace,
            &repaint,
            scope,
            command,
            membership.map(Box::new),
            None,
            CommandSelection::Follow,
        )
        .context("restore submitted")?;
        let result = pending.result.recv_timeout(PANE_TIMEOUT)?;
        let (result, sync_error) = executor::complete_authoritative_command(
            &mut workspace,
            scope,
            &pending.command,
            pending.membership.as_deref(),
            result,
            pending.layout.as_ref(),
        )?;
        result.map_err(|error| anyhow::anyhow!("{error}"))?;
        anyhow::ensure!(
            sync_error.is_none(),
            "restore renderer sync: {sync_error:?}"
        );
        Ok((workspace, scope))
    }

    fn capture_hidden_restored_panes(
        workspace: &bootty_mux::workspace::WorkspaceRuntime,
        scope: bootty_mux::controller::SpaceId,
        session: &bootty_mux::snapshot::MuxSession,
        hidden: &str,
    ) -> Result<Vec<bootty_mux::session_snapshot::SessionPaneCapture>> {
        use bootty_mux::session_snapshot::SessionPaneCapture;
        use bootty_terminal::terminal_capture::{CaptureFormat, CaptureOptions, CaptureScope};
        let options = CaptureOptions {
            scope: CaptureScope::History,
            format: CaptureFormat::Ansi,
            max_bytes: 12 * 1024,
            ..Default::default()
        };
        let mut captures = Vec::new();
        for pane in session.windows.iter().flat_map(|window| &window.panes) {
            let id = pane.pane_id.as_deref().context("capture pane")?;
            let (sender, receiver) = mpsc::channel();
            workspace
                .binding(scope)
                .context("capture Binding")?
                .capture_checkpoint_pane(id, options, move |result| {
                    let _ = sender.send(result);
                });
            let capture = receiver
                .recv_timeout(PANE_TIMEOUT)?
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            if id == hidden {
                assert_eq!(capture.text.matches("HIDDEN_COLD_HISTORY_a").count(), 1);
                anyhow::ensure!(capture.text.contains("38;2;210;80;170"));
                anyhow::ensure!(capture.text.contains("FRESH_HIDDEN_OUTPUT"));
                anyhow::ensure!(
                    capture.text.find("HIDDEN_COLD_HISTORY_a")
                        < capture.text.find("FRESH_HIDDEN_OUTPUT")
                );
                anyhow::ensure!(capture.text.len() <= options.max_bytes);
                anyhow::ensure!(
                    capture.omitted_lines > 0,
                    "large old history must retain a bounded recent tail"
                );
            }
            captures.push(SessionPaneCapture {
                pane_id: id.to_owned(),
                cwd: pane.cwd.clone(),
                cols: capture.cols,
                rows: capture.rows,
                text: capture.text,
                omitted_lines: capture.omitted_lines,
            });
        }
        Ok(captures)
    }

    pub fn saved_session_topology() -> Result<()> {
        let (mut backend, _registry, original_id, _, _) = create_embedded_session(unscoped_tag())?;
        let directory = assert_fs::TempDir::new()?;
        let literal_directory = directory.path().join("#{pane_current_path}");
        std::fs::create_dir(&literal_directory)?;
        let cwd = literal_directory
            .canonicalize()?
            .to_string_lossy()
            .into_owned();
        let saved = saved_terminal_snapshot(&original_id, &cwd);
        let tag = MuxSessionTag {
            identity: Some("saved-logical".into()),
            space: Some("saved-space".into()),
        };
        let restored_name = format!("restored-{}", std::process::id());
        backend.execute(MuxCommand::RestoreSession {
            session_id: restored_name.clone(),
            tag: tag.clone(),
            snapshot: saved.clone(),
        })?;
        let snapshot = backend.snapshot()?;
        let restored = snapshot
            .sessions
            .iter()
            .find(|session| session.id == restored_name)
            .context("restored session")?;
        assert_eq!(restored.tag, tag);
        verify_saved_terminal_topology(restored, &cwd)?;
        let missing_cwd = directory
            .path()
            .join("absent")
            .to_string_lossy()
            .into_owned();
        let partial_name = verify_saved_restore_rejections(
            &mut backend,
            &restored_name,
            tag,
            saved,
            &missing_cwd,
        )?;
        let snapshot = backend.snapshot()?;
        anyhow::ensure!(
            !snapshot
                .sessions
                .iter()
                .any(|session| session.id == partial_name),
            "partial restore session survived rollback"
        );
        anyhow::ensure!(
            snapshot
                .sessions
                .iter()
                .any(|session| session.id == original_id),
            "rollback removed the original unrelated session"
        );
        anyhow::ensure!(
            snapshot
                .sessions
                .iter()
                .any(|session| session.id == restored_name),
            "rollback removed the existing restored session"
        );
        ditch_session(&mut backend, &restored_name)?;
        ditch_session(&mut backend, &original_id)
    }

    fn verify_saved_restore_rejections(
        backend: &mut RmuxBackend,
        restored_name: &str,
        tag: MuxSessionTag,
        saved: bootty_mux::session_snapshot::SavedTerminalSession,
        missing_cwd: &str,
    ) -> Result<String> {
        let snapshot = backend.snapshot()?;
        let first_pane = snapshot
            .sessions
            .iter()
            .find(|session| session.id == restored_name)
            .context("restore before invalid pane cwd")?
            .windows
            .first()
            .context("first restored window")?
            .panes
            .first()
            .context("first restored pane")?;
        require_rejected_command(
            backend,
            MuxCommand::CreatePane {
                session_id: restored_name.to_owned(),
                pane_id: first_pane.pane_id.clone(),
                direction: MuxSplitDirection::Down,
                cwd: Some(missing_cwd.to_owned()),
                argv: Vec::new(),
            },
            "a missing cwd created a pane in a fallback directory",
        )?;
        let retained = backend.snapshot()?;
        assert_eq!(
            retained
                .sessions
                .iter()
                .find(|session| session.id == restored_name)
                .context("retained restore after invalid pane cwd")?
                .windows
                .first()
                .context("retained first window")?
                .panes
                .len(),
            2
        );
        // An explicit retry cannot adopt the existing restore or change its saved identity.
        require_rejected_command(
            backend,
            MuxCommand::RestoreSession {
                session_id: restored_name.to_owned(),
                tag: tag.clone(),
                snapshot: saved.clone(),
            },
            "existing restore name was adopted",
        )?;
        assert_eq!(
            backend
                .snapshot()?
                .sessions
                .iter()
                .find(|session| session.id == restored_name)
                .context("retained restore")?
                .tag,
            tag
        );
        let mut invalid = saved;
        let invalid_cwd = &mut invalid
            .windows
            .get_mut(1)
            .context("saved second window")?
            .panes
            .first_mut()
            .context("saved second-window pane")?
            .cwd;
        missing_cwd.clone_into(invalid_cwd);
        let partial_name = format!("partial-{}", std::process::id());
        require_rejected_command(
            backend,
            MuxCommand::RestoreSession {
                session_id: partial_name.clone(),
                tag,
                snapshot: invalid,
            },
            "invalid cwd did not reject partial restore",
        )?;
        Ok(partial_name)
    }

    fn require_rejected_command(
        backend: &mut RmuxBackend,
        command: MuxCommand,
        failure: &str,
    ) -> Result<()> {
        anyhow::ensure!(backend.execute(command).is_err(), "{failure}");
        Ok(())
    }

    fn saved_terminal_snapshot(
        original_id: &str,
        cwd: &str,
    ) -> bootty_mux::session_snapshot::SavedTerminalSession {
        use bootty_mux::session_snapshot::{
            SavedTerminalPane, SavedTerminalSession, SavedTerminalWindow,
        };
        use bootty_mux::snapshot::{MuxPaneLayout, MuxPaneSplitDirection};
        let pane = |id: &str| SavedTerminalPane {
            native_agent: Some(format!("native:codex:{id}")),
            id: id.into(),
            backend_id: id.into(),
            cwd: cwd.to_owned(),
            cols: 80,
            rows: 24,
            text: "plain history belongs to the renderer".into(),
            omitted_lines: 0,
        };
        SavedTerminalSession {
            captured_at: 1,
            session_id: "saved-logical".into(),
            backend_id: original_id.to_owned(),
            active_window_id: Some("second".into()),
            windows: vec![
                SavedTerminalWindow {
                    id: "first".into(),
                    backend_id: "first".into(),
                    title: "saved first".into(),
                    focused_pane_id: "b".into(),
                    panes: vec![pane("a"), pane("b")],
                    layout: Some(MuxPaneLayout::Split {
                        direction: MuxPaneSplitDirection::Right,
                        ratio_millis: 333,
                        first: Box::new(MuxPaneLayout::Pane("a".into())),
                        second: Box::new(MuxPaneLayout::Pane("b".into())),
                    }),
                },
                SavedTerminalWindow {
                    id: "second".into(),
                    backend_id: "second".into(),
                    title: "saved second".into(),
                    focused_pane_id: "c".into(),
                    panes: vec![pane("c")],
                    layout: None,
                },
            ],
        }
    }

    fn verify_saved_terminal_topology(
        restored: &bootty_mux::snapshot::MuxSession,
        cwd: &str,
    ) -> Result<()> {
        use bootty_mux::snapshot::{MuxPaneLayout, MuxPaneSplitDirection};
        assert_eq!(
            restored
                .windows
                .iter()
                .map(|window| window.name.as_str())
                .collect::<Vec<_>>(),
            vec!["saved first", "saved second"]
        );
        assert_eq!(
            restored
                .windows
                .iter()
                .map(|window| window.panes.len())
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(
            restored
                .windows
                .iter()
                .flat_map(|window| &window.panes)
                .map(|pane| pane.native_agent.as_deref())
                .collect::<Vec<_>>(),
            [
                Some("native:codex:a"),
                Some("native:codex:b"),
                Some("native:codex:c")
            ]
        );
        let first = restored.windows.first().context("first restored window")?;
        let second_window = restored.windows.get(1).context("second restored window")?;
        let first_pane = first.panes.first().context("first restored pane")?;
        let second_pane = first.panes.get(1).context("second restored pane")?;
        assert_eq!(first.anchor.pane_id, second_pane.pane_id);
        let MuxPaneLayout::Split {
            direction,
            ratio_millis,
            first: first_layout,
            second,
        } = first.layout.as_ref().context("saved split")?
        else {
            anyhow::bail!("saved split was dropped")
        };
        assert_eq!(direction, &MuxPaneSplitDirection::Right);
        anyhow::ensure!(
            ratio_millis.abs_diff(333) <= 5,
            "saved split ratio changed: {ratio_millis}"
        );
        assert_eq!(
            first_layout.as_ref(),
            &MuxPaneLayout::Pane(first_pane.pane_id.clone().context("first id")?)
        );
        assert_eq!(
            second.as_ref(),
            &MuxPaneLayout::Pane(second_pane.pane_id.clone().context("second id")?)
        );
        assert_eq!(
            restored.active_window_id.as_deref(),
            Some(second_window.id.as_str())
        );
        for window in &restored.windows {
            for pane in &window.panes {
                assert_eq!(pane.cwd.as_deref(), Some(cwd));
            }
        }
        Ok(())
    }

    pub fn pane_navigation_and_zoom() -> Result<()> {
        let (mut backend, _registry, session_id, window_id, _pane) =
            create_embedded_session(unscoped_tag())?;
        let initial = active_pane_id(&backend, &session_id)?;
        backend.execute(MuxCommand::SplitPane {
            session_id: session_id.clone(),
            pane_id: Some(initial),
            direction: MuxSplitDirection::Down,
        })?;

        let after_split = pane_ids(&backend, &session_id)?;
        assert_eq!(after_split.len(), 2, "split created a second rmux pane");
        let active_after_split = active_pane_id(&backend, &session_id)?;

        backend.execute(MuxCommand::SelectNextPane {
            session_id: session_id.clone(),
            window_id: Some(window_id.clone()),
        })?;
        let after_next = active_pane_id(&backend, &session_id)?;
        assert_ne!(
            after_next, active_after_split,
            "next pane changed the active pane"
        );

        backend.execute(MuxCommand::SelectPreviousPane {
            session_id: session_id.clone(),
            window_id: Some(window_id.clone()),
        })?;
        assert_eq!(active_pane_id(&backend, &session_id)?, active_after_split);

        backend.execute(MuxCommand::SelectPane {
            session_id: session_id.clone(),
            window_id: Some(window_id),
            direction: if after_split.first() == Some(&active_after_split) {
                MuxDirection::Down
            } else {
                MuxDirection::Up
            },
        })?;
        assert_ne!(active_pane_id(&backend, &session_id)?, active_after_split);

        backend.execute(MuxCommand::TogglePaneZoom {
            session_id: session_id.clone(),
            pane_id: None,
        })?;
        backend.execute(MuxCommand::TogglePaneZoom {
            session_id: session_id.clone(),
            pane_id: None,
        })?;

        ditch_session(&mut backend, &session_id)
    }

    pub fn terminal_requests() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry, &pane, &window_id)?;

        terminal.enter_copy_mode()?;
        anyhow::ensure!(terminal.copy_mode_active()?);
        let outcome = terminal.handle_copy_mode_action(
            bootty_terminal::terminal_engine::TerminalCopyModeAction::Cancel,
        )?;
        anyhow::ensure!(!outcome.active);
        assert_eq!(
            terminal.format_selection(
                bootty_terminal::terminal_engine::TerminalSelectionFormat::PlainText
            )?,
            None
        );
        anyhow::ensure!(!terminal.search_viewport(
            "",
            bootty_terminal::terminal_engine::TerminalSearchDirection::Current,
        )?);
        terminal.is_mouse_tracking()?;
        terminal.discard_pending_output()?;

        ditch_session(&mut backend, &session_id)
    }

    pub fn terminal_queries_do_not_leak_into_the_shell() -> Result<()> {
        // The round trip needs a program that can put the pane in raw mode and read
        // its own stdin. Skip rather than spending the whole `wait_for_terminal_text`
        // deadline on a shell that printed "command not found", which would take
        // every scenario chained after this one down with it.
        if !std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        {
            eprintln!("skipping terminal_queries_do_not_leak_into_the_shell: no python3 on PATH");
            return Ok(());
        }
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry.clone(), &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        // Reattach: delayed terminal replies used to cross this boundary and
        // land on the shell after the querying process exited.
        drop(terminal);
        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        // Run from a file: inlining the script would put its backslash escapes
        // through the pane shell's quoting rules.
        let script_path =
            std::env::temp_dir().join(format!("bootty-terminal-query-{session_id}.py"));
        std::fs::write(
            &script_path,
            r#"import os, select, sys, termios, tty

fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
os.write(fd, b"\x1b[?u\x1b[c")
data = b""
# RMUX owns the pane PTY and answers DA1 synchronously. Consuming that answer
# models Crossterm's support probe, which then returns terminal ownership to the
# shell. Bootty must not inject a second, delayed response batch afterwards.
while b"c" not in data:
    if not select.select([fd], [], [], 2.0)[0]:
        break
    data += os.read(fd, 1)
os.write(fd, b"\x1b]11;?\x1b\\")
colour = b""
while b"\x1b\\" not in colour:
    if not select.select([fd], [], [], 2.0)[0]:
        break
    colour += os.read(fd, 1)
os.write(fd, b"\x1b[14t\x1b[16t")
size = b""
while b"\x1b[4;480;800t" not in size or b"\x1b[6;20;10t" not in size:
    if not select.select([fd], [], [], 2.0)[0]:
        break
    size += os.read(fd, 1)
os.write(fd, b"\x1b[?1016h\x1b[?1016$p")
pixel_mouse = b""
while b"\x1b[?1016;1$y" not in pixel_mouse:
    if not select.select([fd], [], [], 2.0)[0]:
        break
    pixel_mouse += os.read(fd, 1)
os.write(fd, b"\x1b[?1016l")
os.write(fd, b"\x1bPtmux;\x1b\x1b_Gi=4207,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\x1b\\\x1b\\")
graphics = b""
while b"\x1b_Gi=4207;OK\x1b\\" not in graphics:
    if not select.select([fd], [], [], 2.0)[0]:
        break
    graphics += os.read(fd, 1)
termios.tcsetattr(fd, termios.TCSADRAIN, saved)
if b"c" not in data:
    sys.exit("rmux did not answer DA1")
if b"rgb:" not in colour:
    sys.exit("Bootty did not answer OSC 11")
if b"\x1b[4;480;800t" not in size or b"\x1b[6;20;10t" not in size:
    sys.exit("Bootty did not answer XTWINOPS pixel size queries")
if b"\x1b[?1016;1$y" not in pixel_mouse:
    sys.exit("Bootty did not answer the SGR pixel mouse mode query")
if b"\x1b_Gi=4207;OK\x1b\\" not in graphics:
    sys.exit("Bootty did not answer the tmux-wrapped Kitty graphics probe")
print("BOOTTY_RMUX_COLOR_QUERY_OK")
    "#,
        )?;
        terminal.write_input(
            format!(
                "stty echo; python3 {}; sleep 1; printf '%s%s\\n' 'BOOTTY_RMUX_QUERY' '_CLEAN'\r",
                script_path.display()
            )
            .as_bytes(),
        )?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_QUERY_CLEAN")?;
        terminal.drain_pty();
        let frame = terminal.extract_frame()?.text.iter().collect::<String>();
        anyhow::ensure!(
            frame.contains("BOOTTY_RMUX_COLOR_QUERY_OK")
                && !frame.contains("?0u")
                && !frame.contains("rgb:")
                && !frame.contains("4;480;800t")
                && !frame.contains("6;20;10t")
                && !frame.contains("Gi=4207")
                && !frame.contains("62;22;52c"),
            "terminal replies leaked into the resumed shell: {frame:?}"
        );
        let _ = std::fs::remove_file(&script_path);

        ditch_session(&mut backend, &session_id)
    }

    pub fn rmux_does_not_claim_the_system_tmux_server() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        terminal.write_input(
            b"printf 'BOOTTY_RMUX_ENV TMUX=<%s> RMUX=<%s>\\n' \"$TMUX\" \"$RMUX\"\r",
        )?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_ENV TMUX=<> RMUX=<")?;
        let frame = terminal.extract_frame()?.text.iter().collect::<String>();
        anyhow::ensure!(
            !frame.contains("TMUX=</"),
            "rmux pane claimed a tmux server: {frame:?}"
        );

        ditch_session(&mut backend, &session_id)
    }

    pub fn kitty_keyboard_protocol_pop_restores_legacy_ctrl_c() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(std::sync::Arc::clone(&registry), &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        // Push the kitty flags and pop them straight back off. Nothing queries
        // the state: the answer would arrive as pane input and land on the next
        // command line.
        terminal.write_input(
            b"printf '\\033[0m\\033[>1u\\033[<1u'; printf 'BOOTTY_RMUX_KEYBOARD_STATE\\n'\r",
        )?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_KEYBOARD_STATE")?;
        drop(terminal);

        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        // Prove the reattached terminal is live before it has to carry a key.
        terminal.write_input(b"printf 'BOOTTY_RMUX_CTRL_C_PANE_LIVE\\n'\r")?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_CTRL_C_PANE_LIVE")?;
        terminal.write_input(
            b"/bin/sh -c 'trap \"printf BOOTTY_RMUX_CTRL_C_HANDLED\\\\n; exit 0\" INT; printf BOOTTY_RMUX_CTRL_C_READY\\n; while :; do read line; done'; stty echo; printf '\\101\\102\\103\\n'\r",
        )?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_CTRL_C_READY")?;
        terminal.encode_key(bootty_terminal::terminal_input_model::KeyInput {
            key: bootty_terminal::terminal_input_model::TerminalKey::C,
            mods: bootty_terminal::terminal_input_model::KeyMods {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("c"),
            unshifted: Some('c'),
        })?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_CTRL_C_HANDLED")?;
        wait_for_terminal_text(&mut terminal, "ABC")?;

        ditch_session(&mut backend, &session_id)
    }

    pub fn kitty_keyboard_protocol_reports_command_alt_key() -> Result<()> {
        if !std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        {
            eprintln!(
                "skipping kitty_keyboard_protocol_reports_command_alt_key: no python3 on PATH"
            );
            return Ok(());
        }
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        let script_path =
            std::env::temp_dir().join(format!("bootty-kitty-keyboard-{session_id}.py"));
        std::fs::write(
            &script_path,
            r#"import os, select, sys, termios, tty

fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
os.write(fd, b"\x1b[>7u\x1b[?u\x1b[cBOOTTY_RMUX_KITTY_READY\r\n")
data = b""
expected = b"\x1b[98;11u"
while expected not in data:
    if not select.select([fd], [], [], 10.0)[0]:
        break
    data += os.read(fd, 4096)
termios.tcsetattr(fd, termios.TCSADRAIN, saved)
if expected in data:
    print("BOOTTY_RMUX_COMMAND_ALT_KEY_OK")
"#,
        )?;
        terminal.write_input(format!("python3 {}\r", script_path.display()).as_bytes())?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_KITTY_READY")?;
        terminal.encode_key(bootty_terminal::terminal_input_model::KeyInput {
            key: bootty_terminal::terminal_input_model::TerminalKey::B,
            mods: bootty_terminal::terminal_input_model::KeyMods {
                alt: true,
                command: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("b"),
            unshifted: Some('b'),
        })?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_COMMAND_ALT_KEY_OK")?;
        let _ = std::fs::remove_file(&script_path);

        ditch_session(&mut backend, &session_id)
    }

    pub fn multi_pane_window_resize_keeps_pane_targets_live() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        backend.execute(MuxCommand::SplitPane {
            session_id: session_id.clone(),
            pane_id: Some(pane.pane_id.context("split pane id")?),
            direction: MuxSplitDirection::Down,
        })?;
        let snapshot = backend.snapshot()?;
        let (panes, focused) = snapshot
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.windows.first())
            .map(|window| {
                let focused = window
                    .panes
                    .iter()
                    .find(|pane| pane.pane_id == window.anchor.pane_id)
                    .or_else(|| window.panes.first())
                    .cloned();
                (window.panes.clone(), focused)
            })
            .context("split rmux window")?;
        let focused = focused.context("split rmux pane")?;
        let mut terminal = open_terminal_with_window(registry, &panes, &focused, &window_id)?;

        for _ in 0..8 {
            terminal.write_input(b"\x7f")?;
        }
        terminal.drain_pty();
        terminal.extract_frame()?;

        for cols in [96, 104, 112, 120] {
            terminal.resize_native_layout_window(cols, 30)?;
            terminal.drain_pty();
        }
        terminal.resize_native_layout_window(128, 30)?;

        ditch_session(&mut backend, &session_id)
    }

    pub fn visible_windows_keep_independent_resize_requests() -> Result<()> {
        use bootty_mux::terminal::{BackendPanePolicy, PaneLayoutResizeRequest};
        use std::sync::Arc;

        let (mut backend, _, session_id, first_id, _) = create_embedded_session(unscoped_tag())?;
        backend.execute(MuxCommand::NewWindow {
            session_id: session_id.clone(),
            cwd: None,
            argv: None,
        })?;
        let second_id = backend
            .snapshot()?
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.windows.iter().find(|window| window.id != first_id))
            .context("second window")?
            .id
            .clone();
        let (tx, rx) = mpsc::channel();
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = tx.send(());
        });
        let mut policy = bootty_mux::rmux::RmuxPanePolicy::new(None);
        for cols in 90..106 {
            for (id, rows) in [(&first_id, 25), (&second_id, 35)] {
                policy.resize_layout_window(PaneLayoutResizeRequest {
                    window_id: Some(id),
                    cols,
                    rows,
                    repaint_wakeup: &repaint,
                })?;
            }
        }
        let endpoint = endpoint_path_for(ApplicationIdentity::Production)?;
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let mut sizes = None;
        for _ in 0..32 {
            rx.recv_timeout(PANE_TIMEOUT)
                .context("window resize did not finish")?;
            sizes = Some(runtime.block_on(async {
                let rmux =
                    rmux_sdk::Rmux::connect(rmux_sdk::RmuxEndpoint::UnixSocket(endpoint.clone()))
                        .await?;
                let name = rmux_sdk::SessionName::new(&session_id).map_err(anyhow::Error::msg)?;
                let session = rmux.session(name).await?;
                let mut info = session.window(0).info().await?;
                info.windows.extend(session.window(1).info().await?.windows);
                let size = |id: &str| {
                    info.windows
                        .iter()
                        .find(|window| window.id.to_string() == id)
                        .map(|window| window.size)
                        .context("visible window missing")
                };
                anyhow::Ok((size(&first_id)?, size(&second_id)?))
            })?);
            if sizes
                == Some((
                    rmux_sdk::TerminalSizeSpec::new(105, 25),
                    rmux_sdk::TerminalSizeSpec::new(105, 35),
                ))
            {
                break;
            }
        }
        assert_eq!(
            sizes,
            Some((
                rmux_sdk::TerminalSizeSpec::new(105, 25),
                rmux_sdk::TerminalSizeSpec::new(105, 35)
            ))
        );
        ditch_session(&mut backend, &session_id)
    }

    pub fn remote_window_resize_request_reaches_the_rmux_owner() -> Result<()> {
        let (mut backend, _registry, session_id, window_id, _pane) =
            create_embedded_session(unscoped_tag())?;

        let request = RemoteRmuxRequest::ResizeWindow {
            window: window_id.clone(),
            cols: 101,
            rows: 31,
        };
        run_remote_rmux_command(&request.encode()?)?;

        let endpoint = endpoint_path_for(ApplicationIdentity::Production)?;
        let reported_size = Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let rmux =
                    rmux_sdk::Rmux::connect(rmux_sdk::RmuxEndpoint::UnixSocket(endpoint)).await?;
                let session_name =
                    rmux_sdk::SessionName::new(&session_id).map_err(anyhow::Error::msg)?;
                let info = rmux.session(session_name).await?.window(0).info().await?;
                info.windows
                    .into_iter()
                    .find(|window| window.id.to_string() == window_id)
                    .map(|window| window.size)
                    .context("resized rmux window was not found")
            })?;
        assert_eq!(reported_size, rmux_sdk::TerminalSizeSpec::new(101, 31));

        ditch_session(&mut backend, &session_id)
    }

    pub fn remote_pane_stream_rebase_publishes_a_frame() -> Result<()> {
        let (_backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        terminal.write_input(b"printf 'BOOTTY_RMUX_REMOTE_REBASE\\n'\r")?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_REMOTE_REBASE")?;

        let request = RemoteRmuxRequest::PaneStream {
            session: session_id,
            pane: pane.pane_id.context("remote pane id")?,
        };
        let (mut stream, frames) = spawn_remote_pane_stream(request.encode()?)?;
        let result = (|| -> Result<()> {
            let RemotePaneStreamFrame::Rebase(keyframe) = next_remote_pane_stream_frame(&frames)?
            else {
                anyhow::bail!("remote pane stream sent bytes before its initial rebase")
            };

            let mut frame = TerminalEngine::new(TerminalGeometry {
                cols: 80,
                rows: 24,
                cell_width: 10,
                cell_height: 20,
            })?;
            // A rebase is the authoritative reset-and-reconstruct transition.
            frame.write_vt_without_pty_responses(&keyframe);
            frame.scroll_viewport_bottom();
            anyhow::ensure!(
                frame
                    .extract_frame()?
                    .text_rows()
                    .iter()
                    .any(|row| row.contains("BOOTTY_RMUX_REMOTE_REBASE")),
                "initial remote rebase did not reconstruct the pane frame"
            );

            terminal.write_input(b"printf 'BOOTTY_RMUX_REMOTE_BYTES\\n'\r")?;
            loop {
                match next_remote_pane_stream_frame(&frames)? {
                    RemotePaneStreamFrame::End => {
                        anyhow::bail!("remote pane ended before new output")
                    }
                    RemotePaneStreamFrame::Rebase(_) => {
                        anyhow::bail!("remote pane stream rebased before delivering new pane bytes")
                    }
                    RemotePaneStreamFrame::Bytes(bytes) => {
                        frame.write_vt(&bytes);
                        let rows = frame.extract_frame()?.text_rows();
                        if rows
                            .iter()
                            .any(|row| row.contains("BOOTTY_RMUX_REMOTE_BYTES"))
                        {
                            anyhow::ensure!(
                                rows.iter()
                                    .any(|row| row.contains("BOOTTY_RMUX_REMOTE_REBASE")),
                                "remote pane bytes replaced the rebase frame"
                            );
                            break;
                        }
                    }
                }
            }
            terminal.write_input(b"\x04")?;
            loop {
                if matches!(
                    next_remote_pane_stream_frame(&frames)?,
                    RemotePaneStreamFrame::End
                ) {
                    return Ok(());
                }
            }
        })();

        let _ = stream.kill();
        let _ = stream.wait();
        result
    }

    pub fn closing_session_with_pending_resize_is_quiet() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry, &pane, &window_id)?;

        terminal.resize_native_layout_window(96, 30)?;
        backend.execute(MuxCommand::DitchSession { session_id })?;

        for cols in [100, 104, 108, 112] {
            terminal.resize_native_layout_window(cols, 30)?;
            terminal.drain_pty();
            terminal.extract_frame()?;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Ok(())
    }

    pub fn closed_pane_accepts_teardown_updates() -> Result<()> {
        use bootty_mux::terminal::TerminalRuntime as _;
        use bootty_terminal::geometry::CellMetrics;
        use bootty_terminal::terminal_capture::CaptureOptions;

        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        backend.execute(MuxCommand::ClosePane {
            session_id,
            pane_id: pane.pane_id,
        })?;

        let deadline = std::time::Instant::now()
            .checked_add(PANE_TIMEOUT)
            .context("pane deadline")?;
        while !terminal.child_exited()? {
            anyhow::ensure!(std::time::Instant::now() < deadline, "pane did not close");
            thread::yield_now();
        }
        // A request must fail once the worker has finished, never fabricate a result.
        while terminal.discard_pending_output().is_ok() {
            anyhow::ensure!(std::time::Instant::now() < deadline, "worker did not stop");
            thread::yield_now();
        }

        terminal.encode_focus(false)?;
        terminal.resize(TerminalGeometry {
            cols: 100,
            rows: 30,
            cell_width: 10,
            cell_height: 20,
        })?;
        terminal.set_display_scale(2.0)?;
        terminal.set_render_cell_metrics(CellMetrics::new(12.0, 24.0))?;
        terminal.force_resize()?;
        terminal.write_input(b"stale input")?;
        terminal.extract_frame()?;
        anyhow::ensure!(terminal.child_exited()?);
        anyhow::ensure!(terminal.capture(CaptureOptions::default()).is_err());
        Ok(())
    }

    pub fn shell_exit_is_quiet() -> Result<()> {
        let (_backend, registry, _session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        terminal.write_input(b"\x04")?;

        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(5))
            .context("shell exit deadline")?;
        loop {
            terminal.drain_pty();
            if terminal.child_exited()? {
                while terminal.discard_pending_output().is_ok() {
                    anyhow::ensure!(std::time::Instant::now() < deadline, "worker did not stop");
                    thread::yield_now();
                }
                anyhow::ensure!(!terminal.copy_mode_active()?);
                return Ok(());
            }
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "shell did not exit; last frame: {:?}",
                terminal
                    .extract_frame()?
                    .text
                    .iter()
                    .collect::<String>()
                    .trim_end()
            );
            thread::yield_now();
        }
    }

    pub fn kitty_images_reach_terminal_frames() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(std::sync::Arc::clone(&registry), &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        let mut second = open_terminal(registry, &pane, &window_id)?;
        prepare_pane(&mut second)?;
        let fixture = assert_fs::NamedTempFile::new("kitty.vt")?;
        let image_bytes = (0_usize..1024 * 512 * 4)
            .map(|index| (index ^ (index >> 8) ^ (index >> 16)).to_le_bytes()[0])
            .collect::<Vec<_>>();
        let payload = base64::engine::general_purpose::STANDARD.encode(&image_bytes);
        let mut output = Vec::new();
        let chunks = payload.as_bytes().chunks(4096);
        let count = chunks.len();
        for (index, chunk) in chunks.enumerate() {
            let more = u8::from(index.saturating_add(1) < count);
            let header = if index == 0 {
                format!("\x1b_Ga=T,i=42,q=2,f=32,s=1024,v=512,c=20,r=10,m={more};")
            } else {
                format!("\x1b_Gm={more};")
            };
            output.extend_from_slice(header.as_bytes());
            output.extend_from_slice(chunk);
            output.extend_from_slice(b"\x1b\\");
        }
        output.extend_from_slice(b"\r\nBOOTTY_IMAGE_COMPLETE\r\n");
        std::fs::write(fixture.path(), &output)?;
        terminal.write_input(format!("cat {}\r", fixture.path().display()).as_bytes())?;
        // Both bounded consumers must drain while the producer is running.
        let deadline = std::time::Instant::now()
            .checked_add(PANE_TIMEOUT)
            .context("image stream deadline")?;
        loop {
            terminal.drain_pty();
            second.drain_pty();
            let first_ready = terminal
                .extract_frame()?
                .text
                .iter()
                .collect::<String>()
                .contains("BOOTTY_IMAGE_COMPLETE");
            let second_ready = second
                .extract_frame()?
                .text
                .iter()
                .collect::<String>()
                .contains("BOOTTY_IMAGE_COMPLETE");
            if first_ready && second_ready {
                break;
            }
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "image stream did not complete for both readers"
            );
            thread::yield_now();
        }
        let second_frame = second.extract_frame()?;
        assert_eq!(second_frame.images.placements.len(), 1);
        let second_image = second_frame
            .images
            .placements
            .first()
            .context("second rendered image")?;
        assert_eq!(second_image.data.as_slice(), image_bytes);
        drop(second);
        terminal.write_input(b"printf 'BOOTTY_READER_REMAINS\\n'\r")?;
        wait_for_terminal_text(&mut terminal, "BOOTTY_READER_REMAINS")?;
        let frame = terminal.extract_frame()?;
        assert_eq!(frame.images.placements.len(), 1);
        let image = frame.images.placements.first().context("rendered image")?;
        assert_eq!(image.image_width, 1024);
        assert_eq!(image.image_height, 512);
        assert_eq!(image.data.as_slice(), image_bytes);
        ditch_session(&mut backend, &session_id)
    }

    pub fn closing_reader_during_large_output_keeps_other_reader_live() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(std::sync::Arc::clone(&registry), &pane, &window_id)?;
        prepare_pane(&mut terminal)?;
        let second = open_terminal(registry, &pane, &window_id)?;
        // Exceed the pipe's in-flight bound without spending the test on millions of line scrolls.
        terminal.write_input(
            b"head -c 8000000 /dev/zero | tr '\\000' X; printf '\\nBOOTTY_READER_CLOSED_COMPLETE\\n'\r",
        )?;
        wait_for_terminal_text(&mut terminal, "X")?;
        drop(second);
        wait_for_terminal_text(&mut terminal, "BOOTTY_READER_CLOSED_COMPLETE")?;
        ditch_session(&mut backend, &session_id)
    }

    pub fn bounded_live_output() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut terminal = open_terminal(registry, &pane, &window_id)?;
        prepare_pane(&mut terminal)?;

        // 2MB is past the in-flight bound (`RMUX_OUTPUT_CHANNEL_CAPACITY` events
        // of `RMUX_OUTPUT_EVENT_MAX_BYTES`), so the producer has to be held back
        // rather than spooled to disk.
        terminal.write_input(
            b"printf 'BOOTTY_RMUX_BOUND_START\\n'; yes X | head -c 2000000; printf '\\nBOOTTY_RMUX_BOUND_END\\n'\r",
        )?;
        std::thread::sleep(std::time::Duration::from_millis(100));
        wait_for_terminal_text(&mut terminal, "BOOTTY_RMUX_BOUND_END")?;
        let spool_files = std::fs::read_dir(
            endpoint_path_for(ApplicationIdentity::Production)?
                .parent()
                .context("embedded rmux endpoint parent")?,
        )?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with("bootty-rmux-output-"))
        .collect::<Vec<_>>();
        anyhow::ensure!(
            spool_files.is_empty(),
            "bounded live output must not create disk spools: {spool_files:?}"
        );

        ditch_session(&mut backend, &session_id)
    }

    pub fn large_restore_progress() -> Result<()> {
        let (mut backend, registry, session_id, window_id, pane) =
            create_embedded_session(unscoped_tag())?;
        let mut producer = open_terminal(std::sync::Arc::clone(&registry), &pane, &window_id)?;
        prepare_pane(&mut producer)?;
        producer.write_input(b"yes RESTORE | head -c 2000000\r")?;
        wait_for_terminal_text(&mut producer, "RESTORE")?;

        // Attach mid-flood: the reader has a backlog to restore and still has to
        // take a resize and input.
        let mut reader = open_terminal(registry, &pane, &window_id)?;
        reader.resize_native_layout_window(100, 30)?;
        reader.write_input(b"printf 'BOOTTY_RMUX_RESTORE_INPUT_RESIZE\n'\r")?;
        wait_for_terminal_text(&mut reader, "BOOTTY_RMUX_RESTORE_INPUT_RESIZE")?;

        ditch_session(&mut backend, &session_id)
    }
}

/// Run one scenario in a child process with its own rmux daemon and a pane
/// environment that is the same on every machine.
fn run_embedded_scenario(scenario: &str) -> Result<()> {
    let directory = assert_fs::TempDir::new()?;
    let helper = directory.path().join("bootty-daemon");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\nexport BOOTTY_RMUX_PIPE_ENDPOINT=\"$2\"\nexec {} --exact embedded_rmux_scenario_child --nocapture\n",
            bootty_host::shell_quote(&std::env::current_exe()?.to_string_lossy()),
        ),
    )?;
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", SCENARIO_CHILD_TEST])
        .env(SCENARIO_ENV, scenario)
        .env("BOOTTY_DAEMON_BINARY", helper)
        .env("RMUX_TMPDIR", directory.path())
        .env("BOOTTY_APPLICATION_IDENTITY", "bootty")
        .env("PATH", ISOLATED_PATH)
        // Panes inherit the daemon's environment. Pin the shell and its rc file
        // so a developer's login shell does not decide how long every pane takes
        // to answer: an interactive zsh with plugins costs seconds per pane.
        .env("SHELL", POSIX_SHELL)
        .env("ENV", "")
        .status()?;

    anyhow::ensure!(
        status.success(),
        "embedded rmux scenario {scenario} failed: {status}"
    );
    Ok(())
}

enum RemotePaneStreamFrame {
    End,
    Rebase(Vec<u8>),
    Bytes(Vec<u8>),
}

fn spawn_remote_pane_stream(
    payload: String,
) -> Result<(Child, mpsc::Receiver<std::result::Result<String, String>>)> {
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", REMOTE_STREAM_CHILD_TEST, "--nocapture"])
        .env(REMOTE_STREAM_PAYLOAD_ENV, payload)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .context("remote pane stream has no stdout")?;
    let (lines_tx, lines_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.map_err(|error| error.to_string());
            let stopped = line.is_err();
            if lines_tx.send(line).is_err() || stopped {
                return;
            }
        }
    });
    Ok((child, lines_rx))
}

fn next_remote_pane_stream_frame(
    frames: &mpsc::Receiver<std::result::Result<String, String>>,
) -> Result<RemotePaneStreamFrame> {
    let deadline = std::time::Instant::now()
        .checked_add(PANE_TIMEOUT)
        .context("pane deadline")?;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let line = frames
            .recv_timeout(remaining)
            .map_err(|error| {
                anyhow::anyhow!("remote pane stream did not produce a frame: {error}")
            })?
            .map_err(anyhow::Error::msg)?;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(frame) = value.as_object() else {
            continue;
        };
        if let Some(keyframe) = frame.get("Rebase").and_then(serde_json::Value::as_str) {
            return Ok(RemotePaneStreamFrame::Rebase(
                URL_SAFE_NO_PAD
                    .decode(keyframe)
                    .context("decode remote pane rebase")?,
            ));
        }
        if let Some(bytes) = frame.get("Bytes").and_then(serde_json::Value::as_str) {
            return Ok(RemotePaneStreamFrame::Bytes(
                URL_SAFE_NO_PAD
                    .decode(bytes)
                    .context("decode remote pane output")?,
            ));
        }
        if let Some(error) = frame.get("Error").and_then(serde_json::Value::as_str) {
            anyhow::bail!("remote pane stream failed: {error}");
        }
        if let Some(reason) = frame.get("End") {
            anyhow::ensure!(reason.is_null(), "remote pane stream failed: {reason}");
            return Ok(RemotePaneStreamFrame::End);
        }
    }
}

fn unscoped_tag() -> MuxSessionTag {
    MuxSessionTag {
        identity: Some(new_session_identity()),
        space: None,
    }
}

fn create_embedded_session(
    tag: MuxSessionTag,
) -> Result<(
    RmuxBackend,
    std::sync::Arc<MuxBackendRegistry>,
    String,
    String,
    bootty_mux::snapshot::MuxPaneAnchor,
)> {
    start_embedded_rmux_daemon_for_tests()?;
    // Providers are registered by bootty-mux itself.
    let registry = std::sync::Arc::new(MuxBackendRegistry::collect([MuxBackendKind::Rmux])?);
    let session_id = format!("bootty-mux-test-{}", std::process::id());
    let mut backend = RmuxBackend::new();
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: session_id.clone(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        tag,
        argv: None,
    })?;

    let snapshot = backend.snapshot()?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .context("created rmux session")?;
    let window = session.windows.first().context("created rmux window")?;
    let pane = window.panes.first().context("created rmux pane")?.clone();
    Ok((backend, registry, session_id, window.id.clone(), pane))
}

fn open_terminal(
    registry: std::sync::Arc<MuxBackendRegistry>,
    pane: &bootty_mux::snapshot::MuxPaneAnchor,
    window_id: &str,
) -> Result<ActiveTerminal> {
    open_terminal_with_window(registry, std::slice::from_ref(pane), pane, window_id)
}

fn open_terminal_with_window(
    registry: std::sync::Arc<MuxBackendRegistry>,
    panes: &[bootty_mux::snapshot::MuxPaneAnchor],
    focused: &bootty_mux::snapshot::MuxPaneAnchor,
    window_id: &str,
) -> Result<ActiveTerminal> {
    open_terminal_with_window_config(
        registry,
        panes,
        focused,
        window_id,
        TerminalSessionConfig::default(),
    )
}

fn open_terminal_with_window_config(
    registry: std::sync::Arc<MuxBackendRegistry>,
    panes: &[bootty_mux::snapshot::MuxPaneAnchor],
    focused: &bootty_mux::snapshot::MuxPaneAnchor,
    window_id: &str,
    config: TerminalSessionConfig,
) -> Result<ActiveTerminal> {
    let mut terminal = ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        registry,
        &MuxBindingConfig {
            backend: MuxBackendKind::Rmux,
            ..MuxBindingConfig::default()
        },
        config,
        std::sync::Arc::new(|| {}),
    )?;
    terminal.sync_native_window(
        panes,
        Some(focused),
        Some(window_id),
        MuxBackendKind::Rmux,
        false,
    )?;
    Ok(terminal)
}

fn ditch_session(backend: &mut RmuxBackend, session_id: &str) -> Result<()> {
    backend.execute(MuxCommand::DitchSession {
        session_id: session_id.to_owned(),
    })?;
    let snapshot = backend.snapshot()?;
    anyhow::ensure!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.id == session_id)
    );
    Ok(())
}

fn active_pane_id(backend: &RmuxBackend, session_id: &str) -> Result<String> {
    backend
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .context("rmux test session was not found")?
        .windows
        .into_iter()
        .find(|window| window.active)
        .context("rmux test active window was not found")?
        .anchor
        .pane_id
        .context("rmux test active pane was not found")
}

fn pane_ids(backend: &RmuxBackend, session_id: &str) -> Result<Vec<String>> {
    Ok(backend
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .context("rmux test session was not found")?
        .windows
        .into_iter()
        .find(|window| window.active)
        .context("rmux test active window was not found")?
        .panes
        .into_iter()
        .filter_map(|pane| pane.pane_id)
        .collect())
}

/// Poll the pane's frames until `matches` holds. `Some(text)` is the last frame
/// seen before the deadline passed, so a failure reports what the pane was
/// actually showing.
fn poll_frames_until(
    terminal: &mut ActiveTerminal,
    deadline: std::time::Instant,
    matches: impl Fn(&str) -> bool,
) -> Result<Option<String>> {
    loop {
        terminal.drain_pty();
        let frame = terminal.extract_frame()?;
        let text = frame.text.iter().collect::<String>();
        if matches(&text) {
            return Ok(None);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(Some(text));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn wait_for_terminal_text(terminal: &mut ActiveTerminal, expected: &str) -> Result<()> {
    let deadline = std::time::Instant::now()
        .checked_add(PANE_TIMEOUT)
        .context("pane deadline")?;
    match poll_frames_until(terminal, deadline, |text| text.contains(expected))? {
        None => Ok(()),
        Some(text) => anyhow::bail!(
            "rmux terminal did not publish {expected:?}, last frame: {:?}",
            text.trim_end()
        ),
    }
}

/// Put the pane in the state every scenario expects: a shell that has claimed
/// the pty, with echo off.
///
/// The probe is retried instead of waited on, because a pane drops input written
/// before its shell claims the pty and prints nothing to announce that moment.
/// Repeating the probe is harmless -- both halves are idempotent -- and it is
/// the only part of a scenario that may run more than once.
///
/// Echo off matters as much as readiness: with it on, the pane's frame holds the
/// command line that was typed, and a wait for a marker is satisfied by the
/// command asking for it rather than by the pane printing it.
fn prepare_pane(terminal: &mut ActiveTerminal) -> Result<()> {
    let deadline = std::time::Instant::now()
        .checked_add(PANE_TIMEOUT)
        .context("pane deadline")?;
    loop {
        // Bash readline flushes queued input when returning to its prompt. Use the
        // canonical POSIX reader so the readiness receipt also covers queued Ctrl-D.
        // Split the marker so this line's echo cannot answer for the pane.
        terminal.write_input(b"if [ -n \"${BASH_VERSION-}\" ]; then set +o emacs; fi; stty -echo; printf '%s%s\\n' 'BOOTTY_PANE' '_READY'\r")?;
        let attempt = (std::time::Instant::now()
            .checked_add(PANE_PROBE_INTERVAL)
            .context("probe deadline")?)
        .min(deadline);
        if poll_frames_until(terminal, attempt, |text| text.contains(PANE_READY))?.is_none() {
            return Ok(());
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "pane shell never answered a readiness probe"
        );
    }
}
