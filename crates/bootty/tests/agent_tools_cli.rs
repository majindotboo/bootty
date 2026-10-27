#![cfg(test)]
#![cfg(unix)]

use std::{
    fs,
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Instant,
};

use assert_fs::TempDir;
use bootty_agents::{
    AgentCommandExecutor, AgentKind, ToolBridge, ToolBridgeContext, ToolPolicy, ToolScope,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn target(kind: ResourceKind, handle: &str, generation: u64) -> CommandTarget {
    CommandTarget {
        kind,
        handle: handle.to_owned(),
        generation,
    }
}

fn bridge_context(binding: CommandTarget) -> ToolBridgeContext {
    ToolBridgeContext {
        scope: ToolScope {
            provider: AgentKind::Codex,
            binding,
        },
        caller: Caller::Cli,
        policy: ToolPolicy::own_terminal(),
        captures: Vec::new(),
        spawn: None,
    }
}

#[test]
fn malformed_hidden_stdio_arguments_reject_paths_without_echoing_secrets() {
    let cases = [
        vec!["--agent-tool-stdio"],
        vec!["--agent-tool-stdio", "relative-secret-connection"],
        vec![
            "--agent-tool-stdio",
            "/secret/private-connection",
            "secret-extra-argument",
        ],
    ];

    for arguments in &cases {
        let error = bootty::cli::agent_tool_stdio_connection(
            arguments.iter().map(std::ffi::OsString::from),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("--agent-tool-stdio"));
        assert!(!error.contains("secret"));
    }
}

#[test]
fn hidden_stdio_mode_round_trips_mcp_through_tool_bridge() {
    let root = TempDir::new().expect("isolated roots");
    let config = root.path().join("config");
    let runtime = root.path().join("runtime");
    let rmux = root.path().join("rmux");
    let binding = target(ResourceKind::Binding, "captured binding", 19);
    let terminal = target(ResourceKind::Terminal, "captured terminal", 23);
    let calls = Arc::new(Mutex::new(Vec::<CommandInvocation>::new()));
    let recorded_calls = Arc::clone(&calls);
    let commands: Arc<dyn AgentCommandExecutor> = Arc::new(
        move |invocation: CommandInvocation, _: Instant, _: CommandCancellation| {
            recorded_calls
                .lock()
                .expect("recording lock")
                .push(invocation);
            CommandOutcome::Success {
                value: json!({"text":"round trip"}),
                warnings: Vec::new(),
            }
        },
    );
    let bridge = ToolBridge::prepare(
        bridge_context(binding.clone()),
        Path::new(env!("CARGO_BIN_EXE_bootty")),
        commands,
    )
    .expect("prepare private tool bridge");
    bridge
        .lease()
        .bind(&binding, terminal.clone())
        .expect("bind exact terminal");

    let bridge_arguments = bridge.arguments();
    let encoded_arguments = bridge_arguments
        .iter()
        .find_map(|argument| argument.split_once(".args=").map(|(_, value)| value))
        .expect("Codex launch arguments include the stdio connection");
    let arguments: Vec<String> = serde_json::from_str(encoded_arguments).expect("decode args");
    assert_eq!(arguments[0], "--agent-tool-stdio");
    let connection = &arguments[1];
    let secret: Value = serde_json::from_slice(&fs::read(connection).expect("read private file"))
        .expect("decode private file");
    let token = secret["token"].as_str().expect("connection token");

    let mut child = Command::new(env!("CARGO_BIN_EXE_bootty"))
        .args(&arguments)
        .env("HOME", root.path().join("home"))
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("RMUX_TMPDIR", &rmux)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start hidden stdio process");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(
            concat!(
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n",
                "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"terminal_read\"}}\n",
            )
            .as_bytes(),
        )
        .expect("send MCP requests");
    let output = child.wait_with_output().expect("wait for stdio process");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, Vec::<u8>::new());
    assert!(!config.exists());
    assert!(!runtime.exists());
    assert!(!rmux.exists());
    let stdout = String::from_utf8(output.stdout).expect("MCP output is UTF-8");
    assert!(!stdout.contains(connection));
    assert!(!stdout.contains(token));
    let responses = stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap_or_else(|error| {
                panic!(
                    "invalid MCP response line {line:?}: {error}; stdout bytes: {:?}",
                    stdout.as_bytes()
                )
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 3);
    assert_eq!(responses[2]["result"]["isError"], false);
    let calls = calls.lock().expect("recording lock");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].command, "terminal.capture");
    assert_eq!(calls[0].caller, Caller::Cli);
    assert_eq!(calls[0].target, Some(terminal));
    drop(calls);
    bridge.stop();
}
