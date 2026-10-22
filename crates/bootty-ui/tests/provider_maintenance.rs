use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use assert_fs::{TempDir, prelude::*};
use bootty_agents::{AgentKind, NativeModelOption, NativeSessionConfig, ProviderUse};
use bootty_config::config::{AgentProfileConfig, BoottyConfig, MultiplexerBackendConfig};
use bootty_control::{Caller, CommandCancellation, CommandInvocation, CommandOutcome};
use bootty_mux::provider::MuxBackendRegistry;
use bootty_ui::AppState;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[path = "support/idle_frames.rs"]
mod frames;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const DEADLINE: Duration = Duration::from_secs(30);

struct Installation {
    program: String,
    updates: PathBuf,
    prefix: PathBuf,
}

fn installation(directory: &TempDir) -> TestResult<Installation> {
    let prefix = directory.child("npm installation");
    let (package, program, manager) = if cfg!(windows) {
        (
            prefix.child("node_modules/@openai/codex"),
            prefix.child("codex.cmd"),
            prefix.child("updater.py"),
        )
    } else {
        (
            prefix.child("lib/node_modules/@openai/codex"),
            prefix.child("lib/node_modules/@openai/codex/bin/codex.js"),
            prefix.child("bin/npm"),
        )
    };
    package
        .child("package.json")
        .write_str(r#"{"name":"@openai/codex","bin":{"codex":"bin/codex.js"}}"#)?;
    let provider_script = r"#!/usr/bin/env python3
import json,sys
if sys.argv[1:] == ['--version']:
    print('fixture 2.0'); sys.exit(0)
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request: continue
    result = {'account': None, 'requiresOpenaiAuth': True} if request.get('method') == 'account/read' else {}
    print(json.dumps({'id': request['id'], 'result': result}), flush=True)
";
    bootty_test_support::write_executable(package.child("bin/codex.js").path(), provider_script)?;
    let updater_script = r"#!/usr/bin/env python3
import json,pathlib,sys
with pathlib.Path(__file__).with_name('updates.jsonl').open('a', encoding='utf-8') as output:
    output.write(json.dumps(sys.argv[1:]) + '\n')
";
    bootty_test_support::write_executable(manager.path(), updater_script)?;
    if cfg!(windows) {
        program.write_str(
            "@echo off\r\n@python -u \"%~dp0\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n",
        )?;
        prefix
            .child("npm.cmd")
            .write_str("@echo off\r\n@python -u \"%~dp0updater.py\" %*\r\n")?;
    }
    Ok(Installation {
        program: program.path().to_string_lossy().into_owned(),
        updates: manager.path().with_file_name("updates.jsonl"),
        prefix: if cfg!(unix) {
            prefix.path().canonicalize()?
        } else {
            prefix.path().to_owned()
        },
    })
}

struct Host {
    state: AppState,
    wakes: mpsc::Receiver<()>,
    program: String,
    account: String,
    catalog: NativeSessionConfig,
    updates: PathBuf,
    prefix: PathBuf,
    _directory: TempDir,
}

impl Host {
    fn new(auto_update: bool, provider_enabled: bool) -> TestResult<Self> {
        let directory = TempDir::new()?;
        let Installation {
            program,
            updates,
            prefix,
        } = installation(&directory)?;
        let account = directory.child("account");
        account.create_dir_all()?;
        let account = account.path().to_string_lossy().into_owned();
        let mut config = BoottyConfig {
            config_path: directory.path().join("config.toml"),
            ..BoottyConfig::default()
        };
        config.multiplexer.backend = MultiplexerBackendConfig::Native;
        config.session.working_directory = Some(directory.path().to_owned());
        config.agents.auto_update = auto_update;
        config.agents.codex.enabled = provider_enabled;
        config.agents.codex.program.clone_from(&program);
        "fixture".clone_into(&mut config.agents.codex.selected);
        config.agents.codex.profiles = BTreeMap::from([(
            "fixture".into(),
            AgentProfileConfig {
                name: "Fixture".into(),
                directory: Some(account.clone()),
                arguments: Vec::new(),
            },
        )]);
        config.agents.claude.enabled = false;
        config.agents.pi.enabled = false;
        let missing = directory
            .path()
            .join("missing-provider")
            .to_string_lossy()
            .into_owned();
        config.agents.claude.program.clone_from(&missing);
        config.agents.pi.program = missing;
        let (wake, wakes) = mpsc::channel();
        let (events, _receiver) = bootty_control::event_queue();
        let state = AppState::new_for_window_with_agents(
            config,
            "provider-update-fixture".into(),
            Arc::new(MuxBackendRegistry::desktop()?),
            Arc::new(move || {
                let _ = wake.send(());
            }),
            None,
            None,
            Some(events),
        )?;
        let mut catalog = NativeSessionConfig::new(AgentKind::Codex, directory.path());
        catalog.program.clone_from(&program);
        catalog.account_directory = Some(account.clone());
        state
            .native_agent_service()
            .ok_or("native service missing")?
            .cache_model_catalog(
                &catalog,
                &[NativeModelOption {
                    id: "before-update".into(),
                    display_name: "Before update".into(),
                    reasoning_efforts: Vec::new(),
                    default_reasoning_effort: None,
                    is_default: true,
                    is_legacy: false,
                    is_favorite: false,
                    supports_fast_mode: false,
                    fast_mode_detail: None,
                }],
            )?;
        Ok(Self {
            state,
            wakes,
            program,
            account,
            catalog,
            updates,
            prefix,
            _directory: directory,
        })
    }

    fn frame(&mut self, now: Instant) {
        self.state.update_frame(frames::idle_frame(now));
    }

    fn submit(
        &mut self,
        now: Instant,
        invocation: CommandInvocation,
    ) -> TestResult<CommandOutcome> {
        let deadline = Instant::now()
            .checked_add(DEADLINE)
            .ok_or("deadline overflow")?;
        let response = self
            .state
            .app_command_sender(invocation.caller)
            .submit(invocation, deadline, CommandCancellation::new())
            .map_err(|error| format!("command submission failed: {error:?}"))?;
        loop {
            self.frame(now);
            match response.try_recv() {
                Ok(outcome) => return Ok(outcome),
                Err(mpsc::TryRecvError::Disconnected) => return Err("command disconnected".into()),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            self.wait(deadline)?;
        }
    }

    fn wait(&self, deadline: Instant) -> TestResult {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("provider update fixture timed out".into());
        }
        match self
            .wakes
            .recv_timeout(remaining.min(Duration::from_millis(10)))
        {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn cached(&self) -> bool {
        self.state
            .native_agent_service()
            .is_some_and(|service| service.cached_provider_catalog(&self.catalog).is_some())
    }

    fn await_update(&mut self, start: Instant, clock_rate: u32) -> TestResult {
        let started = Instant::now();
        let deadline = started.checked_add(DEADLINE).ok_or("deadline overflow")?;
        loop {
            let now = start
                .checked_add(started.elapsed().saturating_mul(clock_rate))
                .ok_or("frame clock overflow")?;
            self.frame(now);
            let inspected = self.state.terminal_agent_service().and_then(|service| {
                service.provider_status(AgentKind::Codex, &self.program, Some(&self.account), None)
            });
            if !self.cached()
                && inspected.is_some_and(|status| {
                    status.version.as_deref() == Some("fixture 2.0")
                        && status.authenticated == Some(false)
                })
            {
                // Publish the completed worker reply through the shared mailbox too.
                self.frame(now);
                return Ok(());
            }
            self.wait(deadline)?;
        }
    }

    fn updates(&self) -> TestResult<Vec<Vec<String>>> {
        match std::fs::read_to_string(&self.updates) {
            Ok(text) => text
                .lines()
                .map(|line| serde_json::from_str(line).map_err(Into::into))
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }

    fn shutdown(&self) -> TestResult {
        if let Some(service) = self.state.terminal_agent_service() {
            service.shutdown_and_wait()?;
        }
        if let Some(service) = self.state.native_agent_service() {
            service.shutdown()?;
        }
        Ok(())
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn update_command() -> CommandInvocation {
    CommandInvocation::new(
        "agents.providers.update",
        vec!["codex".into()],
        Caller::Socket,
    )
}

#[rstest]
#[case::automatic_updates_disabled(false, true)]
#[case::provider_disabled(true, false)]
fn disabled_updates_preserve_the_installation_and_catalog(
    #[case] auto_update: bool,
    #[case] provider_enabled: bool,
) {
    let mut host = Host::new(auto_update, provider_enabled).unwrap();
    let now = Instant::now();
    for offset in [0, 61, 86_461] {
        let now = now.checked_add(Duration::from_secs(offset)).unwrap();
        let outcome = host
            .submit(
                now,
                CommandInvocation::new("resource.current", vec!["binding".into()], Caller::Socket),
            )
            .unwrap();
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
    assert_eq!(host.updates().unwrap(), Vec::<Vec<String>>::new());
    assert!(host.cached());
    host.shutdown().unwrap();
}

#[test]
fn idle_updates_run_after_the_initial_delay_and_invalidate_cached_models() {
    let mut host = Host::new(true, true).unwrap();
    let now = Instant::now();
    host.frame(now);
    host.frame(now.checked_add(Duration::from_secs(59)).unwrap());
    assert_eq!(host.updates().unwrap(), Vec::<Vec<String>>::new());
    assert!(host.cached());
    host.await_update(now.checked_add(Duration::from_secs(61)).unwrap(), 0)
        .unwrap();
    assert_eq!(
        host.updates().unwrap(),
        vec![vec![
            "install".to_owned(),
            "--global".into(),
            "--prefix".into(),
            host.prefix.to_string_lossy().into_owned(),
            "@openai/codex@latest".into(),
        ]]
    );
    assert!(!host.cached());
    host.shutdown().unwrap();
}

#[test]
fn busy_provider_keeps_its_catalog_and_updates_after_it_becomes_idle() {
    let mut host = Host::new(true, true).unwrap();
    let busy = ProviderUse::acquire(&host.program).unwrap().unwrap();
    let now = Instant::now();
    host.frame(now);
    let due = now.checked_add(Duration::from_secs(61)).unwrap();
    let outcome = host.submit(due, update_command()).unwrap();
    assert!(matches!(outcome, CommandOutcome::Failed { code, .. } if code == "provider_busy"));
    assert_eq!(host.updates().unwrap(), Vec::<Vec<String>>::new());
    assert!(host.cached());
    // The automatic attempt shares the explicit command's worker and busy guard.
    host.frame(due);
    host.frame(due);
    drop(busy);
    // Advance one scheduler minute per real second while waiting. A pending busy
    // reply can arrive on any frame; it must retry within minutes, not the daily interval.
    host.await_update(due, 60).unwrap();
    assert_eq!(host.updates().unwrap().len(), 1);
    assert!(!host.cached());
    host.shutdown().unwrap();
}

#[test]
fn failed_update_preserves_the_catalog_and_releases_the_installation() -> TestResult {
    let mut host = Host::new(false, true).unwrap();
    let updater = host
        .updates
        .with_file_name(if cfg!(windows) { "updater.py" } else { "npm" });
    bootty_test_support::write_executable(
        updater,
        "#!/usr/bin/env python3\nimport sys\nprint('fixture update failed', file=sys.stderr)\nsys.exit(7)\n",
    )
    .unwrap();
    let service = host.state.native_agent_service().unwrap();
    let before = service.cached_provider_catalog(&host.catalog).unwrap();

    let outcome = host.submit(Instant::now(), update_command()).unwrap();
    let CommandOutcome::Failed { code, message } = outcome else {
        return Err(format!("expected updater failure, received {outcome:?}").into());
    };
    assert_eq!(code, "provider_update_failed");
    if !message.contains("Some(7)") || !message.contains("fixture update failed") {
        return Err(
            format!("Installer failure lost its exit status or diagnostic: {message}").into(),
        );
    }
    assert_eq!(service.cached_provider_catalog(&host.catalog), Some(before));

    let available = ProviderUse::acquire(&host.program).unwrap().unwrap();
    drop(available);
    host.shutdown().unwrap();
    Ok(())
}
