#![cfg(test)]

use bootty_agents::{AgentKind, NativeSessionConfig, TerminalQueryOutput};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::{fs, path::Path, process::Command};

pub(crate) fn config(root: &Path, answer: &Value) -> NativeSessionConfig {
    fs::write(root.join("answer.json"), answer.to_string()).unwrap();
    let mut config = NativeSessionConfig::new(AgentKind::Codex, root);
    config.program = root
        .join("fixture-quick-provider")
        .to_string_lossy()
        .into_owned();
    config.account_directory = Some(root.to_string_lossy().into_owned());
    config
}

pub(crate) fn output(command: &Command, expected: &str) -> TerminalQueryOutput {
    let args = command
        .get_args()
        .map(|argument| argument.to_str().unwrap())
        .collect::<Vec<_>>();
    let value = |flag| {
        args[args
            .iter()
            .position(|argument| *argument == flag)
            .unwrap()
            .checked_add(1)
            .unwrap()]
    };
    assert!(args.contains(&"exec") && args.contains(&"--ephemeral"));
    assert_eq!(value("--sandbox"), "read-only");
    assert_eq!(value("--model"), "quick-model");
    assert_eq!(
        args.iter()
            .filter(|argument| **argument == "--model")
            .count(),
        1
    );
    for setting in ["features.shell_tool=false", "mcp_servers={}"] {
        assert!(args.contains(&setting));
    }
    assert!(args.last().unwrap().contains(expected));
    let account = command
        .get_envs()
        .find(|(name, _)| *name == "CODEX_HOME")
        .unwrap()
        .1
        .unwrap();
    let account = Path::new(account);
    assert_eq!(
        Path::new(command.get_program()),
        account.join("fixture-quick-provider")
    );
    let directory = command.get_current_dir().unwrap();
    assert_ne!(directory, account);
    let schema: Value =
        serde_json::from_slice(&fs::read(value("--output-schema")).unwrap()).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        Path::new(value("--output-last-message")).parent(),
        Some(directory)
    );
    fs::write(
        account.join("query-directory"),
        directory.to_string_lossy().as_bytes(),
    )
    .unwrap();
    fs::write(
        value("--output-last-message"),
        fs::read(account.join("answer.json")).unwrap(),
    )
    .unwrap();
    TerminalQueryOutput {
        successful: true,
        exit_code: Some(0),
        stdout: Vec::new(),
        stderr: Vec::new(),
    }
}
