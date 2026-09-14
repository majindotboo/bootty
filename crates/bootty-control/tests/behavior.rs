#![cfg(unix)]
use assert_fs::{TempDir, prelude::*};
use bootty_control::{
    AppCommandReceiver, AppCommandRequest, Caller, CommandCancellation, CommandDescriptor,
    CommandOutcome, ControlCatalog, ControlPlane, ControlServer, InstanceDescriptor, RpcResponse,
    app_command_channel, invoke_instance, running_instance,
};
use bootty_identity::ApplicationIdentity;
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
        *self.active.lock().unwrap() = Some((module.to_owned(), generation));
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
            .unwrap()
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
        let active = self.active.lock().unwrap();
        if active.as_ref() == Some(&(module.to_owned(), generation)) && topic == "test.changed" {
            publish();
            Ok(())
        } else {
            Err("control event topic is not active".to_owned())
        }
    }
}

fn isolated(kind: &str) {
    let dir = TempDir::new().unwrap();
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "isolated_helper"])
        .env(HELPER, kind)
        .env("XDG_RUNTIME_DIR", dir.path())
        .env("RMUX_TMPDIR", dir.path());
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "{kind} failed or ran zero tests\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn singleton_lifecycle_recovery_and_concurrency() {
    isolated("singleton");
}

#[test]
fn rpc_task_generation_and_ring_overflow() {
    isolated("rpc");
}

#[test]
fn isolated_helper() {
    match std::env::var(HELPER).as_deref() {
        Ok("rpc") => rpc_and_events(65),
        Ok("singleton") => singleton_behaviors(),
        Ok(value) => panic!("unknown helper {value}"),
        Err(_) => {}
    }
}

struct S {
    host: ControlServer,
    source: Arc<TestCatalog>,
    plane: ControlPlane,
    rx: AppCommandReceiver,
    instance: InstanceDescriptor,
}

impl S {
    fn new() -> Self {
        let command: CommandDescriptor = serde_json::from_value(json!({
            "id":"control.read", "title":"control.read", "description":"", "mutation":"read", "arguments":{}
        })).unwrap();
        let source = Arc::new(TestCatalog::default());
        let catalog = Arc::new(ControlCatalog::new(vec![command], source.clone()));
        let plane = ControlPlane::default();
        let (tx, rx) = app_command_channel(4, Arc::new(|| {}));
        let host = ControlServer::spawn(
            "test",
            tx.for_caller(Caller::Socket),
            Arc::clone(&catalog),
            &plane,
        )
        .unwrap();
        Self {
            host,
            source,
            plane,
            rx,
            instance: running_instance().unwrap().unwrap(),
        }
    }
    fn call(&self, method: &str, params: Value) -> Value {
        invoke_instance(&self.instance, method, params)
            .unwrap()
            .result
            .unwrap()
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
    fn recv(&self) -> AppCommandRequest {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.rx.try_recv() {
                Ok(value) => return value,
                Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => thread::yield_now(),
                Err(mpsc::TryRecvError::Empty) => panic!("request timed out"),
                Err(mpsc::TryRecvError::Disconnected) => panic!("request channel disconnected"),
            }
        }
    }
    fn subscribe(&self, topic: &str) -> String {
        self.call("event.subscribe", json!({"topics":[topic]}))["subscription"]
            .as_str()
            .unwrap()
            .into()
    }
}

fn rpc_and_events(n: usize) {
    let s = S::new();
    assert_eq!(
        s.call("system.describe", Value::Null)["protocol"]["current"],
        1
    );
    assert_eq!(s.call("command.list", Value::Null)[0]["id"], "control.read");
    assert_eq!(
        s.call("command.describe", json!({"command":"control.read"}))["mutation"],
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
    let request = s.recv();
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

    let completed = s.subscribe("command.completed");
    let task = s.detached();
    let request = s.recv();
    let id = task.join().unwrap().unwrap().result.unwrap()["task"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        s.call("task.status", json!({"task":id}))["task"]["state"]["status"],
        "running"
    );
    assert_eq!(
        s.call("task.cancel", json!({"task":id}))["task"]["state"]["status"],
        "cancelling"
    );
    assert!(request.cancellation.is_cancelled());
    let deadline = Instant::now() + Duration::from_secs(2);
    while s.call("task.status", json!({"task":id}))["task"]["state"]["status"] != "completed" {
        assert!(Instant::now() < deadline);
    }
    let events = s.call(
        "event.subscribe",
        json!({"subscription":completed, "cursor":0}),
    );
    assert_eq!(events["events"][0]["topic"], "command.completed");

    let module = "test.luau";
    s.source.activate(module, 1);
    let subscription = s.subscribe("test.changed");
    let sender = s.plane.event_sender();
    let cancellation = CommandCancellation::new();
    for sequence in 0..n {
        sender
            .publish(
                module.to_owned(),
                1,
                "test.changed".into(),
                json!(sequence),
                Instant::now() + Duration::from_secs(5),
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
                Instant::now() + Duration::from_secs(5),
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
    let request = s.recv();
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
fn path() -> PathBuf {
    PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join(ApplicationIdentity::current().cli_name())
        .join("control.json")
}
fn descriptor() -> InstanceDescriptor {
    serde_json::from_slice(&fs::read(path()).unwrap()).unwrap()
}

fn singleton_behaviors() {
    let first = singleton().unwrap();
    let old = descriptor();
    invoke_instance(&old, "instance.describe", Value::Null).unwrap();
    assert!(singleton().is_err());
    let mut stale = old.clone();
    stale.started_at_ms += 1000;
    assert_fs::fixture::ChildPath::new(path())
        .write_binary(&serde_json::to_vec(&stale).unwrap())
        .unwrap();
    let replacement = singleton().unwrap();
    let current = descriptor();
    assert_ne!(
        (current.generation, &current.endpoint),
        (old.generation, &old.endpoint)
    );
    drop(first);
    assert_eq!(descriptor(), current);
    invoke_instance(&current, "instance.describe", Value::Null).unwrap();
    drop(replacement);

    let path = path();
    assert_fs::fixture::ChildPath::new(path.parent().unwrap())
        .create_dir_all()
        .unwrap();
    assert_fs::fixture::ChildPath::new(path)
        .write_binary(b"bad")
        .unwrap();
    let recovered = singleton().unwrap();
    assert_eq!(
        descriptor().instance_id,
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
    let contenders = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                singleton()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let winners = contenders
        .into_iter()
        .flat_map(|handle| handle.join().unwrap())
        .count();
    assert_eq!(winners, 1);
}
