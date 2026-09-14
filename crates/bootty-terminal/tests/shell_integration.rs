use assert_fs::prelude::*;
use bootty_terminal::{
    SessionLaunchConfig, TerminalSession, TerminalSessionConfig,
    geometry::TerminalGeometry,
    shell_integration::ShellIntegration,
    shell_lifecycle::ShellEvent,
    terminal_side_effect::{TerminalSideEffect, TerminalSideEffectEvent},
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};

fn program(name: &str) -> Option<PathBuf> {
    ["/bin", "/usr/bin", "/opt/homebrew/bin"]
        .into_iter()
        .map(|directory| Path::new(directory).join(name))
        .find(|path| path.is_file())
}
fn next_event(events: &mpsc::Receiver<TerminalSideEffectEvent>) -> anyhow::Result<ShellEvent> {
    loop {
        let event = events.recv_timeout(Duration::from_secs(5))?;
        if let TerminalSideEffect::ShellLifecycle(event_type) = event.effect {
            anyhow::ensure!(event.observed_at.is_some(), "live events carry worker time");
            if event_type != ShellEvent::PromptEnd {
                return Ok(event_type);
            }
        }
    }
}
fn expect_event(
    events: &mpsc::Receiver<TerminalSideEffectEvent>,
    expected: ShellEvent,
) -> anyhow::Result<()> {
    loop {
        let event = next_event(events)?;
        // Fish can redraw its prompt while input arrives; that is not a command start.
        if event == ShellEvent::PromptStart && expected != ShellEvent::PromptStart {
            continue;
        }
        anyhow::ensure!(
            event == expected,
            "shell event {event:?}, expected {expected:?}"
        );
        return Ok(());
    }
}
#[rstest]
#[case("bash")]
#[case("zsh")]
#[case("fish")]
fn supported_shells_keep_rc_files_and_report_exit_status_and_directory(#[case] name: &str) {
    let Some(program) = program(name) else {
        eprintln!("{name} is not installed on this platform");
        return;
    };
    let home = assert_fs::TempDir::new().unwrap();
    home.child("cwd % # ?").create_dir_all().unwrap();
    let cwd = home.child("cwd % # ?");
    let rc = "printf 'RC_LOADED\\n' > \"$HOME/loaded\"\n";
    match name {
        "bash" => home.child(".bashrc").write_str(rc).unwrap(),
        "zsh" => home.child(".zshrc").write_str(rc).unwrap(),
        "fish" => {
            home.child(".config/fish").create_dir_all().unwrap();
            home.child(".config/fish/config.fish")
                .write_str(rc)
                .unwrap();
        }
        _ => panic!("unsupported shell test case: {name}"),
    }
    let (sender, events) = mpsc::channel();
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some(program.to_string_lossy().into_owned()),
            shell_integration: true,
            working_directory: Some(cwd.path().to_owned()),
            env: vec![
                ("HOME".into(), home.path().to_string_lossy().into_owned()),
                (
                    "XDG_CONFIG_HOME".into(),
                    home.path().join(".config").to_string_lossy().into_owned(),
                ),
            ],
            env_remove: vec!["ZDOTDIR".into(), "BASH_ENV".into(), "ENV".into()],
            ..SessionLaunchConfig::default()
        },
        side_effect_tx: Some(sender),
        ..TerminalSessionConfig::default()
    };
    let session = TerminalSession::new_with_config(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        },
        config,
        Arc::new(|| {}),
    )
    .unwrap();
    assert_eq!(
        next_event(&events).expect("shell lifecycle event"),
        ShellEvent::PromptStart
    );
    home.child("loaded").assert("RC_LOADED\n");
    session.write_input(b"false\r").unwrap();
    expect_event(&events, ShellEvent::CommandStart).expect("shell lifecycle event");
    assert_eq!(
        next_event(&events).expect("shell lifecycle event"),
        ShellEvent::CommandFinish { exit_code: Some(1) }
    );
    assert_eq!(
        next_event(&events).expect("shell lifecycle event"),
        ShellEvent::PromptStart
    );
    let script = if name == "fish" {
        "printf '%s\\n' $status > \"$HOME/status\"\r"
    } else {
        "printf '%s\\n' \"$?\" > \"$HOME/status\"\r"
    };
    session.write_input(script.as_bytes()).unwrap();
    expect_event(&events, ShellEvent::CommandStart).expect("shell lifecycle event");
    assert_eq!(
        next_event(&events).expect("shell lifecycle event"),
        ShellEvent::CommandFinish { exit_code: Some(0) }
    );
    assert_eq!(
        next_event(&events).expect("shell lifecycle event"),
        ShellEvent::PromptStart
    );
    home.child("status").assert("1\n");
    let cwd = session.current_working_directory().unwrap();
    assert!(cwd.starts_with("file://"), "{cwd}");
    assert!(cwd.ends_with("/cwd%20%25%20%23%20%3F"), "{cwd}");
}
#[rstest]
fn custom_commands_and_unknown_shells_are_not_rewritten() {
    assert!(
        ShellIntegration::prepare("/bin/bash", &["-c".into(), "printf test".into()], None)
            .unwrap()
            .is_none()
    );
    assert!(
        ShellIntegration::prepare("/bin/sh", &[], None)
            .unwrap()
            .is_none()
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn zsh_preserves_zdotdir_presence_and_user_startup_redirects(#[case] redirect: bool) {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let Some(program) = program("zsh") else {
        return;
    };
    let home = assert_fs::TempDir::new().unwrap();
    if redirect {
        home.child("custom").create_dir_all().unwrap();
        home.child(".zshenv")
            .write_str("ZDOTDIR=$HOME/custom\n")
            .unwrap();
        home.child("custom/.zshrc")
            .write_str("printf 'CUSTOM_RC\\n'\n")
            .unwrap();
    } else {
        home.child(".zshrc")
            .write_str("[[ -z ${ZDOTDIR+x} ]] && printf 'UNSET_RC\\n'\n")
            .unwrap();
    }
    let integration = ShellIntegration::prepare(program.to_str().unwrap(), &[], None)
        .unwrap()
        .unwrap();
    let mut child = Command::new(program)
        .arg("-i")
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color")
        .envs(integration.env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"exit\n").unwrap();
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(if redirect { "CUSTOM_RC" } else { "UNSET_RC" }),
        "{text}"
    );
}

#[rstest]
fn bash_keeps_an_existing_debug_trap() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let Some(program) = program("bash") else {
        return;
    };
    let home = assert_fs::TempDir::new().unwrap();
    home.child(".bashrc")
        .write_str("trap 'printf KEEP_DEBUG' DEBUG\n")
        .unwrap();
    let integration = ShellIntegration::prepare(program.to_str().unwrap(), &[], None)
        .unwrap()
        .unwrap();
    let mut child = Command::new(program)
        .args(&integration.args)
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"printf COMMAND\nexit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("KEEP_DEBUG"));
    assert!(text.contains("COMMAND"));
    assert!(!text.contains("133;C"));
}

#[rstest]
#[case("bash")]
#[case("zsh")]
#[case("fish")]
fn prompt_editor_handoff_preserves_multiline_text_and_rejects_reuse(#[case] name: &str) {
    let Some(program) = ["/opt/homebrew/bin", "/usr/bin", "/bin"]
        .into_iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.is_file())
    else {
        return;
    };
    let home = assert_fs::TempDir::new().unwrap();
    home.child(".bashrc")
        .write_str("HISTFILE=$HOME/history\n")
        .unwrap();
    home.child(".zshrc")
        .write_str("HISTFILE=$HOME/history\nHISTSIZE=100\nSAVEHIST=100\nbindkey -e\n")
        .unwrap();
    let (sender, events) = mpsc::channel();
    let (wake, wakes) = mpsc::channel();
    let mut session = TerminalSession::new_with_config(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        },
        TerminalSessionConfig {
            launch: SessionLaunchConfig {
                shell: Some(program.to_string_lossy().into_owned()),
                shell_integration: true,
                working_directory: Some(home.path().to_owned()),
                env: vec![
                    ("HOME".into(), home.path().to_string_lossy().into_owned()),
                    (
                        "XDG_CONFIG_HOME".into(),
                        home.path().join("config").to_string_lossy().into_owned(),
                    ),
                ],
                env_remove: vec!["ZDOTDIR".into(), "BASH_ENV".into(), "ENV".into()],
                ..Default::default()
            },
            side_effect_tx: Some(sender),
            ..Default::default()
        },
        Arc::new(move || {
            let _ = wake.send(());
        }),
    )
    .unwrap();
    let command = if name == "fish" {
        "begin\nprintf 'one\\ntwo\\n' > \"$HOME/result\"\nfalse\nend"
    } else {
        "{\nprintf 'one\\ntwo\\n' > \"$HOME/result\"\nfalse\n}"
    };
    let deadline = std::time::Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("test deadline");
    let prompt = loop {
        let prompt = session
            .prompt(None)
            .unwrap()
            .receive("prompt")
            .unwrap()
            .unwrap();
        if prompt.editable {
            let result = session
                .prompt(Some((prompt.revision, command.to_owned(), false)))
                .unwrap()
                .receive("handoff")
                .unwrap();
            match result {
                Ok(_) => break prompt,
                // Fish can redraw between observation and claim. Reacquire a lease;
                // the worker must continue rejecting the stale one without inserting text.
                Err(error) if error.starts_with("Prompt changed or shell owns its input") => {}
                Err(error) => panic!("handoff failed: {error}"),
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "shell did not accept an empty-prompt lease: {prompt:?}"
        );
        wakes.recv_timeout(Duration::from_millis(500)).ok();
    };
    assert!(!home.child("result").path().exists());
    assert!(
        session
            .prompt(Some((prompt.revision, "echo WRONG".to_owned(), true)))
            .unwrap()
            .receive("stale")
            .unwrap()
            .is_err()
    );
    session.write_input(b"\r").unwrap();
    loop {
        let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
        if event.effect
            == TerminalSideEffect::ShellLifecycle(ShellEvent::CommandFinish { exit_code: Some(1) })
        {
            break;
        }
    }
    home.child("result").assert("one\ntwo\n");
    let prompt = session
        .prompt(None)
        .unwrap()
        .receive("history")
        .unwrap()
        .unwrap();
    assert!(
        prompt
            .recent
            .iter()
            .any(|record| record.command.contains("false") && record.exit_code == Some(1)),
        "{name}: {:?}",
        prompt.recent
    );
}
