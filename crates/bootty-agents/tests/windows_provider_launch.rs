#![cfg(windows)]

use assert_fs::{TempDir, prelude::*};
use bootty_agents::{AgentKind, NativeAgentSession, NativeSessionConfig};
use pretty_assertions::assert_eq;

fn provider() -> Result<TempDir, Box<dyn std::error::Error>> {
    let root = TempDir::new()?;
    root.child("provider").write_str("#!/bin/sh\n")?;
    root.child("provider.cmd")
        .write_str("@echo off\r\n@python -u \"%~dp0provider.py\" %*\r\n")?;
    root.child("provider.py").write_str(r"
import json,os,pathlib,subprocess,sys,time
if sys.argv[1:] == ['--version']:
    child = subprocess.Popen([sys.executable, '-c', 'import time;time.sleep(60)'])
    pathlib.Path(__file__).with_name('version-child.json').write_text(json.dumps(child.pid))
    print('provider-fixture 1.0'); sys.exit(0)
if sys.argv[1:3] == ['auth','login']:
    print(json.dumps({'args':sys.argv[1:], 'account':os.environ.get('CLAUDE_CONFIG_DIR'), 'cwd':os.getcwd()})); sys.exit(0)
child = subprocess.Popen([sys.executable, '-c', 'import time;time.sleep(60)'], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path(os.environ['CODEX_HOME'], 'pids.json').write_text(json.dumps([os.getpid(), child.pid]))
for line in sys.stdin:
    request=json.loads(line)
    if 'id' not in request: continue
    result={}
    if request['method']=='account/read': result={'account':None,'requiresOpenaiAuth':True}
    if request['method']=='thread/start': result={'thread':{'id':'fixture-thread','turns':[]}}
    if request['method']=='__launch': result={'args':sys.argv[1:],'cwd':os.getcwd(),'account':os.environ['CODEX_HOME']}
    print(json.dumps({'id':request['id'],'result':result}), flush=True)
")?;
    Ok(root)
}

fn assert_processes_stopped(root: &TempDir) -> Result<(), Box<dyn std::error::Error>> {
    let pids: Vec<u32> = serde_json::from_slice(&std::fs::read(root.path().join("pids.json"))?)?;
    let inspector = rmux_os::process::ProcessInspector;
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(5))
        .ok_or("query deadline overflow")?;
    loop {
        if pids
            .iter()
            .map(|pid| inspector.is_live(*pid))
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .all(|live| *live == Some(false))
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("query descendants survived: {pids:?}").into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn provider_discovery_and_account_query_share_the_windows_launcher() {
    let root = provider().unwrap();
    let status = bootty_agents::terminal_provider_status(
        AgentKind::Codex,
        root.path().join("provider").to_str().unwrap(),
        Some(root.path().to_str().unwrap()),
        None,
    );
    assert_eq!(
        status.executable.unwrap().canonicalize().unwrap(),
        root.path().join("provider.cmd").canonicalize().unwrap()
    );
    assert_eq!(status.version.as_deref(), Some("provider-fixture 1.0"));
    assert_eq!(status.authenticated, Some(false));
    assert_eq!(status.message, None);
    assert_processes_stopped(&root).unwrap();
    let version_child: u32 =
        serde_json::from_slice(&std::fs::read(root.path().join("version-child.json")).unwrap())
            .unwrap();
    assert_eq!(
        rmux_os::process::ProcessInspector
            .is_live(version_child)
            .unwrap(),
        Some(false)
    );
}

#[test]
fn native_session_uses_the_same_launcher_and_owns_its_descendants() {
    let root = provider().unwrap();
    let mut config = NativeSessionConfig::new(AgentKind::Codex, root.path());
    config.program = root.path().join("provider").to_string_lossy().into_owned();
    config.account_directory = Some(root.path().to_string_lossy().into_owned());
    let session = NativeAgentSession::spawn(config).unwrap();
    assert_eq!(
        session.snapshot().session_id.as_deref(),
        Some("fixture-thread")
    );
    let launch = session.rpc("__launch", serde_json::json!({})).unwrap();
    assert_eq!(launch["account"], root.path().to_string_lossy().as_ref());
    assert_eq!(launch["args"][0], "app-server");
    session.stop();
    assert_processes_stopped(&root).unwrap();
}
