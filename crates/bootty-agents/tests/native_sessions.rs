#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use assert_fs::TempDir;
use bootty_agents::{
    AgentKind, NativeAgentService, NativeAgentSession, NativeSessionConfig, NativeSessionStatus,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn fixture(root: &Path) -> std::io::Result<String> {
    let path = root.join("provider.py");
    fs::write(
        &path,
        r"#!/usr/bin/env python3
import json,sys
provider = 'codex' if 'app-server' in sys.argv else 'claude' if '--input-format' in sys.argv else 'pi'
turn = 0
def emit(value): print(json.dumps(value),flush=True)
for line in sys.stdin:
 value=json.loads(line)
 operation=value.get('method',value.get('type'))
 if provider == 'codex':
  ident=value.get('id')
  if ident is None: continue
  if operation == 'initialize': result={}
  elif operation in ['thread/start','thread/resume']: result={'thread':{'id':value.get('params',{}).get('threadId','native-thread'),'turns':[]}}
  elif operation == 'turn/start':
   turn += 1
   user={'id':'provider-user-'+str(turn),'type':'userMessage','content':value['params']['input']}
   emit({'method':'item/started','params':{'item':user}})
   emit({'method':'item/completed','params':{'item':user}})
   emit({'method':'turn/started','params':{'turn':{'id':'turn'}}})
   emit({'method':'item/agentMessage/delta','params':{'itemId':'answer','delta':'hello'}})
   emit({'method':'item/completed','params':{'item':{'id':'answer','type':'agentMessage','text':'hello'}}})
   emit({'method':'turn/completed','params':{'turn':{'status':'completed'}}})
   result={}
  elif operation == 'account/read': result={'account':{'type':'chatgpt','email':'test@example.com'}}
  else: result={}
  emit({'id':ident,'result':result})
 elif provider == 'claude':
  if operation == 'control_request': emit({'type':'control_response','response':{'request_id':value['request_id'],'subtype':'success','response':{'session_id':'native-thread'}}})
  elif operation == 'user':
   emit({'type':'assistant','session_id':'native-thread','message':{'id':'answer','content':[{'type':'text','text':'hello'}]}})
   emit({'type':'result','session_id':'native-thread','is_error':False,'usage':{}})
 elif provider == 'pi':
  if operation == 'prompt':
   emit({'type':'message_end','message':{'role':'assistant','content':[{'type':'text','text':'hello'}]}})
   emit({'type':'agent_end'})
  elif operation == 'get_state': emit({'id':value['id'],'type':'response','success':True,'data':{'sessionId':'native-thread','sessionFile':str(__file__)}})
  else: emit({'id':value.get('id'),'type':'response','success':True,'data':{'messages':[]}})
",
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path.to_string_lossy().into_owned())
}

fn config(provider: AgentKind, root: &Path) -> std::io::Result<NativeSessionConfig> {
    let mut config = NativeSessionConfig::new(provider, root);
    config.program = fixture(root)?;
    Ok(config)
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn native_process_owns_handshake_and_stops_explicitly(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(provider, root.path()).unwrap()).unwrap();
    assert_eq!(
        session.snapshot().session_id.as_deref(),
        Some("native-thread")
    );
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    session.stop();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Stopped);
    assert!(session.send_prompt("hello").is_err());
}

#[rstest]
fn completed_turn_cannot_be_overwritten_by_late_dispatch_acknowledgement() {
    let root = TempDir::new().unwrap();
    let session =
        NativeAgentSession::spawn(config(AgentKind::Codex, root.path()).unwrap()).unwrap();
    session.send_prompt("hello").unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Idle);
    assert_eq!(
        snapshot
            .transcript
            .iter()
            .find(|item| item.role == "assistant")
            .unwrap()
            .text,
        "hello"
    );
    assert_eq!(
        session.account_status().unwrap()["account"]["email"],
        "test@example.com"
    );
}

#[rstest]
fn provider_user_echo_replaces_only_its_dispatch_and_preserves_repeated_prompts() {
    let root = TempDir::new().unwrap();
    let session =
        NativeAgentSession::spawn(config(AgentKind::Codex, root.path()).unwrap()).unwrap();
    session.send_prompt("same prompt").unwrap();
    session.send_prompt("same prompt").unwrap();
    let snapshot = session.snapshot();
    let messages = snapshot
        .transcript
        .iter()
        .filter(|item| item.role == "user")
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].text, "same prompt");
    assert_eq!(messages[1].text, "same prompt");
    assert_ne!(messages[0].id, messages[1].id);
}

#[rstest]
fn registry_persists_identity_and_rejects_retired_process_generations() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let first = service
        .create(
            "binding",
            "Coding",
            config(AgentKind::Codex, root.path()).unwrap(),
        )
        .unwrap();
    let target = first.target();
    assert!(service.resolve(&target).is_ok());
    service.stop(&target).unwrap();
    assert!(service.resolve(&target).is_err());
    let second = service.resume(&first.target()).unwrap();
    assert_eq!(second.id, first.id);
    assert!(second.generation > first.generation);
    assert!(service.resolve(&target).is_err());
    service.stop(&second.target()).unwrap();
    assert!(service.resume(&first.target()).is_err());
    assert!(service.remove(&first.target()).is_err());
    assert!(service.rename(&first.target(), "Retired title").is_err());
    assert_eq!(service.sessions().len(), 1);
    drop(service);
    let restored = NativeAgentService::open(path).unwrap();
    assert_eq!(
        restored.sessions()[0].snapshot.status,
        NativeSessionStatus::Stopped
    );
    let third = restored.resume(&second.target()).unwrap();
    assert!(third.generation > second.generation);
    assert!(restored.resolve(&second.target()).is_err());
}

#[rstest]
#[case(true)]
#[case(false)]
fn pi_information_never_becomes_an_approval_and_confirm_uses_native_boolean(#[case] allow: bool) {
    let root = TempDir::new().unwrap();
    let provider = root.path().join("pi.py");
    fs::write(&provider, r"#!/usr/bin/env python3
import json,sys
confirmed=None
def emit(value): print(json.dumps(value),flush=True)
for line in sys.stdin:
 request=json.loads(line)
 method=request.get('type')
 if method=='extension_ui_response':
  confirmed=request.get('confirmed')
  emit({'type':'agent_end'})
  continue
 if method=='get_messages':
  for i in range(360):
   name=['notify','setStatus','setWidget','setTitle','set_editor_text'][i%5]
   emit({'type':'extension_ui_request','id':str(i),'method':name,'message':'notice','statusKey':'progress','statusText':str(i),'widgetKey':'summary','widgetLines':['info'],'title':'provider title','text':'draft'})
  data={'messages':[]}
 elif method=='request_confirmation':
  emit({'type':'extension_ui_request','id':'confirm-request','method':'confirm','title':'Proceed?'})
  data={}
 else: data={'sessionId':'pi-session','confirmed':confirmed}
 emit({'id':request.get('id'),'type':'response','success':True,'data':data})
").unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700)).unwrap();
    let mut launch = NativeSessionConfig::new(AgentKind::Pi, root.path());
    launch.program = provider.to_string_lossy().into_owned();
    let session = NativeAgentSession::spawn(launch).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Idle);
    assert_eq!(
        snapshot.requests,
        Vec::<bootty_agents::NativeAgentRequest>::new()
    );
    assert!(!snapshot.transcript.is_empty() && snapshot.transcript.len() <= 256);
    assert_eq!(
        snapshot
            .transcript
            .iter()
            .filter(|item| item.id.contains("status:"))
            .count(),
        1
    );
    session
        .rpc("request_confirmation", serde_json::json!({}))
        .unwrap();
    assert_eq!(session.snapshot().requests.len(), 1);
    assert_eq!(session.snapshot().status, NativeSessionStatus::Waiting);
    session.approve("confirm-request", allow).unwrap();
    let state = session.rpc("get_state", serde_json::json!({})).unwrap();
    assert_eq!(state["confirmed"].as_bool(), Some(allow));
    assert_eq!(
        session.snapshot().requests,
        Vec::<bootty_agents::NativeAgentRequest>::new()
    );
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
}

#[rstest]
#[case(false)]
#[case(true)]
fn native_host_shutdown_reaps_process_and_saves_before_return(#[case] explicit: bool) {
    let root = TempDir::new().unwrap();
    let mut launch = config(AgentKind::Codex, root.path()).unwrap();
    let executable = std::path::PathBuf::from(&launch.program);
    let pid_file = root.path().join("pid");
    let source = fs::read_to_string(&executable).unwrap();
    fs::write(
        &executable,
        source.replace(
            "import json,sys",
            &format!(
                "import json,sys,os\nwith open({},'w') as output: output.write(str(os.getpid()))",
                serde_json::to_string(&pid_file.to_string_lossy()).unwrap()
            ),
        ),
    )
    .unwrap();
    launch.arguments.clear();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let record = service.create("binding", "Owned process", launch).unwrap();
    service.prompt(&record.target(), "Save reply").unwrap();
    let pid = fs::read_to_string(pid_file).unwrap();
    let session = service.resolve(&record.target()).unwrap();
    if explicit {
        service.shutdown().unwrap();
    }
    drop(service);
    assert_eq!(session.snapshot().status, NativeSessionStatus::Stopped);
    let check = std::process::Command::new("/usr/bin/python3").args(["-c", "import os,sys\ntry: os.kill(int(sys.argv[1]),0)\nexcept ProcessLookupError: sys.exit(0)\nsys.exit(1)", &pid]).status().unwrap();
    assert!(
        check.success(),
        "Owned process must be reaped before shutdown returns"
    );
    let restored = NativeAgentService::open(&path).unwrap();
    let saved = restored.sessions();
    assert_eq!(saved.len(), 1);
    assert!(
        saved[0]
            .snapshot
            .transcript
            .iter()
            .any(|item| item.role == "assistant" && item.text == "hello")
    );
}

#[rstest]
fn native_shutdown_cancels_a_provider_waiting_for_its_initial_handshake() {
    use std::io::Read as _;
    let root = TempDir::new().unwrap();
    let ready_path = root.path().join("ready.sock");
    let ready = std::os::unix::net::UnixListener::bind(&ready_path).unwrap();
    let program = root.path().join("starting.py");
    fs::write(&program, format!("#!/usr/bin/env python3\nimport os,socket,sys\ns=socket.socket(socket.AF_UNIX)\ns.connect({})\ns.sendall(str(os.getpid()).encode())\ns.close()\nsys.stdin.read()\n", serde_json::to_string(&ready_path.to_string_lossy()).unwrap())).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let mut launch = NativeSessionConfig::new(AgentKind::Codex, root.path());
    launch.program = program.to_string_lossy().into_owned();
    let service =
        std::sync::Arc::new(NativeAgentService::open(root.path().join("native.json")).unwrap());
    let worker_service = std::sync::Arc::clone(&service);
    let worker = std::thread::spawn(move || worker_service.create("binding", "Starting", launch));
    let (mut stream, _) = ready.accept().unwrap();
    let mut pid = String::new();
    stream.read_to_string(&mut pid).unwrap();
    service.shutdown().unwrap();
    assert!(worker.join().unwrap().is_err());
    let check = std::process::Command::new("/usr/bin/python3").args(["-c", "import os,sys\ntry: os.kill(int(sys.argv[1]),0)\nexcept ProcessLookupError: sys.exit(0)\nsys.exit(1)", &pid]).status().unwrap();
    assert!(
        check.success(),
        "An unresponsive handshake must not outlive its host"
    );
}

#[rstest]
fn invalid_or_oversized_provider_frames_fail_bounded_handshake() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("bad.py");
    fs::write(&path,"#!/usr/bin/env python3\nimport sys\nsys.stdin.readline()\nprint('x'*1048577,flush=True)\nsys.stdin.read()\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = NativeSessionConfig::new(AgentKind::Codex, root.path());
    config.program = path.to_string_lossy().into_owned();
    assert!(
        NativeAgentSession::spawn(config)
            .err()
            .unwrap()
            .contains("larger than 1 MiB")
    );
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
#[ignore = "Requires installed provider CLIs and current local account setup"]
fn installed_native_provider_handshake(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    let session =
        NativeAgentSession::spawn(NativeSessionConfig::new(provider, root.path())).unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    println!(
        "{provider}: native handshake complete, session {:?}",
        session.snapshot().session_id
    );
}

fn await_turn(session: &NativeAgentSession) -> Result<(), String> {
    let (wake, wakes) = std::sync::mpsc::channel();
    session.set_change_handler(std::sync::Arc::new(move || {
        let _ = wake.send(());
    }));
    session.send_prompt("Reply only Ready. Do not use tools.")?;
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(90))
        .ok_or("Turn deadline overflow")?;
    loop {
        let snapshot = session.snapshot();
        if matches!(
            snapshot.status,
            NativeSessionStatus::Idle | NativeSessionStatus::Error | NativeSessionStatus::Stopped
        ) {
            if snapshot.status != NativeSessionStatus::Idle {
                return Err(format!(
                    "Provider ended {:?}: {:?}",
                    snapshot.status, snapshot.error
                ));
            }
            if !snapshot
                .transcript
                .iter()
                .any(|item| item.role == "assistant" && !item.text.is_empty())
            {
                return Err("Provider completed without an assistant reply".to_owned());
            }
            break;
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
#[ignore = "Uses a small real provider turn with the installed local account"]
fn installed_native_provider_completes_a_real_turn(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    let session =
        NativeAgentSession::spawn(NativeSessionConfig::new(provider, root.path())).unwrap();
    await_turn(&session).unwrap();
    println!("{provider}: real native turn completed");
}

#[rstest]
#[ignore = "Exercises installed Claude resume/fork; turns may require account sign-in"]
fn installed_claude_fork_gets_a_distinct_native_identity() {
    let root = TempDir::new().unwrap();
    let original =
        NativeAgentSession::spawn(NativeSessionConfig::new(AgentKind::Claude, root.path()))
            .unwrap();
    let original_result = await_turn(&original);
    if let Err(error) = &original_result {
        assert!(error.contains("Not logged in"), "{error}");
    }
    let original_id = original
        .snapshot()
        .session_id
        .expect("original session identity");
    original.stop();
    let mut config = NativeSessionConfig::new(AgentKind::Claude, root.path());
    config.session_id = Some(original_id.clone());
    config.arguments.push("--fork-session".to_owned());
    let fork = NativeAgentSession::spawn(config).unwrap();
    let fork_result = await_turn(&fork);
    if let Err(error) = &fork_result {
        assert!(error.contains("Not logged in"), "{error}");
    }
    let fork_id = fork.snapshot().session_id.expect("fork session identity");
    assert_ne!(fork_id, original_id);
    println!(
        "Claude fork has a distinct observed session identity; authenticated turns: {}",
        original_result.is_ok() && fork_result.is_ok()
    );
}

#[rstest]
fn completed_provider_reply_is_saved_before_publication() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let record = service
        .create(
            "binding",
            "Session",
            config(AgentKind::Claude, root.path()).unwrap(),
        )
        .unwrap();
    let (wake, wakes) = std::sync::mpsc::channel();
    service.set_change_handler(std::sync::Arc::new(move || {
        let _ = wake.send(());
    }));
    service
        .prompt(&record.target(), "A persisted reply")
        .unwrap();
    loop {
        wakes
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let records = persisted
            .get("records")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        let transcript = records
            .first()
            .unwrap()
            .get("snapshot")
            .unwrap()
            .get("transcript")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        if transcript.iter().any(|item| {
            item.get("role") == Some(&serde_json::json!("assistant"))
                && item.get("text") == Some(&serde_json::json!("hello"))
        }) {
            break;
        }
    }
    service.rename(&record.target(), "Saved title").unwrap();
    service.stop(&record.target()).unwrap();
    let resumed = service.resume(&record.target()).unwrap();
    assert!(
        resumed
            .snapshot
            .transcript
            .iter()
            .any(|item| item.role == "assistant" && item.text == "hello")
    );
    service.stop(&resumed.target()).unwrap();
    service.remove(&resumed.target()).unwrap();
    assert!(service.sessions().is_empty());
}
