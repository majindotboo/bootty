#![cfg(windows)]

use std::{
    fs,
    io::{BufRead as _, BufReader, Cursor, Write as _},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bootty_agents::{
    AgentKind, NativeAgentSession, NativeSessionConfig, ToolBridge, ToolBridgeContext, ToolPolicy,
    ToolScope, tool_stdio,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use serde_json::{Value, json};

fn context() -> ToolBridgeContext {
    ToolBridgeContext {
        scope: ToolScope {
            provider: AgentKind::Codex,
            binding: CommandTarget {
                kind: ResourceKind::Binding,
                handle: "windows-binding".into(),
                generation: 7,
            },
        },
        caller: Caller::Cli,
        policy: ToolPolicy::own_terminal(),
        captures: Vec::new(),
        spawn: None,
    }
}

fn connection(bridge: &ToolBridge) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let arguments = bridge.arguments();
    let serialized = arguments
        .iter()
        .find_map(|argument| argument.split_once(".args=").map(|(_, value)| value))
        .ok_or("Missing MCP stdio args")?;
    let args: Vec<String> = serde_json::from_str(serialized)?;
    Ok(PathBuf::from(args.get(1).ok_or("Missing connection file")?))
}

#[test]
fn windows_private_stdio_authenticates_dispatches_and_retires() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let executor = move |invocation: CommandInvocation, _: Instant, _: CommandCancellation| {
        recorded.lock().unwrap().push(invocation);
        CommandOutcome::Success {
            value: json!({"text":"x".repeat(128 * 1024)}),
            warnings: Vec::new(),
        }
    };
    let context = context();
    let binding = context.scope.binding.clone();
    let bridge = ToolBridge::prepare(
        context,
        &std::env::current_exe().unwrap(),
        Arc::new(executor),
    )
    .unwrap();
    let terminal = CommandTarget {
        kind: ResourceKind::Terminal,
        handle: "owned-terminal".into(),
        generation: 9,
    };
    bridge.lease().bind(&binding, terminal.clone()).unwrap();
    let connection = connection(&bridge).unwrap();
    let requests = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"terminal_read\",\"arguments\":{}}}\n"
    );
    let mut output = Vec::new();
    tool_stdio(&connection, &mut Cursor::new(requests), &mut output).unwrap();
    let responses: Vec<Value> = String::from_utf8(output.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 3);
    assert_eq!(responses.last().unwrap()["result"]["isError"], false);
    assert!(
        responses
            .last()
            .unwrap()
            .to_string()
            .contains(&"x".repeat(128 * 1024))
    );
    let actual = calls.lock().unwrap().clone();
    assert_eq!(actual.len(), 1);
    assert_eq!(actual.first().unwrap().caller, Caller::Cli);
    assert_eq!(actual.first().unwrap().target, Some(terminal));
    let secret: Value = serde_json::from_slice(&fs::read(&connection).unwrap()).unwrap();
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains(secret["token"].as_str().unwrap())
    );
    let endpoint =
        rmux_ipc::LocalEndpoint::from_path(PathBuf::from(secret["socket"].as_str().unwrap()));
    let mut forged = rmux_ipc::connect_blocking(&endpoint, Duration::from_secs(2)).unwrap();
    forged
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    forged.write_all(b"{\"token\":\"forged\",\"request\":{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"terminal_read\"}}}\n").unwrap();
    let mut discarded = Vec::new();
    let read = BufReader::new(forged).read_until(b'\n', &mut discarded);
    assert!(matches!(read, Ok(0)) || read.is_err());
    assert_eq!(discarded, [0_u8; 0]);
    assert_eq!(calls.lock().unwrap().len(), 1);
    bridge.stop();
    assert!(!bridge.lease().enabled(None));
    for _ in 0..100 {
        if !connection.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !connection.exists(),
        "Stopped transport must remove its private files"
    );
    assert!(tool_stdio(&connection, &mut Cursor::new(requests), &mut Vec::new()).is_err());
}

#[test]
#[ignore = "Requires BOOTTY_TEST_NODE to name a Node.js executable; runs a local provider fixture, no API calls."]
fn windows_native_provider_uses_stdio_and_owns_its_descendants() {
    let root = assert_fs::TempDir::new().unwrap();
    fs::write(root.path().join("app-server"), r"
const readline = require('node:readline');
const {spawn} = require('node:child_process');
readline.createInterface({input:process.stdin}).on('line', line => {
  const message = JSON.parse(line);
  if (message.id === undefined) return;
  let result = {};
  if (message.method === 'thread/start') result = {thread:{id:'windows-fixture-thread',turns:[]}};
  if (message.method === '__launch') result = {argv:process.argv.slice(2),cwd:process.cwd(),account:process.env.CODEX_HOME,pid:process.pid};
  if (message.method === '__child') result = {pid:spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore',windowsHide:true}).pid};
  process.stdout.write(JSON.stringify({id:message.id,result})+'\n');
});
").unwrap();
    let mut config = NativeSessionConfig::new(AgentKind::Codex, root.path());
    config.program = std::env::var("BOOTTY_TEST_NODE").unwrap();
    config.account_directory = Some(root.path().to_string_lossy().into_owned());
    let session = NativeAgentSession::spawn(config).unwrap();
    assert_eq!(
        session.snapshot().session_id.as_deref(),
        Some("windows-fixture-thread")
    );
    let launch = session.rpc("__launch", json!({})).unwrap();
    assert_eq!(launch["account"], root.path().to_string_lossy().as_ref());
    assert_eq!(launch["argv"][0], "--listen");
    assert_eq!(launch["argv"][1], "stdio://");
    let child = session.rpc("__child", json!({})).unwrap();
    let child_pid = u32::try_from(child["pid"].as_u64().unwrap()).unwrap();
    let parent_pid = u32::try_from(launch["pid"].as_u64().unwrap()).unwrap();
    session.stop();
    let inspector = rmux_os::process::ProcessInspector;
    assert_eq!(inspector.is_live(parent_pid).unwrap(), Some(false));
    assert_eq!(inspector.is_live(child_pid).unwrap(), Some(false));
}
