use std::{
    collections::BTreeMap,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use bootty_agents::{AgentKind, OrchestrationNodeState};
use bootty_config::config::{AgentProfileConfig, AgentProvidersConfig, MultiplexerBackendConfig};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget,
};
use bootty_ui::{
    AppEffect, AppState,
    commands::{RunCommand, RunNodeRequest, capture_run_plan},
};
use pretty_assertions::{assert_eq, assert_ne};
use rstest::{fixture, rstest};
use serde_json::json;

#[path = "support/idle_frames.rs"]
mod frames;
#[allow(
    clippy::expect_used,
    reason = "Shared desktop backend fixture follows existing test module convention"
)]
mod support;
#[path = "support/config.rs"]
mod test_config;

#[fixture]
fn node() -> RunNodeRequest {
    RunNodeRequest {
        id: "a".to_owned(),
        title: "First task".to_owned(),
        prompt: "Inspect literal $(task)\nthen report".to_owned(),
        provider: AgentKind::Claude,
        profile: None,
        dependencies: Vec::new(),
        task_identity: None,
    }
}

#[rstest]
#[case("argv", json!(["--permission-mode", "bypassPermissions"]))]
#[case("cwd", json!("/another/project"))]
#[case("account_directory", json!("/another/account"))]
#[case("caller", json!("internal"))]
#[case("grants", json!(["agents.spawn"]))]
fn wire_requests_cannot_choose_host_authority(
    node: RunNodeRequest,
    #[case] field: &str,
    #[case] value: serde_json::Value,
) {
    let mut encoded = serde_json::to_value(node).unwrap();
    encoded[field] = value;
    let invocation = CommandInvocation::new(
        "runs.create",
        vec![serde_json::to_string(&vec![encoded]).unwrap()],
        Caller::Socket,
    );
    assert!(RunCommand::parse(&invocation).is_err());
}

#[rstest]
#[case(None, Some("work"))]
#[case(Some(""), None)]
#[case(Some("personal"), Some("personal"))]
fn capture_freezes_exact_profile_launch_and_account(
    mut node: RunNodeRequest,
    #[case] selected: Option<&str>,
    #[case] expected: Option<&str>,
) {
    let mut providers = AgentProvidersConfig::default();
    providers.claude.program = "/host/claude".to_owned();
    providers.claude.selected = "work".to_owned();
    providers.claude.profiles = ["work", "personal"]
        .into_iter()
        .map(|id| {
            (
                id.to_owned(),
                AgentProfileConfig {
                    name: id.to_owned(),
                    directory: Some(format!("/accounts/{id}")),
                    arguments: vec!["--model".to_owned(), id.to_owned()],
                },
            )
        })
        .collect();
    node.profile = selected.map(str::to_owned);
    let plan = capture_run_plan(
        vec![node.clone()],
        "/project",
        &BTreeMap::new(),
        &providers,
        |_, profile| {
            Ok(profile
                .and_then(|profile| profile.directory.clone())
                .unwrap_or_else(|| "/accounts/default".to_owned()))
        },
    )
    .unwrap();
    providers.claude.program = "/changed/claude".to_owned();
    providers.claude.selected.clear();
    let captured = &plan.nodes()[0];
    assert_eq!(captured.launch.profile(), expected);
    assert_eq!(captured.launch.launch().program, "/host/claude");
    assert_eq!(captured.launch.launch().cwd.as_deref(), Some("/project"));
    assert_eq!(
        captured.launch.launch().account_directory.as_deref(),
        Some(
            expected
                .map_or_else(
                    || "/accounts/default".to_owned(),
                    |id| format!("/accounts/{id}")
                )
                .as_str()
        )
    );
    assert_eq!(captured.prompt.text(), node.prompt);
}

#[rstest]
#[case("--resume")]
#[case("--session-id=previous")]
#[case("--continue")]
#[case("--session")]
fn previous_session_profiles_require_turn_identity(node: RunNodeRequest, #[case] selector: &str) {
    let mut providers = AgentProvidersConfig::default();
    providers.claude.selected = "work".to_owned();
    providers.claude.profiles.insert(
        "work".to_owned(),
        AgentProfileConfig {
            name: "Work".to_owned(),
            directory: Some("/account".to_owned()),
            arguments: vec![selector.to_owned()],
        },
    );
    assert!(
        capture_run_plan(
            vec![node],
            "/project",
            &BTreeMap::new(),
            &providers,
            |_, _| Ok("/account".to_owned())
        )
        .is_err()
    );
}

struct HostFixture {
    directory: Arc<assert_fs::TempDir>,
    state: AppState,
    wakes: mpsc::Receiver<()>,
    binding: CommandTarget,
}

impl HostFixture {
    fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(service) = self.state.native_agent_service() {
            service.shutdown()?;
        }
        if let Some(service) = self.state.terminal_agent_service() {
            let records = service.live_records();
            // Join owned query workers before retirement can hand them to asynchronous cleanup.
            service.shutdown_and_wait()?;
            for record in records {
                let mut stop = CommandInvocation::new(
                    format!("agents.{}.stop", record.provider),
                    Vec::new(),
                    Caller::Socket,
                );
                stop.target = Some(record.target);
                stop.confirmation = Some(stop.confirmation());
                if !matches!(
                    submit(&mut self.state, &self.wakes, stop)?,
                    CommandOutcome::Success { .. }
                ) {
                    return Err("Fixture terminal did not close through its shared owner".into());
                }
            }
        }
        Ok(())
    }
}

impl Drop for HostFixture {
    fn drop(&mut self) {
        // Actual tests assert teardown; a failed assertion still attempts bounded owned cleanup.
        let _ = self.shutdown();
    }
}

fn submit(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    invocation: CommandInvocation,
) -> Result<CommandOutcome, Box<dyn std::error::Error>> {
    let command = invocation.command.clone();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .ok_or("test deadline overflow")?;
    let response = state
        .app_command_sender(invocation.caller)
        .submit(invocation, deadline, CommandCancellation::new())
        .map_err(|error| format!("Fixture command submission failed: {error:?}"))?;
    loop {
        let effects = state.update_frame(frames::idle_frame(Instant::now()));
        match response.try_recv() {
            Ok(outcome) => return Ok(outcome),
            Err(mpsc::TryRecvError::Disconnected) => return Err("command disconnected".into()),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("Fixture {command} did not complete before its deadline").into());
        }
        // Drive the timers requested by the public frame contract, as the real window does.
        let delay = effects
            .into_iter()
            .filter_map(|effect| match effect {
                AppEffect::RequestRepaint => Some(Duration::ZERO),
                AppEffect::RepaintAfter(delay) => Some(delay),
                _ => None,
            })
            .min()
            .unwrap_or(remaining)
            .min(remaining);
        match wakes.recv_timeout(delay) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn issued_binding(value: &serde_json::Value) -> Result<CommandTarget, Box<dyn std::error::Error>> {
    let issued = value
        .get("target")
        .cloned()
        .ok_or("Binding discovery omitted its issued target")?;
    let target: CommandTarget = serde_json::from_value(issued)?;
    if target.kind != bootty_control::ResourceKind::Binding {
        return Err("Discovery did not issue a Binding target".into());
    }
    Ok(target)
}

#[fixture]
fn host() -> Result<HostFixture, Box<dyn std::error::Error>> {
    let directory = Arc::new(assert_fs::TempDir::new()?);
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let project = directory.path().join("project");
    std::fs::create_dir(&project)?;
    config.session.working_directory = Some(project);
    // Keep provider queries independent of installed providers and credentials.
    config.agents.claude.program = directory
        .path()
        .join("missing-provider")
        .to_string_lossy()
        .into_owned();
    "fixture".clone_into(&mut config.agents.claude.selected);
    config.agents.claude.profiles = BTreeMap::from([(
        "fixture".to_owned(),
        AgentProfileConfig {
            name: "Fixture".to_owned(),
            directory: Some(
                directory
                    .path()
                    .join("account")
                    .to_string_lossy()
                    .into_owned(),
            ),
            arguments: Vec::new(),
        },
    )]);
    reopen_host(directory, config)
}

fn reopen_host(
    directory: Arc<assert_fs::TempDir>,
    config: bootty_config::config::BoottyConfig,
) -> Result<HostFixture, Box<dyn std::error::Error>> {
    let (wake, wakes) = mpsc::channel();
    let (events, _receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_agents(
        config,
        "runs-fixture".to_owned(),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )?;
    let CommandOutcome::Success { value, .. } = submit(
        &mut state,
        &wakes,
        CommandInvocation::new(
            "resource.current",
            vec!["binding".to_owned()],
            Caller::Socket,
        ),
    )?
    else {
        return Err("Binding discovery failed".into());
    };
    Ok(HostFixture {
        directory,
        state,
        wakes,
        binding: issued_binding(&value)?,
    })
}

fn bound(mut invocation: CommandInvocation, binding: &CommandTarget) -> CommandInvocation {
    invocation.target = Some(binding.clone());
    invocation
}

fn run_mutation(
    host: &mut HostFixture,
    command: &str,
    arguments: Vec<String>,
    binding: &CommandTarget,
) -> Result<CommandOutcome, Box<dyn std::error::Error>> {
    submit(
        &mut host.state,
        &host.wakes,
        bound(
            CommandInvocation::new(command, arguments, Caller::Socket),
            binding,
        ),
    )
}

fn create(
    host: &mut HostFixture,
    node: RunNodeRequest,
) -> Result<String, Box<dyn std::error::Error>> {
    let outcome = submit(
        &mut host.state,
        &host.wakes,
        bound(
            CommandInvocation::new(
                "runs.create",
                vec![serde_json::to_string(&vec![node])?],
                Caller::Socket,
            ),
            &host.binding,
        ),
    )?;
    let CommandOutcome::Success { value, .. } = outcome else {
        return Err(format!("creation failed: {outcome:?}").into());
    };
    value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "Run ID absent".into())
}

#[rstest]
fn shared_creation_cancel_retry_and_recovery_are_durable(
    host: Result<HostFixture, Box<dyn std::error::Error>>,
    node: RunNodeRequest,
) {
    let mut host = host.unwrap();
    let run = create(&mut host, node).unwrap();
    let service = host.state.orchestration_service().unwrap();
    let snapshot = service.snapshot_arc();
    assert_eq!(snapshot.runs[0].context.caller(), Caller::Socket);
    assert_eq!(snapshot.runs[0].context.binding(), &host.binding);
    assert_eq!(
        snapshot.runs[0].nodes[0].state,
        OrchestrationNodeState::Pending
    );
    let binding = host.binding.clone();
    let outcome = run_mutation(&mut host, "runs.cancel", vec![run.clone()], &binding).unwrap();
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(matches!(
        service.snapshot().runs[0].nodes[0].state,
        OrchestrationNodeState::Cancelled { target: None }
    ));
    // An env wrapper can be accepted even when its eventual provider exec fails. A missing
    // captured cwd instead fails the actual owner creation before any terminal is accepted.
    std::fs::remove_dir(host.directory.path().join("project")).unwrap();
    let outcome =
        run_mutation(&mut host, "runs.retry", vec![run, "a".to_owned()], &binding).unwrap();
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let failed = service.snapshot();
    assert!(
        matches!(
            failed.runs[0].nodes[0].state,
            OrchestrationNodeState::Failed { target: None, .. }
        ),
        "{failed:?}"
    );
    assert_eq!(host.state.mux().all_sessions(), []);
    let config = host.state.config().clone();
    let directory = Arc::clone(&host.directory);
    let old_binding = host.binding.clone();
    host.shutdown().unwrap();
    drop(host);
    drop(service);
    let mut reopened = reopen_host(directory, config).unwrap();
    let recovered = reopened.state.orchestration_service().unwrap();
    assert_eq!(recovered.snapshot().runs[0].nodes, failed.runs[0].nodes);
    let fresh = reopened
        .state
        .orchestration_restart_target(&failed.runs[0])
        .unwrap();
    assert_ne!(fresh, old_binding);
    let outcome = run_mutation(
        &mut reopened,
        "runs.restart",
        vec![failed.runs[0].id.clone()],
        &fresh,
    )
    .unwrap();
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let restarted = recovered.snapshot();
    assert_eq!(restarted.runs[0].context.binding(), &fresh);
    assert_eq!(restarted.runs[0].context.caller(), Caller::Socket);
    assert_eq!(
        restarted.runs[0].nodes[0].spec.launch,
        failed.runs[0].nodes[0].spec.launch
    );
    assert!(
        matches!(
            restarted.runs[0].nodes[0].state,
            OrchestrationNodeState::Failed { target: None, .. }
        ),
        "{restarted:?}"
    );
    assert!(restarted.runs[0].generation > failed.runs[0].generation);
    assert!(restarted.runs[0].nodes[0].attempt > failed.runs[0].nodes[0].attempt);
    assert_eq!(reopened.state.mux().all_sessions(), []);
    reopened.shutdown().unwrap();
}

#[rstest]
fn read_omits_launch_prompt_and_account_metadata(
    host: Result<HostFixture, Box<dyn std::error::Error>>,
    node: RunNodeRequest,
) {
    let mut host = host.unwrap();
    let run = create(&mut host, node.clone()).unwrap();
    let outcome = submit(
        &mut host.state,
        &host.wakes,
        CommandInvocation::new("runs.read", vec![run], Caller::Socket),
    )
    .unwrap();
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(value["nodes"][0]["title"], node.title);
    assert_eq!(value["nodes"][0]["provider"], "claude");
    for private in ["prompt", "launch", "argv", "account_directory", "cwd"] {
        assert_eq!(value["nodes"][0].get(private), None);
    }
    host.shutdown().unwrap();
}

#[rstest]
fn queued_creation_cancelled_before_owner_acceptance_creates_no_terminal(
    host: Result<HostFixture, Box<dyn std::error::Error>>,
    node: RunNodeRequest,
) {
    let mut host = host.unwrap();
    let run = create(&mut host, node).unwrap();
    let service = host.state.orchestration_service().unwrap();
    let claims = service.claim_ready(&run, 1).unwrap();
    let guard = service.begin_dispatch(&claims[0].token).unwrap();
    let deadline = Instant::now().checked_add(Duration::from_secs(2)).unwrap();
    let create = bound(
        CommandInvocation::new(
            "session.create",
            vec![
                "never-start".to_owned(),
                host.directory.path().to_string_lossy().into_owned(),
                "[]".to_owned(),
                bootty_mux::snapshot::new_session_identity(),
                "Never starts".to_owned(),
            ],
            Caller::Socket,
        ),
        &host.binding,
    );
    let response = host
        .state
        .app_command_sender(Caller::Socket)
        .submit(create, deadline, guard.cancellation())
        .unwrap();
    service.cancel(&run).unwrap();
    host.state.update_frame(frames::idle_frame(Instant::now()));
    assert_eq!(response.try_recv().unwrap(), CommandOutcome::cancelled());
    assert_eq!(host.state.mux().all_sessions(), []);
    host.shutdown().unwrap();
}

#[cfg(unix)]
struct ProviderFixture {
    host: HostFixture,
    _gates: Vec<std::fs::File>,
}

#[cfg(unix)]
fn providers(first_state: &str) -> Result<ProviderFixture, Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = Arc::new(assert_fs::TempDir::new()?);
    let program = directory.path().join("provider.py");
    std::fs::write(
        &program,
        r"#!/usr/bin/env python3
import json, os, pathlib, sys
account = pathlib.Path(os.environ['CLAUDE_CONFIG_DIR'])
if sys.argv[1:3] == ['agents', '--json']:
    if not (account / 'identity').exists():
        with open(account / 'started', 'rb', buffering=0) as gate:
            gate.read(1)
    print(json.dumps([{'sessionId': (account / 'identity').read_text(), 'state': (account / 'state').read_text()}]))
    sys.exit(0)
args = sys.argv[1:]
session = args[args.index('--session-id') + 1]
(account / 'identity').write_text(session)
with open(account.parent / 'order', 'a') as order:
    order.write(account.name + '\n')
with open(account / 'started', 'wb', buffering=0) as gate:
    gate.write(b'x')
print('ready', flush=True)
for line in sys.stdin:
    pass
",
    )?;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.session.working_directory = Some(directory.path().to_path_buf());
    config.agents.claude.program = program.to_string_lossy().into_owned();
    let mut gates = Vec::new();
    for (id, state) in [("a", first_state), ("b", "done")] {
        let account = directory.path().join(id);
        std::fs::create_dir(&account)?;
        std::fs::write(account.join("state"), state)?;
        if !std::process::Command::new("mkfifo")
            .arg(account.join("started"))
            .status()?
            .success()
        {
            return Err("fixture FIFO creation failed".into());
        }
        gates.push(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(account.join("started"))?,
        );
        config.agents.claude.profiles.insert(
            id.to_owned(),
            AgentProfileConfig {
                name: id.to_owned(),
                directory: Some(account.to_string_lossy().into_owned()),
                arguments: Vec::new(),
            },
        );
    }
    let (wake, wakes) = mpsc::channel();
    let (events, _receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_agents(
        config,
        "provider-run".to_owned(),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )?;
    let CommandOutcome::Success { value, .. } = submit(
        &mut state,
        &wakes,
        CommandInvocation::new(
            "resource.current",
            vec!["binding".to_owned()],
            Caller::Socket,
        ),
    )?
    else {
        return Err("Binding discovery failed".into());
    };
    Ok(ProviderFixture {
        host: HostFixture {
            directory,
            state,
            wakes,
            binding: issued_binding(&value)?,
        },
        _gates: gates,
    })
}

#[cfg(unix)]
#[rstest]
#[case("done", "succeeded", "a\nb\n")]
#[case("failed", "failed", "a\n")]
#[case("idle", "running", "a\n")]
fn two_provider_workers_observe_dependency_order_without_idle_success(
    mut node: RunNodeRequest,
    #[case] provider_state: &str,
    #[case] expected: &str,
    #[case] order: &str,
) {
    let mut fixture = providers(provider_state).unwrap();
    node.profile = Some("a".to_owned());
    let mut dependent = node.clone();
    dependent.id = "b".to_owned();
    dependent.title = "Dependent".to_owned();
    dependent.profile = Some("b".to_owned());
    dependent.dependencies = vec!["a".to_owned()];
    let create = submit(
        &mut fixture.host.state,
        &fixture.host.wakes,
        bound(
            CommandInvocation::new(
                "runs.create",
                vec![serde_json::to_string(&vec![node, dependent]).unwrap()],
                Caller::Socket,
            ),
            &fixture.host.binding,
        ),
    )
    .unwrap();
    let CommandOutcome::Success { value, .. } = create else {
        panic!("{create:?}");
    };
    let run = value["id"].as_str().unwrap().to_owned();
    let result = submit(
        &mut fixture.host.state,
        &fixture.host.wakes,
        bound(
            CommandInvocation::new("runs.dispatch", vec![run], Caller::Socket),
            &fixture.host.binding,
        ),
    )
    .unwrap();
    assert!(
        matches!(result, CommandOutcome::Success { .. }),
        "{result:?}"
    );
    let service = fixture.host.state.orchestration_service().unwrap();
    let deadline = Instant::now().checked_add(Duration::from_secs(2)).unwrap();
    loop {
        fixture
            .host
            .state
            .update_frame(frames::idle_frame(Instant::now()));
        let snapshot = service.snapshot_arc();
        let observed = serde_json::to_value(&snapshot.runs[0].nodes[0].state).unwrap();
        let done = observed["state"] == expected
            && (provider_state != "done"
                || matches!(
                    snapshot.runs[0].nodes[1].state,
                    OrchestrationNodeState::Succeeded { .. }
                ));
        let native = fixture.host.state.terminal_agent_service().unwrap();
        let idle_observed = provider_state != "idle"
            || native.records().iter().any(|record| {
                record.observation.status == bootty_agents::TerminalAgentStatus::Idle
            });
        if done && idle_observed {
            break;
        }
        fixture
            .host
            .wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(fixture.host.directory.path().join("order")).unwrap(),
        order
    );
    if provider_state != "done" {
        assert_eq!(
            service.snapshot().runs[0].nodes[1].state,
            OrchestrationNodeState::Pending
        );
    }
    fixture.host.shutdown().unwrap();
}

#[cfg(unix)]
fn native_provider_host(mode: &str) -> Result<HostFixture, Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = Arc::new(assert_fs::TempDir::new()?);
    let program = directory.path().join("native-provider.py");
    std::fs::write(
        &program,
        r"#!/usr/bin/env python3
import json,os,pathlib,sys
assert sys.argv[1:4] == ['app-server','--listen','stdio://']
account=pathlib.Path(os.environ['CODEX_HOME'])
thread='thread-'+str(os.getpid())
turn=0
def emit(value): print(json.dumps(value),flush=True)
def reply(ident,result): emit({'id':ident,'result':result})
def notice(method,**params): emit({'method':method,'params':{'threadId':thread,'turnId':'turn-'+str(turn),**params}})
for line in sys.stdin:
 value=json.loads(line); method=value.get('method'); ident=value.get('id'); params=value.get('params',{})
 if method == 'initialized': continue
 if method == 'initialize': reply(ident,{})
 elif method in ['thread/start','thread/resume']: reply(ident,{'thread':{'id':thread,'turns':[]}})
 elif method == 'turn/start':
  turn+=1; text=params['input'][0]['text']
  with open(account.parent/'order','a') as order: order.write(text+'\n')
  notice('turn/started',turn={'id':'turn-'+str(turn),'status':'inProgress','items':[]})
  status=(account/'mode').read_text() if text == 'first' else 'completed'
  notice('turn/completed',turn={'id':'turn-'+str(turn),'status':'completed' if status == 'wrong_ack' else status,'items':[]})
  reply(ident,{'turn':{'id':'wrong-turn' if status == 'wrong_ack' else 'turn-'+str(turn)}})
 elif method == 'turn/interrupt':
  notice('turn/completed',turn={'id':'turn-'+str(turn),'status':'interrupted','items':[]}); reply(ident,{})
 else: reply(ident,{})
",
    )?;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let project = directory.path().join("project");
    std::fs::create_dir(&project)?;
    let account = directory.path().join("account");
    std::fs::create_dir(&account)?;
    std::fs::write(account.join("mode"), mode)?;
    config.session.working_directory = Some(project);
    // Native run behavior does not depend on the user's interactive shell startup.
    config.session.shell = Some("/bin/sh".into());
    config.session.shell_integration = false;
    config.agents.codex.program = program.to_string_lossy().into_owned();
    "fixture".clone_into(&mut config.agents.codex.selected);
    config.agents.codex.profiles.insert(
        "fixture".into(),
        AgentProfileConfig {
            name: "Fixture".into(),
            directory: Some(account.to_string_lossy().into_owned()),
            arguments: Vec::new(),
        },
    );
    reopen_host(directory, config)
}

#[cfg(unix)]
fn native_parent(
    host: &mut HostFixture,
) -> Result<(CommandTarget, String), Box<dyn std::error::Error>> {
    let identity = bootty_mux::snapshot::new_session_identity();
    let cwd = host
        .directory
        .path()
        .join("project")
        .to_string_lossy()
        .into_owned();
    let binding = host.binding.clone();
    let created = run_mutation(
        host,
        "session.create",
        vec![
            "existing-task".into(),
            cwd.clone(),
            "[]".into(),
            identity.clone(),
            "Existing task".into(),
        ],
        &binding,
    )?;
    if !matches!(created, CommandOutcome::Success { .. }) {
        return Err(format!("Saved task creation failed: {created:?}").into());
    }
    let program = host.state.config().agents.codex.program.clone();
    let created = run_mutation(
        host,
        "agents.native.tab",
        vec![
            "codex".into(),
            cwd,
            program,
            "[]".into(),
            "parent".into(),
            "fixture".into(),
            identity.clone(),
            "Parent".into(),
            String::new(),
        ],
        &binding,
    )?;
    let CommandOutcome::Success { value, .. } = created else {
        return Err(format!("Parent creation failed: {created:?}").into());
    };
    let record: bootty_agents::NativeSessionRecord =
        serde_json::from_value(value.get("native").ok_or("missing native record")?.clone())?;
    Ok((record.target(), identity))
}

#[cfg(unix)]
#[rstest]
#[case("completed", "succeeded", "first\nsecond\n")]
#[case("failed", "failed", "first\n")]
#[case("interrupted", "failed", "first\n")]
#[case("unknown", "running", "first\n")]
#[case("wrong_ack", "failed", "first\n")]
fn native_dependent_work_uses_exact_first_turn_without_creating_another_saved_task(
    #[case] mode: &str,
    #[case] expected: &str,
    #[case] order: &str,
) {
    let mut host = native_provider_host(mode).unwrap();
    let (parent, identity) = native_parent(&mut host).unwrap();
    let sessions = host.state.mux().all_sessions().len();
    let mut first = node();
    first.provider = AgentKind::Codex;
    first.prompt = "first".into();
    first.profile = Some("fixture".into());
    first.task_identity = Some(identity);
    let mut dependent = first.clone();
    dependent.id = "b".into();
    dependent.prompt = "second".into();
    dependent.dependencies = vec!["a".into()];
    let nodes = vec![first, dependent];
    let outcome = submit(
        &mut host.state,
        &host.wakes,
        bound(
            CommandInvocation::new(
                "runs.create_for_session",
                vec![serde_json::to_string(&nodes).unwrap()],
                Caller::Socket,
            ),
            &parent,
        ),
    )
    .unwrap();
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("Plan failed: {outcome:?}");
    };
    let run = value["id"].as_str().unwrap().to_owned();
    let binding = host.binding.clone();
    let dispatched = run_mutation(&mut host, "runs.dispatch", vec![run], &binding).unwrap();
    assert!(
        matches!(dispatched, CommandOutcome::Success { .. }),
        "{dispatched:?}"
    );
    let service = host.state.orchestration_service().unwrap();
    let deadline = Instant::now().checked_add(Duration::from_secs(2)).unwrap();
    loop {
        host.state.update_frame(frames::idle_frame(Instant::now()));
        let snapshot = service.snapshot_arc();
        let actual = serde_json::to_value(&snapshot.runs[0].nodes[0].state).unwrap();
        if actual["state"] == expected
            && (mode != "completed"
                || matches!(
                    snapshot.runs[0].nodes[1].state,
                    OrchestrationNodeState::Succeeded { .. }
                ))
        {
            break;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(host.directory.path().join("order")).unwrap(),
        order
    );
    assert_eq!(host.state.mux().all_sessions().len(), sessions);
    assert!(service.snapshot().runs[0].nodes[0].state.target().is_some());
    if mode != "completed" {
        assert_eq!(
            service.snapshot().runs[0].nodes[1].state,
            OrchestrationNodeState::Pending
        );
    }
    host.shutdown().unwrap();
}

#[cfg(unix)]
#[rstest]
#[case("generation")]
#[case("kind")]
#[case("task")]
#[case("provider")]
#[case("account")]
#[case("program")]
#[case("argv")]
fn session_work_rejects_stale_parent_or_configuration_drift_without_creating_work(
    #[case] boundary: &str,
) {
    let mut host = native_provider_host("completed").unwrap();
    let (mut parent, identity) = native_parent(&mut host).unwrap();
    let native = host.state.native_agent_service().unwrap();
    let records = native.activities();
    let sessions = host.state.mux().all_sessions().len();
    let mut node = RunNodeRequest {
        id: "a".into(),
        title: "Child".into(),
        prompt: "first".into(),
        provider: AgentKind::Codex,
        profile: Some("fixture".into()),
        dependencies: Vec::new(),
        task_identity: Some(identity),
    };
    match boundary {
        "generation" => parent.generation = parent.generation.checked_add(1).unwrap(),
        "kind" => parent = host.binding.clone(),
        "task" => node.task_identity = Some("another-task".into()),
        "provider" => node.provider = AgentKind::Pi,
        "account" | "program" | "argv" => {
            let preferences = &host.state.config().agents.codex;
            let program = if boundary == "program" {
                "/changed/provider"
            } else {
                &preferences.program
            };
            let account = if boundary == "account" {
                host.directory.path().join("changed-account")
            } else {
                host.directory.path().join("account")
            };
            let arguments = if boundary == "argv" {
                vec!["--model", "changed"]
            } else {
                Vec::new()
            };
            std::fs::write(&host.state.config().config_path, format!(
                "[agents.codex]\nprogram = {}\nselected = \"fixture\"\n[agents.codex.profiles.fixture]\nname = \"Fixture\"\ndirectory = {}\narguments = {}\n",
                serde_json::to_string(program).unwrap(), serde_json::to_string(&account.to_string_lossy()).unwrap(), serde_json::to_string(&arguments).unwrap(),
            )).unwrap();
            assert!(host.state.reload_config(&mut Vec::new()));
        }
        _ => panic!("Unknown fixture boundary: {boundary}"),
    }
    let outcome = submit(
        &mut host.state,
        &host.wakes,
        bound(
            CommandInvocation::new(
                "runs.create_for_session",
                vec![serde_json::to_string(&vec![node]).unwrap()],
                Caller::Socket,
            ),
            &parent,
        ),
    )
    .unwrap();
    assert!(
        !matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        host.state.orchestration_service().unwrap().snapshot().runs,
        Vec::new()
    );
    assert_eq!(native.activities(), records);
    assert_eq!(host.state.mux().all_sessions().len(), sessions);
    assert!(!host.directory.path().join("order").exists());
    host.shutdown().unwrap();
}
