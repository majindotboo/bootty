#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use assert_fs::TempDir;
use bootty_agents::{
    AgentKind, AgentLaunch, NativeAgentService, NativeAgentSession, NativeSessionConfig,
    NativeSessionStatus, NativeToolStatus, NativeTurnOutcome,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

const SESSION: &str = "01234567-89ab-4cde-8123-456789abcdef";

fn fixture(root: &Path) -> std::io::Result<String> {
    let path = root.join("claude-stream.py");
    fs::write(
        &path,
        r#"#!/usr/bin/env python3
import json,os,sys
assert sys.argv[sys.argv.index('--input-format')+1]=='stream-json'
assert sys.argv[sys.argv.index('--output-format')+1]=='stream-json'
assert sys.argv[sys.argv.index('--permission-prompt-tool')+1]=='stdio'
assert '--replay-user-messages' in sys.argv
session=sys.argv[sys.argv.index('--resume' if '--resume' in sys.argv else '--session-id')+1]
with open('launch.json','w') as output: json.dump({'argv':sys.argv[1:],'account':os.environ.get('CLAUDE_CONFIG_DIR'),'pid':os.getpid()},output)
def emit(value): print(json.dumps(value),flush=True)
def control(value): emit({'type':'control_response','response':{'subtype':'success','request_id':value['request_id'],'response':{'models':[],'account':{},'commands':[{'name':'review','description':'Review changes','argumentHint':'[path]'}]}}})
def result(reason='completed',subtype='success',identity=None):
 emit({'type':'result','subtype':subtype,'session_id':identity or session,'uuid':'result-uuid-not-user-uuid','is_error':subtype!='success','terminal_reason':reason,'result':'Hello world'})
def event(value): emit({'type':'stream_event','session_id':session,'parent_tool_use_id':None,'event':value})
for line in sys.stdin:
 value=json.loads(line)
 if value['type']=='control_request':
  if value['request']['subtype']=='interrupt': result('aborted_streaming')
  control(value)
 elif value['type']=='user':
  prompt=value['message']['content']
  with open('prompt.json','w') as output: json.dump(value,output)
  if prompt=='oversize': sys.stdout.write('x'*(1024*1024+1)+'\n');sys.stdout.flush();continue
  if prompt=='partial': sys.stdout.write('{"type":"result"}');sys.stdout.flush();break
  emit({'type':'system','subtype':'init','session_id':'foreign' if prompt=='wrong-session' else session,'cwd':'/foreign' if prompt=='wrong-cwd' else os.getcwd()})
  result(identity='foreign')
  result() # No user ACK yet: never a completed turn.
  if prompt=='no-ack': continue
  echo=dict(value);echo['isReplay']=True;echo['message']={'role':'user','content':[{'type':'text','text':prompt}]};emit(echo)
  if prompt in ['permission','cancel','question']:
   emit({'type':'control_request','request_id':'permission-id','request':{'subtype':'can_use_tool','tool_name':'Bash','tool_use_id':'tool-id','input':{'command':'echo hello'}}})
   if prompt=='question':
    emit({'type':'control_cancel_request','request_id':'permission-id'})
    emit({'type':'control_request','request_id':'question-id','request':{'subtype':'can_use_tool','tool_name':'AskUserQuestion','tool_use_id':'question-tool','input':{'questions':[{'question':'Choose color','options':[{'label':'Red'},{'label':'Blue'}],'multiSelect':True}],'marker':'preserve'}}})
   if prompt=='cancel': emit({'type':'control_cancel_request','request_id':'permission-id'})
   continue
  if prompt=='hold': continue
  event({'type':'message_start','message':{'id':'assistant-id'}})
  event({'type':'content_block_start','index':0,'content_block':{'type':'text','text':''}})
  event({'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'Hello '}})
  event({'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'world'}})
  emit({'type':'assistant','uuid':'block-id','session_id':session,'parent_tool_use_id':None,'message':{'id':'assistant-id','content':[{'type':'text','text':'Hello world'}]}})
  event({'type':'content_block_stop','index':0})
  if prompt in ['tool-success','tool-failure']:
   emit({'type':'assistant','uuid':'tool-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'tool-message','content':[{'type':'tool_use','id':'bash-call','name':'Bash','input':{'command':'echo hello'}}]}})
   emit({'type':'user','session_id':session,'parent_tool_use_id':None,'message':{'role':'user','content':[{'type':'tool_result','tool_use_id':'bash-call','content':[{'type':'text','text':'observed output'}],'is_error':prompt=='tool-failure'}]}})
  if prompt.startswith('subagent-'):
   status=prompt.removeprefix('subagent-')
   emit({'type':'assistant','uuid':'agent-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'agent-message','content':[{'type':'tool_use','id':'agent-call','name':'Agent','input':{'description':'Review source','prompt':'Review the changes','model':'sonnet'}}]}})
   emit({'type':'system','subtype':'task_started','session_id':'foreign','task_id':'foreign-child','tool_use_id':'agent-call'})
   emit({'type':'system','subtype':'task_started','session_id':session,'task_id':'child','tool_use_id':'agent-call','description':'Review source'})
   emit({'type':'system','subtype':'task_notification','session_id':session,'task_id':'child','status':status,'summary':'Observed result'})
   emit({'type':'system','subtype':'task_progress','session_id':session,'task_id':'child'})
  if prompt=='failed': result('model_error','error_during_execution')
  else: result()
 elif value['type']=='control_response':
  with open('answer.json','w') as output: json.dump(value,output)
  result()
"#,
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path.to_string_lossy().into_owned())
}

fn config(root: &Path) -> std::io::Result<NativeSessionConfig> {
    let mut config = NativeSessionConfig::new(AgentKind::Claude, root);
    config.program = fixture(root)?;
    config.account_directory = Some(root.join("account").to_string_lossy().into_owned());
    config.fresh_session_id = Some(SESSION.to_owned());
    config.arguments = vec!["--permission-mode".to_owned(), "manual".to_owned()];
    Ok(config)
}

fn barrier(session: &NativeAgentSession) -> Result<(), String> {
    // A correlated control reply follows all preceding fixture events; no clock polling.
    session.rpc("initialize", json!({})).map(|_| ())
}

#[rstest]
#[case::success("tool-success", NativeToolStatus::Completed)]
#[case::failure("tool-failure", NativeToolStatus::Failed)]
fn claude_tool_result_retains_input_and_the_observed_error(
    #[case] prompt: &str,
    #[case] status: NativeToolStatus,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(prompt).unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    let item = snapshot
        .transcript
        .iter()
        .find(|item| item.id == "claude-tool-bash-call")
        .unwrap();
    let tool = item.tool.as_ref().unwrap();
    assert!(item.complete);
    assert_eq!(item.text, "observed output");
    assert_eq!(tool.name, "Bash");
    assert_eq!(tool.input, json!({"command":"echo hello"}).to_string());
    assert_eq!(tool.status, status);
    session.stop();
}

#[rstest]
#[case(false)]
#[case(true)]
fn claude_fast_mode_is_passed_as_a_launch_setting(#[case] fast: bool) {
    let root = TempDir::new().unwrap();
    let mut config = config(root.path()).unwrap();
    config.fast_mode = fast;
    let session = NativeAgentSession::spawn(config).unwrap();
    let launch: Value =
        serde_json::from_slice(&fs::read(root.path().join("launch.json")).unwrap()).unwrap();
    let arguments = launch["argv"].as_array().unwrap();
    let setting = arguments
        .iter()
        .position(|argument| argument == "--settings")
        .and_then(|ix| arguments.get(ix.saturating_add(1)))
        .map(|value| serde_json::from_str::<Value>(value.as_str().unwrap()).unwrap());
    assert_eq!(setting, fast.then(|| json!({"fastMode": true})));
    session.stop();
}

#[rstest]
#[case::success("literal\nquoted '$()' \"text\"", NativeTurnOutcome::Succeeded)]
#[case::failure("failed", NativeTurnOutcome::Failed)]
fn exact_session_acknowledgement_and_streaming_settle_only_the_owned_turn(
    #[case] prompt: &str,
    #[case] outcome: NativeTurnOutcome,
) {
    let root = TempDir::new().unwrap();
    let config = config(root.path()).unwrap();
    let session = NativeAgentSession::spawn(config.clone()).unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    assert_eq!(session.snapshot().session_id, None);
    assert_eq!(session.snapshot().first_turn, None);
    session.send_prompt(prompt).unwrap();
    barrier(&session).unwrap();
    let sent: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.session_id.as_deref(), Some(SESSION));
    assert_eq!(
        snapshot.first_turn.as_ref().unwrap().id,
        sent["uuid"].as_str().unwrap()
    );
    assert_eq!(snapshot.first_turn.as_ref().unwrap().outcome, outcome);
    assert_eq!(
        snapshot.completed_turn,
        outcome == NativeTurnOutcome::Succeeded
    );
    assert_eq!(
        snapshot
            .transcript
            .iter()
            .filter(|item| item.role == "user")
            .count(),
        1
    );
    assert_eq!(
        snapshot
            .transcript
            .iter()
            .filter(|item| item.role == "assistant")
            .count(),
        1
    );
    assert_eq!(snapshot.transcript.last().unwrap().text, "Hello world");
    assert!(snapshot.transcript.last().unwrap().complete);
    assert_eq!(sent["message"]["content"], prompt);
    let launch: Value =
        serde_json::from_slice(&fs::read(root.path().join("launch.json")).unwrap()).unwrap();
    assert_eq!(launch["account"], config.account_directory.unwrap());
    let arguments = launch["argv"].as_array().unwrap();
    assert!(
        arguments
            .windows(2)
            .any(|args| args == [json!("--permission-mode"), json!("manual")])
    );
    assert!(!arguments.iter().any(|arg| arg == "bypassPermissions"));
    session.stop();
}

#[rstest]
#[case::accept(true)]
#[case::deny(false)]
fn permissions_preserve_exact_request_and_input_without_changing_policy(#[case] allow: bool) {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let session = service.resolve(&record.target()).unwrap();
    service.prompt(&record.target(), "permission").unwrap();
    barrier(&session).unwrap();
    service.checkpoint().unwrap();
    assert!(service.activities()[0].approval);
    assert!(!service.activities()[0].input);
    let request = session.snapshot().requests.remove(0);
    assert_eq!(session.snapshot().status, NativeSessionStatus::Waiting);
    assert!(
        session
            .respond(
                &request.id,
                json!({"decision":"accept","updatedPermissions":[]})
            )
            .is_err()
    );
    session.approve(&request.id, allow).unwrap();
    barrier(&session).unwrap();
    let answer: Value =
        serde_json::from_slice(&fs::read(root.path().join("answer.json")).unwrap()).unwrap();
    assert_eq!(answer["response"]["request_id"], "permission-id");
    assert_eq!(answer["response"]["response"]["toolUseID"], "tool-id");
    assert_eq!(
        answer["response"]["response"]["behavior"],
        if allow { "allow" } else { "deny" }
    );
    assert_eq!(
        answer["response"]["response"].get("updatedPermissions"),
        None
    );
    if allow {
        assert_eq!(
            answer["response"]["response"]["updatedInput"],
            json!({"command":"echo hello"})
        );
    }
    assert!(
        session
            .respond(&request.id, json!({"decision":"accept"}))
            .is_err()
    );
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    service.shutdown().unwrap();
}

#[rstest]
fn control_cancellation_and_unacknowledged_results_cannot_invent_completion() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("cancel").unwrap();
    barrier(&session).unwrap();
    assert_eq!(session.snapshot().requests, []);
    assert!(
        session
            .respond("claude-permission-id", json!({"decision":"accept"}))
            .is_err()
    );
    session.interrupt().unwrap();
    barrier(&session).unwrap();
    let receipt = session.snapshot().first_turn.unwrap();
    assert_eq!(receipt.outcome, NativeTurnOutcome::Interrupted);
    session.send_prompt("second").unwrap();
    barrier(&session).unwrap();
    assert_eq!(session.snapshot().first_turn, Some(receipt));
    session.stop();
    let unacknowledged = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    unacknowledged.send_prompt("no-ack").unwrap();
    barrier(&unacknowledged).unwrap();
    assert_eq!(unacknowledged.snapshot().first_turn, None);
    assert!(!unacknowledged.snapshot().completed_turn);
    assert_eq!(
        unacknowledged.snapshot().status,
        NativeSessionStatus::Working
    );
    assert!(unacknowledged.send_prompt("another").is_err());
    unacknowledged.stop();
}

#[rstest]
#[case::wrong_session("wrong-session")]
#[case::wrong_directory("wrong-cwd")]
#[case::oversize("oversize")]
#[case::unterminated("partial")]
fn invalid_identity_or_frames_end_the_owned_lease_without_accepting_a_turn(#[case] prompt: &str) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(prompt).unwrap();
    assert!(session.rpc("initialize", json!({})).is_err());
    assert_eq!(session.snapshot().status, NativeSessionStatus::Error);
    assert_eq!(session.snapshot().first_turn, None);
    assert!(!session.snapshot().completed_turn);
    session.stop();
}

#[rstest]
fn fresh_intent_is_durable_and_resume_preserves_exact_task_account_identity() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let mut config = config(root.path()).unwrap();
    config.fresh_session_id = None;
    let record = service
        .create_for_task("binding", "task", "Claude", config)
        .unwrap();
    let intent = record.config.fresh_session_id.clone().unwrap();
    assert_eq!(record.snapshot.session_id, None);
    let persisted: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        persisted["records"][0]["config"]["fresh_session_id"],
        intent
    );
    let session = service.resolve(&record.target()).unwrap();
    service.prompt(&record.target(), "first").unwrap();
    barrier(&session).unwrap();
    service.stop(&record.target()).unwrap();
    let stopped = service.sessions().remove(0);
    assert_eq!(stopped.config.session_id.as_deref(), Some(intent.as_str()));
    let persisted: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted["records"][0]["config"]["session_id"], intent);
    service.shutdown().unwrap();
    drop(service);
    let service = NativeAgentService::open(&path).unwrap();
    assert_eq!(
        service.sessions()[0].config.session_id,
        stopped.config.session_id
    );
    let resumed = service.resume(&stopped.target()).unwrap();
    assert_eq!(resumed.id, stopped.id);
    assert_ne!(resumed.generation, stopped.generation);
    assert_eq!(resumed.task_identity.as_deref(), Some("task"));
    assert_eq!(resumed.binding_id, "binding");
    assert_eq!(
        resumed.config.account_directory,
        stopped.config.account_directory
    );
    assert_eq!(resumed.config.session_id, stopped.config.session_id);
    assert_eq!(resumed.snapshot.first_turn, None);
    assert_eq!(resumed.snapshot.transcript, stopped.snapshot.transcript);
    assert!(service.resolve(&stopped.target()).is_err());
    let launch: Value =
        serde_json::from_slice(&fs::read(root.path().join("launch.json")).unwrap()).unwrap();
    assert!(
        launch["argv"]
            .as_array()
            .unwrap()
            .windows(2)
            .any(|args| args == [json!("--resume"), json!(&intent)])
    );
    // Resumed Claude may report system/init only after another prompt. Closing while ready
    // must preserve the already captured resume selector through another owner restart.
    assert_eq!(resumed.snapshot.session_id, None);
    service.stop(&resumed.target()).unwrap();
    service.shutdown().unwrap();
    drop(service);
    let service = NativeAgentService::open(&path).unwrap();
    let saved = service.sessions().remove(0);
    assert_eq!(saved.config.session_id.as_deref(), Some(intent.as_str()));
    let reopened = service.resume(&saved.target()).unwrap();
    assert_eq!(reopened.config.session_id.as_deref(), Some(intent.as_str()));
    service.shutdown().unwrap();
}

#[rstest]
fn claude_receives_only_the_session_attachment_directory_on_launch_and_resume() {
    let root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let note = root.path().join("notes.md");
    fs::write(&note, "private file contents").unwrap();
    let reference = service.import_attachment(&record.target(), &note).unwrap();
    let attachments = service
        .resolve_prompt_attachments(&record.target(), std::slice::from_ref(&reference.id))
        .unwrap();
    let prompt = bootty_agents::NativePrompt::new_with_context(
        "Review this note.".to_owned(),
        Vec::new(),
        attachments,
        Vec::new(),
    )
    .unwrap();
    service.prompt_input(&record.target(), &prompt).unwrap();
    let session = service.resolve(&record.target()).unwrap();
    barrier(&session).unwrap();

    let user = session
        .snapshot()
        .transcript
        .into_iter()
        .find(|item| item.role == "user")
        .unwrap();
    assert_eq!(user.text, "Review this note.");
    assert_eq!(user.attachments, vec![reference]);
    let sent: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    let provider_message = sent["message"]["content"].as_str().unwrap();
    assert!(provider_message.contains("[Attached file \"notes.md\" is saved at:"));
    assert!(provider_message.contains("native-attachments"));
    assert!(!provider_message.contains(&note.to_string_lossy().to_string()));

    let read_attachment_directory = |launch: &Value| {
        let args = launch["argv"].as_array().unwrap();
        let position = args
            .iter()
            .position(|argument| argument == "--add-dir")
            .unwrap();
        Path::new(
            args.get(position.saturating_add(1))
                .and_then(Value::as_str)
                .unwrap(),
        )
        .to_path_buf()
    };
    let launch = || -> Value {
        serde_json::from_slice(&fs::read(root.path().join("launch.json")).unwrap()).unwrap()
    };
    let directory = read_attachment_directory(&launch());
    assert!(directory.starts_with(root.path().join("native-attachments")));
    assert!(directory.is_dir());

    service.stop(&record.target()).unwrap();
    let stopped = service.sessions().remove(0);
    let resumed = service.resume(&stopped.target()).unwrap();
    assert_eq!(read_attachment_directory(&launch()), directory);
    service.stop(&resumed.target()).unwrap();
    service.shutdown().unwrap();
}

proptest! {
    #[test]
    fn unsafe_native_launch_options_never_replace_transport_identity_or_permission_policy(
        option in prop::sample::select(vec!["--input-format", "--output-format", "--session-id", "--resume", "--dangerously-skip-permissions", "--no-session-persistence"]),
    ) {
        let root = TempDir::new().unwrap();
        let mut config = config(root.path()).unwrap();
        config.arguments = vec![option.to_owned(), "foreign".to_owned()];
        prop_assert!(NativeAgentSession::spawn(config).is_err());
        prop_assert!(!root.path().join("launch.json").exists());
        let launch = AgentLaunch {program:"claude".to_owned(),cwd:Some(root.path().to_string_lossy().into_owned()),arguments:vec!["--no-session-persistence".to_owned()],ephemeral:false,account_directory:Some(root.path().to_string_lossy().into_owned())};
        prop_assert!(NativeSessionConfig::from_launch(AgentKind::Claude, launch).is_err());
    }
}

#[rstest]
#[case::selection("Red, Blue")]
#[case::custom("A custom color")]
fn question_answers_preserve_original_input_and_cannot_change_permission_policy(
    #[case] answer: &str,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("question").unwrap();
    barrier(&session).unwrap();
    let request = session.snapshot().requests.remove(0);
    assert!(session.approve(&request.id, true).is_err());
    assert!(
        session
            .respond(&request.id, json!({"answers":{"foreign":"Red"}}))
            .is_err()
    );
    assert!(
        session
            .respond(
                &request.id,
                json!({"answers":{"Choose color":"Red"},"updatedPermissions":[]})
            )
            .is_err()
    );
    session
        .respond(&request.id, json!({"answers":{"Choose color":answer}}))
        .unwrap();
    barrier(&session).unwrap();
    let reply: Value =
        serde_json::from_slice(&fs::read(root.path().join("answer.json")).unwrap()).unwrap();
    assert_eq!(reply["response"]["request_id"], "question-id");
    assert_eq!(reply["response"]["response"]["toolUseID"], "question-tool");
    let updated = &reply["response"]["response"]["updatedInput"];
    assert_eq!(updated["marker"], "preserve");
    assert_eq!(
        updated["questions"],
        request.parameters["input"]["questions"]
    );
    assert_eq!(updated["answers"], json!({"Choose color":answer}));
    assert_eq!(
        reply["response"]["response"].get("updatedPermissions"),
        None
    );
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    session.stop();
}

#[rstest]
fn claude_command_discovery_uses_the_control_handshake_without_a_prompt() {
    let root = TempDir::new().unwrap();
    let catalog = NativeAgentSession::discover_completions(config(root.path()).unwrap()).unwrap();
    assert_eq!(catalog.options.len(), 1);
    assert_eq!(catalog.options[0].name, "review");
    assert_eq!(catalog.options[0].argument_hint.as_deref(), Some("[path]"));
    assert!(!root.path().join("prompt.json").exists());
}

#[rstest]
#[case("completed", NativeToolStatus::Completed)]
#[case("failed", NativeToolStatus::Failed)]
#[case("stopped", NativeToolStatus::Interrupted)]
fn claude_subagent_tasks_preserve_reported_results_and_reject_foreign_sessions(
    #[case] status: &str,
    #[case] expected: NativeToolStatus,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(&format!("subagent-{status}")).unwrap();
    // This RPC is ordered after all notifications in the provider transport.
    session.rpc("initialize", json!({})).unwrap();
    let snapshot = session.snapshot();
    let children = snapshot
        .transcript
        .iter()
        .filter(|item| item.subagent.is_some())
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1);
    let child = children[0].subagent.as_ref().unwrap();
    assert_eq!(child.status, expected);
    assert_eq!(child.prompt, "Review the changes");
    assert_eq!(child.model.as_deref(), Some("sonnet"));
    assert_eq!(child.thread_id, None);
    assert_eq!(children[0].text, "Observed result");
    session.stop();
}

#[rstest]
#[case("supervised", "default", false)]
#[case("auto-accept-edits", "acceptEdits", false)]
#[case("auto", "auto", false)]
#[case("full-access", "bypassPermissions", true)]
fn claude_launch_uses_the_selected_permission_mode(
    #[case] mode: &str,
    #[case] expected: &str,
    #[case] bypass: bool,
) {
    let root = TempDir::new().unwrap();
    let mut config = config(root.path()).unwrap();
    config.arguments.clear();
    config.permissions = mode.parse().unwrap();
    let session = NativeAgentSession::spawn(config).unwrap();
    let launch: Value =
        serde_json::from_slice(&fs::read(root.path().join("launch.json")).unwrap()).unwrap();
    let arguments = launch["argv"].as_array().unwrap();
    assert!(
        arguments
            .windows(2)
            .any(|args| args == [json!("--permission-mode"), json!(expected)])
    );
    assert_eq!(
        arguments
            .iter()
            .any(|arg| arg == "--allow-dangerously-skip-permissions"),
        bypass
    );
    session.stop();
}
