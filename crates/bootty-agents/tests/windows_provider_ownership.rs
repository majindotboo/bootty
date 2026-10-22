#![cfg(windows)]

use assert_fs::{TempDir, prelude::*};
use bootty_agents::{AgentKind, terminal_provider_installer, terminal_provider_update};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{path::Path, process::Command};

fn npm_fixture(
    root: &Path,
    command: &str,
    package: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = assert_fs::fixture::ChildPath::new(root);
    directory
        .child(format!("node_modules/{package}/package.json"))
        .write_str(
            &serde_json::json!({
                "name": package,
                "bin": {command: "bin/cli.js"}
            })
            .to_string(),
        )?;
    directory
        .child(format!("node_modules/{package}/bin/cli.js"))
        .write_str("fixture")?;
    directory
        .child(format!("{command}.cmd"))
        .write_str(&format!(
            "@echo off\r\n@node \"%dp0%\\node_modules\\{}\\bin\\cli.js\" %*\r\n",
            package.replace('/', "\\")
        ))?;
    directory
        .child("npm.cmd")
        .write_str("@echo off\r\n@exit /b 0\r\n")?;
    Ok(())
}

#[rstest]
#[case(AgentKind::Codex, "codex", "@openai/codex")]
#[case(AgentKind::Claude, "claude", "@anthropic-ai/claude-code")]
#[case(AgentKind::Pi, "pi", "@earendil-works/pi-coding-agent")]
fn npm_shims_update_only_their_selected_prefix(
    #[case] provider: AgentKind,
    #[case] command: &str,
    #[case] package: &str,
) {
    let root = TempDir::new().unwrap();
    let prefix = root.child("npm prefix & ' ü");
    npm_fixture(prefix.path(), command, package).unwrap();
    let selected = prefix.child(format!("{command}.cmd"));
    let argv = terminal_provider_installer(provider, selected.path()).unwrap();
    assert_eq!(
        argv,
        vec![
            prefix
                .child("npm.cmd")
                .path()
                .to_string_lossy()
                .into_owned(),
            "install".into(),
            "--global".into(),
            "--prefix".into(),
            prefix.path().to_string_lossy().into_owned(),
            format!("{package}@latest"),
        ]
    );
    let launch = terminal_provider_update(provider, selected.path().to_str().unwrap()).unwrap();
    assert_eq!(launch.program, argv[0]);
    assert_eq!(launch.arguments, argv[1..]);
    assert!(launch.ephemeral);
    assert!(launch.account_directory.is_none());
}

#[rstest]
#[case("wrong_package")]
#[case("wrong_entry")]
#[case("missing_entry")]
#[case("unrelated_shim")]
#[case("escaping_entry")]
fn npm_neighbors_do_not_prove_ownership(
    #[case] fault: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TempDir::new().unwrap();
    npm_fixture(root.path(), "codex", "@openai/codex").unwrap();
    let manifest = root.child("node_modules/@openai/codex/package.json");
    match fault {
        "wrong_package" => manifest
            .write_str(r#"{"name":"other","bin":{"codex":"bin/cli.js"}}"#)
            .unwrap(),
        "wrong_entry" => manifest
            .write_str(r#"{"name":"@openai/codex","bin":{"codex":"bin/other.js"}}"#)
            .unwrap(),
        "missing_entry" => {
            std::fs::remove_file(root.child("node_modules/@openai/codex/bin/cli.js").path())
                .unwrap();
        }
        "unrelated_shim" => root
            .child("codex.cmd")
            .write_str("@echo custom executable\r\n")
            .unwrap(),
        "escaping_entry" => manifest
            .write_str(r#"{"name":"@openai/codex","bin":{"codex":"../other/cli.js"}}"#)
            .unwrap(),
        other => return Err(format!("Unknown ownership fixture: {other}").into()),
    }
    assert_eq!(
        terminal_provider_installer(AgentKind::Codex, root.child("codex.cmd").path()),
        None
    );
    Ok(())
}

#[test]
fn native_claude_copy_uses_its_selected_executable_and_manual_copy_stays_manual() {
    let root = TempDir::new().unwrap();
    let native = root.child(".local/bin/claude.exe");
    native.write_binary(b"MZfixture").unwrap();
    assert_eq!(
        terminal_provider_installer(AgentKind::Claude, native.path()),
        Some(vec![
            native.path().to_string_lossy().into_owned(),
            "update".into(),
        ])
    );
    let manual = root.child("manual/claude.exe");
    manual.write_binary(b"MZfixture").unwrap();
    assert_eq!(
        terminal_provider_installer(AgentKind::Claude, manual.path()),
        None
    );
    assert!(terminal_provider_update(AgentKind::Claude, manual.path().to_str().unwrap()).is_err());
}

#[test]
fn npm_update_execution_keeps_literal_prefix_and_exit_status() {
    let root = TempDir::new().unwrap();
    let prefix = root.child("npm prefix & ' ü");
    npm_fixture(prefix.path(), "codex", "@openai/codex").unwrap();
    prefix
        .child("npm.cmd")
        .write_str("@echo off\r\n@python \"%~dp0record.py\" %*\r\n")
        .unwrap();
    prefix
        .child("record.py")
        .write_str("import json,sys\nprint(json.dumps(sys.argv[1:]))\nsys.exit(7)\n")
        .unwrap();
    let launch = terminal_provider_update(
        AgentKind::Codex,
        prefix.child("codex.cmd").path().to_str().unwrap(),
    )
    .unwrap();
    let output = bootty_host::local_command(&launch.program)
        .args(&launch.arguments)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let args: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        args,
        vec![
            "install".to_owned(),
            "--global".into(),
            "--prefix".into(),
            prefix.path().to_string_lossy().into_owned(),
            "@openai/codex@latest".into()
        ]
    );
    let update = bootty_agents::ProviderUpdate::prepare(
        AgentKind::Codex,
        prefix.child("codex.cmd").path().to_str().unwrap(),
    )
    .unwrap()
    .unwrap();
    let error = update
        .run(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            || false,
        )
        .unwrap_err();
    assert!(error.contains("Some(7)"), "{error}");
    assert!(
        update
            .run(
                std::time::Instant::now() + std::time::Duration::from_secs(30),
                || true
            )
            .unwrap_err()
            .contains("cancelled")
    );
}

#[test]
fn winget_claude_uses_its_package_manager_instead_of_native_update() {
    const FIXTURE: &str = "BOOTTY_TEST_WINGET_INSTALLER";
    if let Some(root) = std::env::var_os(FIXTURE) {
        let root = Path::new(&root);
        let selected = root.join("AppData/Local/Microsoft/WinGet/Packages/Anthropic.ClaudeCode_Microsoft.Winget.Source_8wekyb3d8bbwe/claude.exe");
        let argv = terminal_provider_installer(AgentKind::Claude, &selected).unwrap();
        assert_eq!(
            Path::new(&argv[0]).canonicalize().unwrap(),
            root.join("winget.exe").canonicalize().unwrap()
        );
        assert_eq!(
            argv[1..],
            [
                "upgrade",
                "--id",
                "Anthropic.ClaudeCode",
                "--exact",
                "--source",
                "winget",
                "--scope",
                "user",
                "--silent",
                "--disable-interactivity",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ]
        );
        assert_eq!(
            terminal_provider_installer(AgentKind::Codex, &selected),
            None
        );
        std::fs::remove_file(root.join("winget.exe")).unwrap();
        assert_eq!(
            terminal_provider_installer(AgentKind::Claude, &selected),
            None
        );
        return;
    }
    let root = TempDir::new().unwrap();
    root.child("AppData/Local/Microsoft/WinGet/Packages/Anthropic.ClaudeCode_Microsoft.Winget.Source_8wekyb3d8bbwe/claude.exe")
        .write_binary(b"MZfixture").unwrap();
    root.child("winget.exe").write_binary(b"MZfixture").unwrap();
    // Isolate installer discovery without mutating the test runner's environment.
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "winget_claude_uses_its_package_manager_instead_of_native_update",
            "--nocapture",
        ])
        .env(FIXTURE, root.path())
        .env("PATH", root.path())
        .env("PATHEXT", ".EXE;.CMD")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
