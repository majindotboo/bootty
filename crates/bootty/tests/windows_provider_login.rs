#![cfg(windows)]

use assert_fs::{TempDir, prelude::*};
use bootty_agents::{AgentKind, AgentLaunch, LaunchShell, TerminalAgentService};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

#[rstest]
#[case(0, true)]
#[case(17, false)]
fn login_relay_preserves_account_arguments_and_exit_status(
    #[case] exit_code: i32,
    #[case] explicit_account: bool,
) {
    let root = TempDir::new().unwrap();
    root.child("provider").write_str("#!/bin/sh\n").unwrap();
    root.child("provider.cmd")
        .write_str("@echo off\r\n@python -u \"%~dp0provider.py\" %*\r\n")
        .unwrap();
    root.child("provider.py")
        .write_str(
            r"
import json,os,pathlib,sys
result = {'args':sys.argv[1:], 'account':os.environ.get('CLAUDE_CONFIG_DIR'), 'cwd':os.getcwd(), 'input':sys.stdin.readline().rstrip('\n')}
pathlib.Path(os.environ['BOOTTY_TEST_LOGIN_RESULT']).write_text(json.dumps(result))
print('login output marker', flush=True)
sys.exit(int(os.environ['BOOTTY_TEST_EXIT']))
",
        )
        .unwrap();
    let account = root
        .path()
        .join("account ' $HOME & ü")
        .to_string_lossy()
        .into_owned();
    let arguments = [
        "auth",
        "login",
        "%PATH%",
        "a&b",
        "x'y",
        "quote\"inside",
        "",
        "trailing\\",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let launch = AgentLaunch {
        program: root.path().join("provider").to_string_lossy().into_owned(),
        cwd: Some(root.path().to_string_lossy().into_owned()),
        arguments: arguments.clone(),
        ephemeral: false,
        account_directory: explicit_account.then(|| account.clone()),
    };
    let prepared =
        TerminalAgentService::prepare_unobserved(AgentKind::Claude, launch, "login fixture".into())
            .unwrap();
    let argv = prepared
        .argv_for_host(
            LaunchShell::Windows,
            Path::new(env!("CARGO_BIN_EXE_bootty")),
        )
        .unwrap();
    let result = root.path().join("result.json");
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .env("BOOTTY_TEST_LOGIN_RESULT", &result)
        .env("BOOTTY_TEST_EXIT", exit_code.to_string())
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("LOCALAPPDATA", root.path())
        .env("APPDATA", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"literal auth input & value\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(exit_code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let received: serde_json::Value =
        serde_json::from_slice(&std::fs::read(result).unwrap()).unwrap();
    assert_eq!(received["args"], serde_json::json!(arguments));
    assert_eq!(received["input"], "literal auth input & value");
    assert!(String::from_utf8_lossy(&output.stdout).contains("login output marker"));
    assert_eq!(
        received["account"],
        serde_json::json!(explicit_account.then_some(account))
    );
    assert_eq!(
        Path::new(received["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        root.path().canonicalize().unwrap()
    );
}
