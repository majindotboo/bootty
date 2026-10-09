#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::Instant,
};

use assert_fs::TempDir;
use bootty_agents::{
    AgentCommandExecutor, AgentKind, NativeAgentRequest, NativeAgentService, NativeSessionConfig,
    TerminalAgentService, ToolBridge, ToolBridgeContext, ToolLease, ToolPolicy, ToolProtocol,
    ToolScope, ToolSpawnContext, ToolSpawnRequest,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Value, json};

#[derive(Default)]
struct Commands(Mutex<Vec<CommandInvocation>>);

impl AgentCommandExecutor for Commands {
    fn execute(
        &self,
        invocation: CommandInvocation,
        _: Instant,
        _: CommandCancellation,
    ) -> CommandOutcome {
        let value = match invocation.command.as_str() {
            "agents.native.models" | "agents.native.activities" => json!([]),
            "agents.native.status" => json!({
                "id": invocation.target.as_ref().map(|target| &target.handle),
                "title":"Inspect changes", "status":"idle"
            }),
            _ => json!({"text":"captured task shell"}),
        };
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(invocation);
        CommandOutcome::Success {
            value,
            warnings: Vec::new(),
        }
    }
}

fn target(kind: ResourceKind, handle: &str) -> CommandTarget {
    CommandTarget {
        kind,
        handle: handle.to_owned(),
        generation: 17,
    }
}

fn bridge(
    commands: &Arc<Commands>,
    caller: Caller,
    terminal: Option<&CommandTarget>,
) -> Result<(ToolBridge, ToolLease), String> {
    let binding = target(ResourceKind::Binding, "opaque captured binding");
    let tools = ToolBridge::prepare(
        ToolBridgeContext {
            scope: ToolScope {
                provider: AgentKind::Codex,
                binding: binding.clone(),
            },
            caller,
            policy: ToolPolicy::own_terminal(),
            captures: Vec::new(),
            spawn: None,
        },
        &std::env::current_exe().map_err(|error| error.to_string())?,
        commands.clone(),
    )?;
    let lease = tools.lease().clone();
    if let Some(terminal) = terminal {
        lease.bind(&binding, terminal.clone())?;
    }
    Ok((tools, lease))
}

// The fake app-server consumes the actual ephemeral MCP overrides and private transport.
// It returns each MCP result through a normal provider RPC, without printing private tokens.
fn provider(root: &Path) -> std::io::Result<NativeSessionConfig> {
    let path = root.join("provider.py");
    fs::write(
        &path,
        r"#!/usr/bin/env python3
import json,socket,sys
assert sys.argv[1:4] == ['app-server','--listen','stdio://']
config={}
for i in range(4,len(sys.argv),2):
 assert sys.argv[i] == '--config'
 key,value=sys.argv[i+1].split('=',1)
 config[key]=json.loads(value)
key=next(key for key in config if key.startswith('mcp_servers.') and key.endswith('.args'))
name=key.removesuffix('.args')
assert config[name+'.required'] is True
assert config[name+'.command']
args=config[key]
assert args[0]=='--agent-tool-stdio'
with open(args[1]) as f: connection=json.load(f)
def tools(method,params={}):
 request={'jsonrpc':'2.0','id':1,'method':method,'params':params}
 with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as stream:
  stream.connect(connection['socket'])
  stream.sendall((json.dumps({'token':connection['token'],'request':request})+'\n').encode())
  return json.loads(stream.makefile().readline())['result']
def emit(value): print(json.dumps(value),flush=True)
initial_catalog=None
for line in sys.stdin:
 value=json.loads(line)
 method=value.get('method')
 if method=='initialized': continue
 if method=='initialize': result={}
 elif method in ['thread/start','thread/resume']:
  initial_catalog=tools('tools/list')
  result={'thread':{'id':'tools-thread','turns':[]}}
 elif method=='__tools': result={'catalog':initial_catalog,'read':tools('tools/call',{'name':'terminal_read','arguments':{'scope':'history','max_lines':7}}),'status':tools('tools/call',{'name':'get_agent_status'}),'models':tools('tools/call',{'name':'list_models'}),'computer':tools('tools/call',{'name':'computer_snapshot','arguments':{'application':'unmentioned'}})}
 elif method=='__approval':
  server=value['params']['server']
  if server=='attached': server=name.removeprefix('mcp_servers.')
  emit({'id':'approval','method':'mcpServer/elicitation/request','params':{'threadId':'tools-thread','turnId':None,'serverName':server,'mode':'form','message':'Allow this tool?','requestedSchema':{'type':'object','properties':{}}}})
  result={}
 else: result={}
 emit({'id':value['id'],'result':result})
",
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    let mut config = NativeSessionConfig::new(AgentKind::Codex, root);
    config.program = path.to_string_lossy().into_owned();
    config.account_directory = Some(root.join("captured-account").to_string_lossy().into_owned());
    config.arguments = vec!["--model".to_owned(), "captured-model".to_owned()];
    Ok(config)
}

#[rstest]
#[case::attached("attached", true)]
#[case::other_attachment(
    "bootty_0000000000000000000000000000000000000000000000000000000000000000",
    false
)]
#[case::external("external-tools", false)]
fn mcp_requests_identify_only_the_captured_attachment(
    #[case] server: &str,
    #[case] attached: bool,
) {
    let root = TempDir::new().unwrap();
    let commands = Arc::new(Commands::default());
    let (tools, _) = bridge(
        &commands,
        Caller::Socket,
        Some(&target(ResourceKind::Terminal, "task pane")),
    )
    .unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task_with_tools(
            "binding",
            "task",
            "Confirm tool",
            provider(root.path()).unwrap(),
            Arc::new(tools),
        )
        .unwrap();
    let session = service.resolve(&record.target()).unwrap();
    session.rpc("__approval", json!({"server":server})).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.requests.len(), 1);
    assert!(snapshot.requests[0].is_mcp_approval());
    assert_eq!(snapshot.requests[0].is_from_attached_tools(), attached);
    let mut wire = serde_json::to_value(&snapshot.requests[0]).unwrap();
    let forwarded: NativeAgentRequest = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(forwarded.is_from_attached_tools(), attached);
    assert_eq!(forwarded.parameters, snapshot.requests[0].parameters);
    wire.as_object_mut().unwrap().remove("from_attached_tools");
    let previous: NativeAgentRequest = serde_json::from_value(wire).unwrap();
    assert!(!previous.is_from_attached_tools());
}

#[rstest]
#[case(true)]
#[case(false)]
fn spawned_child_publishes_before_startup_and_retains_failure(#[case] reject_placement: bool) {
    let root = TempDir::new().unwrap();
    let commands = Arc::new(Commands::default());
    let binding = target(ResourceKind::Binding, "captured binding");
    let parent_tools = ToolBridge::prepare(
        ToolBridgeContext {
            scope: ToolScope {
                provider: AgentKind::Codex,
                binding: binding.clone(),
            },
            caller: Caller::Internal,
            policy: ToolPolicy {
                spawn_children: true,
                ..ToolPolicy::own_terminal()
            },
            captures: Vec::new(),
            spawn: Some(ToolSpawnContext { profile: None }),
        },
        &std::env::current_exe().unwrap(),
        commands.clone(),
    )
    .unwrap();
    parent_tools
        .lease()
        .bind(&binding, target(ResourceKind::Terminal, "parent pane"))
        .unwrap();
    let parent_lease = parent_tools.lease().clone();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let parent = service
        .create_for_task_with_tools(
            "binding",
            "parent task",
            "Parent",
            provider(root.path()).unwrap(),
            Arc::new(parent_tools),
        )
        .unwrap();
    let authority = parent_lease
        .authorize_spawn(&ToolSpawnRequest::Agent {
            name: "child".into(),
            title: None,
            provider: AgentKind::Codex,
            profile: None,
            prompt: "Child request".into(),
        })
        .unwrap();
    let child_tools =
        ToolBridge::prepare_child(authority, &std::env::current_exe().unwrap(), commands).unwrap();
    child_tools
        .lease()
        .bind(&binding, target(ResourceKind::Terminal, "child pane"))
        .unwrap();
    let published = std::cell::RefCell::new(None);
    let program = root.path().join("provider.py");
    let disabled = root.path().join("provider.disabled");
    let result = service.create_spawned_for_task_placed(
        &parent.target(),
        "child task",
        "Child",
        Arc::new(child_tools),
        |child| {
            *published.borrow_mut() = Some(child.target());
            assert_eq!(
                child.snapshot.status,
                bootty_agents::NativeSessionStatus::Starting
            );
            assert_eq!(
                child.config.account_directory,
                parent.config.account_directory
            );
            assert_eq!(child.config.cwd, parent.config.cwd);
            assert!(service.resolve(&child.target()).is_err());
            if reject_placement {
                Err("native placement rejected".into())
            } else {
                fs::rename(&program, &disabled).unwrap();
                Ok(())
            }
        },
    );
    if !reject_placement {
        fs::rename(&disabled, &program).unwrap();
    }
    let failed = result.unwrap_err();
    let target = published.into_inner().unwrap();
    assert_eq!(failed.target, Some(target.clone()));
    let child = service
        .sessions()
        .into_iter()
        .find(|record| record.target() == target)
        .unwrap();
    assert_eq!(
        child.snapshot.status,
        bootty_agents::NativeSessionStatus::Error
    );
    assert_eq!(child.spawn_parent, Some(parent.target()));
    assert_eq!(child.task_identity.as_deref(), Some("child task"));
    assert!(service.resolve(&target).is_err());
    assert_eq!(
        service.resolve(&parent.target()).unwrap().snapshot().status,
        bootty_agents::NativeSessionStatus::Idle
    );
}

fn catalog(lease: &ToolLease, commands: &Commands) -> Option<Value> {
    ToolProtocol::new(lease.clone())
        .handle(
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            Instant::now(),
            commands,
        )
        .and_then(|response| response.get("result")?.get("tools").cloned())
}

#[rstest]
fn browser_attachment_is_live_metadata_and_never_restored_from_history() {
    let root = TempDir::new().unwrap();
    let commands = Arc::new(Commands::default());
    let binding = target(ResourceKind::Binding, "captured binding");
    let tools = ToolBridge::prepare(
        ToolBridgeContext {
            scope: ToolScope {
                provider: AgentKind::Codex,
                binding: binding.clone(),
            },
            caller: Caller::Socket,
            policy: ToolPolicy {
                browser_capture: true,
                ..ToolPolicy::own_terminal()
            },
            captures: Vec::new(),
            spawn: None,
        },
        &std::env::current_exe().unwrap(),
        commands,
    )
    .unwrap();
    tools
        .lease()
        .bind(&binding, target(ResourceKind::Terminal, "task pane"))
        .unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let record = service
        .create_for_task_with_tools(
            "binding",
            "task",
            "Read page",
            provider(root.path()).unwrap(),
            Arc::new(tools),
        )
        .unwrap();
    let initial = service
        .resolve(&record.target())
        .unwrap()
        .rpc("__tools", json!({}))
        .unwrap();
    assert!(
        initial["catalog"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "browser_snapshot")
    );
    let attachment = bootty_agents::NativeBrowserAttachment {
        window: target(ResourceKind::ApplicationWindow, "host-window"),
        page: 19,
        document: "1234567890abcdef1234567890abcdef".into(),
    };
    service
        .attach_browser(&record.target(), Some(attachment.clone()))
        .unwrap();
    let observed = service
        .sessions()
        .into_iter()
        .find(|session| session.target() == record.target())
        .unwrap();
    assert_eq!(
        observed.snapshot.browser_access,
        bootty_agents::NativeBrowserAccess::Attached(attachment)
    );
    assert!(
        serde_json::to_value(&observed.snapshot)
            .unwrap()
            .get("browser_access")
            .is_none()
    );
    service.stop(&record.target()).unwrap();
    assert_eq!(
        service.sessions()[0].snapshot.browser_access,
        bootty_agents::NativeBrowserAccess::Unavailable
    );
    drop(service);
    let restored = NativeAgentService::open(&path).unwrap();
    assert_eq!(
        restored.sessions()[0].snapshot.browser_access,
        bootty_agents::NativeBrowserAccess::Unavailable
    );
    assert!(restored.attach_browser(&record.target(), None).is_err());
}

#[rstest]
#[case(Caller::Socket)]
#[case(Caller::Cli)]
fn native_mcp_attachment_preserves_exact_authority_and_resume_configuration(
    #[case] caller: Caller,
) {
    let root = TempDir::new().expect("Fixture directory");
    let commands = Arc::new(Commands::default());
    let terminal = target(ResourceKind::Terminal, "captured task terminal");
    let policy =
        TerminalAgentService::open(root.path().join("terminal.json")).expect("Policy owner");
    let service = NativeAgentService::open(root.path().join("native.json")).expect("Native owner");
    let config = provider(root.path()).expect("Provider fixture");
    let (tools, lease) = bridge(&commands, caller, Some(&terminal)).expect("Exact attachment");
    let tools = policy
        .retain_tool_attachment(AgentKind::Codex, tools)
        .expect("Admitted policy");
    let record = service
        .create_for_task_with_tools(
            "saved binding",
            "saved task",
            "Tools",
            config.clone(),
            tools,
        )
        .expect("Native launch");
    let response = service
        .resolve(&record.target())
        .expect("Live native session")
        .rpc("__tools", json!({}))
        .expect("MCP provider request");
    assert_eq!(response["computer"]["isError"], true);
    assert_eq!(
        response["catalog"]["tools"]
            .as_array()
            .expect("Catalog")
            .iter()
            .map(|tool| tool["name"].clone())
            .collect::<Vec<_>>(),
        [
            json!("get_workspace_info"),
            json!("list_terminals"),
            json!("list_agents"),
            json!("list_profiles"),
            json!("get_agent_activity"),
            json!("get_agent_status"),
            json!("list_models"),
            json!("list_providers"),
            json!("inspect_provider"),
            json!("terminal_read"),
            json!("computer_snapshot"),
            json!("computer_input")
        ]
    );
    assert_eq!(response["read"]["isError"], false);
    assert_eq!(response["status"]["isError"], false);
    assert_eq!(response["models"]["isError"], false);
    let calls = commands
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].caller, caller);
    assert_eq!(calls[0].target, Some(terminal.clone()));
    assert_eq!(calls[0].command, "terminal.capture");
    assert_eq!(calls[0].arguments, ["plain", "history", "7"]);
    assert_native_reads(&calls[1..], caller, &record.target());
    assert_eq!(service.activity(&record.target()).unwrap().id, record.id);
    let info = service.provider_info(&record.target()).unwrap();
    assert_eq!(info.provider, config.provider);
    assert_eq!(info.profile, config.profile);
    assert_eq!(info.model, config.model);
    assert_eq!(info.reasoning_effort, config.reasoning_effort);
    assert_eq!(info.fast_mode, config.fast_mode);
    assert_eq!(info.permissions, config.permissions);
    assert_eq!(
        info.permission_modes,
        [
            bootty_agents::NativePermissionMode::Supervised,
            bootty_agents::NativePermissionMode::AutoAcceptEdits,
            bootty_agents::NativePermissionMode::Auto,
            bootty_agents::NativePermissionMode::FullAccess,
        ]
    );
    assert_eq!(
        serde_json::to_value(&info)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "fast_mode",
            "model",
            "permission_modes",
            "permissions",
            "profile",
            "provider",
            "reasoning_effort"
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
    );
    assert_eq!(service.activities_for_binding("saved binding").len(), 1);
    assert_eq!(service.activities_for_binding("other binding"), Vec::new());
    let mut stale = record.target();
    stale.generation = stale.generation.saturating_add(1);
    assert!(service.activity(&stale).is_err());
    assert!(service.provider_info(&stale).is_err());
    let mut foreign = record.target();
    foreign.kind = ResourceKind::Terminal;
    assert!(service.provider_info(&foreign).is_err());
    let mut expected = serde_json::to_value(&config).expect("Captured config");
    expected["session_id"] = json!("tools-thread");
    assert_eq!(
        serde_json::to_value(&record.config).expect("Live captured config"),
        expected
    );
    service.stop(&record.target()).expect("Native stop");
    assert_eq!(service.provider_info(&record.target()).unwrap(), info);
    assert_eq!(catalog(&lease, &commands), Some(json!([])));
    service.checkpoint().expect("Durable checkpoint");
    let saved: Value = serde_json::from_slice(
        &fs::read(root.path().join("native.json")).expect("Saved native catalog"),
    )
    .expect("Catalog JSON");
    assert_eq!(saved["records"][0]["config"], expected);
    let (tools, fresh_lease) =
        bridge(&commands, caller, Some(&terminal)).expect("Fresh resume attachment");
    let tools = policy
        .retain_tool_attachment(AgentKind::Codex, tools)
        .expect("Fresh admission");
    let resumed = service
        .resume_with_tools(&record.target(), tools)
        .expect("Exact native resume");
    assert_eq!(resumed.id, record.id);
    assert_eq!(resumed.config.arguments, config.arguments);
    service
        .resolve(&resumed.target())
        .expect("Resumed session")
        .rpc("__tools", json!({}))
        .expect("Fresh MCP transport");
    assert_eq!(
        commands
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len(),
        6
    );
    service.stop(&resumed.target()).expect("Resumed stop");
    assert_eq!(catalog(&fresh_lease, &commands), Some(json!([])));
}

fn assert_native_reads(calls: &[CommandInvocation], caller: Caller, target: &CommandTarget) {
    for (call, command) in calls
        .iter()
        .zip(["agents.native.status", "agents.native.models"])
    {
        assert_eq!(call.command, command);
        assert_eq!(call.caller, caller);
        assert_eq!(call.target.as_ref(), Some(target));
        assert_eq!(
            call.arguments,
            [target.handle.clone(), target.generation.to_string()]
        );
    }
}

#[rstest]
fn associated_terminal_close_revokes_native_attachment_without_revoking_sibling() {
    let root = TempDir::new().expect("Fixture directory");
    let commands = Arc::new(Commands::default());
    let policy =
        TerminalAgentService::open(root.path().join("terminal.json")).expect("Policy owner");
    let terminal = target(ResourceKind::Terminal, "closed task");
    let sibling = target(ResourceKind::Terminal, "retained task");
    let (tools, lease) =
        bridge(&commands, Caller::Socket, Some(&terminal)).expect("First attachment");
    let tools = policy
        .retain_tool_attachment(AgentKind::Codex, tools)
        .expect("First admission");
    let (other, sibling_lease) =
        bridge(&commands, Caller::Socket, Some(&sibling)).expect("Sibling attachment");
    let other = policy
        .retain_tool_attachment(AgentKind::Codex, other)
        .expect("Sibling admission");
    policy.revoke_terminal_tools(&terminal);
    assert_eq!(catalog(&lease, &commands), Some(json!([])));
    assert_eq!(
        catalog(&sibling_lease, &commands)
            .as_ref()
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(5)
    );
    policy.set_provider_tools_enabled(AgentKind::Codex, false);
    assert_eq!(catalog(&sibling_lease, &commands), Some(json!([])));
    policy.set_provider_tools_enabled(AgentKind::Codex, true);
    assert_eq!(catalog(&sibling_lease, &commands), Some(json!([])));
    drop((tools, other));
}

#[rstest]
#[case(false, false)]
#[case(true, true)]
fn unbound_or_revoked_tools_fail_before_native_identity_publication(
    #[case] bound: bool,
    #[case] revoked: bool,
) {
    let root = TempDir::new().expect("Fixture directory");
    let commands = Arc::new(Commands::default());
    let terminal = target(ResourceKind::Terminal, "captured task");
    let service = NativeAgentService::open(root.path().join("native.json")).expect("Native owner");
    let (tools, lease) =
        bridge(&commands, Caller::Socket, bound.then_some(&terminal)).expect("Attachment");
    if revoked {
        lease.revoke();
    }
    assert!(
        service
            .create_for_task_with_tools(
                "saved binding",
                "saved task",
                "Tools",
                provider(root.path()).expect("Fixture"),
                Arc::new(tools),
            )
            .is_err()
    );
    assert_eq!(service.sessions().len(), 0);
    assert_eq!(
        commands
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len(),
        0
    );
}
