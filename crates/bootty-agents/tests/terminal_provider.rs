use assert_fs::prelude::*;
use bootty_agents::{AgentKind, AgentLaunch, TerminalAgentService, terminal_provider_installer};
use pretty_assertions::assert_eq;
use rstest::rstest;
mod support;

#[rstest]
#[case(AgentKind::Codex, "CODEX_HOME")]
#[case(AgentKind::Claude, "CLAUDE_CONFIG_DIR")]
#[case(AgentKind::Pi, "PI_CODING_AGENT_DIR")]
fn account_store_and_arguments_are_literal(#[case] provider: AgentKind, #[case] variable: &str) {
    let account = support::synthetic_path("private/account ' ; $HOME")
        .to_string_lossy()
        .into_owned();
    let launch = AgentLaunch {
        program: "provider".to_owned(),
        cwd: None,
        arguments: vec!["a; $HOME".to_owned()],
        ephemeral: false,
        account_directory: Some(account.clone()),
    };
    let prepared =
        TerminalAgentService::prepare_unobserved(provider, launch.clone(), "test host".to_owned())
            .unwrap();
    assert_eq!(
        prepared
            .argv_for_shell(bootty_agents::LaunchShell::Posix)
            .unwrap(),
        [
            "env",
            &format!("{variable}={account}"),
            "provider",
            "a; $HOME"
        ]
    );
    assert_eq!(
        launch.retained(provider).account_directory,
        launch.account_directory
    );
}

#[rstest]
#[case(AgentKind::Codex, "@openai/codex")]
#[case(AgentKind::Pi, "@earendil-works/pi-coding-agent")]
fn updates_respect_supported_installer_and_proven_global_installation(
    #[case] provider: AgentKind,
    #[case] package: &str,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let executable = directory.child(format!("install/global/node_modules/{package}/bin/cli.js"));
    executable.write_str("fixture").unwrap();
    directory
        .child(format!(
            "install/global/node_modules/{package}/package.json"
        ))
        .write_str(&format!(r#"{{"name":"{package}"}}"#))
        .unwrap();
    let bun = directory.child("bin/bun");
    bun.write_str("fixture").unwrap();
    assert_eq!(
        terminal_provider_installer(provider, executable.path()),
        cfg!(unix).then(|| vec![
            "env".to_owned(),
            format!(
                "BUN_INSTALL_GLOBAL_DIR={}",
                directory.path().join("install/global").display()
            ),
            format!("BUN_INSTALL_BIN={}", bun.path().parent().unwrap().display()),
            bun.path().to_string_lossy().into_owned(),
            "update".to_owned(),
            "--global".to_owned(),
            "--latest".to_owned(),
            package.to_owned()
        ])
    );
    assert_eq!(
        terminal_provider_installer(provider, directory.child("manual/provider").path()),
        None
    );
    assert_eq!(
        terminal_provider_installer(AgentKind::Claude, executable.path()),
        None
    );
}

#[test]
fn native_claude_installation_uses_its_own_updater() {
    let directory = assert_fs::TempDir::new().unwrap();
    let executable = directory.child("claude/versions/1.0.0.exe");
    executable.write_str("fixture").unwrap();
    assert_eq!(
        terminal_provider_installer(AgentKind::Claude, executable.path()),
        Some(vec![
            executable.path().to_string_lossy().into_owned(),
            "update".into()
        ])
    );
}

#[cfg(unix)]
#[rstest]
fn update_retains_the_real_exit_status_until_acknowledged() {
    use bootty_agents::terminal_provider_update;
    use std::io::{BufRead as _, Write as _};

    use std::process::{Command, Stdio};

    let temporary = assert_fs::TempDir::new().unwrap();
    let directory = temporary.child("install ' ; $HOME");
    let package = "@openai/codex";
    let executable = directory.child(format!("install/global/node_modules/{package}/bin/cli.js"));
    bootty_test_support::write_executable(executable.path(), "#!/bin/sh\nexit 0\n").unwrap();
    directory
        .child(format!(
            "install/global/node_modules/{package}/package.json"
        ))
        .write_str(&format!(r#"{{"name":"{package}"}}"#))
        .unwrap();
    let installer = directory.child("bin/bun");
    bootty_test_support::write_executable(installer.path(), "#!/bin/sh\nprintf '%s\\n' \"$BUN_INSTALL_GLOBAL_DIR\" \"$BUN_INSTALL_BIN\" \"$@\"\nexit 7\n").unwrap();
    let launch =
        terminal_provider_update(AgentKind::Codex, executable.path().to_str().unwrap()).unwrap();
    let mut child = Command::new(launch.program)
        .args(launch.arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "Installer result disappeared"
        );
        let done = line.contains("Press Enter to close");
        lines.push(line.trim_end().to_owned());
        if done {
            break;
        }
    }
    assert_eq!(
        lines,
        vec![
            directory
                .path()
                .join("install/global")
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            directory
                .path()
                .join("bin")
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "update".to_owned(),
            "--global".to_owned(),
            "--latest".to_owned(),
            package.to_owned(),
            String::new(),
            "Update exited with status 7. Press Enter to close.".to_owned(),
        ]
    );
    assert!(child.try_wait().unwrap().is_none());
    child.stdin.take().unwrap().write_all(b"\n").unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(7));
}
