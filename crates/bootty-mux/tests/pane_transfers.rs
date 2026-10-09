//! Exercise the real tmux server on a private socket: transfers must preserve process identity,
//! pane input and capture must address one pane without changing the session's selection, and an
//! explicit create must deliver its argv exactly and never reuse an existing session.
#![cfg(unix)]
use anyhow::{Context as _, Result};
use bootty_host::{CommandOutput, CommandRunner, SystemCommandRunner};
use bootty_mux::{
    backend::{MuxBackend, PaneCapture, PaneInput},
    command::{MuxCommand, MuxDirection},
    session_snapshot::{SavedTerminalPane, SavedTerminalSession, SavedTerminalWindow},
    snapshot::{MuxPaneLayout, MuxPaneSplitDirection, MuxSessionTag},
    tmux::TmuxBackend,
};
use pretty_assertions::{assert_eq, assert_ne};
use rstest::rstest;
use std::{collections::BTreeMap, process::Command};

struct PrivateTmux {
    directory: assert_fs::TempDir,
}
impl PrivateTmux {
    fn args(&self, args: &[String]) -> Vec<String> {
        [
            vec![
                "-S".to_owned(),
                self.directory
                    .path()
                    .join("socket")
                    .to_string_lossy()
                    .into_owned(),
                "-f".to_owned(),
                "/dev/null".to_owned(),
            ],
            args.to_vec(),
        ]
        .concat()
    }
    fn run_checked(&self, args: &[&str]) -> Result<String> {
        let output = self.run(
            "tmux",
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        )?;
        anyhow::ensure!(output.success, "{}", output.stderr);
        Ok(output.stdout)
    }
    fn processes(&self) -> Result<BTreeMap<String, String>> {
        self.run_checked(&["list-panes", "-a", "-F", "#{pane_id} #{pane_pid}"])?
            .lines()
            .map(|line| {
                let (id, pid) = line.split_once(' ').context("pane process")?;
                Ok((id.to_owned(), pid.to_owned()))
            })
            .collect()
    }
}
impl CommandRunner for PrivateTmux {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        SystemCommandRunner.run(program, &self.args(args))
    }

    fn run_with_input(
        &self,
        program: &str,
        args: &[String],
        input: Vec<u8>,
    ) -> Result<CommandOutput> {
        SystemCommandRunner.run_with_input(program, &self.args(args), input)
    }
}
impl Drop for PrivateTmux {
    fn drop(&mut self) {
        let _ = self.run("tmux", &["kill-server".to_owned()]);
    }
}
#[rstest]
fn tmux_transfers_keep_processes_and_reject_foreign_panes() -> Result<()> {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux unavailable; real-server acceptance not run");
        return Ok(());
    }
    let server = PrivateTmux {
        directory: assert_fs::TempDir::new().expect("private socket"),
    };
    // Separate argv bypasses the user shell and its asynchronous startup/exec.
    server.run_checked(&[
        "new-session",
        "-d",
        "-s",
        "transfer",
        "-x",
        "160",
        "-y",
        "80",
        "/bin/sh",
        "-i",
    ])?;
    // Compare transfer effects, not tmux's asynchronous shell-title updates.
    server.run_checked(&["set-option", "-gw", "automatic-rename", "off"])?;
    server.run_checked(&["split-window", "-h", "-t", "transfer", "/bin/sh", "-i"])?;
    server.run_checked(&["new-session", "-d", "-s", "foreign", "/bin/sh", "-i"])?;
    let before = server.processes()?;
    let ids = server
        .run_checked(&["list-panes", "-t", "transfer", "-F", "#{pane_id}"])?
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let foreign = server
        .run_checked(&["display-message", "-p", "-t", "foreign", "#{pane_id}"])?
        .trim()
        .to_owned();
    let mut backend = TmuxBackend::with_runner("tmux", server);
    backend
        .execute(MuxCommand::SwapPanes {
            session_id: "transfer".to_owned(),
            source_pane_id: ids[0].clone(),
            target_pane_id: ids[1].clone(),
        })
        .expect("swap");
    backend
        .execute(MuxCommand::ExtractPane {
            session_id: "transfer".to_owned(),
            pane_id: ids[0].clone(),
        })
        .expect("extract");
    let snapshot = backend.snapshot().expect("extracted snapshot");
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.name == "transfer")
            .expect("session")
            .windows
            .len(),
        2
    );
    backend
        .execute(MuxCommand::MovePane {
            session_id: "transfer".to_owned(),
            pane_id: ids[0].clone(),
            target_pane_id: ids[1].clone(),
            direction: MuxDirection::Down,
        })
        .expect("move back");
    assert_eq!(backend.runner().processes()?, before);
    let before_rejection = backend.snapshot().expect("snapshot");
    anyhow::ensure!(
        backend
            .execute(MuxCommand::SwapPanes {
                session_id: "transfer".to_owned(),
                source_pane_id: ids[0].clone(),
                target_pane_id: foreign
            })
            .is_err()
    );
    assert_eq!(
        backend.snapshot().expect("unchanged snapshot"),
        before_rejection
    );
    assert_eq!(backend.runner().processes()?, before);
    Ok(())
}

#[rstest]
fn tmux_pane_input_reaches_an_unselected_pane_and_keeps_selection() -> Result<()> {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux unavailable; real-server acceptance not run");
        return Ok(());
    }
    let server = PrivateTmux {
        directory: assert_fs::TempDir::new().expect("private socket"),
    };
    // The reader asks for bracketed paste and shows every byte it receives, `^[` for ESC.
    server.run_checked(&[
        "new-session",
        "-d",
        "-s",
        "io",
        "-x",
        "120",
        "-y",
        "20",
        "/bin/sh",
        "-c",
        "printf '\\033[?2004h'; stty raw -echo; exec cat -v",
    ])?;
    let pane = server
        .run_checked(&["list-panes", "-t", "io:0", "-F", "#{pane_id}"])?
        .trim()
        .to_owned();
    server.run_checked(&["new-window", "-t", "io", "/bin/sh", "-c", "exec sleep 600"])?;
    let before = selected_window(&server)?;
    let backend = TmuxBackend::with_runner("tmux", server);
    let screen = |backend: &TmuxBackend<PrivateTmux>| {
        backend
            .capture_pane(
                &pane,
                PaneCapture {
                    history: false,
                    max_lines: 20,
                    ansi: false,
                },
            )
            .map(|captured| captured.text)
    };
    let settled = |backend: &TmuxBackend<PrivateTmux>, expected: &str| -> Result<String> {
        for _ in 0..200 {
            let text = screen(backend)?;
            if text.contains(expected) {
                return Ok(text);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        screen(backend)
    };
    // tmux only brackets a paste once the reader has enabled bracketed paste.
    backend.send_pane_input(&pane, &PaneInput::Write(b"ready".to_vec()))?;
    anyhow::ensure!(
        settled(&backend, "ready")?.contains("ready"),
        "reader never started"
    );

    backend.send_pane_input(&pane, &PaneInput::Paste("first\nsecond".to_owned()))?;
    backend.send_pane_input(&pane, &PaneInput::Submit)?;
    backend.send_pane_input(&pane, &PaneInput::Write(b"\x1b".to_vec()))?;
    let expected = "ready^[[200~first^Msecond^[[201~^M^[";
    assert_eq!(
        settled(&backend, expected)?
            .lines()
            .next()
            .unwrap_or_default(),
        expected
    );
    assert_eq!(selected_window(backend.runner())?, before);

    // Bounds match a terminal capture: the last rows only, with the rest counted as omitted.
    let one_row = backend.capture_pane(
        &pane,
        PaneCapture {
            history: false,
            max_lines: 1,
            ansi: false,
        },
    )?;
    assert_eq!(
        (
            one_row.captured_lines,
            one_row.omitted_lines,
            one_row.text.lines().count()
        ),
        (1, 19, 1)
    );
    let history = backend.capture_pane(
        &pane,
        PaneCapture {
            history: true,
            max_lines: 3,
            ansi: false,
        },
    )?;
    assert_eq!(
        (history.captured_lines, history.text.lines().count()),
        (3, 3)
    );
    Ok(())
}

#[rstest]
fn tmux_explicit_create_delivers_argv_exactly_and_never_reuses_a_name() -> Result<()> {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux unavailable; real-server acceptance not run");
        return Ok(());
    }
    let server = PrivateTmux {
        directory: assert_fs::TempDir::new().expect("private socket"),
    };
    let cwd = server.directory.path().to_string_lossy().into_owned();
    let output = server.directory.path().join("argv");
    // tmux splits its command line at arguments ending in `;`, and a prompt is several KiB of
    // text that a shell or tmux format could reinterpret.
    let prompt = "Fix it: 'single' \"double\" $HOME `tick` #{session_name} ~ a;b\\ c;\n".repeat(64);
    let arguments = [
        "a;",
        "b\\;",
        ";",
        "x;;",
        "",
        "-x",
        "#{session_name}",
        prompt.as_str(),
    ]
    .map(str::to_owned)
    .to_vec();
    let argv = [
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "printf '%s\\0' \"$@\" > \"$0\"; exec sleep 600".to_owned(),
        output.to_string_lossy().into_owned(),
    ]
    .into_iter()
    .chain(arguments.iter().cloned())
    .collect();
    let name = "crash-12zw;";
    let tag = MuxSessionTag {
        identity: Some("identity-1".to_owned()),
        space: Some("space-1".to_owned()),
    };
    let mut backend = TmuxBackend::with_runner("tmux", server);
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: name.to_owned(),
        cwd: cwd.clone(),
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
    for _ in 0..500 {
        let text = read();
        if text.ends_with('\0') && fields(&text).len() == arguments.len() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(fields(&read()), arguments);

    let created = backend
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.name == name)
        .context("created session")?;
    assert_eq!(created.tag, tag);

    let duplicate = backend.execute(MuxCommand::CreateProjectSession {
        session_id: name.to_owned(),
        cwd,
        tag: MuxSessionTag {
            identity: Some("identity-2".to_owned()),
            space: Some("space-2".to_owned()),
        },
        argv: Some(Vec::new()),
    });
    anyhow::ensure!(duplicate.is_err(), "a taken name must be refused");
    let named = backend
        .snapshot()?
        .sessions
        .into_iter()
        .filter(|session| session.name == name)
        .collect::<Vec<_>>();
    assert_eq!(named, [created], "the existing session and its stamps stay");
    Ok(())
}

#[rstest]
#[case::right(MuxPaneSplitDirection::Right)]
#[case::down(MuxPaneSplitDirection::Down)]
fn tmux_restores_windows_splits_directories_and_focus_without_adopting_live_work(
    #[case] direction: MuxPaneSplitDirection,
) -> Result<()> {
    let server = PrivateTmux {
        directory: assert_fs::TempDir::new()?,
    };
    server.run_checked(&["new-session", "-d", "-s", "unrelated", "/bin/sh", "-i"])?;
    server.run_checked(&["set-option", "-g", "default-shell", "/bin/sh"])?;
    server.run_checked(&["set-option", "-gw", "automatic-rename", "off"])?;
    let unrelated = server.processes()?;
    let pane = |id: &str| -> Result<SavedTerminalPane> {
        let cwd = server.directory.path().join(format!("{id} cwd # literal"));
        std::fs::create_dir(&cwd)?;
        Ok(SavedTerminalPane {
            native_agent: (id != "a").then(|| format!("native:codex:{id}")),
            id: id.into(),
            backend_id: format!("old-{id}"),
            cwd: cwd.canonicalize()?.to_string_lossy().into_owned(),
            cols: 80,
            rows: 24,
            text: format!(
                "\x1b[31mRESTORED_{id} 🥟\x1b[0m\ntouch '{}'\n",
                server.directory.path().join("must-not-execute").display()
            ),
            omitted_lines: 0,
        })
    };
    let saved = SavedTerminalSession {
        captured_at: 1,
        session_id: "logical".into(),
        backend_id: "$old".into(),
        active_window_id: Some("second".into()),
        windows: vec![
            SavedTerminalWindow {
                id: "first".into(),
                backend_id: "@old-first".into(),
                title: "First window".into(),
                focused_pane_id: "b".into(),
                layout: Some(MuxPaneLayout::Split {
                    direction: direction.clone(),
                    ratio_millis: 333,
                    first: Box::new(MuxPaneLayout::Pane("a".into())),
                    second: Box::new(MuxPaneLayout::Pane("b".into())),
                }),
                panes: vec![pane("a")?, pane("b")?],
            },
            SavedTerminalWindow {
                id: "second".into(),
                backend_id: "@old-second".into(),
                title: "Second window".into(),
                focused_pane_id: "c".into(),
                layout: None,
                panes: vec![pane("c")?],
            },
        ],
    };
    let tag = MuxSessionTag {
        identity: Some(saved.session_id.clone()),
        space: Some("saved-space".into()),
    };
    let restore = MuxCommand::RestoreSession {
        session_id: "restored".into(),
        tag: tag.clone(),
        snapshot: saved.clone(),
    };
    let mut backend = TmuxBackend::with_runner("tmux", server);
    backend.execute(restore.clone())?;
    let snapshot = backend.snapshot()?;
    let restored = snapshot
        .sessions
        .iter()
        .find(|s| s.name == "restored")
        .context("restored session")?;
    assert_eq!(restored.tag, tag);
    assert_eq!(restored.windows.len(), 2);
    for (actual, expected) in restored.windows.iter().zip(&saved.windows) {
        assert_eq!(actual.name, expected.title);
        assert_eq!(actual.panes.len(), expected.panes.len());
        assert_eq!(
            actual
                .panes
                .iter()
                .map(|pane| &pane.native_agent)
                .collect::<Vec<_>>(),
            expected
                .panes
                .iter()
                .map(|pane| &pane.native_agent)
                .collect::<Vec<_>>()
        );
        for (actual, expected) in actual.panes.iter().zip(&expected.panes) {
            let id = actual.pane_id.as_deref().context("restored pane id")?;
            let capture = backend.capture_pane(
                id,
                PaneCapture {
                    history: true,
                    ansi: true,
                    max_lines: 1000,
                },
            )?;
            anyhow::ensure!(
                capture
                    .text
                    .contains(&format!("RESTORED_{} 🥟", expected.id)),
                "saved rows missing from tmux history: {}",
                capture.text
            );
            anyhow::ensure!(
                capture.text.contains("\x1b[31m"),
                "saved SGR style was lost: {}",
                capture.text
            );
        }
        assert_eq!(
            actual
                .panes
                .iter()
                .map(|pane| &pane.native_agent)
                .collect::<Vec<_>>(),
            expected
                .panes
                .iter()
                .map(|pane| &pane.native_agent)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            actual
                .panes
                .iter()
                .map(|p| p.cwd.as_deref())
                .collect::<Vec<_>>(),
            expected
                .panes
                .iter()
                .map(|p| Some(p.cwd.as_str()))
                .collect::<Vec<_>>()
        );
    }
    let first = &restored.windows[0];
    assert_eq!(first.anchor.pane_id, first.panes[1].pane_id);
    assert_eq!(
        restored.active_window_id.as_deref(),
        Some(restored.windows[1].id.as_str())
    );
    let MuxPaneLayout::Split {
        direction: actual_direction,
        ratio_millis,
        first: left,
        second: right,
    } = first.layout.as_ref().context("restored split")?
    else {
        anyhow::bail!("split was lost");
    };
    assert_eq!(*actual_direction, direction);
    // tmux rounds layout ratios to integer cells; a 48-row split has at most 22/1000 error.
    anyhow::ensure!(
        ratio_millis.abs_diff(333) <= 22,
        "restored ratio: {ratio_millis}"
    );
    assert_eq!(
        left.as_ref(),
        &MuxPaneLayout::Pane(first.panes[0].pane_id.clone().context("first pane")?)
    );
    assert_eq!(
        right.as_ref(),
        &MuxPaneLayout::Pane(first.panes[1].pane_id.clone().context("second pane")?)
    );
    let processes = backend.runner().processes()?;
    anyhow::ensure!(
        !backend
            .runner()
            .directory
            .path()
            .join("must-not-execute")
            .exists(),
        "restored text was executed as shell input"
    );
    for (pane, pid) in unrelated {
        assert_eq!(processes.get(&pane), Some(&pid));
    }
    anyhow::ensure!(
        backend.execute(restore).is_err(),
        "a retry must not adopt an existing session"
    );
    assert_eq!(backend.snapshot()?, snapshot);
    assert_eq!(backend.runner().processes()?, processes);
    Ok(())
}

#[rstest]
#[case(2, 2, "tiny saved 🥟")]
#[case(80, 24, "")]
fn tmux_restores_single_pane_with_tiny_or_empty_history(
    #[case] cols: u16,
    #[case] rows: u16,
    #[case] text: &str,
) -> Result<()> {
    let server = PrivateTmux {
        directory: assert_fs::TempDir::new()?,
    };
    server.run_checked(&["new-session", "-d", "-s", "unrelated", "/bin/sh", "-i"])?;
    server.run_checked(&["set-option", "-g", "default-shell", "/bin/sh"])?;
    let unrelated = server.processes()?;
    let saved = SavedTerminalSession {
        captured_at: 1,
        session_id: "logical".into(),
        backend_id: "$old".into(),
        active_window_id: Some("window".into()),
        windows: vec![SavedTerminalWindow {
            id: "window".into(),
            backend_id: "@old".into(),
            title: "restored".into(),
            focused_pane_id: "pane".into(),
            layout: None,
            panes: vec![SavedTerminalPane {
                id: "pane".into(),
                backend_id: "%old".into(),
                cwd: "/tmp".into(),
                cols,
                rows,
                text: text.into(),
                omitted_lines: 0,
                native_agent: None,
            }],
        }],
    };
    let mut backend = TmuxBackend::with_runner("tmux", server);
    backend.execute(MuxCommand::RestoreSession {
        session_id: "restored".into(),
        tag: MuxSessionTag {
            identity: Some("logical".into()),
            space: None,
        },
        snapshot: saved,
    })?;
    let snapshot = backend.snapshot()?;
    let session = snapshot
        .sessions
        .iter()
        .find(|s| s.name == "restored")
        .context("restored session")?;
    let pane = session.windows[0]
        .anchor
        .pane_id
        .as_deref()
        .context("restored pane")?;
    if !text.is_empty() {
        let capture = backend.capture_pane(
            pane,
            PaneCapture {
                history: true,
                ansi: false,
                max_lines: 1000,
            },
        )?;
        // Reflow the two-cell pane back to a readable width before checking its Unicode text.
        backend.runner().run_checked(&[
            "resize-window",
            "-t",
            &session.windows[0].id,
            "-x",
            "80",
            "-y",
            "24",
        ])?;
        let wide = backend.capture_pane(
            pane,
            PaneCapture {
                history: true,
                ansi: false,
                max_lines: 1000,
            },
        )?;
        anyhow::ensure!(
            wide.text.contains(text),
            "saved history was lost: {} / {}",
            capture.text,
            wide.text
        );
    }
    for (id, pid) in unrelated {
        assert_eq!(backend.runner().processes()?.get(&id), Some(&pid));
    }
    Ok(())
}

fn selected_window(server: &PrivateTmux) -> Result<String> {
    server.run_checked(&["display-message", "-p", "-t", "io", "#{window_index}"])
}

#[rstest]
fn tmux_direct_respawn_keeps_pane_identity_and_literal_argv() -> Result<()> {
    let directory = assert_fs::TempDir::new()?;
    let cwd = directory.path().join("#{pane_current_path}; literal");
    std::fs::create_dir(&cwd)?;
    let cwd = cwd.canonicalize()?;
    let output = directory.path().join("resumed-argv");
    let server = PrivateTmux {
        directory: assert_fs::TempDir::new()?,
    };
    server.run_checked(&["new-session", "-d", "-s", "resume", "/bin/sh", "-i"])?;
    server.run_checked(&["split-window", "-h", "-t", "resume", "/bin/sh", "-i"])?;
    let ids = server
        .run_checked(&["list-panes", "-t", "resume", "-F", "#{pane_id}"])?
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let before = server.processes()?;
    let backend = TmuxBackend::with_runner("tmux", server);
    let literals = [
        "a;",
        "$(literal)",
        "multi\nline 'single' \"double\" $HOME",
        "",
        "-x",
        "last;",
    ];
    let mut argv = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "printf '%s\\0' \"$PWD\" \"$@\" > \"$0\"; printf 'DIRECT_RESUME_READY\\n'; exec /bin/cat"
            .to_owned(),
        output.to_string_lossy().into_owned(),
    ];
    argv.extend(literals.map(str::to_owned));
    backend.respawn_pane_command(&ids[0], &argv, Some(&cwd.to_string_lossy()))?;
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(3))
        .context("process startup deadline")?;
    let expected = std::iter::once(cwd.to_string_lossy().into_owned())
        .chain(literals.map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\0")
        + "\0";
    loop {
        if std::fs::read_to_string(&output).unwrap_or_default() == expected {
            break;
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "direct argv not delivered: {:?}",
            std::fs::read_to_string(&output)
        );
        std::thread::yield_now();
    }
    let after = backend.runner().processes()?;
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    assert_ne!(before[&ids[0]], after[&ids[0]]);
    assert_eq!(
        before[&ids[1]], after[&ids[1]],
        "unrelated process retained"
    );
    backend.send_pane_input(
        &ids[0],
        &PaneInput::Write(b"AFTER_DIRECT_RESUME\n".to_vec()),
    )?;
    let capture = PaneCapture {
        history: true,
        max_lines: 100,
        ansi: false,
    };
    loop {
        let text = backend.capture_pane(&ids[0], capture)?.text;
        if text.contains("AFTER_DIRECT_RESUME") {
            break;
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "respawned pane input unavailable: {text:?}"
        );
        std::thread::yield_now();
    }
    Ok(())
}
