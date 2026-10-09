#![cfg(unix)]
#![allow(
    clippy::panic_in_result_fn,
    reason = "Test assertions retain failure diagnostics; Result propagates fixture and command errors"
)]

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use bootty_agents::{
    AgentKind, AgentLaunch, NativeSessionConfig, NativeSessionRecord, NativeSessionStatus,
    ToolBridge, ToolBridgeContext, ToolPolicy, ToolScope, ToolSpawnContext, ToolSpawnRequest,
};
use bootty_browser::{Annotation, AnnotationAnchor, AnnotationStore, annotation_batch};
use bootty_config::config::{AgentProfileConfig, AgentProvidersConfig, MultiplexerBackendConfig};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use bootty_mux::repository::{SpaceMuxOverride, WorkspaceRepository};
use bootty_mux::session_membership::{SessionMembership, SessionState, WorkspaceSession};
use bootty_ui::{
    AppEffect, AppState,
    gpui::{ActionId, DialogIntent, DialogPayload, RowId},
    presentation::{
        dialogs::{NewSessionDialog, NewSessionPickerEvent},
        new_session_form::{NewSessionDraft, NewSessionForm, NewSessionMode, SessionDestination},
    },
};
use pretty_assertions::{assert_eq, assert_ne};
use rstest::rstest;
use serde_json::{Value, json};

#[path = "support/idle_frames.rs"]
mod frames;
#[allow(
    clippy::expect_used,
    reason = "Shared desktop backend fixture follows existing test convention"
)]
mod support;
#[path = "support/config.rs"]
mod test_config;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

// Real provider subprocess, public app-server protocol, and reply barriers; no timed sleeps.
fn provider(root: &Path) -> TestResult<String> {
    let program = root.join("codex-provider.py");
    fs::write(
        &program,
        r"#!/usr/bin/env python3
import json,os,socket,sys
if 'exec' in sys.argv:
 assert '--ephemeral' in sys.argv
 with open(sys.argv[sys.argv.index('--output-last-message')+1],'w') as result: json.dump({'title':'Fix native composer','slug':'fix-native-composer'},result)
 sys.exit(0)
assert sys.argv[1:3] == ['app-server','--listen']
source=sys.stdin
output=sys.stdout
if sys.argv[3].startswith('unix://'):
 listener=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
 listener.bind(sys.argv[3].removeprefix('unix://'));listener.listen()
 connection,_=listener.accept()
 source=connection.makefile('r');output=connection.makefile('w')
else: assert sys.argv[3]=='stdio://'
thread='thread-'+str(os.getpid())
turn=0
waiting=None
held_start=None
methods=[]
def emit(value): print(json.dumps(value),file=output,flush=True)
def reply(ident,result): emit({'id':ident,'result':result})
def notice(method,**params): emit({'method':method,'params':{'threadId':thread,'turnId':'turn-'+str(turn),**params}})
def tool_call(params):
 config={}
 for index,arg in enumerate(sys.argv):
  if arg=='--config' and index+1<len(sys.argv):
   key,value=sys.argv[index+1].split('=',1)
   if key.startswith('mcp_servers.') and key.endswith('.args'): config=json.loads(value)
 with open(config[1]) as source: connection=json.load(source)
 with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as stream:
  stream.connect(connection['socket'])
  stream.sendall((json.dumps({'token':connection['token'],'request':{'jsonrpc':'2.0','id':1,'method':'tools/call','params':params}})+'\n').encode())
  return json.loads(stream.makefile().readline())['result']
for line in source:
 value=json.loads(line)
 method=value.get('method')
 methods.append(method)
 ident=value.get('id')
 params=value.get('params',{})
 if method == 'initialized': continue
 if method == 'initialize':
  if os.path.exists('fail-initialize'): emit({'id':ident,'error':{'code':-32603,'message':'provider initialization failed'}})
  else: reply(ident,{})
 elif method in ['thread/start','thread/resume']:
  thread=params.get('threadId',thread)
  if method=='thread/start' and os.path.exists('hold-thread-start'):
   held_start=ident
   ready=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);ready.connect('start-barrier');ready.sendall(b'1');ready.close()
  else: reply(ident,{'thread':{'id':thread,'turns':[]}})
 elif method == '__allow_start':
  reply(held_start,{'thread':{'id':thread,'turns':[]}});held_start=None;reply(ident,{})
 elif method == 'model/list': reply(ident,{'data':[{'model':'qa-model','displayName':'QA Model','isDefault':True,'supportedReasoningEfforts':[{'reasoningEffort':'medium'}],'defaultReasoningEffort':'medium'}],'nextCursor':None})
 elif method == 'thread/fork': reply(ident,{'thread':{'id':'fork-'+str(os.getpid())}})
 elif method == 'thread/items/list': reply(ident,{'data':[],'nextCursor':None})
 elif method == 'turn/start':
  assert params['threadId'] == thread
  turn+=1
  text=params['input'][0]['text']
  if text=='visual-image':
   import base64,struct
   assert len(params['input'])==2 and params['input'][1]['type']=='image'
   url=params['input'][1]['url'];assert url.startswith('data:image/png;base64,')
   png=base64.b64decode(url.split(',',1)[1]);assert png[:8]==b'\x89PNG\r\n\x1a\n'
   assert struct.unpack('>II',png[16:24])==(2,2)
  if text=='reject-first':
   emit({'id':ident,'error':{'code':-32600,'message':'first prompt rejected'}});continue
  notice('turn/started',turn={'id':'turn-'+str(turn),'status':'inProgress','items':[]})
  notice('item/completed',item={'id':'user-'+str(turn),'type':'userMessage','content':params['input']})
  if text == 'wait': waiting=ident; continue
  notice('item/started',item={'id':'command-'+str(turn),'type':'commandExecution','command':'echo probe','aggregatedOutput':''})
  notice('item/commandExecution/outputDelta',itemId='command-'+str(turn),delta='tool output')
  notice('item/agentMessage/delta',itemId='answer-'+str(turn),delta='reply: '+text)
  notice('item/completed',item={'id':'answer-'+str(turn),'type':'agentMessage','text':'reply: '+text})
  notice('turn/completed',turn={'id':'turn-'+str(turn),'status':'completed','items':[]})
  reply(ident,{'turn':{'id':'turn-'+str(turn)}})
 elif method == 'turn/interrupt':
  assert params['threadId'] == thread and params['turnId'] == 'turn-'+str(turn)
  notice('turn/completed',turn={'id':'turn-'+str(turn),'status':'interrupted','items':[]})
  if waiting is not None: reply(waiting,{'turn':{'id':'turn-'+str(turn)}}); waiting=None
  reply(ident,{})
 elif method == '__spawn': reply(ident,tool_call(params))
 elif method == '__barrier': reply(ident,{})
 elif method == '__calls': reply(ident,{'methods':methods,'account_directory':os.environ.get('CODEX_HOME'),'bootty_tools':any(arg.startswith('mcp_servers.') for arg in sys.argv)})
 else: reply(ident,{})
",
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
    Ok(program.to_string_lossy().into_owned())
}

struct Host {
    state: AppState,
    wakes: mpsc::Receiver<()>,
    wake: mpsc::Sender<()>,
    effects: Vec<AppEffect>,
    program: String,
    providers: AgentProvidersConfig,
    // Keep provider/catalog files until every app-owned service has dropped.
    directory: assert_fs::TempDir,
}

impl Host {
    fn new(enabled: bool) -> TestResult<Self> {
        Self::new_with_spawn(enabled, false)
    }

    fn new_with_spawn(enabled: bool, allow_spawn: bool) -> TestResult<Self> {
        Self::new_with_backend(enabled, allow_spawn, MultiplexerBackendConfig::Native)
    }

    fn new_with_backend(
        enabled: bool,
        allow_spawn: bool,
        backend: MultiplexerBackendConfig,
    ) -> TestResult<Self> {
        let directory = assert_fs::TempDir::new()?;
        let program = provider(directory.path())?;
        let config_path = directory.path().join("config.toml");
        WorkspaceRepository::open(&config_path)?
            .0
            .create_space(
                "Other",
                "2",
                [1, 2, 3],
                false,
                SpaceMuxOverride::default(),
                false,
            )?
            .ok_or("Other Space was not created")?;
        let mut config = test_config::config(config_path, backend);
        config.session.working_directory = Some(directory.path().to_owned());
        config.session.shell = Some("/bin/sh".to_owned());
        config.agents.allow_spawn = allow_spawn;
        config.agents.codex.enabled = enabled;
        config.agents.codex.program.clone_from(&program);
        "captured".clone_into(&mut config.agents.codex.selected);
        config.agents.codex.profiles.insert(
            "captured".to_owned(),
            AgentProfileConfig {
                name: "Captured".to_owned(),
                directory: Some(
                    directory
                        .path()
                        .join("account")
                        .to_string_lossy()
                        .into_owned(),
                ),
                arguments: vec!["--model".to_owned(), "fixture-model".to_owned()],
            },
        );
        let providers = config.agents.clone();
        let (wake, wakes) = mpsc::channel();
        let repaint = wake.clone();
        let (events, _receiver) = bootty_control::event_queue();
        let state = AppState::new_for_window_with_agents(
            config,
            "native-conversation-tests".to_owned(),
            support::backends(),
            Arc::new(move || {
                let _ = repaint.send(());
            }),
            None,
            None,
            Some(events),
        )?;
        Ok(Self {
            directory,
            state,
            wakes,
            wake,
            effects: Vec::new(),
            program,
            providers,
        })
    }

    fn native_tool(
        &mut self,
        record: &NativeSessionRecord,
        name: &str,
        arguments: Value,
    ) -> TestResult<CommandOutcome> {
        let result = self.native_tool_reply(record, name, arguments)?;
        let encoded = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .ok_or("Tool outcome text")?;
        Ok(serde_json::from_str(encoded)?)
    }

    fn spawned_record(&self, receipt: &Value) -> TestResult<NativeSessionRecord> {
        let native = receipt.get("native").ok_or("Native spawn receipt")?;
        let id = native
            .get("id")
            .and_then(Value::as_str)
            .ok_or("Issued native ID")?;
        let generation = native
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or("Issued generation")?;
        self.state
            .native_agent_service()
            .ok_or("Native owner")?
            .sessions()
            .into_iter()
            .find(|record| record.id == id && record.generation == generation)
            .ok_or_else(|| "Issued native record is unavailable".into())
    }

    fn native_tool_reply(
        &mut self,
        record: &NativeSessionRecord,
        name: &str,
        arguments: Value,
    ) -> TestResult<Value> {
        let session = self
            .state
            .native_agent_service()
            .ok_or("Native service")?
            .resolve(&record.target())?;
        let (sender, receiver) = mpsc::channel();
        let name = name.to_owned();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let outcome = match session.rpc("__spawn", json!({"name":name,"arguments":arguments})) {
                Ok(value) => CommandOutcome::Success {
                    value,
                    warnings: Vec::new(),
                },
                Err(message) => CommandOutcome::Failed {
                    code: "fixture_rpc".to_owned(),
                    message,
                },
            };
            let _ = sender.send(outcome);
            let _ = wake.send(());
        });
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .ok_or("Deadline")?;
        success(self.receive(&receiver, deadline)?)
    }

    fn restart(&mut self) -> TestResult<()> {
        self.restart_with_provider_enabled(self.state.config().agents.codex.enabled)
    }

    fn restart_with_provider_enabled(&mut self, enabled: bool) -> TestResult<()> {
        self.restart_with_provider_selection(enabled, None)
    }

    fn restart_with_provider_selection(
        &mut self,
        enabled: bool,
        selected: Option<&str>,
    ) -> TestResult<()> {
        if let Some(service) = self.state.terminal_agent_service() {
            service.shutdown_and_wait()?;
        }
        self.state
            .native_agent_service()
            .ok_or("Native owner missing")?
            .shutdown()?;
        let repaint = self.wake.clone();
        let (events, _receiver) = bootty_control::event_queue();
        let mut config = self.state.config().clone();
        config.agents.codex.enabled = enabled;
        if let Some(selected) = selected {
            selected.clone_into(&mut config.agents.codex.selected);
        }
        self.state = AppState::new_for_window_with_agents(
            config,
            "native-conversation-tests".to_owned(),
            support::backends(),
            Arc::new(move || {
                let _ = repaint.send(());
            }),
            None,
            None,
            Some(events),
        )?;
        Ok(())
    }

    fn tick(&mut self) {
        self.effects
            .extend(self.state.update_frame(frames::idle_frame(Instant::now())));
    }

    fn submit(&mut self, invocation: CommandInvocation) -> TestResult<CommandOutcome> {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .ok_or("deadline overflow")?;
        let response = self
            .state
            .app_command_sender(invocation.caller)
            .submit(invocation, deadline, CommandCancellation::new())
            .map_err(|error| format!("mailbox submission failed: {error:?}"))?;
        self.receive(&response, deadline)
    }

    fn receive(
        &mut self,
        response: &mpsc::Receiver<CommandOutcome>,
        deadline: Instant,
    ) -> TestResult<CommandOutcome> {
        loop {
            self.tick();
            match response.try_recv() {
                Ok(outcome) => return Ok(outcome),
                Err(mpsc::TryRecvError::Disconnected) => return Err("command disconnected".into()),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            self.wakes
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        }
    }

    fn spaces(&mut self) -> TestResult<Value> {
        success(self.submit(CommandInvocation::new(
            "spaces.list",
            Vec::new(),
            Caller::Socket,
        ))?)
    }

    fn saved(&mut self, binding: &CommandTarget) -> TestResult<Value> {
        let mut command = CommandInvocation::new("session.saved", Vec::new(), Caller::Socket);
        command.target = Some(binding.clone());
        success(self.submit(command)?)
    }

    fn binding(&mut self, other: bool) -> TestResult<CommandTarget> {
        let spaces = self.spaces()?;
        let space = spaces
            .as_array()
            .ok_or("Spaces are not an array")?
            .iter()
            .find(|space| {
                if other {
                    space["name"] == "Other"
                } else {
                    space["active"] == true
                }
            })
            .ok_or("Space missing")?;
        Ok(serde_json::from_value(space["target"].clone())?)
    }

    fn start(
        &mut self,
        binding: &CommandTarget,
        name: &str,
        identity: &str,
        prompt: &str,
    ) -> TestResult<Value> {
        self.start_with_profile(binding, name, identity, prompt, "captured")
    }

    fn start_with_profile(
        &mut self,
        binding: &CommandTarget,
        name: &str,
        identity: &str,
        prompt: &str,
        profile: &str,
    ) -> TestResult<Value> {
        let mut invocation = CommandInvocation::new(
            "agents.native.start",
            vec![
                "codex".to_owned(),
                self.directory.path().to_string_lossy().into_owned(),
                self.program.clone(),
                "[]".to_owned(),
                name.to_owned(),
                profile.to_owned(),
                identity.to_owned(),
                "Purpose title".to_owned(),
                prompt.to_owned(),
            ],
            Caller::Socket,
        );
        invocation.target = Some(binding.clone());
        success(self.submit(invocation)?)
    }

    fn wait_working(&mut self, target: &CommandTarget, deadline: Instant) -> TestResult<()> {
        loop {
            self.tick();
            let service = self
                .state
                .native_agent_service()
                .ok_or("native owner missing")?;
            if service.sessions().iter().any(|record| {
                record.target() == *target
                    && record.snapshot.status == NativeSessionStatus::Working
                    && record.snapshot.turn_id.is_some()
            }) {
                return Ok(());
            }
            self.wakes
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Closing sessions detaches observers, so join their workers while still owned.
        let observer_shutdown = self
            .state
            .terminal_agent_service()
            .map_or(Ok(()), |service| service.shutdown_and_wait());
        if let Some(service) = self.state.native_agent_service() {
            let _ = service.shutdown();
        }
        // Shared backends expose other fixtures too; the private cwd proves ownership.
        let root = self
            .directory
            .path()
            .canonicalize()
            .unwrap_or_else(|_| self.directory.path().to_owned());
        let owned = WorkspaceRepository::open(&self.state.config().config_path).map_or_else(
            |_| std::collections::HashSet::new(),
            |(_, snapshot)| {
                snapshot
                    .spaces()
                    .iter()
                    .flat_map(|space| space.binding().sessions().sessions())
                    .filter(|session| Path::new(&session.cwd).starts_with(&root))
                    .map(|session| session.backend_name.clone())
                    .collect::<std::collections::HashSet<_>>()
            },
        );
        if let Ok(spaces) = self.spaces() {
            for session in spaces
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|space| space["sessions"].as_array().into_iter().flatten())
                .filter(|session| {
                    session["name"]
                        .as_str()
                        .is_some_and(|name| owned.contains(name))
                })
            {
                if let Ok(target) =
                    serde_json::from_value::<CommandTarget>(session["target"].clone())
                {
                    let mut close =
                        CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
                    close.target = Some(target);
                    close.confirmation = Some(close.confirmation());
                    let _ = self.submit(close);
                }
            }
        }
        if let Err(error) = observer_shutdown {
            eprintln!("Terminal observation teardown failed: {error}");
            assert!(
                std::thread::panicking(),
                "Terminal observation teardown failed: {error}"
            );
        }
    }
}

fn success(outcome: CommandOutcome) -> TestResult<Value> {
    match outcome {
        CommandOutcome::Success { value, .. } => Ok(value),
        outcome => Err(format!("command failed: {outcome:?}").into()),
    }
}
fn record(value: &Value) -> TestResult<NativeSessionRecord> {
    Ok(serde_json::from_value(value["native"].clone())?)
}
fn native_command(
    command: &str,
    target: &CommandTarget,
    payload: &[&str],
    caller: Caller,
) -> CommandInvocation {
    let arguments = std::iter::once(target.handle.clone())
        .chain(std::iter::once(target.generation.to_string()))
        .chain(payload.iter().map(|value| (*value).to_owned()))
        .collect();
    CommandInvocation {
        target: Some(target.clone()),
        ..CommandInvocation::new(command, arguments, caller)
    }
}

#[rstest]
fn native_conversation_commands_stream_through_every_shared_mailbox_caller() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let launched = host.start(
        &binding,
        "conversation",
        "task-conversation",
        "first prompt",
    )?;
    let record = record(&launched)?;
    assert_eq!(record.task_identity.as_deref(), Some("task-conversation"));
    assert_eq!(record.snapshot.status, NativeSessionStatus::Idle);
    assert!(
        record
            .snapshot
            .transcript
            .iter()
            .any(|item| item.role == "tool" && item.text.contains("tool output"))
    );
    assert!(
        record
            .snapshot
            .transcript
            .iter()
            .any(|item| item.role == "assistant" && item.text == "reply: first prompt")
    );
    let terminal: CommandTarget = serde_json::from_value(launched["terminal"].clone())?;
    assert_eq!(terminal.kind, ResourceKind::Terminal);
    assert_ne!(terminal.handle, record.id);
    for caller in [
        Caller::CommandPalette,
        Caller::Keybinding,
        Caller::BuiltinKeybinding,
        Caller::Cli,
        Caller::Socket,
        Caller::Luau,
        Caller::Internal,
    ] {
        let text = format!("prompt from {caller:?}");
        let snapshot = success(host.submit(native_command(
            "agents.native.prompt",
            &record.target(),
            &[&text],
            caller,
        ))?)?;
        assert!(snapshot["transcript"].as_array().ok_or("transcript missing")?.iter().any(|item| item["role"] == "assistant" && item["text"] == format!("reply: {text}")));
        let title = format!("Title from {caller:?}");
        assert_eq!(
            success(host.submit(native_command(
                "agents.native.rename",
                &record.target(),
                &[&title],
                caller
            ))?)?,
            Value::Bool(true)
        );
        assert_eq!(
            host.state
                .native_agent_service()
                .ok_or("owner missing")?
                .activities()[0]
                .title,
            title
        );
    }
    let mut list = CommandInvocation::new("agents.native.list", Vec::new(), Caller::Socket);
    list.target = Some(binding);
    assert_eq!(
        success(host.submit(list)?)?
            .as_array()
            .ok_or("list missing")?
            .len(),
        1
    );
    Ok(())
}

#[rstest]
fn saved_annotation_batch_reaches_exact_native_conversation_alongside_message() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let record = record(&host.start(&binding, "conversation", "task-annotations", "")?)?;
    let notes = [(1, "Fix this spacing"), (2, "Keep the label 日本語")]
        .into_iter()
        .map(|(id, note)| Annotation {
            id,
            page: 1,
            address: "https://example.com/review".to_owned(),
            anchor: AnnotationAnchor {
                selection: None,
                selector: format!("#element-{id}"),
                text: format!("Captured element {id}"),
                tag: "button".to_owned(),
            },
            note: note.to_owned(),
            draft: None,
            conversation: Some(record.id.clone()),
            image: None,
            pending_conversation: None,
            revision: 1,
        })
        .collect::<Vec<_>>();
    let store = AnnotationStore::new(&host.directory.path().join("browser"));
    store.commit(&[], &notes)?;
    let committed = store.load()?;
    let message = format!(
        "Review both changes together.\n\n{}",
        annotation_batch(&committed)?
    );
    let snapshot = success(host.submit(native_command(
        "agents.native.prompt",
        &record.target(),
        &[&message],
        Caller::Internal,
    ))?)?;
    let transcript = snapshot["transcript"]
        .as_array()
        .ok_or("transcript missing")?;
    assert!(
        transcript
            .iter()
            .any(|item| item["role"] == "user" && item["text"] == message)
    );
    assert!(
        transcript
            .iter()
            .any(|item| item["role"] == "assistant" && item["text"] == format!("reply: {message}"))
    );
    // Provider delivery never deletes the local saved notes.
    assert_eq!(store.load()?, notes);
    Ok(())
}

#[rstest]
fn annotation_images_forward_only_exact_saved_versions() -> TestResult<()> {
    const CHILD: &str = "BOOTTY_ANNOTATION_IMAGE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let directory = assert_fs::TempDir::new()?;
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "annotation_images_forward_only_exact_saved_versions",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env("XDG_STATE_HOME", directory.path())
            .env("LOCALAPPDATA", directory.path())
            .env("APPDATA", directory.path())
            .output()?;
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    }
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let record = record(&host.start(&binding, "conversation", "task-visual-annotation", "")?)?;
    let child_state = std::path::PathBuf::from(
        std::env::var_os("XDG_STATE_HOME").ok_or("Missing isolated child state")?,
    );
    let browser_directory = bootty_config::identity::unix_daemon_state_path(
        bootty_config::identity::ApplicationIdentity::for_process(),
        None,
        Some(&child_state),
        None,
    )
    .ok_or("Missing browser state directory")?
    .with_file_name("browser");
    let store = AnnotationStore::new(&browser_directory);
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([12, 34, 56, 255]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)?;
    let rect = bootty_browser::AnnotationRect {
        x: 0.0,
        y: 0.0,
        width: 2.0,
        height: 2.0,
    };
    let geometry = bootty_browser::AnnotationImageGeometry {
        viewport: rect,
        selection: rect,
        crop: rect,
        source: rect,
        requested_source: rect,
        pixel_width: 2,
        pixel_height: 2,
    };
    let (image, _) = store.commit_image(png.get_ref(), &geometry)?;
    let note = Annotation {
        id: 1,
        page: 1,
        address: "https://example.com/review".into(),
        anchor: AnnotationAnchor {
            selector: "#button".into(),
            text: "Button".into(),
            tag: "button".into(),
            selection: None,
        },
        note: "Fix this button".into(),
        draft: None,
        conversation: Some(record.id.clone()),
        revision: 1,
        image: Some(image.clone()),
        pending_conversation: None,
    };
    store.commit(&[], std::slice::from_ref(&note))?;
    let refs = serde_json::to_string(std::slice::from_ref(&note))?;
    let snapshot = success(host.submit(native_command(
        "agents.native.prompt",
        &record.target(),
        &["visual-image", &refs],
        Caller::Internal,
    ))?)?;
    assert!(
        snapshot["transcript"]
            .as_array()
            .ok_or("Missing transcript")?
            .iter()
            .any(|item| item["role"] == "user" && item["images"][0]["id"] == image.id)
    );
    let mut foreign = note.clone();
    foreign.conversation = Some("other-conversation".into());
    let refs = serde_json::to_string(&[foreign])?;
    assert!(matches!(
        host.submit(native_command(
            "agents.native.prompt",
            &record.target(),
            &["visual-image", &refs],
            Caller::Internal
        ))?,
        CommandOutcome::Failed { .. }
    ));
    let captured = serde_json::to_string(std::slice::from_ref(&note))?;
    let mut changed = note.clone();
    changed.prepare_attachment("Changed after Send capture".into(), &record.id)?;
    store.commit(std::slice::from_ref(&note), std::slice::from_ref(&changed))?;
    assert!(matches!(
        host.submit(native_command(
            "agents.native.prompt",
            &record.target(),
            &["visual-image", &captured],
            Caller::Internal
        ))?,
        CommandOutcome::Failed { .. }
    ));
    assert_eq!(store.load()?, vec![changed]);
    Ok(())
}

#[rstest]
fn shared_command_interrupt_remains_available_while_prompt_reply_is_pending() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let record = record(&host.start(&binding, "conversation", "task-cancel", "")?)?;
    let target = record.target();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("deadline overflow")?;
    let waiting = host
        .state
        .app_command_sender(Caller::Socket)
        .submit(
            native_command("agents.native.prompt", &target, &["wait"], Caller::Socket),
            deadline,
            CommandCancellation::new(),
        )
        .map_err(|error| format!("mailbox failed: {error:?}"))?;
    host.wait_working(&target, deadline)?;
    assert!(matches!(waiting.try_recv(), Err(mpsc::TryRecvError::Empty)));
    success(host.submit(native_command(
        "agents.native.interrupt",
        &target,
        &[],
        Caller::Keybinding,
    ))?)?;
    success(host.receive(&waiting, deadline)?)?;
    assert_eq!(
        host.state
            .native_agent_service()
            .ok_or("owner missing")?
            .sessions()[0]
            .snapshot
            .status,
        NativeSessionStatus::Idle
    );
    let completed = success(host.submit(native_command(
        "agents.native.prompt",
        &target,
        &["after interrupt"],
        Caller::Socket,
    ))?)?;
    assert_eq!(completed["completed_turn"], true);
    assert!(
        completed["transcript"]
            .as_array()
            .ok_or("Transcript missing")?
            .iter()
            .any(|item| item["role"] == "assistant" && item["text"] == "reply: after interrupt")
    );
    let service = host.state.native_agent_service().ok_or("Native owner")?;
    let records = service.sessions();
    assert_eq!(records.len(), 1);
    let continued = records.first().ok_or("Conversation missing")?;
    assert_eq!(continued.target(), target);
    assert_eq!(continued.binding_id, record.binding_id);
    assert_eq!(continued.task_identity, record.task_identity);
    assert_eq!(continued.snapshot.session_id, record.snapshot.session_id);
    assert_eq!(continued.snapshot.status, NativeSessionStatus::Idle);
    let calls = service.resolve(&target)?.rpc("__calls", json!({}))?;
    let methods = calls["methods"]
        .as_array()
        .ok_or("Provider calls missing")?;
    assert_eq!(
        methods
            .iter()
            .filter(|method| method.as_str() == Some("thread/start"))
            .count(),
        1
    );
    assert!(
        !methods
            .iter()
            .any(|method| method.as_str() == Some("thread/resume"))
    );
    assert_eq!(
        methods
            .iter()
            .filter(|method| method.as_str() == Some("turn/start"))
            .count(),
        2
    );
    Ok(())
}

#[rstest]
fn native_target_generation_and_argument_identity_cannot_redirect_a_prompt() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let record = record(&host.start(&binding, "conversation", "task-target", "")?)?;
    let mut stale = record.target();
    stale.generation = stale
        .generation
        .checked_add(1)
        .ok_or("generation overflow")?;
    assert!(matches!(
        host.submit(native_command(
            "agents.native.prompt",
            &stale,
            &["stale"],
            Caller::Socket
        ))?,
        CommandOutcome::StaleTarget { .. }
    ));
    let mut mismatched = native_command(
        "agents.native.prompt",
        &record.target(),
        &["redirected"],
        Caller::Socket,
    );
    mismatched.arguments[0] = "another-native-id".to_owned();
    assert!(matches!(
        host.submit(mismatched)?,
        CommandOutcome::StaleTarget { .. }
    ));
    let mut absent = native_command(
        "agents.native.prompt",
        &record.target(),
        &["fallback"],
        Caller::Socket,
    );
    absent.target = None;
    assert!(!matches!(
        host.submit(absent)?,
        CommandOutcome::Success { .. }
    ));
    assert_eq!(
        host.state
            .native_agent_service()
            .ok_or("owner missing")?
            .sessions()[0]
            .snapshot
            .transcript
            .as_slice(),
        &[],
    );
    Ok(())
}

#[rstest]
#[case::disabled("codex", false)]
#[case::unsupported("unknown", true)]
fn rejected_provider_does_not_create_a_shell_or_native_record(
    #[case] provider: &str,
    #[case] enabled: bool,
) -> TestResult<()> {
    let mut host = Host::new(enabled)?;
    let binding = host.binding(false)?;
    let mut invocation = CommandInvocation::new(
        "agents.native.start",
        vec![
            provider.to_owned(),
            host.directory.path().to_string_lossy().into_owned(),
            host.program.clone(),
            "[]".to_owned(),
            "rejected".to_owned(),
            "captured".to_owned(),
            "task-rejected".to_owned(),
            "Rejected".to_owned(),
            String::new(),
        ],
        Caller::Socket,
    );
    invocation.target = Some(binding);
    let outcome = host.submit(invocation)?;
    assert!(
        matches!(
            outcome,
            CommandOutcome::Denied { .. } | CommandOutcome::Unsupported { .. }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        host.state
            .native_agent_service()
            .ok_or("owner missing")?
            .activities()
            .as_slice(),
        &[],
    );
    assert!(
        host.spaces()?
            .as_array()
            .ok_or("Spaces missing")?
            .iter()
            .all(|space| space["sessions"].as_array().is_some_and(Vec::is_empty))
    );
    Ok(())
}

#[rstest]
fn checkpointing_a_shared_backend_saves_only_each_spaces_own_sessions() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let other = host.binding(true)?;
    let home = host.binding(false)?;
    host.start(&other, "other-checkpoint", "other-task", "")?;
    host.start(&home, "home-checkpoint", "home-task", "")?;
    host.state.clear_last_error();
    host.state
        .checkpoint_sessions(chrono::Utc::now().timestamp());
    assert_eq!(host.state.last_error(), None);
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("checkpoint deadline")?;
    while host.state.session_checkpoint_pending() {
        host.tick();
        if host.state.session_checkpoint_pending() {
            host.wakes
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        }
    }
    assert_eq!(host.state.last_error(), None);
    let (_, reloaded) = WorkspaceRepository::open(&host.state.config().config_path)?;
    for space in reloaded.spaces() {
        let saved = space.binding().sessions().sessions();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].terminal_snapshot.is_some());
    }
    Ok(())
}

#[rstest]
fn native_task_identity_is_exactly_scoped_and_shell_keeps_real_tabs_and_splits() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let other = host.binding(true)?;
    let home = host.binding(false)?;
    let launched = host.start(
        &other,
        "other-conversation",
        "task-owned-elsewhere",
        "other prompt",
    )?;
    let record = record(&launched)?;
    let mut home_list = CommandInvocation::new("agents.native.list", Vec::new(), Caller::Socket);
    home_list.target = Some(home.clone());
    assert_eq!(
        success(host.submit(home_list)?)?
            .as_array()
            .ok_or("native list missing")?
            .as_slice(),
        Vec::<Value>::new().as_slice(),
    );
    let mut collision = CommandInvocation::new(
        "agents.native.start",
        vec![
            "codex".to_owned(),
            host.directory.path().to_string_lossy().into_owned(),
            host.program.clone(),
            "[]".to_owned(),
            "home-conversation".to_owned(),
            "captured".to_owned(),
            "task-owned-elsewhere".to_owned(),
            "Wrong Space".to_owned(),
            String::new(),
        ],
        Caller::Socket,
    );
    collision.target = Some(home);
    assert!(!matches!(
        host.submit(collision)?,
        CommandOutcome::Success { .. }
    ));
    assert_eq!(
        host.state
            .native_agent_service()
            .ok_or("owner missing")?
            .activities()
            .len(),
        1
    );
    assert_eq!(
        host.state
            .native_agent_service()
            .ok_or("owner missing")?
            .activities()[0]
            .id,
        record.id
    );
    let spaces = host.spaces()?;
    let other_space = spaces
        .as_array()
        .ok_or("Spaces missing")?
        .iter()
        .find(|space| space["name"] == "Other")
        .ok_or("Other missing")?;
    assert_eq!(
        other_space["sessions"]
            .as_array()
            .ok_or("sessions missing")?
            .len(),
        1
    );
    let shell: CommandTarget =
        serde_json::from_value(other_space["sessions"][0]["target"].clone())?;
    let mut tab = CommandInvocation::new(
        "terminal.create_tab",
        vec![
            "[]".to_owned(),
            host.directory.path().to_string_lossy().into_owned(),
        ],
        Caller::Socket,
    );
    tab.target = Some(shell);
    let tab_value = success(host.submit(tab)?)?;
    let terminal: CommandTarget = serde_json::from_value(tab_value["created"].clone())?;
    assert_eq!(terminal.kind, ResourceKind::Terminal);
    let mut focus = CommandInvocation::new("agents.focus", Vec::new(), Caller::Internal);
    focus.target = Some(terminal);
    success(host.submit(focus)?)?;
    success(host.submit(CommandInvocation::from_action(
        "split_right",
        Caller::Keybinding,
    ))?)?;
    assert_eq!(host.state.mux().selected_window_panes().len(), 1);
    let request_id = host
        .state
        .pending_new_surface()
        .ok_or("split chooser missing")?
        .id;
    success(host.submit(CommandInvocation::new(
        "surface.choose",
        vec![request_id.to_string(), "terminal".to_owned()],
        Caller::Keybinding,
    ))?)?;
    assert_eq!(host.state.mux().selected_window_panes().len(), 2);
    assert_eq!(
        host.state
            .native_agent_service()
            .ok_or("owner missing")?
            .activities()[0]
            .id,
        record.id
    );
    Ok(())
}

#[rstest]
#[case::foreground(false)]
#[case::background(true)]
fn observed_dialog_completion_focuses_only_foreground_native_start(
    #[case] background: bool,
) -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let scope = host.state.mux_scope();
    let cwd = host.directory.path().to_string_lossy().into_owned();
    let form = NewSessionForm::new(
        NewSessionDraft {
            scope,
            cwd: cwd.clone(),
            mode: NewSessionMode::Agent,
            prompt: "dialog prompt".to_owned(),
            applications: Vec::new(),
            attachments: Vec::new(),
            command: String::new(),
            provider: "codex".to_owned(),
            profiles: BTreeMap::from([("codex".to_owned(), "captured".to_owned())]),
            model_selection: None,
            permissions: bootty_agents::NativePermissionMode::ProviderDefault,
            isolated: false,
            isolation_preference: false,
            branch: String::new(),
            folder: String::new(),
            start_ref: String::new(),
            suffix: "dialog".to_owned(),
            identity: "task-dialog".to_owned(),
            directories: HashMap::new(),
        },
        vec![SessionDestination {
            scope,
            label: "Local".to_owned(),
            icon: "folder".to_owned(),
            color: [122, 162, 247],
            cwd,
            remote: None,
            target: binding,
            worktrees: false,
        }],
        host.providers.clone(),
    );
    let (discovery_wake, discovery_wakes) = mpsc::channel();
    let repaint: bootty_mux::RepaintHandle = Arc::new(move || {
        let _ = discovery_wake.send(());
    });
    let mut dialog = NewSessionDialog::open_form(form, &repaint);
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("dialog deadline overflow")?;
    // The public dialog starts discovery and enables Start only after observing its completion.
    let spec = loop {
        if dialog.poll().is_some() {
            continue;
        }
        let spec = dialog.spec();
        if spec.rows.first().is_some_and(|row| row.enabled) {
            break spec;
        }
        discovery_wakes.recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    };
    let event = dialog
        .apply(
            &DialogIntent::Activate {
                dialog: spec.id,
                row: RowId::new("start-session"),
                action: ActionId::new(if background {
                    "start-session-background"
                } else {
                    "start-session"
                }),
                payload: DialogPayload::default(),
            },
            &[],
        )
        .ok_or("start event missing")?;
    let NewSessionPickerEvent::Submit(invocation) = event else {
        return Err("Ordinary creation must submit before background naming".into());
    };
    assert_eq!(invocation.arguments[7], "dialog prompt");
    let outcome = host.submit(invocation)?;
    let value = success(outcome.clone())?;
    let native = record(&value)?.target();
    let (sender, receiver) = mpsc::channel();
    sender.send(outcome)?;
    dialog.started(receiver);
    let observed = dialog.poll().ok_or("completion missing")?;
    assert!(
        matches!(&observed,NewSessionPickerEvent::Started {foreground,..} if *foreground != background)
    );
    host.state.open_new_session_dialog_from_ui();
    *host
        .state
        .modal_dialog_mut()
        .ok_or("creation view missing")? = bootty_ui::ModalDialog::NewSession(Box::new(dialog));
    host.effects.clear();
    host.state.apply_picker_event(observed);
    host.tick();
    if !background {
        while !host.effects.iter().any(
            |effect| matches!(effect, AppEffect::NativeConversation(target) if target == &native),
        ) {
            host.wakes
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
            host.tick();
        }
    }
    let focuses = host
        .effects
        .iter()
        .filter_map(|effect| match effect {
            AppEffect::NativeConversation(target) => Some(target),
            _ => None,
        })
        .collect::<Vec<_>>();
    if background {
        assert_eq!(focuses.as_slice(), Vec::<&CommandTarget>::new().as_slice());
    } else {
        assert_eq!(focuses, [&native]);
    }
    loop {
        host.state.dialog_projection();
        host.tick();
        let activity = host
            .state
            .native_agent_service()
            .ok_or("native owner missing")?
            .activities()
            .into_iter()
            .find(|record| record.id == native.handle)
            .ok_or("native activity missing")?;
        let binding = host.binding(false)?;
        let mut invocation = CommandInvocation::new("session.saved", Vec::new(), Caller::Socket);
        invocation.target = Some(binding);
        let saved = success(host.submit(invocation)?)?;
        let title = saved
            .as_array()
            .ok_or("saved sessions missing")?
            .iter()
            .find(|session| session["identity"] == "task-dialog")
            .ok_or("saved task missing")?["title"]
            .as_str()
            .ok_or("saved title missing")?;
        if activity.title == "Fix native composer" && title == "Fix native composer" {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Naming did not finish: tab {:?}, task {title:?}",
                activity.title
            )
            .into());
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        host.tick();
    }
    Ok(())
}

#[rstest]
fn a_failed_provider_keeps_its_exact_native_pane_and_retries_in_place() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    fs::write(host.directory.path().join("fail-initialize"), "")?;
    let attachment = host.directory.path().join("initial.txt");
    fs::write(&attachment, "Retain this attachment across a failed start")?;
    let mut start = CommandInvocation::new(
        "agents.native.start",
        vec![
            "codex".into(),
            host.directory.path().to_string_lossy().into_owned(),
            host.program.clone(),
            "[]".into(),
            "failed-task".into(),
            "captured".into(),
            "failed-task-identity".into(),
            "Failed conversation".into(),
            "Do not replay".into(),
        ],
        Caller::Internal,
    );
    start.arguments.resize(10, String::new());
    start
        .arguments
        .push(serde_json::to_string(&vec![&attachment])?);
    start.target = Some(binding);
    let outcome = host.submit(start)?;
    assert!(
        matches!(outcome, CommandOutcome::Failed { .. }),
        "{outcome:?}"
    );
    let failed = host
        .state
        .native_agent_service()
        .ok_or("native owner missing")?
        .sessions()
        .into_iter()
        .next()
        .ok_or("failed reservation missing")?;
    assert_eq!(failed.snapshot.status, NativeSessionStatus::Error);
    assert_eq!(
        failed.pending_initial_message.as_deref(),
        Some("Do not replay")
    );
    assert_eq!(failed.attachments.len(), 1);
    fs::remove_file(attachment)?;
    let attachments = host
        .state
        .native_agent_service()
        .ok_or("native owner")?
        .resolve_prompt_attachments(
            &failed.target(),
            &failed
                .attachments
                .iter()
                .map(|reference| reference.id.clone())
                .collect::<Vec<_>>(),
        )?;
    let prompt = bootty_agents::NativePrompt::new_with_context(
        "Review the saved attachment".into(),
        Vec::new(),
        attachments,
        Vec::new(),
    )?;
    assert_eq!(prompt.attachment_references(), failed.attachments);
    let window = host
        .state
        .mux()
        .selected_window()
        .ok_or("failed pane not selected")?
        .to_owned();
    assert_eq!(host.state.mux().selected_window_panes().len(), 1);
    assert_eq!(
        host.state.mux().selected_window_panes()[0]
            .native_agent
            .as_deref(),
        Some(failed.id.as_str())
    );

    fs::remove_file(host.directory.path().join("fail-initialize"))?;
    let mut focus = CommandInvocation::new("agents.native.focus", Vec::new(), Caller::Socket);
    focus.target = Some(failed.target());
    let recovered: NativeSessionRecord = serde_json::from_value(success(host.submit(focus)?)?)?;
    assert_eq!(recovered.id, failed.id);
    assert_eq!(recovered.task_identity, failed.task_identity);
    assert_eq!(recovered.snapshot.status, NativeSessionStatus::Idle);
    assert_eq!(
        recovered.pending_initial_message.as_deref(),
        Some("Do not replay")
    );
    assert!(
        recovered.snapshot.transcript.is_empty(),
        "initial input was never accepted or replayed"
    );
    assert_eq!(host.state.mux().selected_window(), Some(window.as_str()));
    assert_eq!(host.state.mux().selected_session_windows().len(), 1);
    Ok(())
}

#[rstest]
fn native_tabs_share_the_existing_task_and_create_real_backend_windows() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let other = host.binding(true)?;
    let cwd = host.directory.path().to_string_lossy().into_owned();
    let mut shell = CommandInvocation::new(
        "session.create",
        vec![
            "shared-task".to_owned(),
            cwd.clone(),
            "[]".to_owned(),
            "existing-task".to_owned(),
            "Existing purpose".to_owned(),
        ],
        Caller::Socket,
    );
    shell.target = Some(binding.clone());
    let terminal = success(host.submit(shell)?)?;
    let before = host.spaces()?;
    let mut tab = CommandInvocation::new(
        "agents.native.tab",
        vec![
            "codex".to_owned(),
            cwd,
            host.program.clone(),
            "[]".to_owned(),
            "shared-task".to_owned(),
            "captured".to_owned(),
            "existing-task".to_owned(),
            "First conversation".to_owned(),
            "first prompt".to_owned(),
        ],
        Caller::Socket,
    );
    tab.target = Some(binding.clone());
    let first = record(&success(host.submit(tab.clone())?)?)?;
    "Second conversation".clone_into(&mut tab.arguments[7]);
    "second prompt".clone_into(&mut tab.arguments[8]);
    let second = record(&success(host.submit(tab.clone())?)?)?;
    assert_ne!(first.id, second.id);
    assert_ne!(first.target(), second.target());
    assert_eq!(first.task_identity.as_deref(), Some("existing-task"));
    assert_eq!(second.task_identity, first.task_identity);
    assert_eq!(second.binding_id, first.binding_id);
    assert_eq!(second.config.cwd, host.directory.path());
    assert_eq!(
        second.config.account_directory.as_deref(),
        Some(
            host.directory
                .path()
                .join("account")
                .to_string_lossy()
                .as_ref()
        ),
    );
    assert_eq!(
        host.spaces()?,
        before,
        "conversation tabs leave backend topology unchanged"
    );
    assert!(
        terminal["terminal"].is_object(),
        "the original shell is retained"
    );
    let session = host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.tag.identity.as_deref() == Some("existing-task"))
        .ok_or("Task attachment missing")?;
    assert_eq!(session.windows.len(), 3);
    for (record, window) in [
        (&first, &session.windows[1]),
        (&second, &session.windows[2]),
    ] {
        assert_eq!(window.panes.len(), 1);
        assert_eq!(
            window.panes[0].native_agent.as_deref(),
            Some(record.id.as_str())
        );
    }
    let second_window = session.windows[2].id.clone();
    let mut focus = CommandInvocation::new("agents.native.focus", vec![], Caller::Socket);
    focus.target = Some(second.target());
    success(host.submit(focus)?)?;
    assert_eq!(
        host.state.mux().selected_window(),
        Some(second_window.as_str())
    );
    for index in [1, 2, 3] {
        success(host.submit(CommandInvocation::new(
            "select_tab",
            vec![index.to_string()],
            Caller::Keybinding,
        ))?)?;
        assert_eq!(
            host.state
                .mux()
                .selected_session_windows()
                .iter()
                .find(|window| Some(window.id.as_str()) == host.state.mux().selected_window())
                .map(|window| window.index),
            Some(index)
        );
    }

    for (target, index, replacement) in [
        (other, 6, "existing-task"),
        (binding.clone(), 6, "missing-task"),
        (binding, 1, "/different-directory"),
    ] {
        let mut rejected = tab.clone();
        rejected.target = Some(target);
        replacement.clone_into(&mut rejected.arguments[index]);
        assert!(matches!(
            host.submit(rejected)?,
            CommandOutcome::StaleTarget { .. }
        ));
    }
    assert_eq!(
        host.state
            .native_agent_service()
            .ok_or("owner missing")?
            .activities()
            .len(),
        2
    );
    Ok(())
}

#[rstest]
#[case::tab(false)]
#[case::resumed_tab(true)]
fn native_tab_tools_read_the_captured_pane_after_another_task_is_focused(
    #[case] resume: bool,
) -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let mut shell = CommandInvocation::new(
        "session.create",
        vec![
            "tool-task".to_owned(),
            host.directory.path().to_string_lossy().into_owned(),
            "[]".to_owned(),
            "tool-task-id".to_owned(),
            "Tool task".to_owned(),
        ],
        Caller::Socket,
    );
    shell.target = Some(binding.clone());
    let shell = success(host.submit(shell)?)?;
    let terminal: CommandTarget = serde_json::from_value(
        shell
            .get("terminal")
            .cloned()
            .ok_or("Task Terminal missing")?,
    )?;
    let mut tab = CommandInvocation::new(
        "agents.native.tab",
        vec![
            "codex".to_owned(),
            host.directory.path().to_string_lossy().into_owned(),
            host.program.clone(),
            "[]".to_owned(),
            "tool-task".to_owned(),
            "captured".to_owned(),
            "tool-task-id".to_owned(),
            "Tool conversation".to_owned(),
            String::new(),
        ],
        Caller::Socket,
    );
    tab.target = Some(binding);
    let tab = success(host.submit(tab)?)?;
    let mut native = record(&tab)?;
    let native_terminal: CommandTarget = serde_json::from_value(tab["terminal"].clone())?;
    assert_ne!(native_terminal, terminal);
    if resume {
        host.state
            .native_agent_service()
            .ok_or("Native owner missing")?
            .stop(&native.target())?;
        native = serde_json::from_value(success(host.submit(native_command(
            "agents.native.resume",
            &native.target(),
            &[],
            Caller::Socket,
        ))?)?)?;
    }
    let other = host.binding(true)?;
    let foreign = host.start(&other, "foreign-task", "foreign-task-id", "")?;
    let foreign_terminal: CommandTarget = serde_json::from_value(
        foreign
            .get("terminal")
            .cloned()
            .ok_or("Foreign Terminal missing")?,
    )?;
    let mut focus = CommandInvocation::new("agents.focus", Vec::new(), Caller::Internal);
    focus.target = Some(foreign_terminal.clone());
    success(host.submit(focus)?)?;
    let capture = success(host.native_tool(&native, "terminal_read", json!({}))?)?;
    assert_eq!(capture.get("target"), Some(&json!(native_terminal)));
    assert_ne!(capture.get("target"), Some(&json!(foreign_terminal)));
    assert!(capture.get("capture").is_some());

    captured_workspace_reads(&mut host, &native, &native_terminal)?;

    let mut stale = terminal;
    stale.generation = stale
        .generation
        .checked_add(1)
        .ok_or("Generation overflow")?;
    let mut capture = CommandInvocation::new("terminal.capture", Vec::new(), Caller::Socket);
    capture.target = Some(stale);
    assert!(matches!(
        host.submit(capture)?,
        CommandOutcome::StaleTarget { .. }
    ));
    Ok(())
}

fn captured_workspace_reads(
    host: &mut Host,
    native: &NativeSessionRecord,
    native_terminal: &CommandTarget,
) -> TestResult<()> {
    let reply = host.native_tool_reply(native, "get_workspace_info", json!({}))?;
    assert_eq!(reply.get("isError"), Some(&json!(false)), "{reply}");
    let spaces = host.spaces()?;
    let captured = spaces
        .as_array()
        .ok_or("Spaces missing")?
        .iter()
        .find(|space| space["scope"] == native.binding_id)
        .ok_or("Captured Space missing")?;
    assert_eq!(
        reply.get("structuredContent"),
        Some(&json!({
            "name":captured["name"],"backend":captured["backend"],"host":captured["host"],
        }))
    );
    assert!(!captured["active"].as_bool().ok_or("Active flag missing")?);

    let reply = host.native_tool_reply(native, "list_terminals", json!({}))?;
    assert_eq!(reply.get("isError"), Some(&json!(false)), "{reply}");
    let content = reply
        .get("structuredContent")
        .ok_or("Structured terminal metadata missing")?;
    assert_eq!(content.get("truncated"), Some(&json!(false)));
    let terminals = content
        .get("terminals")
        .and_then(Value::as_array)
        .ok_or("Terminal metadata missing")?;
    let terminal = terminals
        .iter()
        .find(|terminal| terminal["session"] == "tool-task")
        .ok_or("Captured shell missing")?;
    assert!(
        terminals
            .iter()
            .all(|terminal| terminal["session"] != "foreign-task"
                && terminal["target"] != json!(native_terminal))
    );
    let target: CommandTarget = serde_json::from_value(terminal["target"].clone())?;
    let mut capture = CommandInvocation::new("terminal.capture", Vec::new(), Caller::Socket);
    capture.target = Some(target);
    assert!(success(host.submit(capture)?)?.get("capture").is_some());
    Ok(())
}

#[rstest]
#[case::captured("captured", "", Some("captured"))]
#[case::unprofiled("", "captured", None)]
fn restored_and_forked_native_tools_keep_the_captured_profile(
    #[case] initial_profile: &str,
    #[case] selected_after_restart: &str,
    #[case] expected_profile: Option<&str>,
) -> TestResult<()> {
    let mut host = Host::new_with_spawn(true, true)?;
    let binding = host.binding(false)?;
    let saved = record(&host.start_with_profile(
        &binding,
        "profile-owner",
        "profile-owner-task",
        "Saved answer",
        initial_profile,
    )?)?;
    assert_eq!(saved.config.profile.as_deref(), expected_profile);
    host.restart_with_provider_selection(true, Some(selected_after_restart))?;
    let saved = host
        .state
        .native_agent_service()
        .ok_or("Native owner")?
        .sessions()
        .into_iter()
        .find(|record| record.id == saved.id)
        .ok_or("Saved conversation")?;
    let resumed: NativeSessionRecord = serde_json::from_value(success(host.submit(
        native_command("agents.native.resume", &saved.target(), &[], Caller::Socket),
    )?)?)?;
    let forked: NativeSessionRecord = serde_json::from_value(success(host.submit(
        native_command("agents.native.fork", &resumed.target(), &[], Caller::Socket),
    )?)?)?;
    for parent in [resumed, forked] {
        assert_eq!(parent.config.profile.as_deref(), expected_profile);
        assert_eq!(
            host.native_tool_reply(&parent, "inspect_provider", json!({}))?["structuredContent"]["profile"],
            json!(expected_profile)
        );
        let profiles = host.native_tool_reply(&parent, "list_profiles", json!({}))?;
        assert_eq!(profiles["isError"], false);
        assert_eq!(
            profiles["structuredContent"],
            json!({
                "provider":"codex","captured_profile":expected_profile,
                "profiles":[{"id":"captured","name":"Captured"}],
            })
        );
        assert_eq!(
            host.native_tool_reply(&parent, "list_profiles", json!({"provider":"claude"}))?["isError"],
            true
        );
        assert_eq!(
            parent.config.account_directory,
            saved.config.account_directory
        );
        let child = success(host.native_tool(
            &parent,
            "spawn_agent",
            json!({"name":format!("profile-child-{}",parent.id.replace(':',"-")),
                   "provider":"codex","profile":expected_profile,"prompt":"Captured work"}),
        )?)?;
        let child = host.spawned_record(&child)?;
        assert_eq!(child.config.profile, parent.config.profile);
        assert_eq!(
            child.config.account_directory,
            parent.config.account_directory
        );
        let denied_profile = if expected_profile.is_some() {
            "other"
        } else {
            "captured"
        };
        assert_eq!(
            host.native_tool_reply(
                &parent,
                "spawn_agent",
                json!({"name":"wrong-profile-child","provider":"codex",
                   "profile":denied_profile,"prompt":"Wrong account"}),
            )?["isError"],
            true
        );
    }
    Ok(())
}

#[rstest]
fn concurrent_native_restore_keeps_the_first_launch_and_its_tools() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let launched = host.start(
        &binding,
        "restore-owner",
        "restore-owner-task",
        "Saved answer",
    )?;
    let saved = record(&launched)?;
    host.state
        .native_agent_service()
        .ok_or("Native owner missing")?
        .stop(&saved.target())?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("Resume deadline")?;
    let sender = host.state.app_command_sender(Caller::Socket);
    let first = sender
        .submit(
            native_command("agents.native.resume", &saved.target(), &[], Caller::Socket),
            deadline,
            CommandCancellation::new(),
        )
        .map_err(|error| format!("Resume submission: {error:?}"))?;
    let duplicate = sender
        .submit(
            native_command(
                "agents.native.resume",
                &saved.target(),
                &[],
                Caller::Internal,
            ),
            deadline,
            CommandCancellation::new(),
        )
        .map_err(|error| format!("Duplicate submission: {error:?}"))?;
    let resumed: NativeSessionRecord =
        serde_json::from_value(success(host.receive(&first, deadline)?)?)?;
    assert!(matches!(
        host.receive(&duplicate, deadline)?,
        CommandOutcome::Unavailable { .. } | CommandOutcome::StaleTarget { .. }
    ));
    verify_native_resume(&host, &saved, &resumed)?;
    success(host.native_tool(&resumed, "terminal_read", json!({}))?)?;
    Ok(())
}

#[rstest]
#[case::detached(false)]
#[case::restarted(true)]
fn saved_native_task_keyboard_selection_restores_its_terminal_checkpoint(
    #[case] restart: bool,
) -> TestResult<()> {
    let mut host = Host::new(true)?;
    let mut binding = host.binding(false)?;
    let launched = host.start(
        &binding,
        "saved-conversation",
        "saved-native-task",
        "Saved answer",
    )?;
    let saved_record = record(&launched)?;
    let native = saved_record.target();
    let spaces = host.spaces()?;
    let session = spaces
        .as_array()
        .ok_or("Spaces missing")?
        .iter()
        .flat_map(|space| space["sessions"].as_array().into_iter().flatten())
        .find(|session| session["name"] == "saved-conversation")
        .ok_or("Task shell missing")?;
    let mut close = CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
    close.target = Some(serde_json::from_value(session["target"].clone())?);
    close.confirmation = Some(close.confirmation());
    success(host.submit(close)?).map_err(|error| format!("Close saved task: {error}"))?;
    if restart {
        host.restart()?;
        binding = host.binding(false)?;
        let restored = host
            .state
            .native_agent_service()
            .ok_or("Restored native owner missing")?
            .sessions()
            .into_iter()
            .find(|record| record.target() == native)
            .ok_or("Restored conversation missing")?;
        assert_eq!(restored.snapshot.status, NativeSessionStatus::Stopped);
        assert_eq!(
            restored.snapshot.transcript,
            saved_record.snapshot.transcript
        );
    }
    let before = host.spaces()?;
    host.effects.clear();
    success(host.submit(CommandInvocation::new(
        "ui.sidebar.activate_session",
        Vec::new(),
        Caller::Keybinding,
    ))?)
    .map_err(|error| format!("Select saved history: {error}"))?;
    host.tick();
    let after = host.spaces()?;
    assert_ne!(
        after, before,
        "Selection restores the saved terminal topology"
    );
    assert!(
        host.state
            .mux()
            .all_sessions()
            .iter()
            .any(|session| { session.tag.identity.as_deref() == Some("saved-native-task") })
    );
    assert!(
        !host.effects.iter().any(
            |effect| matches!(effect, AppEffect::NativeConversation(target) if target == &native)
        ),
        "Saved terminal state takes priority over conversation-only focus"
    );
    let mut saved_command = CommandInvocation::new("session.saved", Vec::new(), Caller::Socket);
    saved_command.target = Some(binding);
    let saved = success(host.submit(saved_command)?)?;
    let retained = saved
        .as_array()
        .ok_or("Saved sessions missing")?
        .iter()
        .find(|session| session["identity"] == "saved-native-task")
        .ok_or("Saved native task missing")?;
    assert_ne!(retained["attachment"], Value::Null);
    let resumed = open_saved_native(&mut host, &saved_record)
        .map_err(|error| format!("Resume captured conversation: {error}"))?;
    success(host.native_tool(&resumed, "terminal_read", json!({}))?)?;
    Ok(())
}

fn open_saved_native(
    host: &mut Host,
    saved_record: &NativeSessionRecord,
) -> TestResult<NativeSessionRecord> {
    let mut focus = CommandInvocation::new("agents.native.focus", Vec::new(), Caller::Socket);
    focus.target = Some(saved_record.target());
    let resumed: NativeSessionRecord = serde_json::from_value(success(host.submit(focus)?)?)?;
    verify_native_resume(host, saved_record, &resumed)?;
    Ok(resumed)
}

fn verify_native_resume(
    host: &Host,
    saved_record: &NativeSessionRecord,
    resumed: &NativeSessionRecord,
) -> TestResult<Value> {
    let provider_identity = saved_record
        .config
        .session_id
        .as_deref()
        .ok_or("Captured provider session missing")?;
    assert_eq!(resumed.id, saved_record.id);
    assert_ne!(resumed.generation, saved_record.generation);
    assert_eq!(
        resumed.config.session_id.as_deref(),
        Some(provider_identity)
    );
    assert_eq!(
        resumed.config.account_directory,
        saved_record.config.account_directory
    );
    assert_eq!(
        resumed.snapshot.transcript,
        saved_record.snapshot.transcript
    );
    let calls = host
        .state
        .native_agent_service()
        .ok_or("Native owner missing")?
        .resolve(&resumed.target())?
        .rpc("__calls", json!({}))?;
    let methods = calls
        .get("methods")
        .and_then(Value::as_array)
        .ok_or("Provider calls missing")?;
    assert!(methods.iter().any(|method| method == "thread/resume"));
    assert!(
        !methods
            .iter()
            .any(|method| matches!(method.as_str(), Some("thread/start" | "turn/start")))
    );
    assert_eq!(
        calls.get("account_directory").and_then(Value::as_str),
        saved_record.config.account_directory.as_deref()
    );
    Ok(calls)
}

fn legacy_native_conversation(host: &Host) -> TestResult<NativeSessionRecord> {
    let (mut repository, snapshot) = WorkspaceRepository::open(&host.state.config().config_path)?;
    let space = snapshot
        .spaces()
        .iter()
        .find(|space| space.name() != "Other")
        .ok_or("Fixture primary Space")?;
    assert_eq!(space.binding().sessions().sessions(), &[]);
    // A genuine older saved row has provider metadata but never acquired a terminal checkpoint.
    repository.commit_binding_state(
        space.id(),
        &SessionMembership::from_sessions(vec![WorkspaceSession {
            identity: "legacy-native-id".to_owned(),
            backend_name: "legacy-native".to_owned(),
            display_name: "Legacy conversation".to_owned(),
            explicit: true,
            cwd: host.directory.path().to_string_lossy().into_owned(),
            state: SessionState::default(),
            terminal_snapshot: None,
        }]),
    )?;
    let config = NativeSessionConfig::from_launch(
        AgentKind::Codex,
        AgentLaunch {
            program: host.program.clone(),
            cwd: Some(host.directory.path().to_string_lossy().into_owned()),
            arguments: Vec::new(),
            ephemeral: false,
            account_directory: Some(
                host.directory
                    .path()
                    .join("account")
                    .to_string_lossy()
                    .into_owned(),
            ),
        },
    )?;
    let service = host.state.native_agent_service().ok_or("Native owner")?;
    let saved = service.create_for_task(
        &space.id().persistence_value().to_string(),
        "legacy-native-id",
        "Legacy conversation",
        config,
    )?;
    service.prompt(&saved.target(), "Retained legacy answer")?;
    service
        .sessions()
        .into_iter()
        .find(|record| record.id == saved.id)
        .ok_or_else(|| "Saved legacy native record".into())
}

#[rstest]
fn opening_without_a_checkpoint_starts_a_shell_and_resumes_the_same_provider() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let saved_record = legacy_native_conversation(&host)?;
    host.restart()?;
    let binding = host.binding(false)?;
    let saved_before = host.saved(&binding)?;
    host.effects.clear();
    success(host.submit(CommandInvocation::new(
        "ui.sidebar.activate_session",
        Vec::new(),
        Caller::Keybinding,
    ))?)?;
    host.tick();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("Resume deadline")?;
    let resumed = loop {
        host.tick();
        if let Some(record) = host
            .state
            .native_agent_service()
            .ok_or("Native owner missing")?
            .sessions()
            .into_iter()
            .find(|record| {
                record.id == saved_record.id
                    && record.generation != saved_record.generation
                    && record.snapshot.status == NativeSessionStatus::Idle
                    && host.effects.iter().any(|effect| {
                        matches!(effect, AppEffect::NativeConversation(target) if target == &record.target())
                    })
            })
        {
            break record;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    };
    let calls = verify_native_resume(&host, &saved_record, &resumed)?;
    assert!(host.effects.iter().any(|effect| {
        matches!(effect, AppEffect::NativeConversation(target) if target == &resumed.target())
    }));
    assert!(!host.effects.iter().any(|effect| {
        matches!(effect, AppEffect::NativeConversation(target) if target == &saved_record.target())
    }), "opening never focuses the stopped generation");
    assert_eq!(calls.get("bootty_tools"), Some(&Value::Bool(true)));
    let mut saved_after = host.saved(&binding)?;
    assert!(saved_after[0]["attachment"].is_string());
    saved_after[0]["attachment"] = Value::Null;
    assert_eq!(saved_after, saved_before);
    assert_eq!(host.state.mux().all_sessions().len(), 1);
    assert_eq!(
        host.state.mux().all_sessions()[0].windows[0].panes[0]
            .cwd
            .as_deref(),
        Some(
            host.directory
                .path()
                .to_str()
                .ok_or("Fixture path is not UTF-8")?
        )
    );
    Ok(())
}

#[rstest]
fn disabled_native_resume_does_not_restore_a_dead_task_or_change_its_conversation() -> TestResult<()>
{
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let saved_record = record(&host.start(
        &binding,
        "disabled-resume",
        "disabled-resume-id",
        "Saved work",
    )?)?;
    let spaces = host.spaces()?;
    let shell = spaces
        .as_array()
        .ok_or("Spaces missing")?
        .iter()
        .flat_map(|space| space["sessions"].as_array().into_iter().flatten())
        .find(|session| session["name"] == "disabled-resume")
        .ok_or("Task shell missing")?;
    let mut close = CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
    close.target = Some(serde_json::from_value(shell["target"].clone())?);
    close.confirmation = Some(close.confirmation());
    success(host.submit(close)?)?;
    host.restart_with_provider_enabled(false)?;
    let before = host.spaces()?;
    assert!(matches!(
        host.submit(native_command(
            "agents.native.resume",
            &saved_record.target(),
            &[],
            Caller::Socket
        ))?,
        CommandOutcome::Denied { .. }
    ));
    assert_eq!(host.spaces()?, before);
    let restored = host
        .state
        .native_agent_service()
        .ok_or("Native owner missing")?
        .sessions()
        .into_iter()
        .find(|record| record.id == saved_record.id)
        .ok_or("Saved conversation missing")?;
    assert_eq!(restored.target(), saved_record.target());
    assert_eq!(restored.snapshot.status, NativeSessionStatus::Stopped);
    assert_eq!(restored.config.session_id, saved_record.config.session_id);
    assert_eq!(
        restored.snapshot.transcript,
        saved_record.snapshot.transcript
    );
    Ok(())
}

#[rstest]
#[case::revoked(false)]
#[case::provider_disabled(true)]
fn pending_native_lease_revocation_cancels_saved_task_restore_before_acceptance(
    #[case] disable_provider: bool,
) -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let saved_record = record(&host.start(
        &binding,
        "pending-restore",
        "pending-restore-id",
        "Retained answer",
    )?)?;
    let spaces = host.spaces()?;
    let shell = spaces
        .as_array()
        .ok_or("Spaces missing")?
        .iter()
        .flat_map(|space| space["sessions"].as_array().into_iter().flatten())
        .find(|session| session["name"] == "pending-restore")
        .ok_or("Task shell missing")?;
    let mut close = CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
    close.target = Some(serde_json::from_value(shell["target"].clone())?);
    close.confirmation = Some(close.confirmation());
    success(host.submit(close)?)?;
    let owner = host.state.terminal_agent_service().ok_or("Tool owner")?;
    let bridge = ToolBridge::prepare(
        ToolBridgeContext {
            scope: ToolScope {
                provider: AgentKind::Codex,
                binding: binding.clone(),
            },
            caller: Caller::Socket,
            policy: ToolPolicy::own_terminal(),
            captures: Vec::new(),
            spawn: None,
        },
        &std::env::current_exe()?,
        Arc::new(|_: CommandInvocation, _: Instant, _: CommandCancellation| {
            CommandOutcome::Unavailable {
                message: "This pending attachment has not launched its provider".to_owned(),
            }
        }),
    )?;
    let bridge = owner.retain_tool_attachment(AgentKind::Codex, bridge)?;
    let cancellation = CommandCancellation::new();
    let _launch = bridge.lease().begin_launch(&cancellation)?;
    let before = host.spaces()?;
    let saved_before = host.saved(&binding)?;
    // The public mailbox gives an exact pre-admission barrier: no owner frame is pumped
    // between submission and revocation. This exercises the lease-to-restore boundary.
    let mut restore = CommandInvocation::new(
        "session.reopen",
        vec!["pending-restore-id".to_owned()],
        Caller::Socket,
    );
    restore.target = Some(binding.clone());
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("Deadline")?;
    let response = host
        .state
        .app_command_sender(Caller::Socket)
        .submit(restore, deadline, cancellation.clone())
        .map_err(|error| format!("Mailbox submission: {error:?}"))?;
    if disable_provider {
        owner.set_provider_tools_enabled(AgentKind::Codex, false);
    } else {
        bridge.lease().revoke();
    }
    assert!(cancellation.is_cancelled());
    assert_eq!(
        host.receive(&response, deadline)?,
        CommandOutcome::cancelled()
    );
    assert_eq!(host.spaces()?, before);
    assert_eq!(host.saved(&binding)?, saved_before);
    let retained = host
        .state
        .native_agent_service()
        .ok_or("Native owner")?
        .sessions()
        .into_iter()
        .find(|record| record.id == saved_record.id)
        .ok_or("Retained native conversation")?;
    assert_eq!(retained.target(), saved_record.target());
    assert_eq!(retained.config.session_id, saved_record.config.session_id);
    assert_eq!(
        retained.snapshot.transcript,
        saved_record.snapshot.transcript
    );
    Ok(())
}

#[rstest]
#[case::invalid_terminal("terminal", "[true]", true)]
#[case::unknown_profile("profile", "unknown", true)]
#[case::disabled_profile("profile", "codex", false)]
fn invalid_surface_choices_leave_a_detached_native_task_closed(
    #[case] kind: &str,
    #[case] argument: &str,
    #[case] provider_enabled: bool,
) -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let native = record(&host.start(&binding, "chooser-restore", "chooser-restore-id", "")?)?;
    let spaces = host.spaces()?;
    let shell = spaces
        .as_array()
        .ok_or("Spaces missing")?
        .iter()
        .flat_map(|space| space["sessions"].as_array().into_iter().flatten())
        .find(|session| session["name"] == "chooser-restore")
        .ok_or("Task terminal missing")?;
    let mut close = CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
    close.target = Some(serde_json::from_value(shell["target"].clone())?);
    close.confirmation = Some(close.confirmation());
    success(host.submit(close)?)?;
    if !provider_enabled {
        host.restart_with_provider_enabled(false)?;
    }
    let binding = host.binding(false)?;
    let target = host
        .state
        .native_agent_service()
        .ok_or("Native owner missing")?
        .sessions()
        .into_iter()
        .find(|record| record.id == native.id)
        .ok_or("Retained conversation missing")?
        .target();
    let before = host.spaces()?;
    let saved_before = host.saved(&binding)?;
    let mut open = CommandInvocation::from_action("new_tab", Caller::Socket);
    open.target = Some(target);
    success(host.submit(open)?)?;
    let id = host
        .state
        .pending_new_surface()
        .ok_or("Chooser missing")?
        .id;
    let outcome = host.submit(CommandInvocation::new(
        "surface.choose",
        vec![id.to_string(), kind.to_owned(), argument.to_owned()],
        Caller::Socket,
    ))?;
    assert!(
        matches!(
            outcome,
            CommandOutcome::Failed { .. } | CommandOutcome::Denied { .. }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        host.spaces()?,
        before,
        "invalid choices cannot restore a terminal"
    );
    assert_eq!(host.saved(&binding)?, saved_before);
    assert_eq!(
        host.state
            .pending_new_surface()
            .ok_or("Chooser removed")?
            .id,
        id
    );
    Ok(())
}

#[rstest]
#[case(Caller::Internal, true)]
#[case(Caller::CommandPalette, true)]
#[case(Caller::Keybinding, true)]
#[case(Caller::BuiltinKeybinding, true)]
#[case(Caller::Socket, false)]
#[case(Caller::Cli, false)]
#[case(Caller::Luau, false)]
fn captured_agent_creation_admits_desktop_files_without_widening_external_callers(
    #[case] caller: Caller,
    #[case] admitted: bool,
) -> TestResult<()> {
    let mut host = Host::new(true)?;
    let file = host.directory.path().join("review.md");
    fs::write(&file, "attachment marker")?;
    success(host.submit(CommandInvocation::from_action("new_tab", caller))?)?;
    let request = host
        .state
        .pending_new_surface()
        .ok_or("Chooser missing")?
        .clone();
    let invocation = CommandInvocation::new(
        "surface.create_agent",
        vec![
            request.id.to_string(),
            "codex".to_owned(),
            request.cwd,
            host.program.clone(),
            "[]".to_owned(),
            "attachment-review".to_owned(),
            "captured".to_owned(),
            "attachment-review-id".to_owned(),
            "Attachment review".to_owned(),
            "Read the attachment".to_owned(),
            String::new(),
            serde_json::to_string(&vec![file])?,
        ],
        caller,
    );
    let outcome = host.submit(invocation)?;
    if admitted {
        let created = record(&success(outcome)?)?;
        assert_eq!(created.attachments.len(), 1);
        assert_eq!(created.attachments[0].name, "review.md");
    } else {
        assert!(
            matches!(outcome, CommandOutcome::Failed { .. }),
            "{outcome:?}"
        );
        assert!(
            host.state
                .native_agent_service()
                .ok_or("Native owner missing")?
                .sessions()
                .is_empty()
        );
    }
    Ok(())
}

#[rstest]
fn first_prompt_rejection_returns_the_observed_created_conversation() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let value = host.start(
        &binding,
        "rejected-prompt",
        "rejected-prompt-id",
        "reject-first",
    )?;
    let error = value
        .get("first_prompt_error")
        .and_then(Value::as_str)
        .ok_or("The saved conversation must expose its first prompt rejection")?;
    let rejection: Value = serde_json::from_str(error)?;
    assert_eq!(
        rejection.get("message").and_then(Value::as_str),
        Some("first prompt rejected")
    );
    let record = record(&value)?;
    assert_eq!(record.snapshot.status, NativeSessionStatus::Error);
    assert_eq!(record.snapshot.first_turn, None);
    assert_eq!(record.task_identity.as_deref(), Some("rejected-prompt-id"));
    assert_eq!(
        host.state
            .native_agent_service()
            .ok_or("Native owner")?
            .sessions()
            .len(),
        1
    );
    assert_eq!(host.state.mux().all_sessions().len(), 1);
    Ok(())
}

fn attach_sibling_tui(
    host: &Host,
    binding: &CommandTarget,
    terminal: &CommandTarget,
    binding_id: &str,
) -> TestResult<bootty_agents::ToolLease> {
    let service = host.state.terminal_agent_service().ok_or("Tool owner")?;
    // Public registration preserves a backend-owned TUI's independent account and bridge.
    let bridge = ToolBridge::prepare(
        ToolBridgeContext {
            scope: ToolScope {
                provider: AgentKind::Codex,
                binding: binding.clone(),
            },
            caller: Caller::Socket,
            policy: ToolPolicy {
                spawn_children: true,
                ..ToolPolicy::own_terminal()
            },
            captures: Vec::new(),
            spawn: Some(ToolSpawnContext {
                profile: Some("terminal-profile".to_owned()),
            }),
        },
        &std::env::current_exe()?,
        Arc::new(|_: CommandInvocation, _: Instant, _: CommandCancellation| {
            CommandOutcome::Unavailable {
                message: "The sibling fixture's MCP endpoint is not invoked".to_owned(),
            }
        }),
    )?;
    let sibling = bridge.lease().clone();
    let launch = AgentLaunch {
        program: host.program.clone(),
        cwd: Some(host.directory.path().to_string_lossy().into_owned()),
        arguments: vec!["--model".to_owned(), "terminal-model".to_owned()],
        ephemeral: false,
        account_directory: Some(
            host.directory
                .path()
                .join("other-account")
                .to_string_lossy()
                .into_owned(),
        ),
    };
    let prepared = service.prepare_with_tools(AgentKind::Codex, launch, bridge, None)?;
    service.register(prepared, terminal.clone(), binding_id.to_owned())?;
    Ok(sibling)
}

#[rstest]
fn legacy_spawn_preserves_the_registered_callers_authority() -> TestResult<()> {
    let mut host = Host::new_with_spawn(true, true)?;
    let binding = host.binding(false)?;
    let created = host.start(&binding, "legacy-parent", "legacy-parent-id", "")?;
    let record = record(&created)?;
    let terminal: CommandTarget =
        serde_json::from_value(created.get("terminal").cloned().ok_or("Parent terminal")?)?;
    let _lease = attach_sibling_tui(&host, &binding, &terminal, &record.binding_id)?;
    let before = host.spaces()?;
    let mut legacy = CommandInvocation::new(
        "agents.spawn",
        vec![json!({"kind":"shell","name":"legacy-child"}).to_string()],
        Caller::Cli,
    );
    legacy.target = Some(terminal);
    assert!(matches!(
        host.submit(legacy.clone())?,
        CommandOutcome::Denied { .. }
    ));
    assert_eq!(host.spaces()?, before);
    legacy.caller = Caller::Socket;
    let child = success(host.submit(legacy)?)?;
    let target: CommandTarget =
        serde_json::from_value(child.get("terminal").cloned().ok_or("Child terminal")?)?;
    assert_eq!(target.kind, ResourceKind::Terminal);
    let identity = child
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or("Child identity")?;
    let saved = host.saved(&binding)?;
    assert!(
        saved
            .as_array()
            .ok_or("Saved sessions missing")?
            .iter()
            .any(|session| session.get("identity").and_then(Value::as_str) == Some(identity))
    );
    Ok(())
}

#[rstest]
fn spawn_guard_preserves_caller_cancellation_at_durable_creation_acceptance() -> TestResult<()> {
    let mut host = Host::new_with_spawn(true, true)?;
    let binding = host.binding(false)?;
    let created = host.start(&binding, "cancel-parent", "cancel-parent-id", "")?;
    let record = record(&created)?;
    let terminal: CommandTarget =
        serde_json::from_value(created.get("terminal").cloned().ok_or("Parent terminal")?)?;
    let lease = attach_sibling_tui(&host, &binding, &terminal, &record.binding_id)?;
    let incoming = CommandCancellation::new();
    let guard = lease.begin_spawn_with_cancellation(
        &ToolSpawnRequest::Shell {
            name: "cancelled-child".to_owned(),
            title: None,
        },
        incoming.clone(),
    )?;
    let before = host.spaces()?;
    let saved_before = host.saved(&binding)?;
    // Exercise the public guard-to-owner boundary: AppState does not expose a nested spawn queue
    // barrier, so this test does not claim to drive agents.spawn through that forwarding step.
    let mut create = CommandInvocation::new(
        "session.create",
        vec![
            "cancelled-child".to_owned(),
            host.directory.path().to_string_lossy().into_owned(),
            "[]".to_owned(),
            "cancelled-child-id".to_owned(),
            "Cancelled child".to_owned(),
        ],
        guard.caller(),
    );
    create.target = Some(binding.clone());
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("Deadline")?;
    let response = host
        .state
        .app_command_sender(guard.caller())
        .submit(create, deadline, guard.cancellation())
        .map_err(|error| format!("Mailbox submission: {error:?}"))?;
    assert!(incoming.cancel());
    assert_eq!(
        host.receive(&response, deadline)?,
        CommandOutcome::cancelled()
    );
    assert_eq!(host.spaces()?, before);
    let saved = host.saved(&binding)?;
    assert_eq!(saved, saved_before);
    assert!(
        !saved
            .as_array()
            .ok_or("Saved sessions missing")?
            .iter()
            .any(|session| session.get("identity").and_then(Value::as_str)
                == Some("cancelled-child-id"))
    );
    Ok(())
}

#[rstest]
#[case::interrupt("interrupt_spawned_agent", "interrupt")]
#[case::stop("stop_spawned_agent", "stop")]
fn native_parent_supervises_only_children_from_its_exact_live_grant(
    #[case] tool: &str,
    #[case] operation: &str,
) -> TestResult<()> {
    let mut host = Host::new_with_spawn(true, true)?;
    let binding = host.binding(false)?;
    let parent = record(&host.start(&binding, "supervisor", "supervisor-task", "parent work")?)?;
    let spawned = success(host.native_tool(
        &parent,
        "spawn_agent",
        json!({"name":"supervised-child","provider":"codex","prompt":"child work"}),
    )?)?;
    let child = host.spawned_record(&spawned)?;
    let child_pane = host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.tag.identity == child.task_identity)
        .and_then(|session| session.windows.first())
        .and_then(|window| window.panes.first())
        .ok_or("Child pane")?
        .pane_id
        .clone()
        .ok_or("Child pane ID")?;
    let selected = host.state.mux().selected_session().map(str::to_owned);
    let foreign_binding = host.binding(true)?;
    let foreign = record(&host.start(&foreign_binding, "unrelated", "unrelated-task", "")?)?;
    for (id, generation) in [
        (parent.id.as_str(), parent.generation),
        (foreign.id.as_str(), foreign.generation),
        (child.id.as_str(), child.generation.saturating_add(1)),
    ] {
        assert_eq!(
            host.native_tool_reply(&parent, tool, json!({"id":id,"generation":generation}))?["isError"],
            true
        );
    }
    assert_eq!(
        host.native_tool_reply(
            &child,
            tool,
            json!({"id":parent.id,"generation":parent.generation}),
        )?["isError"],
        true
    );
    assert_eq!(
        host.native_tool_reply(
            &parent,
            tool,
            json!({"id":child.id,"generation":child.generation,"operation":"stop"}),
        )?["isError"],
        true
    );
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("Child deadline")?;
    let waiting = host
        .state
        .app_command_sender(Caller::Socket)
        .submit(
            native_command(
                "agents.native.prompt",
                &child.target(),
                &["wait"],
                Caller::Socket,
            ),
            deadline,
            CommandCancellation::new(),
        )
        .map_err(|error| format!("Child prompt: {error:?}"))?;
    host.wait_working(&child.target(), deadline)?;
    let controlled = host.native_tool_reply(
        &parent,
        tool,
        json!({"id":child.id,"generation":child.generation}),
    )?;
    assert_eq!(controlled["isError"], false);
    let outcome: CommandOutcome = serde_json::from_str(
        controlled["content"][0]["text"]
            .as_str()
            .ok_or("Control reply")?,
    )?;
    let value = success(outcome)?;
    assert_eq!(value["operation"], operation);
    assert_eq!(value["agent"]["id"], child.id);
    assert_eq!(value["agent"]["spawn_parent"], json!(parent.target()));
    let _completed = host.receive(&waiting, deadline)?;
    let service = host.state.native_agent_service().ok_or("Native owner")?;
    if operation == "stop" {
        assert!(service.resolve(&child.target()).is_err());
        let saved = service
            .sessions()
            .into_iter()
            .find(|record| record.target() == child.target())
            .ok_or("Saved child")?;
        assert_eq!(saved.snapshot.status, NativeSessionStatus::Stopped);
        assert!(
            saved
                .snapshot
                .transcript
                .iter()
                .any(|item| { item.role == "user" && item.text == "child work" }),
            "stopping retains the child's accepted conversation"
        );
        assert_eq!(
            host.native_tool_reply(
                &parent,
                tool,
                json!({"id":child.id,"generation":child.generation})
            )?["isError"],
            false
        );
    } else {
        assert_eq!(
            service.resolve(&child.target())?.snapshot().status,
            NativeSessionStatus::Idle
        );
    }
    assert_eq!(host.state.mux().selected_session(), selected.as_deref());
    assert!(
        host.state
            .mux()
            .all_sessions()
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
            .any(|pane| pane.pane_id.as_deref() == Some(child_pane.as_str())
                && pane.native_agent.as_deref() == Some(child.id.as_str()))
    );
    assert!(
        host.state
            .native_agent_service()
            .ok_or("Native owner")?
            .resolve(&parent.target())
            .is_ok()
    );
    host.restart()?;
    let restored = open_saved_native(&mut host, &parent)?;
    assert_ne!(restored.generation, parent.generation);
    assert_eq!(
        host.native_tool_reply(
            &restored,
            tool,
            json!({"id":child.id,"generation":child.generation}),
        )?["isError"],
        true,
        "a renewed parent grant cannot inherit old child control"
    );
    Ok(())
}

#[rstest]
#[case(false)]
#[case(true)]
fn native_private_spawn_uses_its_parent_even_when_a_tui_shares_the_terminal(
    #[case] sibling_tui: bool,
) -> TestResult<()> {
    let mut host = Host::new_with_spawn(true, true)?;
    let binding = host.binding(false)?;
    let created = host.start(&binding, "native-parent", "native-parent-id", "parent work")?;
    let record = record(&created)?;
    let terminal: CommandTarget =
        serde_json::from_value(created.get("terminal").cloned().ok_or("Parent terminal")?)?;
    let service = host.state.terminal_agent_service().ok_or("Tool owner")?;
    let sibling_lease = sibling_tui
        .then(|| attach_sibling_tui(&host, &binding, &terminal, &record.binding_id))
        .transpose()?;
    let selected = host.state.mux().selected_session().map(str::to_owned);
    let child = success(host.native_tool(
        &record,
        "spawn_agent",
        json!({"name":"native-child","provider":"codex","prompt":"captured child work"}),
    )?)?;
    let child_record = host.spawned_record(&child)?;
    assert_eq!(
        child["native"],
        json!({
            "id":child_record.id,"generation":child_record.generation,"title":child_record.title,
            "provider":child_record.config.provider,"status":child_record.snapshot.status,
            "spawn_parent":record.target(),
        })
    );
    assert_eq!(child_record.spawn_parent, Some(record.target()));
    assert_ne!(child_record.config.session_id, record.config.session_id);
    assert_eq!(
        child_record.config.account_directory,
        record.config.account_directory
    );
    assert_eq!(child_record.config.program, record.config.program);
    assert_eq!(child_record.config.arguments, record.config.arguments);
    assert_eq!(
        child_record
            .config
            .remote
            .as_ref()
            .map(|remote| &remote.host),
        record.config.remote.as_ref().map(|remote| &remote.host)
    );
    assert_eq!(
        host.state.mux().selected_session(),
        selected.as_deref(),
        "child spawning keeps foreground selection"
    );
    assert_eq!(
        host.native_tool_reply(&child_record, "terminal_read", json!({}))?["isError"],
        false
    );
    assert_eq!(
        host.native_tool_reply(&child_record, "list_agents", json!({}))?["isError"],
        true
    );
    assert_eq!(
        host.native_tool_reply(&child_record, "spawn_shell", json!({"name":"grandchild"}))?["isError"],
        true
    );
    let task = child_record
        .task_identity
        .as_deref()
        .ok_or("Native child task")?;
    let pane = host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.tag.identity.as_deref() == Some(task))
        .and_then(|session| session.windows.first())
        .and_then(|window| window.panes.first())
        .ok_or("Child backend pane")?;
    assert_eq!(pane.native_agent.as_deref(), Some(child_record.id.as_str()));
    let mut unknown = CommandInvocation::new(
        "agents.spawn",
        vec![
            json!({"kind":"shell","name":"unknown-child"}).to_string(),
            u64::MAX.to_string(),
        ],
        Caller::Socket,
    );
    unknown.target = Some(terminal.clone());
    assert!(matches!(
        host.submit(unknown)?,
        CommandOutcome::Denied { .. }
    ));
    let mut unknown_native = CommandInvocation::new(
        "agents.native.spawn",
        vec![
            record.id.clone(),
            record.generation.to_string(),
            json!({"kind":"shell","name":"foreign-child"}).to_string(),
            u64::MAX.to_string(),
        ],
        Caller::Socket,
    );
    unknown_native.target = Some(record.target());
    assert!(matches!(
        host.submit(unknown_native)?,
        CommandOutcome::Denied { .. }
    ));
    host.state
        .native_agent_service()
        .ok_or("Native owner")?
        .stop(&record.target())?;
    assert_eq!(
        host.native_tool_reply(&child_record, "terminal_read", json!({}))?["isError"],
        true
    );
    if let Some(sibling) = sibling_lease {
        assert_eq!(sibling.spawn_enabled(), true);
        assert!(
            service
                .spawn_parent_for_attachment(&terminal, sibling.attachment_id(), Caller::Cli)
                .is_none()
        );
        assert!(
            service
                .spawn_parent_for_attachment(&terminal, sibling.attachment_id(), Caller::Socket)
                .is_some()
        );
    }
    host.restart()?;
    let mut resume = CommandInvocation::new(
        "agents.native.resume",
        vec![child_record.id.clone(), child_record.generation.to_string()],
        Caller::Socket,
    );
    resume.target = Some(child_record.target());
    let restored: NativeSessionRecord = serde_json::from_value(success(host.submit(resume)?)?)?;
    assert_eq!(restored.spawn_parent, Some(record.target()));
    assert_eq!(restored.config.session_id, child_record.config.session_id);
    assert_eq!(
        restored.config.account_directory,
        child_record.config.account_directory
    );
    let calls = host
        .state
        .native_agent_service()
        .ok_or("Native service")?
        .resolve(&restored.target())?
        .rpc("__calls", json!({}))?;
    assert_eq!(
        calls["bootty_tools"], false,
        "provider restoration cannot renew a parent's revoked grant"
    );
    Ok(())
}

#[rstest]
fn spawned_terminal_tools_keep_the_exact_shell_and_foreground() -> TestResult<()> {
    let mut host = Host::new_with_spawn(true, true)?;
    let binding = host.binding(false)?;
    let created = host.start(&binding, "parent", "parent-terminal-tools", "parent work")?;
    let parent = record(&created)?;
    let parent_terminal = created.get("terminal").ok_or("Parent Terminal")?.clone();
    let other_binding = host.binding(true)?;
    let other = record(&host.start(
        &other_binding,
        "other",
        "other-terminal-tools",
        "other work",
    )?)?;
    let selected = host.state.mux().selected_session().map(str::to_owned);
    let receipt =
        success(host.native_tool(&parent, "spawn_shell", json!({"name":"owned-shell"}))?)?;
    let terminal = receipt.get("terminal").ok_or("Created Terminal")?.clone();
    for owner in [&parent, &other] {
        let target = if owner.id == parent.id {
            parent_terminal.clone()
        } else {
            terminal.clone()
        };
        assert_eq!(
            host.native_tool_reply(
                owner,
                "paste_spawned_terminal",
                json!({"terminal":target,"text":"must not be sent"})
            )?["isError"],
            true
        );
    }
    let mut stale: CommandTarget = serde_json::from_value(terminal.clone())?;
    stale.generation = stale.generation.saturating_add(1);
    for tool in [
        "read_spawned_terminal",
        "submit_spawned_terminal",
        "interrupt_spawned_terminal",
        "close_spawned_terminal",
    ] {
        assert_eq!(
            host.native_tool_reply(&parent, tool, json!({"terminal":stale}))?["isError"],
            true
        );
        assert_eq!(
            host.native_tool_reply(
                &parent,
                tool,
                json!({"terminal":terminal,"operation":"close"})
            )?["isError"],
            true
        );
    }
    let capture = success(host.native_tool(
        &parent,
        "read_spawned_terminal",
        json!({"terminal":terminal}),
    )?)?;
    assert_eq!(capture.get("target"), Some(&terminal));
    assert!(capture.get("capture").is_some());
    success(host.native_tool(&parent, "paste_spawned_terminal", json!({"terminal":terminal,"text":"printf '\\123\\103\\117\\120\\105\\104\\137\\123\\110\\105\\114\\114\\137\\117\\113\\n'"}))?)?;
    success(host.native_tool(
        &parent,
        "submit_spawned_terminal",
        json!({"terminal":terminal}),
    )?)?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("Shell deadline")?;
    loop {
        let captured = success(host.native_tool(
            &parent,
            "read_spawned_terminal",
            json!({"terminal":terminal}),
        )?)?;
        let text = captured
            .get("capture")
            .and_then(|capture| capture.get("text"))
            .and_then(Value::as_str)
            .ok_or("Shell capture text")?;
        if text.lines().any(|line| line.trim() == "SCOPED_SHELL_OK") {
            break;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    }
    success(host.native_tool(
        &parent,
        "interrupt_spawned_terminal",
        json!({"terminal":terminal}),
    )?)?;
    assert_eq!(host.state.mux().selected_session(), selected.as_deref());
    success(host.native_tool(
        &parent,
        "close_spawned_terminal",
        json!({"terminal":terminal}),
    )?)?;
    assert_eq!(
        host.native_tool_reply(
            &parent,
            "read_spawned_terminal",
            json!({"terminal":terminal})
        )?["isError"],
        true
    );
    assert_eq!(host.state.mux().selected_session(), selected.as_deref());
    let renewed_receipt =
        success(host.native_tool(&parent, "spawn_shell", json!({"name":"retained-shell"}))?)?;
    let old_terminal = renewed_receipt
        .get("terminal")
        .ok_or("Retained Terminal")?
        .clone();
    host.restart()?;
    let renewed = open_saved_native(&mut host, &parent)?;
    assert_eq!(
        host.native_tool_reply(
            &renewed,
            "paste_spawned_terminal",
            json!({"terminal":old_terminal,"text":"must not regain input authority"})
        )?["isError"],
        true
    );
    Ok(())
}

#[rstest]
#[case(Caller::CommandPalette)]
#[case(Caller::Keybinding)]
#[case(Caller::BuiltinKeybinding)]
#[case(Caller::Cli)]
#[case(Caller::Socket)]
#[case(Caller::Internal)]
fn closing_native_panes_retains_history_and_reopens_the_same_provider(
    #[case] caller: Caller,
) -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let launched = host.start(&binding, "close-conversation", "close-native-task", "")?;
    let record = record(&launched)?;
    let mut close = CommandInvocation::new("agents.native.close", Vec::new(), caller);
    close.target = Some(record.target());
    assert_eq!(
        success(host.submit(close.clone())?)?,
        serde_json::to_value(record.target())?
    );
    assert!(
        !host
            .state
            .mux()
            .all_sessions()
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
            .any(|pane| pane.native_agent.as_deref() == Some(&record.id))
    );
    let retained = host
        .state
        .native_agent_service()
        .ok_or("native owner")?
        .sessions()
        .into_iter()
        .find(|saved| saved.id == record.id)
        .ok_or("Retained conversation missing")?;
    assert_eq!(retained.snapshot.status, NativeSessionStatus::Stopped);
    assert_eq!(retained.snapshot.transcript, record.snapshot.transcript);
    for target in [
        CommandTarget {
            generation: record.generation.saturating_add(1),
            ..record.target()
        },
        CommandTarget {
            handle: "native:codex:foreign".into(),
            ..record.target()
        },
    ] {
        close.target = Some(target);
        assert!(matches!(
            host.submit(close.clone())?,
            CommandOutcome::StaleTarget { .. }
        ));
    }
    let resumed = open_saved_native(&mut host, &record)?;
    assert!(
        host.state
            .mux()
            .all_sessions()
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
            .any(|pane| pane.native_agent.as_deref() == Some(&resumed.id))
    );
    success(host.native_tool(&resumed, "terminal_read", json!({}))?)?;
    Ok(())
}

#[rstest]
#[case(false)]
#[case(true)]
fn palette_surface_choice_retains_its_exact_native_parent_and_rejects_replaced_generation(
    #[case] replace_parent: bool,
) -> TestResult<()> {
    use bootty_ui::presentation::dialogs::CommandPaletteEvent;
    use bootty_ui::surface_creation::SurfaceParent;
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let created = host.start(&binding, "palette-parent", "palette-task", "")?;
    let native = record(&created)?;
    let target = native.target();
    let before = host.spaces()?;
    host.state
        .capture_command_palette_native_parent(Some(target.clone()));
    success(host.submit(CommandInvocation::from_action(
        "command_palette",
        Caller::Keybinding,
    ))?)?;
    assert!(
        host.state.modal_dialog().is_some(),
        "palette actually opened"
    );
    if replace_parent {
        host.state
            .native_agent_service()
            .ok_or("Native owner missing")?
            .stop(&target)?;
        let resumed: NativeSessionRecord = serde_json::from_value(success(host.submit(
            native_command("agents.native.resume", &target, &[], Caller::Socket),
        )?)?)?;
        assert_ne!(resumed.generation, native.generation);
    }
    host.state
        .apply_command_palette_event(CommandPaletteEvent::Run(
            bootty_ui::action_catalog::Command::SplitRight,
        ));
    host.tick();
    if replace_parent {
        assert!(
            host.state.pending_new_surface().is_none(),
            "old palette target cannot retarget the successor"
        );
        assert!(
            host.state.last_error().is_some(),
            "stale target is reported"
        );
    } else {
        verify_palette_native_split_cancel(&mut host, &target, &before)?;
    }
    // A subsequent palette has no captured native parent after the first palette closes.
    success(host.submit(CommandInvocation::from_action(
        "command_palette",
        Caller::Keybinding,
    ))?)?;
    host.state
        .apply_command_palette_event(CommandPaletteEvent::Run(
            bootty_ui::action_catalog::Command::SplitRight,
        ));
    host.tick();
    let request = host
        .state
        .pending_new_surface()
        .ok_or("terminal split chooser missing")?
        .clone();
    assert!(matches!(request.parent, SurfaceParent::Terminal(_)));
    success(host.submit(CommandInvocation::new(
        "surface.cancel",
        vec![request.id.to_string()],
        Caller::CommandPalette,
    ))?)?;
    Ok(())
}

fn verify_palette_native_split_cancel(
    host: &mut Host,
    target: &CommandTarget,
    before: &Value,
) -> TestResult<()> {
    use bootty_ui::surface_creation::{SurfaceParent, SurfacePlacement};
    let request = host
        .state
        .pending_new_surface()
        .ok_or("native split chooser missing")?
        .clone();
    assert_eq!(request.parent, SurfaceParent::Conversation(target.clone()));
    assert_eq!(
        request.placement,
        SurfacePlacement::Split(bootty_mux::pane_layout::SplitDirection::Right)
    );
    assert_eq!(request.task_identity, "palette-task");
    let cancelled = host.submit(CommandInvocation::new(
        "surface.cancel",
        vec![request.id.to_string()],
        Caller::CommandPalette,
    ))?;
    success(cancelled)?;
    assert!(host.state.pending_new_surface().is_none());
    assert!(
        host.effects.iter().any(
            |effect| matches!(effect, AppEffect::CloseSurfaceChooser(id) if *id == request.id)
        )
    );
    assert!(
        host.state
            .native_agent_service()
            .ok_or("Native owner missing")?
            .sessions()
            .iter()
            .any(|record| record.target() == *target)
    );
    assert_eq!(
        host.spaces()?,
        *before,
        "cancelling the native split leaves terminal topology unchanged"
    );
    Ok(())
}

#[rstest]
#[case("split_right")]
#[case("split_down")]
fn agent_choice_splits_the_captured_pane_instead_of_creating_a_tab(
    #[case] command: &str,
    #[values(false, true)] conversation_parent: bool,
    #[values(
        MultiplexerBackendConfig::Native,
        MultiplexerBackendConfig::Rmux,
        MultiplexerBackendConfig::Tmux
    )]
    backend: MultiplexerBackendConfig,
) -> TestResult<()> {
    let mut host = Host::new_with_backend(true, true, backend)?;
    let binding = host.binding(false)?;
    let name = format!(
        "split-{}",
        host.directory
            .path()
            .file_name()
            .ok_or("fixture directory")?
            .to_string_lossy()
            .trim_start_matches('.')
    );
    let identity = format!("{name}-task");
    let started = host.start(&binding, &name, &identity, "")?;
    let native = record(&started)?;
    success(host.submit(native_command(
        "agents.native.history",
        &native.target(),
        &["latest"],
        Caller::Internal,
    ))?)?;
    let parent = if conversation_parent {
        native.target()
    } else {
        serde_json::from_value(started["terminal"].clone())?
    };
    let before = host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.name == name)
        .ok_or("parent session")?
        .windows
        .clone();
    let mut split = CommandInvocation::from_action(command, Caller::Keybinding);
    split.target = Some(parent);
    success(host.submit(split)?)?;
    let request = host
        .state
        .pending_new_surface()
        .ok_or("split chooser")?
        .clone();
    success(host.submit(CommandInvocation::new(
        "surface.choose",
        vec![request.id.to_string(), "agent".into()],
        Caller::Keybinding,
    ))?)?;
    let created = record(&success(host.submit(CommandInvocation::new(
        "surface.create_agent",
        vec![
            request.id.to_string(),
            "codex".into(),
            request.cwd,
            host.program.clone(),
            "[]".into(),
            "split-child".into(),
            "captured".into(),
            request.task_identity,
            "Split child".into(),
            "reply in the split".into(),
        ],
        Caller::Keybinding,
    ))?)?)?;
    let windows = &host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.name == name)
        .ok_or("parent session")?
        .windows;
    assert_eq!(windows.len(), before.len());
    assert_eq!(windows[0].id, before[0].id);
    assert_eq!(windows[0].panes.len(), 2);
    assert!(
        windows[0]
            .panes
            .iter()
            .any(|pane| pane.native_agent.as_deref() == Some(native.id.as_str()))
    );
    assert!(
        windows[0]
            .panes
            .iter()
            .any(|pane| pane.native_agent.as_deref() == Some(created.id.as_str()))
    );
    assert_eq!(created.task_identity, native.task_identity);
    let spaces = host.spaces()?;
    let target = spaces
        .as_array()
        .ok_or("Spaces")?
        .iter()
        .flat_map(|space| space["sessions"].as_array().into_iter().flatten())
        .find(|session| session["name"] == name)
        .ok_or("created session")?["target"]
        .clone();
    let mut close = CommandInvocation::new("session.close", vec![], Caller::Socket);
    close.target = Some(serde_json::from_value(target)?);
    close.confirmation = Some(close.confirmation());
    success(host.submit(close)?)?;
    Ok(())
}

#[rstest]
#[case("split_right")]
#[case("split_down")]
fn terminal_choice_splits_the_exact_native_pane_instead_of_creating_a_tab(
    #[case] command: &str,
    #[values(Caller::CommandPalette, Caller::Keybinding, Caller::Socket)] caller: Caller,
) -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let native = record(&host.start(&binding, "split-native", "split-native-task", "")?)?;
    let before = host.state.mux().all_sessions()[0].windows.clone();
    let mut split = CommandInvocation::from_action(command, caller);
    split.target = Some(native.target());
    success(host.submit(split)?)?;
    let request = host.state.pending_new_surface().ok_or("split chooser")?.id;
    success(host.submit(CommandInvocation::new(
        "surface.choose",
        vec![request.to_string(), "terminal".into()],
        caller,
    ))?)?;
    let windows = &host.state.mux().all_sessions()[0].windows;
    assert_eq!(windows.len(), before.len());
    assert_eq!(windows[0].id, before[0].id);
    assert_eq!(windows[0].panes.len(), 2);
    assert_eq!(
        windows[0]
            .panes
            .iter()
            .filter(|pane| pane.native_agent.as_deref() == Some(native.id.as_str()))
            .count(),
        1
    );
    assert_eq!(
        windows[0]
            .panes
            .iter()
            .filter(|pane| pane.native_agent.is_none())
            .count(),
        1
    );
    Ok(())
}

#[rstest]
fn a_native_fork_occupies_a_real_right_backend_pane_and_preserves_its_source() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let launched = host.start(&binding, "fork-task", "fork-task-id", "source")?;
    let source = record(&launched)?;
    let before = host.state.mux().all_sessions()[0].windows[0].clone();
    let child: NativeSessionRecord = serde_json::from_value(success(host.submit(
        native_command("agents.native.fork", &source.target(), &[], Caller::Socket),
    )?)?)?;
    assert_ne!(child.id, source.id);
    assert_ne!(child.config.session_id, source.config.session_id);
    let session = host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.tag.identity.as_deref() == Some("fork-task-id"))
        .ok_or("Source task missing")?;
    assert_eq!(session.windows.len(), 1);
    let window = &session.windows[0];
    assert_eq!(window.id, before.id);
    assert_eq!(window.panes.len(), 2);
    assert_eq!(window.panes[0].pane_id, before.panes[0].pane_id);
    assert_eq!(
        window.panes[0].native_agent.as_deref(),
        Some(source.id.as_str())
    );
    assert_eq!(
        window.panes[1].native_agent.as_deref(),
        Some(child.id.as_str())
    );
    let child_pane = window.panes[1]
        .pane_id
        .clone()
        .ok_or("Child pane missing")?;
    let mut focus = CommandInvocation::new("agents.native.focus", vec![], Caller::Socket);
    focus.target = Some(child.target());
    success(host.submit(focus)?)?;
    assert_eq!(host.state.focused_pane(), Some(child_pane));
    let rects = host.state.pane_rects(
        bootty_terminal::geometry::SurfaceRect {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 1000.0,
            max_y: 600.0,
        },
        0.0,
    );
    assert_eq!(rects.len(), 2);
    assert!((rects[0].1.max_x - rects[1].1.min_x).abs() < f32::EPSILON);
    assert!((rects[0].1.min_y - rects[1].1.min_y).abs() < f32::EPSILON);
    assert!(rects[0].1.min_x < rects[1].1.min_x);

    let source_capture = success(host.native_tool(&source, "terminal_read", json!({}))?)?;
    let child_capture = success(host.native_tool(&child, "terminal_read", json!({}))?)?;
    assert_ne!(source_capture["target"], child_capture["target"]);
    success(host.submit(native_command(
        "agents.native.stop",
        &child.target(),
        &[],
        Caller::Socket,
    ))?)?;
    let resumed = open_saved_native(&mut host, &child)?;
    let resumed_capture = success(host.native_tool(&resumed, "terminal_read", json!({}))?)?;
    assert_ne!(source_capture["target"], resumed_capture["target"]);
    let mut close = CommandInvocation::from_action("agents.native.close", Caller::Socket);
    close.target = Some(resumed.target());
    success(host.submit(close)?)?;
    let reopened = open_saved_native(&mut host, &resumed)?;
    let reopened_capture = success(host.native_tool(&reopened, "terminal_read", json!({}))?)?;
    assert_ne!(source_capture["target"], reopened_capture["target"]);
    let session = host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.tag.identity.as_deref() == Some("fork-task-id"))
        .ok_or("Source task missing")?;
    assert_eq!(session.windows.len(), 1);
    assert_eq!(session.windows[0].panes.len(), 2);
    Ok(())
}

#[rstest]
fn closing_a_native_pane_through_the_terminal_command_also_retires_its_provider() -> TestResult<()>
{
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let launched = host.start(&binding, "mux-close-task", "mux-close-task-id", "")?;
    let native = record(&launched)?;
    let mut focus = CommandInvocation::new("agents.native.focus", vec![], Caller::Socket);
    focus.target = Some(native.target());
    success(host.submit(focus)?)?;
    let mut close = CommandInvocation::new("pane.close", vec![], Caller::Socket);
    close.target = Some(serde_json::from_value(launched["terminal"].clone())?);
    close.confirmation = Some(close.confirmation());
    success(host.submit(close)?)?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("retirement deadline")?;
    loop {
        host.tick();
        if host
            .state
            .native_agent_service()
            .ok_or("native owner")?
            .activities()
            .iter()
            .any(|record| record.id == native.id && record.status == NativeSessionStatus::Stopped)
        {
            break;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    }
    assert!(
        host.state
            .native_agent_service()
            .ok_or("native owner")?
            .sessions()
            .iter()
            .any(|record| record.id == native.id)
    );
    Ok(())
}

#[rstest]
fn native_mux_placement_is_published_before_provider_initialization() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    fs::write(host.directory.path().join("hold-thread-start"), "")?;
    let ready =
        std::os::unix::net::UnixListener::bind(host.directory.path().join("start-barrier"))?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("deadline")?;
    let mut invocation = CommandInvocation::new(
        "agents.native.start",
        vec![
            "codex".into(),
            host.directory.path().to_string_lossy().into_owned(),
            host.program.clone(),
            "[]".into(),
            "early-placement".into(),
            "captured".into(),
            "early-placement-id".into(),
            "Early placement".into(),
            String::new(),
        ],
        Caller::Internal,
    );
    invocation.target = Some(binding);
    let receiver = host
        .state
        .app_command_sender(Caller::Internal)
        .submit(invocation, deadline, CommandCancellation::new())
        .map_err(|error| format!("{error:?}"))?;
    let placed_record = loop {
        host.tick();
        let record = host
            .state
            .native_agent_service()
            .ok_or("native owner")?
            .sessions()
            .into_iter()
            .find(|record| {
                host.state
                    .mux()
                    .sessions()
                    .iter()
                    .flat_map(|session| &session.windows)
                    .flat_map(|window| &window.panes)
                    .any(|pane| pane.native_agent.as_deref() == Some(&record.id))
            });
        if let Some(record) = record {
            break record;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    };
    assert_eq!(placed_record.snapshot.status, NativeSessionStatus::Starting);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let (mut ready_stream, _) = ready.accept()?;
    let mut byte = [0];
    std::io::Read::read_exact(&mut ready_stream, &mut byte)?;
    host.state
        .native_agent_service()
        .ok_or("native owner")?
        .resolve(&placed_record.target())?
        .rpc("__allow_start", json!({}))?;
    let completed = record(&success(host.receive(&receiver, deadline)?)?)?;
    assert_eq!(completed.id, placed_record.id);
    assert_eq!(
        completed.task_identity.as_deref(),
        Some("early-placement-id")
    );
    Ok(())
}

#[rstest]
#[case::active(false)]
#[case::inactive(true)]
fn model_catalog_and_favorites_reuse_the_captured_provider_discovery(
    #[case] inactive: bool,
) -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(inactive)?;
    let mut query = CommandInvocation::new(
        "agents.native.catalog",
        vec![
            "codex".into(),
            host.directory.path().to_string_lossy().into_owned(),
            host.program.clone(),
            "[]".into(),
            "catalog".into(),
            "captured".into(),
        ],
        Caller::Internal,
    );
    query.target = Some(binding);
    let models = success(host.submit(query.clone())?)?;
    assert_eq!(models[0]["id"], "qa-model");
    let mut info = query.clone();
    info.command = "agents.native.catalog-info".into();
    assert_eq!(success(host.submit(info.clone())?)?["models"], models);
    let target = info.target.as_mut().ok_or("catalog target")?;
    target.generation = target.generation.saturating_add(1);
    assert!(matches!(
        host.submit(info)?,
        CommandOutcome::StaleTarget { .. }
    ));
    fs::write(
        &host.program,
        "#!/usr/bin/env python3\nraise SystemExit(1)\n",
    )?;
    let mut favorite = query.clone();
    favorite.command = "agents.native.catalog-favorite".into();
    favorite.arguments.push("qa-model".into());
    for selected in [true, false] {
        let models = success(host.submit(favorite.clone())?)?;
        assert_eq!(models[0]["is_favorite"], selected);
    }
    assert_eq!(success(host.submit(query)?)?, models);
    Ok(())
}

#[rstest]
fn an_existing_conversation_is_not_the_receipt_for_its_new_agent_tab() -> TestResult<()> {
    let mut host = Host::new(true)?;
    let binding = host.binding(false)?;
    let started = host.start(&binding, "creation-parent", "creation-parent-task", "")?;
    let parent = record(&started)?;
    let mut open = CommandInvocation::from_action("new_tab", Caller::Keybinding);
    open.target = Some(parent.target());
    success(host.submit(open)?)?;
    let request = host
        .state
        .pending_new_surface()
        .ok_or("surface request")?
        .clone();
    host.state.open_surface_agent_form(&request);
    let invocation = CommandInvocation::new(
        "surface.create_agent",
        vec![
            request.id.to_string(),
            "codex".into(),
            request.cwd.clone(),
            host.program.clone(),
            "[]".into(),
            "New conversation".into(),
            "captured".into(),
            request.task_identity.clone(),
            "New conversation".into(),
            String::new(),
        ],
        Caller::Internal,
    );
    host.state
        .apply_picker_event(NewSessionPickerEvent::Submit(invocation));
    // Projection precedes command execution: the existing same-task record is already placed.
    let _ = host.state.dialog_projection();
    assert!(matches!(
        host.state.modal_dialog(),
        Some(bootty_ui::ModalDialog::NewSession(_))
    ));
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("deadline")?;
    while host.state.pending_new_surface().is_some() || host.state.modal_dialog().is_some() {
        host.tick();
        let _ = host.state.dialog_projection();
        if host.state.pending_new_surface().is_none() && host.state.modal_dialog().is_none() {
            break;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    }
    host.tick();
    let selected = host
        .state
        .mux()
        .selected_session_anchor()
        .ok_or("selected pane")?;
    let selected_agent = selected
        .native_agent
        .as_deref()
        .ok_or("selected conversation")?;
    assert_ne!(selected_agent, parent.id);
    let records = host
        .state
        .native_agent_service()
        .ok_or("native owner")?
        .sessions();
    assert!(
        records
            .iter()
            .any(|record| record.id == selected_agent
                && record.task_identity == parent.task_identity)
    );
    Ok(())
}
