#![cfg(unix)]

use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead as _, BufReader, Read as _},
    os::unix::fs::PermissionsExt as _,
    path::Path,
    process::{Child, Command, Stdio},
    sync::Arc,
};

use bootty_agents::{AgentKind, AgentLaunch, TerminalAgentService};
use bootty_control::{CommandTarget, ResourceKind};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

type FixtureResult<T> = Result<T, Box<dyn std::error::Error>>;

struct QueryFixture {
    directory: assert_fs::TempDir,
    program: String,
    ready: File,
}

struct BackendFixture(Child);

impl Drop for BackendFixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn quoted(path: &Path) -> FixtureResult<String> {
    let text = path.to_str().ok_or("Fixture path is not UTF-8")?;
    Ok(format!("'{}'", text.replace('\'', "'\\''")))
}

#[fixture]
fn query_fixture() -> FixtureResult<QueryFixture> {
    let directory = assert_fs::TempDir::new()?;
    let ready = directory.path().join("ready");
    let blocked = directory.path().join("blocked");
    let witness = directory.path().join("witness");
    if !Command::new("mkfifo")
        .args([&ready, &blocked, &witness])
        .status()?
        .success()
    {
        return Err("Could not create query coordination pipes".into());
    }
    let ready_pipe = OpenOptions::new().read(true).write(true).open(&ready)?;
    let program = directory.path().join("provider");
    fs::write(
        &program,
        format!(
            "#!/bin/sh\n\
             [ \"$*\" = 'agents --json --all' ] || exit 9\n\
             exec 3> {}\n\
             printf '%s\\n' \"$$\" > {}\n\
             exec /bin/cat {}\n",
            quoted(&witness)?,
            quoted(&ready)?,
            quoted(&blocked)?,
        ),
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
    let program = program
        .to_str()
        .ok_or("Fixture program path is not UTF-8")?
        .to_owned();
    Ok(QueryFixture {
        directory,
        program,
        ready: ready_pipe,
    })
}

#[rstest]
fn worker_shutdown_reaps_owned_query_and_closes_pipes_without_stopping_backend(
    query_fixture: FixtureResult<QueryFixture>,
) {
    let fixture = query_fixture.unwrap();
    let catalog = fixture.directory.path().join("catalog.json");
    let service = Arc::new(TerminalAgentService::open(&catalog).unwrap());
    let target = CommandTarget {
        kind: ResourceKind::Terminal,
        handle: "fixture-interactive-terminal".to_owned(),
        generation: 7,
    };
    let prepared = service
        .prepare(
            AgentKind::Claude,
            AgentLaunch {
                program: fixture.program,
                cwd: None,
                arguments: Vec::new(),
                ephemeral: false,
                account_directory: None,
            },
        )
        .unwrap();
    let record = service
        .register(prepared, target.clone(), "fixture-binding".to_owned())
        .unwrap();
    let saved = fs::read(&catalog).unwrap();
    // The query owns this FIFO writer. Opening the reader and receiving its PID coordinate
    // teardown with the running query, without waiting on a clock or releasing its blocked read.
    let mut witness = File::open(fixture.directory.path().join("witness")).unwrap();
    let mut pid = String::new();
    BufReader::new(fixture.ready).read_line(&mut pid).unwrap();
    assert!(pid.trim().parse::<u32>().unwrap() > 0);
    let mut backend = BackendFixture(
        Command::new("/bin/cat")
            .arg(fixture.directory.path().join("blocked"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    service.shutdown_and_wait().unwrap();

    let query_alive = Command::new("/bin/kill")
        .args(["-0", pid.trim()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!query_alive, "The owned query must be reaped before return");
    let mut bytes = Vec::new();
    assert_eq!(witness.read_to_end(&mut bytes).unwrap(), 0);
    assert_eq!(backend.0.try_wait().unwrap(), None);
    assert_eq!(
        serde_json::to_value(service.record(&target)).unwrap(),
        serde_json::to_value(Some(record)).unwrap()
    );
    assert_eq!(fs::read(catalog).unwrap(), saved);
    service.shutdown_and_wait().unwrap();
}
