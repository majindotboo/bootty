#![cfg(unix)]
use anyhow::{Context, Result, bail, ensure};
use assert_fs::{TempDir, prelude::*};
use bootty_config::ApplicationIdentity;
use bootty_control::{
    AppCommandReceiver, AppCommandRequest, Caller, CommandCancellation, CommandDescriptor,
    CommandOutcome, ControlCatalog, ControlPlane, ControlServer, InstanceDescriptor, RpcResponse,
    app_command_channel, invoke_instance, running_instance,
};
use pretty_assertions::{assert_eq, assert_ne};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{Arc, Barrier, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

const HELPER: &str = "BOOTTY_CONTROL_TEST_HELPER";

#[derive(Default)]
struct TestCatalog {
    active: Mutex<Option<(String, u64)>>,
}

impl TestCatalog {
    fn activate(&self, module: &str, generation: u64) {
        *self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((module.to_owned(), generation));
    }
}

impl bootty_control::CommandCatalogSource for TestCatalog {
    fn list(&self) -> Vec<CommandDescriptor> {
        Vec::new()
    }

    fn describe(&self, _id: &str) -> Option<CommandDescriptor> {
        None
    }

    fn topics(&self) -> std::collections::BTreeSet<String> {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|_| std::iter::once("test.changed".to_owned()).collect())
            .unwrap_or_default()
    }

    fn with_active_topic(
        &self,
        module: &str,
        generation: u64,
        topic: &str,
        publish: &mut dyn FnMut(),
    ) -> Result<(), String> {
        let active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active.as_ref() == Some(&(module.to_owned(), generation)) && topic == "test.changed" {
            publish();
            Ok(())
        } else {
            Err("control event topic is not active".to_owned())
        }
    }
}

fn isolated(name: &str) -> Result<bool> {
    if std::env::var(HELPER).as_deref() == Ok(name) {
        return Ok(true);
    }
    let dir = TempDir::new()?;
    let out = Command::new(std::env::current_exe()?)
        .args(["--exact", name])
        .env(HELPER, name)
        .env("XDG_RUNTIME_DIR", dir.path())
        .env("RMUX_TMPDIR", dir.path())
        .output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    ensure!(
        out.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "{name} failed or ran zero tests\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(false)
}

struct ControlHarness {
    host: ControlServer,
    source: Arc<TestCatalog>,
    plane: ControlPlane,
    rx: AppCommandReceiver,
    instance: InstanceDescriptor,
}

impl ControlHarness {
    fn new() -> Result<Self> {
        let command: CommandDescriptor = serde_json::from_value(json!({
            "id":"control.read", "title":"control.read", "description":"", "mutation":"read", "arguments":{}
        }))?;
        let source = Arc::new(TestCatalog::default());
        let catalog = Arc::new(ControlCatalog::new(vec![command], source.clone()));
        let plane = ControlPlane::default();
        let (tx, rx) = app_command_channel(4, Arc::new(|| {}));
        let host = ControlServer::spawn(
            "test",
            tx.for_caller(Caller::Socket),
            Arc::clone(&catalog),
            &plane,
        )?;
        let instance = running_instance()?.context("running test instance")?;
        Ok(Self {
            host,
            source,
            plane,
            rx,
            instance,
        })
    }
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        let response = invoke_instance(&self.instance, method, params)?;
        response
            .result
            .with_context(|| format!("{method}: {:?}", response.error))
    }
    fn detached(&self) -> thread::JoinHandle<anyhow::Result<RpcResponse>> {
        let instance = self.instance.clone();
        thread::spawn(move || {
            invoke_instance(
                &instance,
                "command.invoke",
                json!({
                    "detached":true, "invocation":{"command":"control.read", "caller":"socket"}
                }),
            )
        })
    }
    fn recv(&self) -> Result<AppCommandRequest> {
        let started = Instant::now();
        loop {
            match self.rx.try_recv() {
                Ok(value) => return Ok(value),
                Err(mpsc::TryRecvError::Empty) if started.elapsed() < Duration::from_secs(2) => {
                    thread::yield_now();
                }
                Err(mpsc::TryRecvError::Empty) => bail!("request timed out"),
                Err(mpsc::TryRecvError::Disconnected) => bail!("request channel disconnected"),
            }
        }
    }
    fn subscribe(&self, topic: &str) -> Result<String> {
        self.call("event.subscribe", json!({"topics":[topic]}))?
            .get("subscription")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("subscription ID")
    }
}

#[test]
fn rpc_dispatch_and_task_cancellation() {
    if !isolated("rpc_dispatch_and_task_cancellation").expect("isolated test process") {
        return;
    }
    let s = ControlHarness::new().expect("control harness");
    assert_eq!(
        s.call("system.describe", Value::Null)
            .expect("successful RPC")["protocol"]["current"],
        1
    );
    assert_eq!(
        s.call("command.list", Value::Null).expect("successful RPC")[0]["id"],
        "control.read"
    );
    assert_eq!(
        s.call("command.describe", json!({"command":"control.read"}))
            .expect("successful RPC")["mutation"],
        "read"
    );
    let instance = s.instance.clone();
    let rpc = thread::spawn(move || {
        invoke_instance(
            &instance,
            "command.invoke",
            json!({
                "invocation":{"command":"control.read", "caller":"socket"}
            }),
        )
        .unwrap()
    });
    let request = s.recv().expect("received command");
    assert_eq!(request.invocation.caller, Caller::Socket);
    request
        .response
        .send(CommandOutcome::Success {
            value: json!(42),
            warnings: Vec::new(),
        })
        .unwrap();
    assert_eq!(
        rpc.join().unwrap().result.unwrap(),
        json!({"status":"success", "value":42})
    );

    let completed = s.subscribe("command.completed").expect("subscription");
    let task = s.detached();
    let request = s.recv().expect("received command");
    let id = task.join().unwrap().unwrap().result.unwrap()["task"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        s.call("task.status", json!({"task":id}))
            .expect("successful RPC")["task"]["state"]["status"],
        "running"
    );
    assert_eq!(
        s.call("task.cancel", json!({"task":id}))
            .expect("successful RPC")["task"]["state"]["status"],
        "cancelling"
    );
    assert!(request.cancellation.is_cancelled());
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .expect("test deadline");
    while s
        .call("task.status", json!({"task":id}))
        .expect("successful RPC")["task"]["state"]["status"]
        != "completed"
    {
        assert!(Instant::now() < deadline);
    }
    let events = s
        .call(
            "event.subscribe",
            json!({"subscription":completed, "cursor":0}),
        )
        .expect("successful RPC");
    assert_eq!(events["events"][0]["topic"], "command.completed");
}

#[test]
fn event_wait_timeout_cancellation_and_publication() {
    if !isolated("event_wait_timeout_cancellation_and_publication").expect("isolated test process")
    {
        return;
    }
    let s = ControlHarness::new().expect("control harness");
    let module = "test.luau";
    s.source.activate(module, 1);
    let expired = bootty_control::WaitRequest {
        invocation: bootty_control::CommandInvocation::from_action("control.read", Caller::Cli),
        topics: vec!["test.changed".to_owned()],
        pointer: String::new(),
        expected: Value::Null,
        deadline: Instant::now(),
    };
    assert!(matches!(
        bootty_control::wait_for_command(
            &s.instance,
            expired,
            &std::sync::atomic::AtomicBool::new(false)
        )
        .unwrap(),
        bootty_control::WaitOutcome::TimedOut { value: None }
    ));
    let cancelled = bootty_control::WaitRequest {
        invocation: bootty_control::CommandInvocation::from_action("control.read", Caller::Cli),
        topics: vec!["test.changed".to_owned()],
        pointer: String::new(),
        expected: Value::Null,
        deadline: Instant::now()
            .checked_add(Duration::from_secs(1))
            .expect("test deadline"),
    };
    assert!(matches!(
        bootty_control::wait_for_command(
            &s.instance,
            cancelled,
            &std::sync::atomic::AtomicBool::new(true)
        )
        .unwrap(),
        bootty_control::WaitOutcome::Cancelled { value: None }
    ));
    let waiting_subscription = s.subscribe("test.changed").expect("subscription");
    assert_eq!(
        s.call(
            "event.wait",
            json!({"subscription":waiting_subscription,"cursor":0,"timeout_ms":0})
        )
        .expect("successful RPC")["timed_out"],
        true
    );
    let instance = s.instance.clone();
    let waiting_id = waiting_subscription.clone();
    let waiter = thread::spawn(move || {
        invoke_instance(
            &instance,
            "event.wait",
            json!({"subscription":waiting_id,"cursor":0,"timeout_ms":4000}),
        )
        .unwrap()
    });
    s.plane
        .event_sender()
        .publish(
            module.to_owned(),
            1,
            "test.changed".to_owned(),
            json!("wake"),
            Instant::now()
                .checked_add(Duration::from_secs(1))
                .expect("test deadline"),
            &CommandCancellation::new(),
        )
        .unwrap();
    let batch = waiter.join().unwrap().result.unwrap();
    assert_eq!(batch["timed_out"], false);
    assert_eq!(batch["events"][0]["payload"], "wake");
    s.call(
        "event.unsubscribe",
        json!({"subscription":waiting_subscription}),
    )
    .expect("successful RPC");
}

#[test]
fn snapshot_wait_reconciles_after_publication() {
    if !isolated("snapshot_wait_reconciles_after_publication").expect("isolated test process") {
        return;
    }
    let s = ControlHarness::new().expect("control harness");
    let module = "test.luau";
    s.source.activate(module, 1);
    let instance = s.instance.clone();
    let condition = thread::spawn(move || {
        bootty_control::wait_for_command(
            &instance,
            bootty_control::WaitRequest {
                invocation: bootty_control::CommandInvocation::from_action(
                    "control.read",
                    Caller::Cli,
                ),
                topics: vec!["test.changed".to_owned()],
                pointer: "/status".to_owned(),
                expected: json!("ready"),
                deadline: Instant::now()
                    .checked_add(Duration::from_secs(2))
                    .expect("test deadline"),
            },
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap()
    });
    s.recv()
        .expect("received command")
        .response
        .send(CommandOutcome::Success {
            value: json!({"status":"pending"}),
            warnings: Vec::new(),
        })
        .unwrap();
    s.plane
        .event_sender()
        .publish(
            module.to_owned(),
            1,
            "test.changed".to_owned(),
            json!("ready"),
            Instant::now()
                .checked_add(Duration::from_secs(1))
                .expect("test deadline"),
            &CommandCancellation::new(),
        )
        .unwrap();
    s.recv()
        .expect("received command")
        .response
        .send(CommandOutcome::Success {
            value: json!({"status":"ready"}),
            warnings: Vec::new(),
        })
        .unwrap();
    assert!(
        matches!(condition.join().unwrap(), bootty_control::WaitOutcome::Matched {value} if value["status"] == "ready")
    );
}

#[test]
fn event_overflow_and_generation_rejection() {
    if !isolated("event_overflow_and_generation_rejection").expect("isolated test process") {
        return;
    }
    let s = ControlHarness::new().expect("control harness");
    let module = "test.luau";
    s.source.activate(module, 1);
    let n = 65;
    let subscription = s.subscribe("test.changed").expect("subscription");
    let sender = s.plane.event_sender();
    let cancellation = CommandCancellation::new();
    for sequence in 0..n {
        sender
            .publish(
                module.to_owned(),
                1,
                "test.changed".into(),
                json!(sequence),
                Instant::now()
                    .checked_add(Duration::from_secs(5))
                    .expect("test deadline"),
                &cancellation,
            )
            .unwrap();
    }
    s.source.activate(module, 2);
    assert!(
        sender
            .publish(
                module.to_owned(),
                1,
                "test.changed".into(),
                Value::Null,
                Instant::now()
                    .checked_add(Duration::from_secs(5))
                    .expect("test deadline"),
                &cancellation
            )
            .is_err()
    );
    let error = invoke_instance(
        &s.instance,
        "event.subscribe",
        json!({"subscription":subscription, "cursor":0}),
    )
    .unwrap()
    .error
    .unwrap();
    assert_eq!(
        (error.code, error.data.unwrap()["sequence"].clone()),
        (-32005, json!(n))
    );
    let pending = s.detached();
    let request = s.recv().expect("received command");
    drop(s.host);
    assert!(request.cancellation.is_cancelled());
    let _ = pending.join();
}

fn singleton() -> anyhow::Result<ControlServer> {
    let (tx, _rx) = app_command_channel(1, Arc::new(|| {}));
    ControlServer::spawn(
        "main",
        tx.for_caller(Caller::Socket),
        Arc::new(ControlCatalog::new(
            Vec::new(),
            Arc::new(TestCatalog::default()),
        )),
        &ControlPlane::default(),
    )
}
fn path() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("test runtime directory")?)
            .join(ApplicationIdentity::current().cli_name())
            .join("control.json"),
    )
}
fn descriptor() -> Result<InstanceDescriptor> {
    Ok(serde_json::from_slice(&fs::read(path()?)?)?)
}

#[test]
fn singleton_behaviors() {
    if !isolated("singleton_behaviors").expect("isolated test process") {
        return;
    }
    let first = singleton().unwrap();
    let old = descriptor().expect("instance descriptor");
    invoke_instance(&old, "instance.describe", Value::Null).unwrap();
    assert!(singleton().is_err());
    let mut stale = old.clone();
    stale.started_at_ms = stale
        .started_at_ms
        .checked_add(1000)
        .expect("stale timestamp");
    assert_fs::fixture::ChildPath::new(path().expect("descriptor path"))
        .write_binary(&serde_json::to_vec(&stale).unwrap())
        .unwrap();
    let replacement = singleton().unwrap();
    let current = descriptor().expect("instance descriptor");
    assert_ne!(
        (current.generation, &current.endpoint),
        (old.generation, &old.endpoint)
    );
    drop(first);
    assert_eq!(descriptor().expect("instance descriptor"), current);
    invoke_instance(&current, "instance.describe", Value::Null).unwrap();
    drop(replacement);

    let path = path().expect("descriptor path");
    assert_fs::fixture::ChildPath::new(path.parent().unwrap())
        .create_dir_all()
        .unwrap();
    assert_fs::fixture::ChildPath::new(path)
        .write_binary(b"bad")
        .unwrap();
    let recovered = singleton().unwrap();
    assert_eq!(
        descriptor().expect("instance descriptor").instance_id,
        ApplicationIdentity::current().cli_name()
    );
    drop(recovered);
    let other = if ApplicationIdentity::current().cli_name() == "bootty" {
        ApplicationIdentity::Development.cli_name()
    } else {
        "bootty"
    };
    let marker = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join(other)
        .join("control.json");
    assert_fs::fixture::ChildPath::new(marker.parent().unwrap())
        .create_dir_all()
        .unwrap();
    assert_fs::fixture::ChildPath::new(&marker)
        .write_binary(b"other")
        .unwrap();
    let server = singleton().unwrap();
    assert_eq!(fs::read(marker).unwrap(), b"other");
    drop(server);

    let barrier = Arc::new(Barrier::new(3));
    let contenders = [(); 2].map(|()| {
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            singleton()
        })
    });
    barrier.wait();
    let winners = contenders
        .into_iter()
        .flat_map(|handle| handle.join().unwrap())
        .count();
    assert_eq!(winners, 1);
}
