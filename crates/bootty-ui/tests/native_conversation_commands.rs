#![cfg(test)]
#![cfg(unix)]

use std::{
    collections::{BTreeMap, HashMap},
    fs,
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

/// Bounds waits on real child processes and command replies. Passing tests advance on events;
/// only a broken fixture reaches this, so it is generous enough for a loaded machine.
const FIXTURE_DEADLINE: Duration = Duration::from_secs(30);

#[path = "support/idle_frames.rs"]
mod frames;
pub mod support;
#[path = "support/config.rs"]
mod test_config;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

// Real provider subprocess, public app-server protocol, and reply barriers; no timed sleeps.
fn provider(root: &Path) -> TestResult<String> {
    let program = root.join("codex-provider.py");
    bootty_test_support::write_executable(&program, include_str!("support/codex_provider.py"))?;
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
        Self::new_with_child_provider(enabled, allow_spawn, backend, None)
    }

    fn new_with_child_provider(
        enabled: bool,
        allow_spawn: bool,
        backend: MultiplexerBackendConfig,
        child: Option<AgentKind>,
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
        if let Some(child) = child {
            let program = pi_child_provider(directory.path())?;
            let preferences = match child {
                AgentKind::Pi => &mut config.agents.pi,
                AgentKind::Claude => &mut config.agents.claude,
                AgentKind::Codex => &mut config.agents.codex,
            };
            preferences.enabled = true;
            preferences.program = program;
            preferences.selected = "child-account".into();
            preferences.profiles.insert(
                "child-account".into(),
                AgentProfileConfig {
                    name: "Child account".into(),
                    directory: Some(
                        directory
                            .path()
                            .join("child-account")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    arguments: Vec::new(),
                },
            );
        }
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
            .checked_add(FIXTURE_DEADLINE)
            .ok_or("Deadline")?;
        success(self.receive(&receiver, deadline)?)
    }

    fn restart(&mut self) {
        self.restart_with_provider_enabled(self.state.config().agents.codex.enabled);
    }

    fn restart_with_provider_enabled(&mut self, enabled: bool) {
        self.restart_with_provider_selection(enabled, None);
    }

    fn restart_with_provider_selection(&mut self, enabled: bool, selected: Option<&str>) {
        if let Some(service) = self.state.terminal_agent_service() {
            service.shutdown_and_wait().unwrap();
        }
        self.state
            .native_agent_service()
            .expect("Native owner missing")
            .shutdown()
            .unwrap();
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
        )
        .unwrap();
    }

    fn tick(&mut self) {
        self.effects
            .extend(self.state.update_frame(frames::idle_frame(Instant::now())));
    }

    fn submit(&mut self, invocation: CommandInvocation) -> TestResult<CommandOutcome> {
        let deadline = Instant::now()
            .checked_add(FIXTURE_DEADLINE)
            .ok_or("deadline overflow")?;
        let command = invocation.command.clone();
        let response = self
            .state
            .app_command_sender(invocation.caller)
            .submit(invocation, deadline, CommandCancellation::new())
            .map_err(|error| format!("mailbox submission failed: {error:?}"))?;
        self.receive(&response, deadline)
            .map_err(|error| format!("{command}: {error}").into())
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
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| "no reply before the deadline")?;
        }
    }

    fn close_task(&mut self, name: &str) {
        let spaces = self.spaces().unwrap();
        let session = spaces
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|space| space["sessions"].as_array().into_iter().flatten())
            .find(|session| session["name"] == name)
            .expect("Created task missing");
        let mut close = CommandInvocation::from_action("session.close", Caller::Internal);
        close.target = Some(serde_json::from_value(session["target"].clone()).unwrap());
        close.confirmation = Some(close.confirmation());
        success(self.submit(close).unwrap()).unwrap();
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

    fn wait_working(&mut self, target: &CommandTarget, deadline: Instant) {
        loop {
            self.tick();
            let service = self
                .state
                .native_agent_service()
                .expect("native owner missing");
            if service.sessions().iter().any(|record| {
                record.target() == *target
                    && record.snapshot.status == NativeSessionStatus::Working
                    && record.snapshot.turn_id.is_some()
            }) {
                return;
            }
            self.wakes
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
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
fn native_conversation_commands_stream_through_every_shared_mailbox_caller() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let launched = host
        .start(
            &binding,
            "conversation",
            "task-conversation",
            "first prompt",
        )
        .unwrap();
    let record = record(&launched).unwrap();
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
    let terminal: CommandTarget = serde_json::from_value(launched["terminal"].clone()).unwrap();
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
        let snapshot = success(
            host.submit(native_command(
                "agents.native.prompt",
                &record.target(),
                &[&text],
                caller,
            ))
            .unwrap(),
        )
        .unwrap();
        assert!(snapshot["transcript"].as_array().expect("transcript missing").iter().any(|item| item["role"] == "assistant" && item["text"] == format!("reply: {text}")));
        let title = format!("Title from {caller:?}");
        assert_eq!(
            success(
                host.submit(native_command(
                    "agents.native.rename",
                    &record.target(),
                    &[&title],
                    caller
                ))
                .unwrap()
            )
            .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            host.state
                .native_agent_service()
                .expect("owner missing")
                .activities()[0]
                .title,
            title
        );
    }
    let mut list = CommandInvocation::new("agents.native.list", Vec::new(), Caller::Socket);
    list.target = Some(binding);
    let listed = success(host.submit(list).unwrap()).unwrap();
    let listed = listed.as_array().expect("list missing");
    assert_eq!(listed.len(), 1);
    // A listing names targets without whole transcripts, which exceed the payload limit.
    let entry = listed.first().expect("listed conversation");
    assert_eq!(entry["id"], record.id);
    assert_eq!(entry["generation"], record.generation);
    assert!(entry.get("snapshot").is_none() && entry.get("transcript").is_none());
}

/// A conversation's short name from the agent list stands for it, as the target and in place of
/// its id, at its current generation.
#[rstest]
fn conversation_names_stand_for_their_targets() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let record = record(
        &host
            .start(&binding, "conversation", "task-names", "")
            .unwrap(),
    )
    .unwrap();
    let mut list = CommandInvocation::new("agents.native.activities", Vec::new(), Caller::Socket);
    list.target = Some(binding);
    let listed = success(host.submit(list).unwrap()).unwrap();
    let name = listed[0]["name"].as_str().expect("listed name").to_owned();
    assert!(bootty_control::short_name::names(
        &name,
        &record.full_name()
    ));
    let status = CommandInvocation {
        target: Some(CommandTarget {
            kind: ResourceKind::Session,
            handle: name.clone(),
            generation: 0,
        }),
        ..CommandInvocation::new(
            "agents.native.status",
            vec![name, "0".into()],
            Caller::Socket,
        )
    };
    let value = success(host.submit(status).unwrap()).unwrap();
    assert_eq!(value["id"], record.id);
    assert_eq!(value["generation"], record.generation);
}

/// A message that mentions another conversation sends that conversation's recent words after
/// it, while the transcript keeps the message as written.
#[rstest]
fn mentioned_conversations_follow_the_message_quoted() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let mentioned = record(&host.start(&binding, "earlier", "task-earlier", "").unwrap()).unwrap();
    success(
        host.submit(native_command(
            "agents.native.prompt",
            &mentioned.target(),
            &["the flaky test is in scroll.rs"],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    let asking = record(&host.start(&binding, "asking", "task-asking", "").unwrap()).unwrap();
    let message = "What did the other conversation find?";
    let authored = message.len().to_string();
    let conversations = serde_json::to_string(&[mentioned.target()]).unwrap();
    let snapshot = success(
        host.submit(native_command(
            "agents.native.prompt",
            &asking.target(),
            &[message, "", "", "", "", &authored, "", "", &conversations],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    let transcript = snapshot["transcript"]
        .as_array()
        .expect("transcript missing");
    assert!(
        transcript
            .iter()
            .any(|item| item["role"] == "user" && item["text"] == message)
    );
    let reply = transcript
        .iter()
        .find(|item| item["role"] == "assistant")
        .and_then(|item| item["text"].as_str())
        .expect("reply missing");
    assert!(
        reply.contains(&format!("The conversation “{}”", mentioned.title))
            && reply.contains("User: the flaky test is in scroll.rs"),
        "{reply}"
    );
    // Only Bootty's own composer may attach another conversation's words.
    let forged = host
        .submit(native_command(
            "agents.native.prompt",
            &asking.target(),
            &[message, "", "", "", "", "", "", "", &conversations],
            Caller::Socket,
        ))
        .unwrap();
    assert!(
        !matches!(forged, CommandOutcome::Success { .. }),
        "{forged:?}"
    );
}

#[rstest]
fn saved_annotation_batch_reaches_exact_native_conversation_alongside_message() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let record = record(
        &host
            .start(&binding, "conversation", "task-annotations", "")
            .unwrap(),
    )
    .unwrap();
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
    store.commit(&[], &notes).unwrap();
    let committed = store.load().unwrap();
    let message = format!(
        "Review both changes together.\n\n{}",
        annotation_batch(&committed).unwrap()
    );
    let snapshot = success(
        host.submit(native_command(
            "agents.native.prompt",
            &record.target(),
            &[&message],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    let transcript = snapshot["transcript"]
        .as_array()
        .expect("transcript missing");
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
    assert_eq!(store.load().unwrap(), notes);
}

#[rstest]
fn annotation_images_forward_only_exact_saved_versions() {
    const CHILD: &str = "BOOTTY_ANNOTATION_IMAGE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let directory = assert_fs::TempDir::new().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
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
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let record = record(
        &host
            .start(&binding, "conversation", "task-visual-annotation", "")
            .unwrap(),
    )
    .unwrap();
    let child_state = std::path::PathBuf::from(
        std::env::var_os("XDG_STATE_HOME").expect("Missing isolated child state"),
    );
    let browser_directory = bootty_config::identity::unix_daemon_state_path(
        bootty_config::identity::ApplicationIdentity::for_process(),
        None,
        Some(&child_state),
        None,
    )
    .expect("Missing browser state directory")
    .with_file_name("browser");
    let store = AnnotationStore::new(&browser_directory);
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([12, 34, 56, 255]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
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
    let (image, _) = store.commit_image(png.get_ref(), &geometry).unwrap();
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
    store.commit(&[], std::slice::from_ref(&note)).unwrap();
    let refs = serde_json::to_string(std::slice::from_ref(&note)).unwrap();
    let snapshot = success(
        host.submit(native_command(
            "agents.native.prompt",
            &record.target(),
            &["visual-image", &refs],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    assert!(
        snapshot["transcript"]
            .as_array()
            .expect("Missing transcript")
            .iter()
            .any(|item| item["role"] == "user" && item["images"][0]["id"] == image.id)
    );
    let mut foreign = note.clone();
    foreign.conversation = Some("other-conversation".into());
    let refs = serde_json::to_string(&[foreign]).unwrap();
    assert!(matches!(
        host.submit(native_command(
            "agents.native.prompt",
            &record.target(),
            &["visual-image", &refs],
            Caller::Internal
        ))
        .unwrap(),
        CommandOutcome::Failed { .. }
    ));
    let captured = serde_json::to_string(std::slice::from_ref(&note)).unwrap();
    let mut changed = note.clone();
    changed
        .prepare_attachment("Changed after Send capture".into(), &record.id)
        .unwrap();
    store
        .commit(std::slice::from_ref(&note), std::slice::from_ref(&changed))
        .unwrap();
    assert!(matches!(
        host.submit(native_command(
            "agents.native.prompt",
            &record.target(),
            &["visual-image", &captured],
            Caller::Internal
        ))
        .unwrap(),
        CommandOutcome::Failed { .. }
    ));
    assert_eq!(store.load().unwrap(), vec![changed]);
}

#[rstest]
fn shared_command_interrupt_remains_available_while_prompt_reply_is_pending() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let record = record(
        &host
            .start(&binding, "conversation", "task-cancel", "")
            .unwrap(),
    )
    .unwrap();
    let target = record.target();
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("deadline overflow");
    let waiting = host
        .state
        .app_command_sender(Caller::Socket)
        .submit(
            native_command("agents.native.prompt", &target, &["wait"], Caller::Socket),
            deadline,
            CommandCancellation::new(),
        )
        .expect("mailbox failed");
    host.wait_working(&target, deadline);
    assert!(matches!(waiting.try_recv(), Err(mpsc::TryRecvError::Empty)));
    success(
        host.submit(native_command(
            "agents.native.interrupt",
            &target,
            &[],
            Caller::Keybinding,
        ))
        .unwrap(),
    )
    .unwrap();
    success(host.receive(&waiting, deadline).unwrap()).unwrap();
    assert_eq!(
        host.state
            .native_agent_service()
            .expect("owner missing")
            .sessions()[0]
            .snapshot
            .status,
        NativeSessionStatus::Idle
    );
    let completed = success(
        host.submit(native_command(
            "agents.native.prompt",
            &target,
            &["after interrupt"],
            Caller::Socket,
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(completed["completed_turn"], true);
    assert!(
        completed["transcript"]
            .as_array()
            .expect("Transcript missing")
            .iter()
            .any(|item| item["role"] == "assistant" && item["text"] == "reply: after interrupt")
    );
    let service = host.state.native_agent_service().expect("Native owner");
    let records = service.sessions();
    assert_eq!(records.len(), 1);
    let continued = records.first().expect("Conversation missing");
    assert_eq!(continued.target(), target);
    assert_eq!(continued.binding_id, record.binding_id);
    assert_eq!(continued.task_identity, record.task_identity);
    assert_eq!(continued.snapshot.session_id, record.snapshot.session_id);
    assert_eq!(continued.snapshot.status, NativeSessionStatus::Idle);
    let calls = service
        .resolve(&target)
        .unwrap()
        .rpc("__calls", json!({}))
        .unwrap();
    let methods = calls["methods"].as_array().expect("Provider calls missing");
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
}

/// A path in a reply opens on the conversation's host, relative to its directory, the way a path
/// in its terminal does.
#[rstest]
fn reply_paths_open_relative_to_the_conversation() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let conversation = record(
        &host
            .start(&binding, "conversation", "task-links", "")
            .unwrap(),
    )
    .unwrap();
    fs::write(host.directory.path().join("notes.md"), "one\ntwo\n").unwrap();
    host.effects.clear();
    success(
        host.submit(native_command(
            "agents.native.open-link",
            &conversation.target(),
            &["notes.md:2"],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    let opened = host
        .effects
        .iter()
        .filter_map(|effect| match effect {
            AppEffect::OpenFiles(request) => Some((request.path.clone(), request.line)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let notes = fs::canonicalize(host.directory.path().join("notes.md")).unwrap();
    assert_eq!(opened, [(notes.to_string_lossy().into_owned(), 2)]);
}

/// A sent file opens, or shows in the file manager, from the stored copy its conversation keeps.
/// Another conversation can't reach it.
#[rstest]
fn sent_attachments_open_and_reveal_their_stored_copy() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let conversation = record(
        &host
            .start(&binding, "conversation", "task-sent-file", "")
            .unwrap(),
    )
    .unwrap();
    let other = record(
        &host
            .start(&binding, "other", "task-other-file", "")
            .unwrap(),
    )
    .unwrap();
    let source = host.directory.path().join("notes.md");
    fs::write(&source, "one\ntwo\n").unwrap();
    let reference = success(
        host.submit(native_command(
            "agents.native.import",
            &conversation.target(),
            &[&source.to_string_lossy()],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    let id = reference["id"].as_str().expect("an attachment id");
    host.effects.clear();
    for command in [
        "agents.native.attachment-open",
        "agents.native.attachment-reveal",
    ] {
        success(
            host.submit(native_command(
                command,
                &conversation.target(),
                &[id],
                Caller::Internal,
            ))
            .unwrap(),
        )
        .unwrap();
    }
    let shown = host
        .effects
        .iter()
        .filter_map(|effect| match effect {
            AppEffect::OpenWithSystem(path) => Some(("open", fs::read_to_string(path).ok())),
            AppEffect::RevealPath(path) => Some(("reveal", fs::read_to_string(path).ok())),
            _ => None,
        })
        .collect::<Vec<_>>();
    let copy = Some("one\ntwo\n".to_owned());
    assert_eq!(shown, [("open", copy.clone()), ("reveal", copy)]);
    let outcome = host
        .submit(native_command(
            "agents.native.attachment-open",
            &other.target(),
            &[id],
            Caller::Internal,
        ))
        .unwrap();
    assert!(
        matches!(&outcome, CommandOutcome::Failed { message, .. } if message.contains("does not belong")),
        "{outcome:?}"
    );
}

/// A reply's path and a document's path both show in the file manager: the reply's resolved
/// against the conversation's directory, the way it would open.
#[rstest]
fn reply_and_document_paths_reveal_in_the_file_manager() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let conversation = record(
        &host
            .start(&binding, "conversation", "task-reveal", "")
            .unwrap(),
    )
    .unwrap();
    fs::write(host.directory.path().join("notes.md"), "one\ntwo\n").unwrap();
    let notes = fs::canonicalize(host.directory.path().join("notes.md")).unwrap();
    let revealed = |host: &Host| {
        host.effects
            .iter()
            .filter_map(|effect| match effect {
                AppEffect::RevealPath(path) => Some(path.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    host.effects.clear();
    success(
        host.submit(native_command(
            "agents.native.reveal-link",
            &conversation.target(),
            &["notes.md:2"],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(revealed(&host), std::slice::from_ref(&notes));
    host.effects.clear();
    let mut reveal = CommandInvocation::new(
        "files.reveal",
        vec![notes.to_string_lossy().into_owned()],
        Caller::Internal,
    );
    reveal.target = Some(binding.clone());
    success(host.submit(reveal).unwrap()).unwrap();
    assert_eq!(revealed(&host), std::slice::from_ref(&notes));
    // Editors are the ones this computer has; one it doesn't is refused by name.
    host.effects.clear();
    let mut edit = CommandInvocation::new(
        "files.open_in",
        vec![notes.to_string_lossy().into_owned(), "notepad".to_owned()],
        Caller::Internal,
    );
    edit.target = Some(binding);
    let link = native_command(
        "agents.native.edit-link",
        &conversation.target(),
        &["notes.md:2", "notepad"],
        Caller::Internal,
    );
    for invocation in [edit, link] {
        let outcome = host.submit(invocation).unwrap();
        assert!(
            matches!(&outcome, CommandOutcome::Unavailable { message } if message.contains("notepad")),
            "{outcome:?}"
        );
    }
    assert!(
        !host
            .effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::OpenInEditor { .. }))
    );
}

#[rstest]
#[case(MultiplexerBackendConfig::Native)]
#[case(MultiplexerBackendConfig::Tmux)]
fn reopening_a_conversation_waits_for_terminal_membership_reconciliation(
    #[case] backend: MultiplexerBackendConfig,
) {
    let mut host = Host::new_with_backend(true, false, backend).unwrap();
    let binding = host.binding(false).unwrap();
    let identity = bootty_mux::snapshot::new_session_identity();
    let name = format!("focus-{identity}");
    let launched = host.start(&binding, &name, &identity, "").unwrap();
    let native = record(&launched).unwrap();
    host.close_task(&name);
    host.restart();
    let stopped = host
        .state
        .native_agent_service()
        .expect("Native owner")
        .sessions()
        .into_iter()
        .find(|record| record.id == native.id)
        .expect("Saved conversation");
    let mut focus = CommandInvocation::from_action("agents.native.focus", Caller::Internal);
    focus.target = Some(stopped.target());
    let reopened: NativeSessionRecord =
        serde_json::from_value(success(host.submit(focus).unwrap()).unwrap()).unwrap();
    assert_eq!(reopened.id, native.id);
    assert_eq!(reopened.config.session_id, stopped.config.session_id);
    assert_ne!(reopened.generation, stopped.generation);
    assert!(host.effects.iter().any(|effect| matches!(effect, AppEffect::NativeConversation(target) if target == &reopened.target())));
    host.close_task(&name);
}

#[rstest]
fn failed_native_reopen_preserves_its_cause_instead_of_cancelling_itself() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let cwd = host.directory.path().join("removed-checkout");
    fs::create_dir(&cwd).unwrap();
    let identity = bootty_mux::snapshot::new_session_identity();
    let name = format!("missing-{identity}");
    let mut start = CommandInvocation::new(
        "agents.native.start",
        vec![
            "codex".into(),
            cwd.to_string_lossy().into_owned(),
            host.program.clone(),
            "[]".into(),
            name.clone(),
            "captured".into(),
            identity,
            "Missing directory".into(),
            String::new(),
        ],
        Caller::Internal,
    );
    start.target = Some(binding);
    let native = record(&success(host.submit(start).unwrap()).unwrap()).unwrap();
    host.close_task(&name);
    fs::remove_dir(&cwd).unwrap();
    let mut focus = CommandInvocation::from_action("agents.native.focus", Caller::Internal);
    focus.target = Some(native.target());
    let outcome = host.submit(focus).unwrap();
    assert!(
        matches!(&outcome, CommandOutcome::Failed { code, message } if code == "invalid_arguments" && message.contains("directory")),
        "{outcome:?}"
    );
}

/// Native conversations raise desktop notifications like observed agents: when one needs
/// approval, and when its turn finishes. The Agents panel lists them too.
#[rstest]
fn native_conversations_notify_when_they_need_approval_and_finish() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let background = record(
        &host
            .start(&binding, "conversation", "task-notify", "")
            .unwrap(),
    )
    .unwrap();
    let target = background.target();
    // A conversation in view doesn't notify; another task's conversation takes focus.
    let foreground = record(
        &host
            .start(&binding, "foreground", "task-foreground", "")
            .unwrap(),
    )
    .unwrap();
    let mut focus = CommandInvocation::from_action("agents.native.focus", Caller::Socket);
    focus.target = Some(foreground.target());
    success(host.submit(focus).unwrap()).unwrap();
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("deadline overflow");
    let notified = |host: &mut Host, title: &str| -> TestResult<()> {
        loop {
            host.tick();
            if host.effects.iter().any(|effect| {
                matches!(effect, AppEffect::DesktopNotification { title: shown, .. } if shown == title)
            }) {
                return Ok(());
            }
            host.wakes
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        }
    };
    host.tick();
    host.effects.clear();
    success(
        host.submit(native_command(
            "agents.native.prompt",
            &target,
            &["approval"],
            Caller::Socket,
        ))
        .unwrap(),
    )
    .unwrap();
    notified(&mut host, "Codex: Needs approval").unwrap();
    let request = host
        .state
        .native_agent_service()
        .expect("native owner missing")
        .sessions()
        .into_iter()
        .find(|session| session.id == background.id)
        .expect("conversation")
        .snapshot
        .requests
        .first()
        .expect("pending approval")
        .id
        .clone();
    success(
        host.submit(native_command(
            "agents.native.approve",
            &target,
            &[&request, "allow"],
            Caller::Socket,
        ))
        .unwrap(),
    )
    .unwrap();
    notified(&mut host, "Codex: Finished").unwrap();
    let mut listed = host
        .state
        .agent_overview()
        .into_iter()
        .filter(|entry| entry.source == "native")
        .map(|entry| entry.status)
        .collect::<Vec<_>>();
    listed.sort();
    // The Agents panel lists both conversations: the finished one and the one in view.
    assert_eq!(listed, vec!["complete".to_owned(), "idle".to_owned()]);
    host.effects.clear();
    success(
        host.submit(native_command(
            "agents.native.prompt",
            &target,
            &["async-question"],
            Caller::Socket,
        ))
        .unwrap(),
    )
    .unwrap();
    notified(&mut host, "Codex: Needs input").unwrap();
    let activity = host
        .state
        .native_agent_service()
        .expect("native owner missing")
        .activity(&target)
        .unwrap();
    assert_eq!(
        activity.status,
        NativeSessionStatus::Working,
        "async questions do not block ongoing work"
    );
    assert!(activity.input);
    assert!(activity.pending_question.is_some());
    host.effects.clear();
    host.tick();
    assert!(!host.effects.iter().any(|effect| matches!(effect, AppEffect::DesktopNotification { title, .. } if title == "Codex: Needs input")));
}

/// Interrupt Agent in the palette stops the conversation in the focused pane, the same way the
/// conversation's own interrupt does.
#[rstest]
fn interrupt_agent_stops_the_conversation_in_the_focused_pane() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let record = record(
        &host
            .start(&binding, "conversation", "task-palette", "")
            .unwrap(),
    )
    .unwrap();
    let target = record.target();
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("deadline overflow");
    success(
        host.submit(native_command(
            "agents.native.prompt",
            &target,
            &["stuck"],
            Caller::Socket,
        ))
        .unwrap(),
    )
    .unwrap();
    host.wait_working(&target, deadline);
    success(
        host.submit(CommandInvocation::from_action(
            "interrupt_agent",
            Caller::CommandPalette,
        ))
        .unwrap(),
    )
    .unwrap();
    let service = host.state.native_agent_service().expect("owner missing");
    assert!(service.resolve(&target).unwrap().snapshot().stopping());
}

/// A provider can acknowledge a stop and never end its turn. The conversation shows it is
/// stopping; stopping again restarts the provider, and the queued message goes out afterwards.
#[rstest]
fn a_second_stop_restarts_a_provider_that_never_ends_its_turn() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let record = record(
        &host
            .start(&binding, "conversation", "task-stuck", "")
            .unwrap(),
    )
    .unwrap();
    let target = record.target();
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("deadline overflow");
    success(
        host.submit(native_command(
            "agents.native.prompt",
            &target,
            &["stuck"],
            Caller::Socket,
        ))
        .unwrap(),
    )
    .unwrap();
    host.wait_working(&target, deadline);
    let stop = native_command("agents.native.interrupt", &target, &[], Caller::Keybinding);
    success(host.submit(stop.clone()).unwrap()).unwrap();
    success(
        host.submit(native_command(
            "agents.native.queue",
            &target,
            &["after restart"],
            Caller::Socket,
        ))
        .unwrap(),
    )
    .unwrap();
    let service = host.state.native_agent_service().expect("owner missing");
    let stopping = service.sessions().remove(0);
    assert_eq!(stopping.snapshot.status, NativeSessionStatus::Working);
    assert!(stopping.snapshot.stopping());
    assert_eq!(stopping.queued_messages.len(), 1);

    success(host.submit(stop).unwrap()).unwrap();
    let resumed = loop {
        host.tick();
        let current = service.sessions().remove(0);
        if current.queued_messages.is_empty()
            && current.snapshot.status == NativeSessionStatus::Idle
            && current
                .snapshot
                .transcript
                .iter()
                .any(|item| item.text == "reply: after restart")
        {
            break current;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
    };
    assert!(
        resumed.generation > record.generation,
        "the provider restarted"
    );
    let calls = service
        .resolve(&resumed.target())
        .unwrap()
        .rpc("__calls", json!({}))
        .unwrap();
    let methods = calls["methods"].as_array().expect("Provider calls missing");
    assert!(
        methods
            .iter()
            .any(|method| method.as_str() == Some("thread/resume")),
        "the restart resumes the captured thread: {methods:?}"
    );
}

#[rstest]
fn native_target_generation_and_argument_identity_cannot_redirect_a_prompt() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let record = record(
        &host
            .start(&binding, "conversation", "task-target", "")
            .unwrap(),
    )
    .unwrap();
    let mut stale = record.target();
    stale.generation = stale
        .generation
        .checked_add(1)
        .expect("generation overflow");
    assert!(matches!(
        host.submit(native_command(
            "agents.native.prompt",
            &stale,
            &["stale"],
            Caller::Socket
        ))
        .unwrap(),
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
        host.submit(mismatched).unwrap(),
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
        host.submit(absent).unwrap(),
        CommandOutcome::Success { .. }
    ));
    assert_eq!(
        host.state
            .native_agent_service()
            .expect("owner missing")
            .sessions()[0]
            .snapshot
            .transcript
            .as_slice(),
        &[],
    );
}

#[rstest]
#[case::disabled("codex", false)]
#[case::unsupported("unknown", true)]
fn rejected_provider_does_not_create_a_shell_or_native_record(
    #[case] provider: &str,
    #[case] enabled: bool,
) {
    let mut host = Host::new(enabled).unwrap();
    let binding = host.binding(false).unwrap();
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
    let outcome = host.submit(invocation).unwrap();
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
            .expect("owner missing")
            .activities()
            .as_slice(),
        &[],
    );
    assert!(
        host.spaces()
            .unwrap()
            .as_array()
            .expect("Spaces missing")
            .iter()
            .all(|space| space["sessions"].as_array().is_some_and(Vec::is_empty))
    );
}

#[rstest]
fn checkpointing_a_shared_backend_saves_only_each_spaces_own_sessions() {
    let mut host = Host::new(true).unwrap();
    let other = host.binding(true).unwrap();
    let home = host.binding(false).unwrap();
    host.start(&other, "other-checkpoint", "other-task", "")
        .unwrap();
    host.start(&home, "home-checkpoint", "home-task", "")
        .unwrap();
    host.state.clear_last_error();
    host.state
        .checkpoint_sessions(chrono::Utc::now().timestamp());
    assert_eq!(host.state.last_error(), None);
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("checkpoint deadline");
    while host.state.session_checkpoint_pending() {
        host.tick();
        if host.state.session_checkpoint_pending() {
            host.wakes
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
        }
    }
    assert_eq!(host.state.last_error(), None);
    let (_, reloaded) = WorkspaceRepository::open(&host.state.config().config_path).unwrap();
    for space in reloaded.spaces() {
        let saved = space.binding().sessions().sessions();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].terminal_snapshot.is_some());
    }
}

#[rstest]
fn native_task_identity_is_exactly_scoped_and_shell_keeps_real_tabs_and_splits() {
    let mut host = Host::new(true).unwrap();
    let other = host.binding(true).unwrap();
    let home = host.binding(false).unwrap();
    let launched = host
        .start(
            &other,
            "other-conversation",
            "task-owned-elsewhere",
            "other prompt",
        )
        .unwrap();
    let record = record(&launched).unwrap();
    let mut home_list = CommandInvocation::new("agents.native.list", Vec::new(), Caller::Socket);
    home_list.target = Some(home.clone());
    assert_eq!(
        success(host.submit(home_list).unwrap())
            .unwrap()
            .as_array()
            .expect("native list missing")
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
        host.submit(collision).unwrap(),
        CommandOutcome::Success { .. }
    ));
    assert_eq!(
        host.state
            .native_agent_service()
            .expect("owner missing")
            .activities()
            .len(),
        1
    );
    assert_eq!(
        host.state
            .native_agent_service()
            .expect("owner missing")
            .activities()[0]
            .id,
        record.id
    );
    let spaces = host.spaces().unwrap();
    let other_space = spaces
        .as_array()
        .expect("Spaces missing")
        .iter()
        .find(|space| space["name"] == "Other")
        .expect("Other missing");
    assert_eq!(
        other_space["sessions"]
            .as_array()
            .expect("sessions missing")
            .len(),
        1
    );
    let shell: CommandTarget =
        serde_json::from_value(other_space["sessions"][0]["target"].clone()).unwrap();
    let mut tab = CommandInvocation::new(
        "terminal.create_tab",
        vec![
            "[]".to_owned(),
            host.directory.path().to_string_lossy().into_owned(),
        ],
        Caller::Socket,
    );
    tab.target = Some(shell);
    let tab_value = success(host.submit(tab).unwrap()).unwrap();
    let terminal: CommandTarget = serde_json::from_value(tab_value["created"].clone()).unwrap();
    assert_eq!(terminal.kind, ResourceKind::Terminal);
    let mut focus = CommandInvocation::new("agents.focus", Vec::new(), Caller::Internal);
    focus.target = Some(terminal);
    success(host.submit(focus).unwrap()).unwrap();
    success(
        host.submit(CommandInvocation::from_action(
            "split_right",
            Caller::Keybinding,
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(host.state.mux().selected_window_panes().len(), 1);
    let request_id = host
        .state
        .pending_new_surface()
        .expect("split chooser missing")
        .id;
    success(
        host.submit(CommandInvocation::new(
            "surface.choose",
            vec![request_id.to_string(), "terminal".to_owned()],
            Caller::Keybinding,
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(host.state.mux().selected_window_panes().len(), 2);
    assert_eq!(
        host.state
            .native_agent_service()
            .expect("owner missing")
            .activities()[0]
            .id,
        record.id
    );
}

#[rstest]
#[case::foreground(false)]
#[case::background(true)]
fn observed_dialog_completion_focuses_only_foreground_native_start(#[case] background: bool) {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let scope = host.state.mux_scope();
    let cwd = host.directory.path().to_string_lossy().into_owned();
    let form = NewSessionForm::new(
        NewSessionDraft {
            projectless: false,
            scope,
            cwd: cwd.clone(),
            mode: NewSessionMode::Agent,
            prompt: "dialog prompt".to_owned(),
            applications: Vec::new(),
            provider_mentions: Vec::new(),
            goal: String::new(),
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
            fan_out: Vec::new(),
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
        .checked_add(FIXTURE_DEADLINE)
        .expect("dialog deadline overflow");
    // The public dialog starts discovery and enables Start only after observing its completion.
    let spec = loop {
        if dialog.poll().is_some() {
            continue;
        }
        let spec = dialog.spec();
        if spec.rows.first().is_some_and(|row| row.enabled) {
            break spec;
        }
        discovery_wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
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
        .expect("start event missing");
    let NewSessionPickerEvent::Submit(invocation) = event else {
        panic!("Ordinary creation must submit before background naming");
    };
    assert_eq!(invocation.arguments[7], "dialog prompt");
    let outcome = host.submit(invocation).unwrap();
    let value = success(outcome.clone()).unwrap();
    let native = record(&value).unwrap().target();
    let (sender, receiver) = mpsc::channel();
    sender.send(outcome).unwrap();
    dialog.started(receiver);
    let observed = dialog.poll().expect("completion missing");
    assert!(
        matches!(&observed,NewSessionPickerEvent::Started {foreground,..} if *foreground != background)
    );
    host.state.open_new_session_dialog_from_ui();
    *host
        .state
        .modal_dialog_mut()
        .expect("creation view missing") = bootty_ui::ModalDialog::NewSession(Box::new(dialog));
    host.effects.clear();
    host.state.apply_picker_event(observed);
    host.tick();
    if !background {
        while !host.effects.iter().any(
            |effect| matches!(effect, AppEffect::NativeConversation(target) if target == &native),
        ) {
            host.wakes
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
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
            .expect("native owner missing")
            .activities()
            .into_iter()
            .find(|record| record.id == native.handle)
            .expect("native activity missing");
        let binding = host.binding(false).unwrap();
        let mut invocation = CommandInvocation::new("session.saved", Vec::new(), Caller::Socket);
        invocation.target = Some(binding);
        let saved = success(host.submit(invocation).unwrap()).unwrap();
        let title = saved
            .as_array()
            .expect("saved sessions missing")
            .iter()
            .find(|session| session["identity"] == "task-dialog")
            .expect("saved task missing")["title"]
            .as_str()
            .expect("saved title missing");
        if activity.title == "Fix native composer" && title == "Fix native composer" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Naming did not finish: tab {:?}, task {title:?}",
            activity.title
        );
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        host.tick();
    }
}

#[rstest]
fn a_failed_provider_keeps_its_exact_native_pane_and_retries_in_place() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    fs::write(host.directory.path().join("fail-initialize"), "").unwrap();
    let attachment = host.directory.path().join("initial.txt");
    fs::write(&attachment, "Retain this attachment across a failed start").unwrap();
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
        .push(serde_json::to_string(&vec![&attachment]).unwrap());
    start.target = Some(binding);
    let outcome = host.submit(start).unwrap();
    assert!(
        matches!(outcome, CommandOutcome::Failed { .. }),
        "{outcome:?}"
    );
    let failed = host
        .state
        .native_agent_service()
        .expect("native owner missing")
        .sessions()
        .into_iter()
        .next()
        .expect("failed reservation missing");
    assert_eq!(failed.snapshot.status, NativeSessionStatus::Error);
    assert_eq!(
        failed.pending_initial_message.as_deref(),
        Some("Do not replay")
    );
    assert_eq!(failed.attachments.len(), 1);
    fs::remove_file(attachment).unwrap();
    let attachments = host
        .state
        .native_agent_service()
        .expect("native owner")
        .resolve_prompt_attachments(
            &failed.target(),
            &failed
                .attachments
                .iter()
                .map(|reference| reference.id.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let prompt = bootty_agents::NativePrompt::new_with_context(
        "Review the saved attachment".into(),
        Vec::new(),
        attachments,
        Vec::new(),
    )
    .unwrap();
    assert_eq!(prompt.attachment_references(), failed.attachments);
    let window = host
        .state
        .mux()
        .selected_window()
        .expect("failed pane not selected")
        .to_owned();
    assert_eq!(host.state.mux().selected_window_panes().len(), 1);
    assert_eq!(
        host.state.mux().selected_window_panes()[0]
            .native_agent
            .as_deref(),
        Some(failed.id.as_str())
    );

    fs::remove_file(host.directory.path().join("fail-initialize")).unwrap();
    let mut focus = CommandInvocation::new("agents.native.focus", Vec::new(), Caller::Socket);
    focus.target = Some(failed.target());
    let recovered: NativeSessionRecord =
        serde_json::from_value(success(host.submit(focus).unwrap()).unwrap()).unwrap();
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
}

#[rstest]
fn native_tabs_share_the_existing_task_and_create_real_backend_windows() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let other = host.binding(true).unwrap();
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
    let terminal = success(host.submit(shell).unwrap()).unwrap();
    let before = host.spaces().unwrap();
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
    let first = record(&success(host.submit(tab.clone()).unwrap()).unwrap()).unwrap();
    "Second conversation".clone_into(&mut tab.arguments[7]);
    "second prompt".clone_into(&mut tab.arguments[8]);
    let second = record(&success(host.submit(tab.clone()).unwrap()).unwrap()).unwrap();
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
        host.spaces().unwrap(),
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
        .expect("Task attachment missing");
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
    success(host.submit(focus).unwrap()).unwrap();
    assert_eq!(
        host.state.mux().selected_window(),
        Some(second_window.as_str())
    );
    for index in [1, 2, 3] {
        success(
            host.submit(CommandInvocation::new(
                "select_tab",
                vec![index.to_string()],
                Caller::Keybinding,
            ))
            .unwrap(),
        )
        .unwrap();
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
            host.submit(rejected).unwrap(),
            CommandOutcome::StaleTarget { .. }
        ));
    }
    assert_eq!(
        host.state
            .native_agent_service()
            .expect("owner missing")
            .activities()
            .len(),
        2
    );
}

#[rstest]
#[case::tab(false)]
#[case::resumed_tab(true)]
fn native_tab_tools_read_the_captured_pane_after_another_task_is_focused(#[case] resume: bool) {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
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
    let shell = success(host.submit(shell).unwrap()).unwrap();
    let terminal: CommandTarget = serde_json::from_value(
        shell
            .get("terminal")
            .cloned()
            .expect("Task Terminal missing"),
    )
    .unwrap();
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
    let tab = success(host.submit(tab).unwrap()).unwrap();
    let mut native = record(&tab).unwrap();
    let native_terminal: CommandTarget = serde_json::from_value(tab["terminal"].clone()).unwrap();
    assert_ne!(native_terminal, terminal);
    if resume {
        host.state
            .native_agent_service()
            .expect("Native owner missing")
            .stop(&native.target())
            .unwrap();
        native = serde_json::from_value(
            success(
                host.submit(native_command(
                    "agents.native.resume",
                    &native.target(),
                    &[],
                    Caller::Socket,
                ))
                .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    }
    let other = host.binding(true).unwrap();
    let foreign = host
        .start(&other, "foreign-task", "foreign-task-id", "")
        .unwrap();
    let foreign_terminal: CommandTarget = serde_json::from_value(
        foreign
            .get("terminal")
            .cloned()
            .expect("Foreign Terminal missing"),
    )
    .unwrap();
    let mut focus = CommandInvocation::new("agents.focus", Vec::new(), Caller::Internal);
    focus.target = Some(foreign_terminal.clone());
    success(host.submit(focus).unwrap()).unwrap();
    let capture = success(
        host.native_tool(&native, "terminal_read", json!({}))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(capture.get("target"), Some(&json!(native_terminal)));
    assert_ne!(capture.get("target"), Some(&json!(foreign_terminal)));
    assert!(capture.get("capture").is_some());

    captured_workspace_reads(&mut host, &native, &native_terminal);

    let mut stale = terminal;
    stale.generation = stale
        .generation
        .checked_add(1)
        .expect("Generation overflow");
    let mut capture = CommandInvocation::new("terminal.capture", Vec::new(), Caller::Socket);
    capture.target = Some(stale);
    assert!(matches!(
        host.submit(capture).unwrap(),
        CommandOutcome::StaleTarget { .. }
    ));
}

fn captured_workspace_reads(
    host: &mut Host,
    native: &NativeSessionRecord,
    native_terminal: &CommandTarget,
) {
    let reply = host
        .native_tool_reply(native, "get_workspace_info", json!({}))
        .unwrap();
    assert_eq!(reply.get("isError"), Some(&json!(false)), "{reply}");
    let spaces = host.spaces().unwrap();
    let captured = spaces
        .as_array()
        .expect("Spaces missing")
        .iter()
        .find(|space| space["scope"] == native.binding_id)
        .expect("Captured Space missing");
    assert_eq!(
        reply.get("structuredContent"),
        Some(&json!({
            "name":captured["name"],"backend":captured["backend"],"host":captured["host"],
        }))
    );
    assert!(!captured["active"].as_bool().expect("Active flag missing"));

    let reply = host
        .native_tool_reply(native, "list_terminals", json!({}))
        .unwrap();
    assert_eq!(reply.get("isError"), Some(&json!(false)), "{reply}");
    let content = reply
        .get("structuredContent")
        .expect("Structured terminal metadata missing");
    assert_eq!(content.get("truncated"), Some(&json!(false)));
    let terminals = content
        .get("terminals")
        .and_then(Value::as_array)
        .expect("Terminal metadata missing");
    let terminal = terminals
        .iter()
        .find(|terminal| terminal["session"] == "tool-task")
        .expect("Captured shell missing");
    assert!(
        terminals
            .iter()
            .all(|terminal| terminal["session"] != "foreign-task"
                && terminal["target"] != json!(native_terminal))
    );
    let target: CommandTarget = serde_json::from_value(terminal["target"].clone()).unwrap();
    let mut capture = CommandInvocation::new("terminal.capture", Vec::new(), Caller::Socket);
    capture.target = Some(target);
    assert!(
        success(host.submit(capture).unwrap())
            .unwrap()
            .get("capture")
            .is_some()
    );
}

#[rstest]
#[case::captured("captured", "", Some("captured"))]
#[case::unprofiled("", "captured", None)]
fn restored_and_forked_native_tools_keep_the_captured_profile(
    #[case] initial_profile: &str,
    #[case] selected_after_restart: &str,
    #[case] expected_profile: Option<&str>,
) {
    let mut host = Host::new_with_spawn(true, true).unwrap();
    let binding = host.binding(false).unwrap();
    let saved = record(
        &host
            .start_with_profile(
                &binding,
                "profile-owner",
                "profile-owner-task",
                "Saved answer",
                initial_profile,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved.config.profile.as_deref(), expected_profile);
    host.restart_with_provider_selection(true, Some(selected_after_restart));
    let saved = host
        .state
        .native_agent_service()
        .expect("Native owner")
        .sessions()
        .into_iter()
        .find(|record| record.id == saved.id)
        .expect("Saved conversation");
    let resumed: NativeSessionRecord = serde_json::from_value(
        success(
            host.submit(native_command(
                "agents.native.resume",
                &saved.target(),
                &[],
                Caller::Socket,
            ))
            .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let forked: NativeSessionRecord = serde_json::from_value(
        success(
            host.submit(native_command(
                "agents.native.fork",
                &resumed.target(),
                &[],
                Caller::Socket,
            ))
            .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    for parent in [resumed, forked] {
        assert_eq!(parent.config.profile.as_deref(), expected_profile);
        assert_eq!(
            host.native_tool_reply(&parent, "inspect_provider", json!({}))
                .unwrap()["structuredContent"]["profile"],
            json!(expected_profile)
        );
        let profiles = host
            .native_tool_reply(&parent, "list_profiles", json!({}))
            .unwrap();
        assert_eq!(profiles["isError"], false);
        assert_eq!(
            profiles["structuredContent"],
            json!({
                "provider":"codex","captured_profile":expected_profile,
                "profiles":[{"id":"captured","name":"Captured"}],
            })
        );
        assert_eq!(
            host.native_tool_reply(&parent, "list_profiles", json!({"provider":"claude"}))
                .unwrap()["isError"],
            true
        );
        assert_eq!(
            parent.config.account_directory,
            saved.config.account_directory
        );
        let child = success(
            host.native_tool(
                &parent,
                "spawn_agent",
                json!({"name":format!("profile-child-{}",parent.id.replace(':',"-")),
                   "provider":"codex","profile":expected_profile,"prompt":"Captured work"}),
            )
            .unwrap(),
        )
        .unwrap();
        let child = host.spawned_record(&child).unwrap();
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
            )
            .unwrap()["isError"],
            true
        );
    }
}

#[rstest]
fn concurrent_native_restore_keeps_the_first_launch_and_its_tools() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let launched = host
        .start(
            &binding,
            "restore-owner",
            "restore-owner-task",
            "Saved answer",
        )
        .unwrap();
    let saved = record(&launched).unwrap();
    host.state
        .native_agent_service()
        .expect("Native owner missing")
        .stop(&saved.target())
        .unwrap();
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("Resume deadline");
    let sender = host.state.app_command_sender(Caller::Socket);
    let first = sender
        .submit(
            native_command("agents.native.resume", &saved.target(), &[], Caller::Socket),
            deadline,
            CommandCancellation::new(),
        )
        .expect("Resume submission");
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
        .expect("Duplicate submission");
    let resumed: NativeSessionRecord =
        serde_json::from_value(success(host.receive(&first, deadline).unwrap()).unwrap()).unwrap();
    assert!(matches!(
        host.receive(&duplicate, deadline).unwrap(),
        CommandOutcome::Unavailable { .. } | CommandOutcome::StaleTarget { .. }
    ));
    verify_native_resume(&host, &saved, &resumed);
    success(
        host.native_tool(&resumed, "terminal_read", json!({}))
            .unwrap(),
    )
    .unwrap();
}

#[rstest]
#[case::detached(false)]
#[case::restarted(true)]
fn saved_native_task_keyboard_selection_restores_its_terminal_checkpoint(#[case] restart: bool) {
    let mut host = Host::new(true).unwrap();
    let mut binding = host.binding(false).unwrap();
    let launched = host
        .start(
            &binding,
            "saved-conversation",
            "saved-native-task",
            "Saved answer",
        )
        .unwrap();
    let saved_record = record(&launched).unwrap();
    let native = saved_record.target();
    let spaces = host.spaces().unwrap();
    let session = spaces
        .as_array()
        .expect("Spaces missing")
        .iter()
        .flat_map(|space| space["sessions"].as_array().into_iter().flatten())
        .find(|session| session["name"] == "saved-conversation")
        .expect("Task shell missing");
    let mut close = CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
    close.target = Some(serde_json::from_value(session["target"].clone()).unwrap());
    close.confirmation = Some(close.confirmation());
    success(host.submit(close).unwrap()).expect("Close saved task");
    if restart {
        host.restart();
        binding = host.binding(false).unwrap();
        let restored = host
            .state
            .native_agent_service()
            .expect("Restored native owner missing")
            .sessions()
            .into_iter()
            .find(|record| record.target() == native)
            .expect("Restored conversation missing");
        assert_eq!(restored.snapshot.status, NativeSessionStatus::Stopped);
        assert_eq!(
            restored.snapshot.transcript,
            saved_record.snapshot.transcript
        );
    }
    let before = host.spaces().unwrap();
    host.effects.clear();
    success(
        host.submit(CommandInvocation::new(
            "ui.sidebar.activate_session",
            Vec::new(),
            Caller::Keybinding,
        ))
        .unwrap(),
    )
    .expect("Select saved history");
    host.tick();
    let after = host.spaces().unwrap();
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
    let saved = success(host.submit(saved_command).unwrap()).unwrap();
    let retained = saved
        .as_array()
        .expect("Saved sessions missing")
        .iter()
        .find(|session| session["identity"] == "saved-native-task")
        .expect("Saved native task missing");
    assert_ne!(retained["attachment"], Value::Null);
    let resumed =
        open_saved_native(&mut host, &saved_record).expect("Resume captured conversation");
    wait_for_native_terminal_read(&mut host, &resumed).unwrap();
}

// Restoring topology accepts the worker before its display stream is ready. Wait on
// worker wakeups, rather than assuming the next command can already capture it.
fn wait_for_native_terminal_read(
    host: &mut Host,
    record: &NativeSessionRecord,
) -> TestResult<Value> {
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .ok_or("Deadline")?;
    loop {
        match host.native_tool(record, "terminal_read", json!({}))? {
            CommandOutcome::Failed { code, message }
                if code == "terminal_capture_failed" && message == "Terminal is still starting" =>
            {
                host.wakes
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
            }
            outcome => return success(outcome),
        }
    }
}

fn open_saved_native(
    host: &mut Host,
    saved_record: &NativeSessionRecord,
) -> TestResult<NativeSessionRecord> {
    let mut focus = CommandInvocation::new("agents.native.focus", Vec::new(), Caller::Socket);
    focus.target = Some(saved_record.target());
    let resumed: NativeSessionRecord = serde_json::from_value(success(host.submit(focus)?)?)?;
    verify_native_resume(host, saved_record, &resumed);
    Ok(resumed)
}

fn verify_native_resume(
    host: &Host,
    saved_record: &NativeSessionRecord,
    resumed: &NativeSessionRecord,
) -> Value {
    let provider_identity = saved_record
        .config
        .session_id
        .as_deref()
        .expect("Captured provider session missing");
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
        .expect("Native owner missing")
        .resolve(&resumed.target())
        .unwrap()
        .rpc("__calls", json!({}))
        .unwrap();
    let methods = calls
        .get("methods")
        .and_then(Value::as_array)
        .expect("Provider calls missing");
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
    calls
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
fn opening_without_a_checkpoint_starts_a_shell_and_resumes_the_same_provider() {
    let mut host = Host::new(true).unwrap();
    let saved_record = legacy_native_conversation(&host).unwrap();
    host.restart();
    let binding = host.binding(false).unwrap();
    let saved_before = host.saved(&binding).unwrap();
    host.effects.clear();
    success(
        host.submit(CommandInvocation::new(
            "ui.sidebar.activate_session",
            Vec::new(),
            Caller::Keybinding,
        ))
        .unwrap(),
    )
    .unwrap();
    host.tick();
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("Resume deadline");
    let resumed = loop {
        host.tick();
        if let Some(record) = host
            .state
            .native_agent_service()
            .expect("Native owner missing")
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
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
    };
    let calls = verify_native_resume(&host, &saved_record, &resumed);
    assert!(host.effects.iter().any(|effect| {
        matches!(effect, AppEffect::NativeConversation(target) if target == &resumed.target())
    }));
    assert!(!host.effects.iter().any(|effect| {
        matches!(effect, AppEffect::NativeConversation(target) if target == &saved_record.target())
    }), "opening never focuses the stopped generation");
    assert_eq!(calls.get("bootty_tools"), Some(&Value::Bool(true)));
    let mut saved_after = host.saved(&binding).unwrap();
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
                .expect("Fixture path is not UTF-8")
        )
    );
}

#[rstest]
fn disabled_native_resume_does_not_restore_a_dead_task_or_change_its_conversation() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let saved_record = record(
        &host
            .start(
                &binding,
                "disabled-resume",
                "disabled-resume-id",
                "Saved work",
            )
            .unwrap(),
    )
    .unwrap();
    let spaces = host.spaces().unwrap();
    let shell = spaces
        .as_array()
        .expect("Spaces missing")
        .iter()
        .flat_map(|space| space["sessions"].as_array().into_iter().flatten())
        .find(|session| session["name"] == "disabled-resume")
        .expect("Task shell missing");
    let mut close = CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
    close.target = Some(serde_json::from_value(shell["target"].clone()).unwrap());
    close.confirmation = Some(close.confirmation());
    success(host.submit(close).unwrap()).unwrap();
    host.restart_with_provider_enabled(false);
    let before = host.spaces().unwrap();
    assert!(matches!(
        host.submit(native_command(
            "agents.native.resume",
            &saved_record.target(),
            &[],
            Caller::Socket
        ))
        .unwrap(),
        CommandOutcome::Denied { .. }
    ));
    assert_eq!(host.spaces().unwrap(), before);
    let restored = host
        .state
        .native_agent_service()
        .expect("Native owner missing")
        .sessions()
        .into_iter()
        .find(|record| record.id == saved_record.id)
        .expect("Saved conversation missing");
    assert_eq!(restored.target(), saved_record.target());
    assert_eq!(restored.snapshot.status, NativeSessionStatus::Stopped);
    assert_eq!(restored.config.session_id, saved_record.config.session_id);
    assert_eq!(
        restored.snapshot.transcript,
        saved_record.snapshot.transcript
    );
}

#[rstest]
#[case::revoked(false)]
#[case::provider_disabled(true)]
fn pending_native_lease_revocation_cancels_saved_task_restore_before_acceptance(
    #[case] disable_provider: bool,
) {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let saved_record = record(
        &host
            .start(
                &binding,
                "pending-restore",
                "pending-restore-id",
                "Retained answer",
            )
            .unwrap(),
    )
    .unwrap();
    let spaces = host.spaces().unwrap();
    let shell = spaces
        .as_array()
        .expect("Spaces missing")
        .iter()
        .flat_map(|space| space["sessions"].as_array().into_iter().flatten())
        .find(|session| session["name"] == "pending-restore")
        .expect("Task shell missing");
    let mut close = CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
    close.target = Some(serde_json::from_value(shell["target"].clone()).unwrap());
    close.confirmation = Some(close.confirmation());
    success(host.submit(close).unwrap()).unwrap();
    let owner = host.state.terminal_agent_service().expect("Tool owner");
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
        &std::env::current_exe().unwrap(),
        Arc::new(|_: CommandInvocation, _: Instant, _: CommandCancellation| {
            CommandOutcome::Unavailable {
                message: "This pending attachment has not launched its provider".to_owned(),
            }
        }),
    )
    .unwrap();
    let bridge = owner
        .retain_tool_attachment(AgentKind::Codex, bridge)
        .unwrap();
    let cancellation = CommandCancellation::new();
    let _launch = bridge.lease().begin_launch(&cancellation).unwrap();
    let before = host.spaces().unwrap();
    let saved_before = host.saved(&binding).unwrap();
    // The public mailbox gives an exact pre-admission barrier: no owner frame is pumped
    // between submission and revocation. This exercises the lease-to-restore boundary.
    let mut restore = CommandInvocation::new(
        "session.reopen",
        vec!["pending-restore-id".to_owned()],
        Caller::Socket,
    );
    restore.target = Some(binding.clone());
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("Deadline");
    let response = host
        .state
        .app_command_sender(Caller::Socket)
        .submit(restore, deadline, cancellation.clone())
        .expect("Mailbox submission");
    if disable_provider {
        owner.set_provider_tools_enabled(AgentKind::Codex, false);
    } else {
        bridge.lease().revoke();
    }
    assert!(cancellation.is_cancelled());
    assert_eq!(
        host.receive(&response, deadline).unwrap(),
        CommandOutcome::cancelled()
    );
    assert_eq!(host.spaces().unwrap(), before);
    assert_eq!(host.saved(&binding).unwrap(), saved_before);
    let retained = host
        .state
        .native_agent_service()
        .expect("Native owner")
        .sessions()
        .into_iter()
        .find(|record| record.id == saved_record.id)
        .expect("Retained native conversation");
    assert_eq!(retained.target(), saved_record.target());
    assert_eq!(retained.config.session_id, saved_record.config.session_id);
    assert_eq!(
        retained.snapshot.transcript,
        saved_record.snapshot.transcript
    );
}

#[rstest]
#[case::invalid_terminal("terminal", "[true]", true)]
#[case::unknown_profile("profile", "unknown", true)]
#[case::disabled_profile("profile", "codex", false)]
fn invalid_surface_choices_leave_a_detached_native_task_closed(
    #[case] kind: &str,
    #[case] argument: &str,
    #[case] provider_enabled: bool,
) {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let native = record(
        &host
            .start(&binding, "chooser-restore", "chooser-restore-id", "")
            .unwrap(),
    )
    .unwrap();
    let spaces = host.spaces().unwrap();
    let shell = spaces
        .as_array()
        .expect("Spaces missing")
        .iter()
        .flat_map(|space| space["sessions"].as_array().into_iter().flatten())
        .find(|session| session["name"] == "chooser-restore")
        .expect("Task terminal missing");
    let mut close = CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
    close.target = Some(serde_json::from_value(shell["target"].clone()).unwrap());
    close.confirmation = Some(close.confirmation());
    success(host.submit(close).unwrap()).unwrap();
    if !provider_enabled {
        host.restart_with_provider_enabled(false);
    }
    let binding = host.binding(false).unwrap();
    let target = host
        .state
        .native_agent_service()
        .expect("Native owner missing")
        .sessions()
        .into_iter()
        .find(|record| record.id == native.id)
        .expect("Retained conversation missing")
        .target();
    let before = host.spaces().unwrap();
    let saved_before = host.saved(&binding).unwrap();
    let mut open = CommandInvocation::from_action("new_tab", Caller::Socket);
    open.target = Some(target);
    success(host.submit(open).unwrap()).unwrap();
    let id = host
        .state
        .pending_new_surface()
        .expect("Chooser missing")
        .id;
    let outcome = host
        .submit(CommandInvocation::new(
            "surface.choose",
            vec![id.to_string(), kind.to_owned(), argument.to_owned()],
            Caller::Socket,
        ))
        .unwrap();
    assert!(
        matches!(
            outcome,
            CommandOutcome::Failed { .. } | CommandOutcome::Denied { .. }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        host.spaces().unwrap(),
        before,
        "invalid choices cannot restore a terminal"
    );
    assert_eq!(host.saved(&binding).unwrap(), saved_before);
    assert_eq!(
        host.state
            .pending_new_surface()
            .expect("Chooser removed")
            .id,
        id
    );
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
) {
    let mut host = Host::new(true).unwrap();
    let file = host.directory.path().join("review.md");
    fs::write(&file, "attachment marker").unwrap();
    success(
        host.submit(CommandInvocation::from_action("new_tab", caller))
            .unwrap(),
    )
    .unwrap();
    let request = host
        .state
        .pending_new_surface()
        .expect("Chooser missing")
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
            serde_json::to_string(&vec![file]).unwrap(),
        ],
        caller,
    );
    let outcome = host.submit(invocation).unwrap();
    if admitted {
        let created = record(&success(outcome).unwrap()).unwrap();
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
                .expect("Native owner missing")
                .sessions()
                .is_empty()
        );
    }
}

#[rstest]
fn first_prompt_rejection_returns_the_observed_created_conversation() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let value = host
        .start(
            &binding,
            "rejected-prompt",
            "rejected-prompt-id",
            "reject-first",
        )
        .unwrap();
    let error = value
        .get("first_prompt_error")
        .and_then(Value::as_str)
        .expect("The saved conversation must expose its first prompt rejection");
    let rejection: Value = serde_json::from_str(error).unwrap();
    assert_eq!(
        rejection.get("message").and_then(Value::as_str),
        Some("first prompt rejected")
    );
    let record = record(&value).unwrap();
    assert_eq!(record.snapshot.status, NativeSessionStatus::Error);
    assert_eq!(record.snapshot.first_turn, None);
    assert_eq!(record.task_identity.as_deref(), Some("rejected-prompt-id"));
    assert_eq!(
        host.state
            .native_agent_service()
            .expect("Native owner")
            .sessions()
            .len(),
        1
    );
    assert_eq!(host.state.mux().all_sessions().len(), 1);
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
                providers: Vec::new(),
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
fn legacy_spawn_preserves_the_registered_callers_authority() {
    let mut host = Host::new_with_spawn(true, true).unwrap();
    let binding = host.binding(false).unwrap();
    let created = host
        .start(&binding, "legacy-parent", "legacy-parent-id", "")
        .unwrap();
    let record = record(&created).unwrap();
    let terminal: CommandTarget =
        serde_json::from_value(created.get("terminal").cloned().expect("Parent terminal")).unwrap();
    let _lease = attach_sibling_tui(&host, &binding, &terminal, &record.binding_id).unwrap();
    let before = host.spaces().unwrap();
    let mut legacy = CommandInvocation::new(
        "agents.spawn",
        vec![json!({"kind":"shell","name":"legacy-child"}).to_string()],
        Caller::Cli,
    );
    legacy.target = Some(terminal);
    assert!(matches!(
        host.submit(legacy.clone()).unwrap(),
        CommandOutcome::Denied { .. }
    ));
    assert_eq!(host.spaces().unwrap(), before);
    legacy.caller = Caller::Socket;
    let child = success(host.submit(legacy).unwrap()).unwrap();
    let target: CommandTarget =
        serde_json::from_value(child.get("terminal").cloned().expect("Child terminal")).unwrap();
    assert_eq!(target.kind, ResourceKind::Terminal);
    let identity = child
        .get("task_id")
        .and_then(Value::as_str)
        .expect("Child identity");
    let saved = host.saved(&binding).unwrap();
    assert!(
        saved
            .as_array()
            .expect("Saved sessions missing")
            .iter()
            .any(|session| session.get("identity").and_then(Value::as_str) == Some(identity))
    );
}

#[rstest]
fn spawn_guard_preserves_caller_cancellation_at_durable_creation_acceptance() {
    let mut host = Host::new_with_spawn(true, true).unwrap();
    let binding = host.binding(false).unwrap();
    let created = host
        .start(&binding, "cancel-parent", "cancel-parent-id", "")
        .unwrap();
    let record = record(&created).unwrap();
    let terminal: CommandTarget =
        serde_json::from_value(created.get("terminal").cloned().expect("Parent terminal")).unwrap();
    let lease = attach_sibling_tui(&host, &binding, &terminal, &record.binding_id).unwrap();
    let incoming = CommandCancellation::new();
    let guard = lease
        .begin_spawn_with_cancellation(
            &ToolSpawnRequest::Shell {
                name: "cancelled-child".to_owned(),
                title: None,
            },
            incoming.clone(),
        )
        .unwrap();
    let before = host.spaces().unwrap();
    let saved_before = host.saved(&binding).unwrap();
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
        .checked_add(FIXTURE_DEADLINE)
        .expect("Deadline");
    let response = host
        .state
        .app_command_sender(guard.caller())
        .submit(create, deadline, guard.cancellation())
        .expect("Mailbox submission");
    assert!(incoming.cancel());
    assert_eq!(
        host.receive(&response, deadline).unwrap(),
        CommandOutcome::cancelled()
    );
    assert_eq!(host.spaces().unwrap(), before);
    let saved = host.saved(&binding).unwrap();
    assert_eq!(saved, saved_before);
    assert!(
        !saved
            .as_array()
            .expect("Saved sessions missing")
            .iter()
            .any(|session| session.get("identity").and_then(Value::as_str)
                == Some("cancelled-child-id"))
    );
}

#[rstest]
#[case::interrupt("interrupt_spawned_agent", "interrupt")]
#[case::stop("stop_spawned_agent", "stop")]
fn native_parent_supervises_only_children_from_its_exact_live_grant(
    #[case] tool: &str,
    #[case] operation: &str,
) {
    let mut host = Host::new_with_spawn(true, true).unwrap();
    let binding = host.binding(false).unwrap();
    let parent = record(
        &host
            .start(&binding, "supervisor", "supervisor-task", "parent work")
            .unwrap(),
    )
    .unwrap();
    let spawned = success(
        host.native_tool(
            &parent,
            "spawn_agent",
            json!({"name":"supervised-child","provider":"codex","prompt":"child work"}),
        )
        .unwrap(),
    )
    .unwrap();
    let child = host.spawned_record(&spawned).unwrap();
    let child_pane = host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.tag.identity == child.task_identity)
        .and_then(|session| session.windows.first())
        .and_then(|window| window.panes.first())
        .expect("Child pane")
        .pane_id
        .clone()
        .expect("Child pane ID");
    let selected = host.state.mux().selected_session().map(str::to_owned);
    let foreign_binding = host.binding(true).unwrap();
    let foreign = record(
        &host
            .start(&foreign_binding, "unrelated", "unrelated-task", "")
            .unwrap(),
    )
    .unwrap();
    for (id, generation) in [
        (parent.id.as_str(), parent.generation),
        (foreign.id.as_str(), foreign.generation),
        (child.id.as_str(), child.generation.saturating_add(1)),
    ] {
        assert_eq!(
            host.native_tool_reply(&parent, tool, json!({"id":id,"generation":generation}))
                .unwrap()["isError"],
            true
        );
    }
    assert_eq!(
        host.native_tool_reply(
            &child,
            tool,
            json!({"id":parent.id,"generation":parent.generation}),
        )
        .unwrap()["isError"],
        true
    );
    // A name reaches only the parent's own children too.
    for name in [parent.full_name(), foreign.full_name()] {
        assert_eq!(
            host.native_tool_reply(&parent, tool, json!({"id":name}))
                .unwrap()["isError"],
            true
        );
    }
    assert_eq!(
        host.native_tool_reply(
            &parent,
            tool,
            json!({"id":child.id,"generation":child.generation,"operation":"stop"}),
        )
        .unwrap()["isError"],
        true
    );
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("Child deadline");
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
        .expect("Child prompt");
    host.wait_working(&child.target(), deadline);
    let controlled = host
        .native_tool_reply(
            &parent,
            tool,
            // Interrupting names the child; stopping gives its exact id and generation.
            if operation == "interrupt" {
                json!({"id":child.full_name()})
            } else {
                json!({"id":child.id,"generation":child.generation})
            },
        )
        .unwrap();
    assert_eq!(controlled["isError"], false);
    let outcome: CommandOutcome = serde_json::from_str(
        controlled["content"][0]["text"]
            .as_str()
            .expect("Control reply"),
    )
    .unwrap();
    let value = success(outcome).unwrap();
    assert_eq!(value["operation"], operation);
    assert_eq!(value["agent"]["id"], child.id);
    assert_eq!(value["agent"]["spawn_parent"], json!(parent.target()));
    let _completed = host.receive(&waiting, deadline).unwrap();
    let service = host.state.native_agent_service().expect("Native owner");
    if operation == "stop" {
        assert!(service.resolve(&child.target()).is_err());
        let saved = service
            .sessions()
            .into_iter()
            .find(|record| record.target() == child.target())
            .expect("Saved child");
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
            )
            .unwrap()["isError"],
            false
        );
    } else {
        assert_eq!(
            service.resolve(&child.target()).unwrap().snapshot().status,
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
            .expect("Native owner")
            .resolve(&parent.target())
            .is_ok()
    );
    host.restart();
    let restored = open_saved_native(&mut host, &parent).unwrap();
    assert_ne!(restored.generation, parent.generation);
    assert_eq!(
        host.native_tool_reply(
            &restored,
            tool,
            json!({"id":child.id,"generation":child.generation}),
        )
        .unwrap()["isError"],
        true,
        "a renewed parent grant cannot inherit old child control"
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn native_private_spawn_uses_its_parent_even_when_a_tui_shares_the_terminal(
    #[case] sibling_tui: bool,
) {
    let mut host = Host::new_with_spawn(true, true).unwrap();
    let binding = host.binding(false).unwrap();
    let created = host
        .start(&binding, "native-parent", "native-parent-id", "parent work")
        .unwrap();
    let record = record(&created).unwrap();
    let terminal: CommandTarget =
        serde_json::from_value(created.get("terminal").cloned().expect("Parent terminal")).unwrap();
    let service = host.state.terminal_agent_service().expect("Tool owner");
    let sibling_lease = sibling_tui
        .then(|| attach_sibling_tui(&host, &binding, &terminal, &record.binding_id))
        .transpose()
        .unwrap();
    let selected = host.state.mux().selected_session().map(str::to_owned);
    let child = success(
        host.native_tool(
            &record,
            "spawn_agent",
            json!({"name":"native-child","provider":"codex","prompt":"captured child work"}),
        )
        .unwrap(),
    )
    .unwrap();
    let child_record = host.spawned_record(&child).unwrap();
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
        host.native_tool_reply(&child_record, "terminal_read", json!({}))
            .unwrap()["isError"],
        false
    );
    assert_eq!(
        host.native_tool_reply(&child_record, "list_agents", json!({}))
            .unwrap()["isError"],
        true
    );
    assert_eq!(
        host.native_tool_reply(&child_record, "spawn_shell", json!({"name":"grandchild"}))
            .unwrap()["isError"],
        true
    );
    let task = child_record
        .task_identity
        .as_deref()
        .expect("Native child task");
    let pane = host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.tag.identity.as_deref() == Some(task))
        .and_then(|session| session.windows.first())
        .and_then(|window| window.panes.first())
        .expect("Child backend pane");
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
        host.submit(unknown).unwrap(),
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
        host.submit(unknown_native).unwrap(),
        CommandOutcome::Denied { .. }
    ));
    host.state
        .native_agent_service()
        .expect("Native owner")
        .stop(&record.target())
        .unwrap();
    assert_eq!(
        host.native_tool_reply(&child_record, "terminal_read", json!({}))
            .unwrap()["isError"],
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
    host.restart();
    let mut resume = CommandInvocation::new(
        "agents.native.resume",
        vec![child_record.id.clone(), child_record.generation.to_string()],
        Caller::Socket,
    );
    resume.target = Some(child_record.target());
    let restored: NativeSessionRecord =
        serde_json::from_value(success(host.submit(resume).unwrap()).unwrap()).unwrap();
    assert_eq!(restored.spawn_parent, Some(record.target()));
    assert_eq!(restored.config.session_id, child_record.config.session_id);
    assert_eq!(
        restored.config.account_directory,
        child_record.config.account_directory
    );
    let calls = host
        .state
        .native_agent_service()
        .expect("Native service")
        .resolve(&restored.target())
        .unwrap()
        .rpc("__calls", json!({}))
        .unwrap();
    assert_eq!(
        calls["bootty_tools"], false,
        "provider restoration cannot renew a parent's revoked grant"
    );
}

#[rstest]
fn spawned_terminal_tools_keep_the_exact_shell_and_foreground() {
    let mut host = Host::new_with_spawn(true, true).unwrap();
    let binding = host.binding(false).unwrap();
    let created = host
        .start(&binding, "parent", "parent-terminal-tools", "parent work")
        .unwrap();
    let parent = record(&created).unwrap();
    let parent_terminal = created.get("terminal").expect("Parent Terminal").clone();
    let other_binding = host.binding(true).unwrap();
    let other = record(
        &host
            .start(
                &other_binding,
                "other",
                "other-terminal-tools",
                "other work",
            )
            .unwrap(),
    )
    .unwrap();
    let selected = host.state.mux().selected_session().map(str::to_owned);
    let receipt = success(
        host.native_tool(&parent, "spawn_shell", json!({"name":"owned-shell"}))
            .unwrap(),
    )
    .unwrap();
    let terminal = receipt.get("terminal").expect("Created Terminal").clone();
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
            )
            .unwrap()["isError"],
            true
        );
    }
    let mut stale: CommandTarget = serde_json::from_value(terminal.clone()).unwrap();
    stale.generation = stale.generation.saturating_add(1);
    for tool in [
        "read_spawned_terminal",
        "submit_spawned_terminal",
        "interrupt_spawned_terminal",
        "close_spawned_terminal",
    ] {
        assert_eq!(
            host.native_tool_reply(&parent, tool, json!({"terminal":stale}))
                .unwrap()["isError"],
            true
        );
        assert_eq!(
            host.native_tool_reply(
                &parent,
                tool,
                json!({"terminal":terminal,"operation":"close"})
            )
            .unwrap()["isError"],
            true
        );
    }
    let capture = success(
        host.native_tool(
            &parent,
            "read_spawned_terminal",
            json!({"terminal":terminal}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(capture.get("target"), Some(&terminal));
    assert!(capture.get("capture").is_some());
    success(host.native_tool(&parent, "paste_spawned_terminal", json!({"terminal":terminal,"text":"printf '\\123\\103\\117\\120\\105\\104\\137\\123\\110\\105\\114\\114\\137\\117\\113\\n'"})).unwrap()).unwrap();
    success(
        host.native_tool(
            &parent,
            "submit_spawned_terminal",
            json!({"terminal":terminal}),
        )
        .unwrap(),
    )
    .unwrap();
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("Shell deadline");
    loop {
        let captured = success(
            host.native_tool(
                &parent,
                "read_spawned_terminal",
                json!({"terminal":terminal}),
            )
            .unwrap(),
        )
        .unwrap();
        let text = captured
            .get("capture")
            .and_then(|capture| capture.get("text"))
            .and_then(Value::as_str)
            .expect("Shell capture text");
        if text.lines().any(|line| line.trim() == "SCOPED_SHELL_OK") {
            break;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
    }
    success(
        host.native_tool(
            &parent,
            "interrupt_spawned_terminal",
            json!({"terminal":terminal}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(host.state.mux().selected_session(), selected.as_deref());
    success(
        host.native_tool(
            &parent,
            "close_spawned_terminal",
            json!({"terminal":terminal}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        host.native_tool_reply(
            &parent,
            "read_spawned_terminal",
            json!({"terminal":terminal})
        )
        .unwrap()["isError"],
        true
    );
    assert_eq!(host.state.mux().selected_session(), selected.as_deref());
    let renewed_receipt = success(
        host.native_tool(&parent, "spawn_shell", json!({"name":"retained-shell"}))
            .unwrap(),
    )
    .unwrap();
    let old_terminal = renewed_receipt
        .get("terminal")
        .expect("Retained Terminal")
        .clone();
    host.restart();
    let renewed = open_saved_native(&mut host, &parent).unwrap();
    assert_eq!(
        host.native_tool_reply(
            &renewed,
            "paste_spawned_terminal",
            json!({"terminal":old_terminal,"text":"must not regain input authority"})
        )
        .unwrap()["isError"],
        true
    );
}

#[rstest]
#[case(Caller::CommandPalette)]
#[case(Caller::Keybinding)]
#[case(Caller::BuiltinKeybinding)]
#[case(Caller::Cli)]
#[case(Caller::Socket)]
#[case(Caller::Internal)]
fn closing_native_panes_retains_history_and_reopens_the_same_provider(#[case] caller: Caller) {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let launched = host
        .start(&binding, "close-conversation", "close-native-task", "")
        .unwrap();
    let record = record(&launched).unwrap();
    let mut close = CommandInvocation::new("agents.native.close", Vec::new(), caller);
    close.target = Some(record.target());
    assert_eq!(
        success(host.submit(close.clone()).unwrap()).unwrap(),
        serde_json::to_value(record.target()).unwrap()
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
        .expect("native owner")
        .sessions()
        .into_iter()
        .find(|saved| saved.id == record.id)
        .expect("Retained conversation missing");
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
            host.submit(close.clone()).unwrap(),
            CommandOutcome::StaleTarget { .. }
        ));
    }
    let resumed = open_saved_native(&mut host, &record).unwrap();
    assert!(
        host.state
            .mux()
            .all_sessions()
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
            .any(|pane| pane.native_agent.as_deref() == Some(&resumed.id))
    );
    success(
        host.native_tool(&resumed, "terminal_read", json!({}))
            .unwrap(),
    )
    .unwrap();
}

#[rstest]
#[case(false)]
#[case(true)]
fn palette_surface_choice_retains_its_exact_native_parent_and_rejects_replaced_generation(
    #[case] replace_parent: bool,
) {
    use bootty_ui::presentation::dialogs::CommandPaletteEvent;
    use bootty_ui::surface_creation::SurfaceParent;
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let created = host
        .start(&binding, "palette-parent", "palette-task", "")
        .unwrap();
    let native = record(&created).unwrap();
    let target = native.target();
    let before = host.spaces().unwrap();
    host.state
        .capture_command_palette_native_parent(Some(target.clone()));
    success(
        host.submit(CommandInvocation::from_action(
            "command_palette",
            Caller::Keybinding,
        ))
        .unwrap(),
    )
    .unwrap();
    assert!(
        host.state.modal_dialog().is_some(),
        "palette actually opened"
    );
    if replace_parent {
        host.state
            .native_agent_service()
            .expect("Native owner missing")
            .stop(&target)
            .unwrap();
        let resumed: NativeSessionRecord = serde_json::from_value(
            success(
                host.submit(native_command(
                    "agents.native.resume",
                    &target,
                    &[],
                    Caller::Socket,
                ))
                .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
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
        // The notice names the command that failed and why, not a generic sentence.
        pretty_assertions::assert_eq!(
            host.state
                .last_error_notice()
                .map(|notice| notice.to_string()),
            Some(format!(
                "{} failed. That pane or session no longer exists.",
                bootty_ui::action_catalog::Command::SplitRight.title()
            ))
        );
    } else {
        verify_palette_native_split_cancel(&mut host, &target, &before);
    }
    // A subsequent palette has no captured native parent after the first palette closes.
    success(
        host.submit(CommandInvocation::from_action(
            "command_palette",
            Caller::Keybinding,
        ))
        .unwrap(),
    )
    .unwrap();
    host.state
        .apply_command_palette_event(CommandPaletteEvent::Run(
            bootty_ui::action_catalog::Command::SplitRight,
        ));
    host.tick();
    let request = host
        .state
        .pending_new_surface()
        .expect("terminal split chooser missing")
        .clone();
    assert!(matches!(request.parent, SurfaceParent::Terminal(_)));
    success(
        host.submit(CommandInvocation::new(
            "surface.cancel",
            vec![request.id.to_string()],
            Caller::CommandPalette,
        ))
        .unwrap(),
    )
    .unwrap();
}

fn verify_palette_native_split_cancel(host: &mut Host, target: &CommandTarget, before: &Value) {
    use bootty_ui::surface_creation::{SurfaceParent, SurfacePlacement};
    let request = host
        .state
        .pending_new_surface()
        .expect("native split chooser missing")
        .clone();
    assert_eq!(request.parent, SurfaceParent::Conversation(target.clone()));
    assert_eq!(
        request.placement,
        SurfacePlacement::Split(bootty_mux::pane_layout::SplitDirection::Right)
    );
    assert_eq!(request.task_identity, "palette-task");
    let cancelled = host
        .submit(CommandInvocation::new(
            "surface.cancel",
            vec![request.id.to_string()],
            Caller::CommandPalette,
        ))
        .unwrap();
    success(cancelled).unwrap();
    assert!(host.state.pending_new_surface().is_none());
    assert!(
        host.effects.iter().any(
            |effect| matches!(effect, AppEffect::CloseSurfaceChooser(id) if *id == request.id)
        )
    );
    assert!(
        host.state
            .native_agent_service()
            .expect("Native owner missing")
            .sessions()
            .iter()
            .any(|record| record.target() == *target)
    );
    assert_eq!(
        host.spaces().unwrap(),
        *before,
        "cancelling the native split leaves terminal topology unchanged"
    );
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
) {
    if matches!(backend, MultiplexerBackendConfig::Native) {
        return agent_choice_split(command, conversation_parent, backend);
    }
    run_isolated_split(command, conversation_parent, backend);
}

const SPLIT_ENV: &str = "BOOTTY_SPLIT_CHILD";
const SPLIT_CHILD: &str = "isolated_split_child";

/// rmux and tmux keep their servers in process-global, per-user locations, so their splits run
/// in a child with private `RMUX_TMPDIR` and `TMUX_TMPDIR` instead of the developer's servers.
fn run_isolated_split(command: &str, conversation_parent: bool, backend: MultiplexerBackendConfig) {
    // Socket paths include the development namespace and must fit Unix socket limits.
    let directory = assert_fs::TempDir::new_in("/tmp").unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut child = std::process::Command::new(&executable);
    child
        .args(["--exact", SPLIT_CHILD, "--nocapture"])
        .env(SPLIT_ENV, format!("{command} {conversation_parent}"))
        .env("TMUX_TMPDIR", directory.path())
        .env("RMUX_TMPDIR", directory.path())
        // Panes inherit the server's environment; a developer's login shell must not decide timing.
        .env("SHELL", "/bin/sh")
        .env("ENV", "");
    if matches!(backend, MultiplexerBackendConfig::Rmux) {
        let helper = directory.path().join("bootty-daemon");
        bootty_test_support::write_executable(
            &helper,
            format!(
                "#!/bin/sh\nexport BOOTTY_RMUX_PIPE_ENDPOINT=\"$2\"\nexec {} --exact {SPLIT_CHILD} --nocapture\n",
                bootty_host::shell_quote(&executable.to_string_lossy()),
            ),
        ).unwrap();
        child
            .env("BOOTTY_DAEMON_BINARY", &helper)
            .env("BOOTTY_SPLIT_BACKEND", "rmux");
    } else {
        child.env("BOOTTY_SPLIT_BACKEND", "tmux");
    }
    let output = child.output().unwrap();
    // A tmux server outlives its last session; stop the private one with its directory.
    for socket in fs::read_dir(directory.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("tmux-"))
        .flat_map(|entry| fs::read_dir(entry.path()).into_iter().flatten())
        .filter_map(Result::ok)
    {
        let _ = std::process::Command::new("tmux")
            .arg("-S")
            .arg(socket.path())
            .arg("kill-server")
            .output();
    }
    assert!(
        output.status.success(),
        "isolated {backend:?} split failed: {}\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs inside `run_isolated_split`, as the split itself or as its rmux daemon helper.
#[test]
fn isolated_split_child() {
    if let Some(endpoint) = std::env::var_os("BOOTTY_RMUX_PIPE_ENDPOINT") {
        bootty_mux::rmux::run_pipe_helper(endpoint.into()).unwrap();
        return;
    }
    let Some(spec) = std::env::var_os(SPLIT_ENV) else {
        return;
    };
    let spec = spec.to_string_lossy();
    let (command, conversation_parent) = spec.split_once(' ').expect("malformed split spec");
    if std::env::var_os("BOOTTY_SPLIT_BACKEND").is_some_and(|backend| backend == "tmux") {
        return agent_choice_split(
            command,
            conversation_parent == "true",
            MultiplexerBackendConfig::Tmux,
        );
    }
    // A private daemon at this child's endpoint plays the app's rmux owner.
    let endpoint =
        bootty_mux::rmux::endpoint_path_for(bootty_config::ApplicationIdentity::for_process())
            .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .build()
        .unwrap();
    let daemon = runtime
        .block_on(rmux_server::ServerDaemon::new(rmux_server::DaemonConfig::new(endpoint)).bind())
        .unwrap();
    agent_choice_split(
        command,
        conversation_parent == "true",
        MultiplexerBackendConfig::Rmux,
    );
    runtime.block_on(daemon.shutdown()).unwrap();
    // Pane I/O tasks belong to processes that end with this child; do not wait for them.
    runtime.shutdown_background();
}

fn agent_choice_split(command: &str, conversation_parent: bool, backend: MultiplexerBackendConfig) {
    let mut host = Host::new_with_backend(true, true, backend).unwrap();
    let binding = host.binding(false).unwrap();
    let fixture = host
        .directory
        .path()
        .file_name()
        .expect("fixture directory");
    let name = format!(
        "split-{}",
        fixture.to_string_lossy().trim_start_matches('.')
    );
    let identity = format!("{name}-task");
    let started = host.start(&binding, &name, &identity, "").unwrap();
    let native = record(&started).unwrap();
    success(
        host.submit(native_command(
            "agents.native.history",
            &native.target(),
            &["latest"],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    let parent = if conversation_parent {
        native.target()
    } else {
        serde_json::from_value(started.get("terminal").cloned().expect("terminal target")).unwrap()
    };
    let before = host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.name == name)
        .expect("parent session")
        .windows
        .clone();
    let created = choose_agent_in_split(&mut host, command, parent);
    assert_agents_share_the_parent_window(&host, &name, &before, &native, &created);
    close_session(&mut host, &name);
}

/// Split `parent` with `command` and choose a Codex conversation for the new pane.
fn choose_agent_in_split(
    host: &mut Host,
    command: &str,
    parent: CommandTarget,
) -> NativeSessionRecord {
    let mut split = CommandInvocation::from_action(command, Caller::Keybinding);
    split.target = Some(parent);
    success(host.submit(split).unwrap()).unwrap();
    let request = host
        .state
        .pending_new_surface()
        .expect("split chooser")
        .clone();
    success(
        host.submit(CommandInvocation::new(
            "surface.choose",
            vec![request.id.to_string(), "agent".into()],
            Caller::Keybinding,
        ))
        .unwrap(),
    )
    .unwrap();
    record(
        &success(
            host.submit(CommandInvocation::new(
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
            ))
            .unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

/// The split added a pane to the parent's window rather than a window, holding the new
/// conversation beside the parent's, in the same task.
fn assert_agents_share_the_parent_window(
    host: &Host,
    name: &str,
    before: &[bootty_mux::snapshot::MuxWindow],
    native: &NativeSessionRecord,
    created: &NativeSessionRecord,
) {
    let windows = &host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.name == name)
        .expect("parent session")
        .windows;
    assert_eq!(windows.len(), before.len());
    let (window, before) = windows.first().zip(before.first()).expect("parent window");
    assert_eq!(window.id, before.id);
    let agents = window
        .panes
        .iter()
        .map(|pane| pane.native_agent.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(agents.len(), 2);
    assert!(agents.contains(&Some(native.id.as_str())));
    assert!(agents.contains(&Some(created.id.as_str())));
    assert_eq!(created.task_identity, native.task_identity);
}

fn close_session(host: &mut Host, name: &str) {
    let spaces = host.spaces().unwrap();
    let target = spaces
        .as_array()
        .expect("Spaces")
        .iter()
        .filter_map(|space| space.get("sessions").and_then(serde_json::Value::as_array))
        .flatten()
        .find(|session| session.get("name").and_then(serde_json::Value::as_str) == Some(name))
        .and_then(|session| session.get("target"))
        .expect("created session")
        .clone();
    let mut close = CommandInvocation::new("session.close", vec![], Caller::Socket);
    close.target = Some(serde_json::from_value(target).unwrap());
    close.confirmation = Some(close.confirmation());
    success(host.submit(close).unwrap()).unwrap();
}

#[rstest]
#[case("split_right")]
#[case("split_down")]
fn terminal_choice_splits_the_exact_native_pane_instead_of_creating_a_tab(
    #[case] command: &str,
    #[values(Caller::CommandPalette, Caller::Keybinding, Caller::Socket)] caller: Caller,
) {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let native = record(
        &host
            .start(&binding, "split-native", "split-native-task", "")
            .unwrap(),
    )
    .unwrap();
    let before = host.state.mux().all_sessions()[0].windows.clone();
    let mut split = CommandInvocation::from_action(command, caller);
    split.target = Some(native.target());
    success(host.submit(split).unwrap()).unwrap();
    let request = host.state.pending_new_surface().expect("split chooser").id;
    success(
        host.submit(CommandInvocation::new(
            "surface.choose",
            vec![request.to_string(), "terminal".into()],
            caller,
        ))
        .unwrap(),
    )
    .unwrap();
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
}

#[rstest]
fn a_native_fork_occupies_a_real_right_backend_pane_and_preserves_its_source() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let launched = host
        .start(&binding, "fork-task", "fork-task-id", "source")
        .unwrap();
    let source = record(&launched).unwrap();
    let before = host.state.mux().all_sessions()[0].windows[0].clone();
    let child: NativeSessionRecord = serde_json::from_value(
        success(
            host.submit(native_command(
                "agents.native.fork",
                &source.target(),
                &[],
                Caller::Socket,
            ))
            .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_ne!(child.id, source.id);
    assert_ne!(child.config.session_id, source.config.session_id);
    let session = host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.tag.identity.as_deref() == Some("fork-task-id"))
        .expect("Source task missing");
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
    let child_pane = window.panes[1].pane_id.clone().expect("Child pane missing");
    let mut focus = CommandInvocation::new("agents.native.focus", vec![], Caller::Socket);
    focus.target = Some(child.target());
    success(host.submit(focus).unwrap()).unwrap();
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

    let source_capture = success(
        host.native_tool(&source, "terminal_read", json!({}))
            .unwrap(),
    )
    .unwrap();
    let child_capture = success(
        host.native_tool(&child, "terminal_read", json!({}))
            .unwrap(),
    )
    .unwrap();
    assert_ne!(source_capture["target"], child_capture["target"]);
    success(
        host.submit(native_command(
            "agents.native.stop",
            &child.target(),
            &[],
            Caller::Socket,
        ))
        .unwrap(),
    )
    .unwrap();
    let resumed = open_saved_native(&mut host, &child).unwrap();
    let resumed_capture = success(
        host.native_tool(&resumed, "terminal_read", json!({}))
            .unwrap(),
    )
    .unwrap();
    assert_ne!(source_capture["target"], resumed_capture["target"]);
    let mut close = CommandInvocation::from_action("agents.native.close", Caller::Socket);
    close.target = Some(resumed.target());
    success(host.submit(close).unwrap()).unwrap();
    let reopened = open_saved_native(&mut host, &resumed).unwrap();
    let reopened_capture = success(
        host.native_tool(&reopened, "terminal_read", json!({}))
            .unwrap(),
    )
    .unwrap();
    assert_ne!(source_capture["target"], reopened_capture["target"]);
    let session = host
        .state
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.tag.identity.as_deref() == Some("fork-task-id"))
        .expect("Source task missing");
    assert_eq!(session.windows.len(), 1);
    assert_eq!(session.windows[0].panes.len(), 2);
}

#[rstest]
fn closing_a_native_pane_through_the_terminal_command_also_retires_its_provider() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let launched = host
        .start(&binding, "mux-close-task", "mux-close-task-id", "")
        .unwrap();
    let native = record(&launched).unwrap();
    let mut focus = CommandInvocation::new("agents.native.focus", vec![], Caller::Socket);
    focus.target = Some(native.target());
    success(host.submit(focus).unwrap()).unwrap();
    let mut close = CommandInvocation::new("pane.close", vec![], Caller::Socket);
    close.target = Some(serde_json::from_value(launched["terminal"].clone()).unwrap());
    close.confirmation = Some(close.confirmation());
    success(host.submit(close).unwrap()).unwrap();
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("retirement deadline");
    loop {
        host.tick();
        if host
            .state
            .native_agent_service()
            .expect("native owner")
            .activities()
            .iter()
            .any(|record| record.id == native.id && record.status == NativeSessionStatus::Stopped)
        {
            break;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
    }
    assert!(
        host.state
            .native_agent_service()
            .expect("native owner")
            .sessions()
            .iter()
            .any(|record| record.id == native.id)
    );
}

#[rstest]
fn native_mux_placement_is_published_before_provider_initialization() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    fs::write(host.directory.path().join("hold-thread-start"), "").unwrap();
    let ready = std::os::unix::net::UnixListener::bind(host.directory.path().join("start-barrier"))
        .unwrap();
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("deadline");
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
        .unwrap();
    let placed_record = loop {
        host.tick();
        let record = host
            .state
            .native_agent_service()
            .expect("native owner")
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
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
    };
    assert_eq!(placed_record.snapshot.status, NativeSessionStatus::Starting);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let (mut ready_stream, _) = ready.accept().unwrap();
    let mut byte = [0];
    std::io::Read::read_exact(&mut ready_stream, &mut byte).unwrap();
    host.state
        .native_agent_service()
        .expect("native owner")
        .resolve(&placed_record.target())
        .unwrap()
        .rpc("__allow_start", json!({}))
        .unwrap();
    let completed = record(&success(host.receive(&receiver, deadline).unwrap()).unwrap()).unwrap();
    assert_eq!(completed.id, placed_record.id);
    assert_eq!(
        completed.task_identity.as_deref(),
        Some("early-placement-id")
    );
}

#[rstest]
#[case::active(false)]
#[case::inactive(true)]
fn model_catalog_and_favorites_reuse_the_captured_provider_discovery(#[case] inactive: bool) {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(inactive).unwrap();
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
    let models = success(host.submit(query.clone()).unwrap()).unwrap();
    assert_eq!(models[0]["id"], "qa-model");
    let mut info = query.clone();
    info.command = "agents.native.catalog-info".into();
    assert_eq!(
        success(host.submit(info.clone()).unwrap()).unwrap()["models"],
        models
    );
    let target = info.target.as_mut().expect("catalog target");
    target.generation = target.generation.saturating_add(1);
    assert!(matches!(
        host.submit(info).unwrap(),
        CommandOutcome::StaleTarget { .. }
    ));
    bootty_test_support::write_executable(
        &host.program,
        "#!/usr/bin/env python3\nraise SystemExit(1)\n",
    )
    .unwrap();
    let mut favorite = query.clone();
    favorite.command = "agents.native.catalog-favorite".into();
    favorite.arguments.push("qa-model".into());
    for selected in [true, false] {
        let models = success(host.submit(favorite.clone()).unwrap()).unwrap();
        assert_eq!(models[0]["is_favorite"], selected);
    }
    assert_eq!(success(host.submit(query).unwrap()).unwrap(), models);
}

#[rstest]
fn an_existing_conversation_is_not_the_receipt_for_its_new_agent_tab() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let started = host
        .start(&binding, "creation-parent", "creation-parent-task", "")
        .unwrap();
    let parent = record(&started).unwrap();
    let mut open = CommandInvocation::from_action("new_tab", Caller::Keybinding);
    open.target = Some(parent.target());
    success(host.submit(open).unwrap()).unwrap();
    let request = host
        .state
        .pending_new_surface()
        .expect("surface request")
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
        .checked_add(FIXTURE_DEADLINE)
        .expect("deadline");
    while host.state.pending_new_surface().is_some() || host.state.modal_dialog().is_some() {
        host.tick();
        let _ = host.state.dialog_projection();
        if host.state.pending_new_surface().is_none() && host.state.modal_dialog().is_none() {
            break;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
    }
    host.tick();
    let selected = host
        .state
        .mux()
        .selected_session_anchor()
        .expect("selected pane");
    let selected_agent = selected
        .native_agent
        .as_deref()
        .expect("selected conversation");
    assert_ne!(selected_agent, parent.id);
    let records = host
        .state
        .native_agent_service()
        .expect("native owner")
        .sessions();
    assert!(
        records
            .iter()
            .any(|record| record.id == selected_agent
                && record.task_identity == parent.task_identity)
    );
}

fn pi_child_provider(directory: &Path) -> TestResult<String> {
    let program = directory.join("pi-child.py");
    bootty_test_support::write_executable(
        &program,
        r"#!/usr/bin/env python3
import json,os,sys
account=os.environ['PI_CODING_AGENT_DIR']
os.makedirs(account,exist_ok=True)
session='pi-child-'+str(os.getpid())
file=os.path.join(account,session+'.jsonl')
with open(file,'w') as output: output.write(json.dumps({'type':'session','id':session,'version':3})+'\n')
with open(os.path.join(account,'launch.json'),'w') as output: json.dump({'argv':sys.argv,'account':account},output)
messages=[]
def emit(value): print(json.dumps(value),flush=True)
for line in sys.stdin:
 value=json.loads(line);kind=value['type'];data={}
 if kind=='get_state': data={'sessionId':session,'sessionFile':file,'isStreaming':False,'isCompacting':False}
 elif kind=='get_commands': data={'commands':[{'name':'__bootty_checkpoint'}]}
 elif kind=='switch_session': data={'cancelled':False}
 elif kind=='get_messages': data={'messages':messages}
 elif kind=='get_available_models': data={'models':[]}
 elif kind=='prompt':
  if value['message']=='/__bootty_checkpoint': data={'disposition':'handled'}
  else:
   user={'role':'user','content':value['message'],'timestamp':1}
   answer={'role':'assistant','content':[{'type':'text','text':'Pi child completed'}],'timestamp':2,'stopReason':'stop'}
   emit({'type':'agent_start'})
   for message in [user,answer]: emit({'type':'message_end','message':message})
   messages.extend([user,answer]);emit({'type':'agent_end','messages':[answer],'willRetry':False});emit({'type':'agent_settled'});data={'disposition':'started'}
 emit({'type':'response','id':value['id'],'command':kind,'success':True,'data':data})
",
    )?;
    Ok(program.to_string_lossy().into_owned())
}

#[rstest]
fn native_child_uses_the_captured_other_provider_account_and_protocol() {
    let mut host = Host::new_with_child_provider(
        true,
        true,
        MultiplexerBackendConfig::Native,
        Some(AgentKind::Pi),
    )
    .unwrap();
    let binding = host.binding(false).unwrap();
    let created = host
        .start(
            &binding,
            "cross-provider-parent",
            "cross-provider-parent-id",
            "parent work",
        )
        .unwrap();
    let parent = record(&created).unwrap();
    let child = success(
        host.native_tool(
            &parent,
            "spawn_agent",
            json!({"name":"pi-reviewer","provider":"pi","prompt":"Review the implementation"}),
        )
        .unwrap(),
    )
    .unwrap();
    let child = host.spawned_record(&child).unwrap();
    assert_eq!(child.config.provider, AgentKind::Pi);
    assert_eq!(child.spawn_parent, Some(parent.target()));
    assert_eq!(child.config.profile.as_deref(), Some("child-account"));
    assert_eq!(child.config.cwd, parent.config.cwd);
    assert_ne!(child.config.program, parent.config.program);
    assert_ne!(
        child.config.account_directory,
        parent.config.account_directory
    );
    let account = host.directory.path().join("child-account");
    let launch: Value =
        serde_json::from_slice(&fs::read(account.join("launch.json")).unwrap()).unwrap();
    assert_eq!(launch["account"], account.to_string_lossy().as_ref());
    assert!(
        launch["argv"]
            .as_array()
            .expect("argv")
            .iter()
            .any(|argument| argument == "rpc")
    );
    let snapshot = host
        .state
        .native_agent_service()
        .expect("owner")
        .resolve(&child.target())
        .unwrap()
        .snapshot();
    assert!(
        snapshot
            .transcript
            .iter()
            .any(|item| item.text.contains("Pi child completed"))
    );
    host.state
        .native_agent_service()
        .expect("owner")
        .stop(&parent.target())
        .unwrap();
}

#[rstest]
fn app_access_is_answered_only_in_bootty_and_always_allow_is_saved() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let record = record(&host.start(&binding, "app-user", "app-user-id", "").unwrap()).unwrap();
    let target = record.target();
    // The first prompt binds the conversation's tools to it.
    success(
        host.submit(native_command(
            "agents.native.prompt",
            &target,
            &["hello"],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    let app = bootty_agents::NativeApplication {
        bundle_id: "dev.zed.Zed".into(),
        name: "Zed".into(),
    };
    let answer = host
        .state
        .native_agent_service()
        .expect("native owner missing")
        .request_application_access(&target, &app, Caller::Socket)
        .unwrap();
    let approve = |caller| {
        native_command(
            "agents.native.approve",
            &target,
            &["computer-access:dev.zed.Zed", "always-allow"],
            caller,
        )
    };

    // An agent's shell reaches Bootty as the CLI; it must not approve its own card.
    assert_eq!(
        host.submit(approve(Caller::Cli)).unwrap(),
        CommandOutcome::Denied {
            message: "Answer app access requests in Bootty".into()
        }
    );
    assert_eq!(
        answer.try_recv(),
        Err(mpsc::TryRecvError::Empty),
        "the card stays open"
    );
    assert_eq!(
        host.state.config().computer.always_allowed_apps,
        Vec::<String>::new()
    );

    success(host.submit(approve(Caller::Internal)).unwrap()).unwrap();
    assert_eq!(answer.try_recv(), Ok(true));
    assert_eq!(
        host.state.config().computer.always_allowed_apps,
        ["dev.zed.Zed"]
    );
}

/// A plan carried out in a new conversation starts beside its own, in the same session, with
/// the plan as its first message.
#[rstest]
fn plans_carry_out_in_a_new_conversation_of_the_same_session() {
    let mut host = Host::new(true).unwrap();
    let binding = host.binding(false).unwrap();
    let planned = record(
        &host
            .start(&binding, "conversation", "task-plan", "")
            .unwrap(),
    )
    .unwrap();
    let invocation =
        bootty_ui::commands::implement_plan_invocation(&planned, binding, "1. Fix the bug");
    let created = record(&success(host.submit(invocation).unwrap()).unwrap()).unwrap();
    assert_ne!(created.id, planned.id);
    assert_eq!(created.task_identity, planned.task_identity);
    assert_eq!(created.config.provider, planned.config.provider);
    let first = created.pending_initial_message.clone().or_else(|| {
        created
            .snapshot
            .transcript
            .iter()
            .find(|item| item.role == "user")
            .map(|item| item.text.clone())
    });
    assert_eq!(
        first.as_deref(),
        Some("Implement this plan:\n\n1. Fix the bug")
    );
}

/// Each turn keeps its files before and after: the turn's changes show as a diff, rewinding the
/// conversation can take the files back with it, and the files alone can go back too.
#[rstest]
fn turns_keep_their_files_and_rewind_with_them() {
    let mut host = Host::new(true).unwrap();
    let git = |args: &[&str]| -> TestResult<()> {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(host.directory.path())
            .args(args)
            .output()?;
        if status.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&status.stderr).into_owned().into())
        }
    };
    for args in [
        ["init", "--quiet"].as_slice(),
        &["config", "user.name", "Bootty Test"],
        &["config", "user.email", "test@example.invalid"],
        &["config", "commit.gpgsign", "false"],
        &["config", "core.hooksPath", "/dev/null"],
        &["add", "-A"],
        &["commit", "--quiet", "-m", "start"],
    ] {
        git(args).unwrap();
    }
    let binding = host.binding(false).unwrap();
    let conversation = record(
        &host
            .start(&binding, "conversation", "task-files", "write first")
            .unwrap(),
    )
    .unwrap();
    let target = conversation.target();
    let [(first, reply)] = checkpointed_turns(&mut host, &target).unwrap();
    let diff = success(
        host.submit(native_command(
            "agents.native.turn-diff",
            &target,
            &[&reply],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    let diff = diff["diff"].as_str().unwrap_or_default();
    assert!(
        diff.contains("+++ b/turn.txt") && diff.contains("+first"),
        "{diff}"
    );
    success(
        host.submit(native_command(
            "agents.native.prompt",
            &target,
            &["write second"],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    let [_, (second, _)] = checkpointed_turns(&mut host, &target).unwrap();
    success(
        host.submit(native_command(
            "agents.native.rewind",
            &target,
            &[&second, "files"],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    let service = host
        .state
        .native_agent_service()
        .expect("native owner missing");
    let shown = service
        .sessions()
        .into_iter()
        .find(|record| record.target() == target)
        .expect("conversation missing")
        .snapshot
        .transcript
        .iter()
        .filter(|item| matches!(item.role.as_str(), "user" | "assistant"))
        .map(|item| item.text.clone())
        .collect::<Vec<_>>();
    assert_eq!(shown, ["write first", "reply: write first"]);
    assert_eq!(
        fs::read_to_string(host.directory.path().join("turn.txt")).unwrap(),
        "first\n"
    );
    let calls = service
        .resolve(&target)
        .unwrap()
        .rpc("__calls", json!({}))
        .unwrap();
    assert_eq!(calls["reverted"], json!(["turn-2"]));
    // The provider and the transcript no longer have the message.
    let again = host
        .submit(native_command(
            "agents.native.rewind",
            &target,
            &[&second],
            Caller::Internal,
        ))
        .unwrap();
    assert!(
        format!("{again:?}").contains("That message is no longer in this conversation"),
        "{again:?}"
    );
    success(
        host.submit(native_command(
            "agents.native.rewind-files",
            &target,
            &[&first],
            Caller::Internal,
        ))
        .unwrap(),
    )
    .unwrap();
    assert!(!host.directory.path().join("turn.txt").exists());
}

/// Waits for the conversation's `N` turns to record their files before and after, and returns
/// each turn's prompt and last reply.
fn checkpointed_turns<const N: usize>(
    host: &mut Host,
    target: &CommandTarget,
) -> TestResult<[(String, String); N]> {
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .ok_or("deadline overflow")?;
    loop {
        host.tick();
        let snapshot = host
            .state
            .native_agent_service()
            .ok_or("native owner missing")?
            .sessions()
            .into_iter()
            .find(|record| record.target() == *target)
            .ok_or("conversation missing")?
            .snapshot;
        let recorded = |role: &str| {
            snapshot
                .transcript
                .iter()
                .filter(|item| item.role == role && item.checkpoint.is_some())
                .map(|item| item.id.clone())
                .collect::<Vec<_>>()
        };
        let (prompts, replies) = (recorded("user"), recorded("assistant"));
        let kept = format!(
            "{} prompts and {} replies kept their files",
            prompts.len(),
            replies.len()
        );
        let turns = prompts.into_iter().zip(replies).collect::<Vec<_>>();
        if let Ok(turns) = <[(String, String); N]>::try_from(turns) {
            return Ok(turns);
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| format!("only {kept} before the deadline"))?;
    }
}
