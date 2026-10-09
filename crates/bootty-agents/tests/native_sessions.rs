#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use assert_fs::TempDir;
use bootty_agents::{
    AgentKind, AgentLaunch, NativeAgentService, NativeAgentSession, NativeSessionConfig,
    NativeSessionStatus, NativeToolStatus, NativeTurnOutcome, NativeTurnReceipt,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

// This process speaks app-server JSON-RPC. Barriers order protocol events without clock sleeps.
#[allow(
    clippy::too_many_lines,
    reason = "One embedded provider protocol fixture"
)]
fn fixture(root: &Path) -> std::io::Result<String> {
    let path = root.join("provider.py");
    fs::write(
        &path,
        r"#!/usr/bin/env python3
import fcntl,json,os,subprocess,sys
assert sys.argv[1:4] == ['app-server','--listen','stdio://'],sys.argv
with open('launch.json','w') as f: json.dump({'argv':sys.argv,'home':os.environ.get('CODEX_HOME')},f)
thread='native-thread'
turn=0
waiting=None
deferred=False
release_pending=False
approvals=[]
hold_approval=False
writers=[]
def emit(value): print(json.dumps(value),flush=True)
def notice(method,**params): emit({'method':method,'params':{'threadId':thread,'turnId':'turn-'+str(turn),**params}})
def reply(ident,result): emit({'id':ident,'result':result})
def started(): notice('turn/started',turn={'id':'turn-'+str(turn),'status':'inProgress','items':[]})
def completed(status='completed'): notice('turn/completed',turn={'id':'turn-'+str(turn),'status':status,'items':[]})
for line in sys.stdin:
 value=json.loads(line)
 method=value.get('method')
 ident=value.get('id')
 params=value.get('params',{})
 if method is None:
  if 'result' in value:
   approvals.append(value['result'])
   if not hold_approval: completed()
  continue
 if method == 'initialized': continue
 if method == 'initialize': reply(ident,{})
 elif method in ['thread/start','thread/resume']:
  if method=='thread/resume' and os.path.exists('missing-rollout'):
   emit({'id':ident,'error':{'code':-32600,'message':'no rollout found for thread id '+params['threadId']}})
   continue
  if os.path.exists('fail-launch'):
   emit({'id':ident,'error':{'code':-32600,'message':'captured provider launch failed'}})
   continue
  thread=params.get('threadId','native-thread-'+str(os.getpid()) if os.path.exists('unique-threads') else 'native-thread')
  if os.path.exists('wrong-thread'): thread='different-thread'
  writer=open(thread+'.writer','a+')
  try: fcntl.flock(writer,fcntl.LOCK_EX|fcntl.LOCK_NB)
  except BlockingIOError:
   emit({'id':ident,'error':{'code':-32600,'message':'thread has an active writer'}})
   continue
  writers.append(writer)
  history=[] if method == 'thread/start' else [{'id':'prior','items':[{'id':'history','type':'agentMessage','text':'resumed history'}]}]
  if method == 'thread/resume' and os.path.exists('history-timestamps.json'):
   with open('history-timestamps.json') as source: history=json.load(source)
  reply(ident,{'thread':{'id':thread,'turns':history}})
 elif method == 'thread/compact/start':
  assert params == {'threadId':thread}
  with open('compact-request.json','w') as output: json.dump(params,output)
  turn+=1
  started()
  reply(ident,{})
 elif method == '__finish_compaction':
  completed()
  reply(ident,{})
 elif method == 'turn/start':
  assert params['threadId'] == thread
  with open('turn-policy.json','w') as output: json.dump(params,output)
  turn+=1
  text=params['input'][0]['text']
  if os.path.exists('unique-threads'):
   with open(thread+'-'+str(turn)+'.json','w') as output: json.dump(params,output)
  if text == 'reject-first':
   emit({'id':ident,'error':{'code':-32600,'message':'captured first prompt rejected'}})
   continue
  if text == 'defer':
   waiting=ident
   deferred=not release_pending
   if release_pending: started()
   continue
  started()
  user={'id':'user-'+str(turn),'type':'userMessage','content':params['input']}
  echo_mode=open('history-echo-mode').read() if os.path.exists('history-echo-mode') else 'exact'
  if echo_mode == 'altered': user['content'][0]['text']+='altered'
  if echo_mode == 'metadata': user['unowned']='x'*(1024*1024)
  if echo_mode == 'foreign':
   emit({'method':'item/completed','params':{'threadId':'foreign','turnId':'turn-'+str(turn),'item':user}})
  else: notice('item/completed',item=user)
  if text == 'active':
   reply(ident,{'turn':{'id':'turn-'+str(turn)}})
   continue
  if text == 'wait':
   waiting=ident
   continue
  if text in ['approval','approval-hold','approval-scopes','approval-once-only']:
   hold_approval=text == 'approval-hold'
   emit({'id':'wrong-thread','method':'item/commandExecution/requestApproval','params':{'threadId':'other','turnId':'turn-'+str(turn)}})
   emit({'id':'wrong-turn','method':'item/commandExecution/requestApproval','params':{'threadId':thread,'turnId':'other'}})
   scope={'threadId':thread,'turnId':'turn-'+str(turn),'itemId':'command','command':'echo approved'}
   if text=='approval-scopes': scope.update(proposedExecpolicyAmendment=['echo'],availableDecisions=['accept','decline','acceptForSession',{'acceptWithExecpolicyAmendment':{'execpolicy_amendment':['echo']}}])
   if text=='approval-once-only': scope.update(availableDecisions=['accept','decline'])
   emit({'id':900,'method':'item/commandExecution/requestApproval','params':scope})
   reply(ident,{'turn':{'id':'turn-'+str(turn)}})
   continue
  if text == 'mcp-approval':
   emit({'id':'foreign-mcp','method':'mcpServer/elicitation/request','params':{'threadId':'foreign','turnId':None}})
   emit({'id':'stale-mcp','method':'mcpServer/elicitation/request','params':{'threadId':thread,'turnId':'other'}})
   emit({'id':'mcp-decision','method':'mcpServer/elicitation/request','params':{'threadId':thread,'turnId':None,'serverName':'Bootty tools','mode':'form','message':'Allow stop_spawned_agent?','requestedSchema':{'type':'object','properties':{}}}})
   reply(ident,{'turn':{'id':'turn-'+str(turn)}})
   continue
  if text in ['tool-completed','tool-failed','tool-declined']:
   status=text.removeprefix('tool-')
   notice('item/started',item={'id':'typed-command','type':'commandExecution','command':'echo typed','status':'inProgress'})
   notice('item/commandExecution/outputDelta',itemId='typed-command',delta='streamed output')
   notice('item/completed',item={'id':'typed-command','type':'commandExecution','command':'echo typed','status':status,'aggregatedOutput':''})
   notice('item/completed',item={'id':'typed-mcp','type':'mcpToolCall','server':'fixture','tool':'inspect','arguments':{'path':'source.rs'},'status':'failed' if status=='declined' else status,'result':{'content':[{'type':'text','text':'inspection output'}]}})
  if text == 'bounded':
   for i in range(96): notice('item/agentMessage/delta',itemId='long-'+str(i),delta='界'*6000)
  else:
   emit({'method':'item/agentMessage/delta','params':{'threadId':'other','turnId':'turn-'+str(turn),'itemId':'foreign','delta':'foreign thread'}})
   emit({'method':'item/agentMessage/delta','params':{'threadId':thread,'turnId':'old','itemId':'foreign-turn','delta':'foreign turn'}})
   notice('item/started',item={'id':'command-'+str(turn),'type':'commandExecution','command':'echo probe','aggregatedOutput':''})
   notice('item/commandExecution/outputDelta',itemId='command-'+str(turn),delta='probe output')
   notice('item/agentMessage/delta',itemId='answer-'+str(turn),delta='hello')
   notice('item/completed',item={'id':'answer-'+str(turn),'type':'agentMessage','text':'hello'})
  emit({'method':'turn/completed','params':{'threadId':'other','turn':{'id':'turn-'+str(turn),'status':'completed'}}})
  emit({'method':'turn/completed','params':{'threadId':thread,'turn':{'id':'old','status':'completed'}}})
  completed(text if text in ['failed','interrupted','unknown'] else 'completed')
  reply(ident,{'turn':{'id':'different-ack' if text=='mismatched-ack' else 'turn-'+str(turn)}})
 elif method == 'turn/steer':
  assert params['threadId'] == thread and params['expectedTurnId'] == 'turn-'+str(turn)
  text=params['input'][0]['text']
  if text == 'reject-steer':
   emit({'id':ident,'error':{'code':-32600,'message':'steer rejected'}})
   continue
  notice('item/completed',item={'id':'steered-user-'+str(turn),'type':'userMessage','content':params['input']})
  reply(ident,{'turnId':'turn-'+str(turn)})
 elif method == 'turn/interrupt':
  assert params['threadId'] == thread and params['turnId'] == 'turn-'+str(turn)
  completed('interrupted')
  if waiting is not None: reply(waiting,{'turn':{'id':'turn-'+str(turn)}}); waiting=None
  reply(ident,{})
 elif method == '__release':
  release_pending=True
  if deferred: started(); deferred=False
  reply(ident,{})
 elif method == '__repeat_started':
  started()
  reply(ident,{})
 elif method == '__finish':
  completed(params['status'])
  reply(ident,{})
 elif method == '__descendant':
  descendant=subprocess.Popen([sys.executable,'-c','import signal; signal.pause()'],stdin=subprocess.DEVNULL)
  reply(ident,{'providerPid':os.getpid(),'descendantPid':descendant.pid})
 elif method == '__stop_callback':
  notice('thread/tokenUsage/updated',tokenUsage={})
  reply(ident,{})
 elif method == '__barrier':
  if os.path.exists('events.json'):
   with open('events.json') as source: events=json.load(source)
   os.unlink('events.json')
   for event in events: emit(event)
  reply(ident,{'approvals':approvals})
 elif method == 'thread/read':
  with open('read.json','w') as output: json.dump(value,output)
  child=params.get('threadId',thread)
  if os.path.exists('legacy-history'):
   with open('paged-history.json') as source: items=json.load(source)
   if os.path.exists('malformed-history') and len(items)>1: items[-1]=items[-2]
   turns=[{'id':entry['turnId'],'items':[entry['item']],'startedAtMs':entry.get('startedAtMs'),'completedAtMs':entry.get('completedAtMs')} for entry in items]
   reply(ident,{'thread':{'id':child,'turns':turns}})
   continue
  reply(ident,{'thread':{'id':child,'turns':[{'id':'child-turn','items':[{'id':'child-answer','type':'agentMessage','text':'Child review'}]}] if child=='child-thread' else []}})
 elif method == 'thread/items/list':
  assert params['threadId'] == thread
  if os.path.exists('legacy-history'):
   emit({'id':ident,'error':{'code':-32601,'message':'thread/items/list is not supported yet'}})
   continue
  with open('paged-history.json') as source: items=json.load(source)
  limit=params['limit']
  descending=params['sortDirection']=='desc'
  cursor=params.get('cursor')
  if descending:
   end=len(items) if cursor is None else int(cursor)
   start=max(0,end-limit)
   data=list(reversed(items[start:end]))
   next_cursor=str(start) if start>0 else None
   backwards=str(end) if data else None
  else:
   start=int(cursor)
   end=min(len(items),start+limit)
   data=items[start:end]
   next_cursor=str(end) if end<len(items) else None
   backwards=str(start) if start>0 else None
  if os.path.exists('malformed-history') and len(data)>1: data[-1]=data[0]
  reply(ident,{'data':data,'nextCursor':next_cursor,'backwardsCursor':backwards})
 else: reply(ident,{})
",
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path.to_string_lossy().into_owned())
}

fn config(root: &Path) -> std::io::Result<NativeSessionConfig> {
    let mut config = NativeSessionConfig::new(AgentKind::Codex, root);
    config.program = fixture(root)?;
    config.account_directory = Some(root.join("account").to_string_lossy().into_owned());
    Ok(config)
}

fn barrier(session: &NativeAgentSession) -> Result<Value, String> {
    session.rpc("__barrier", json!({}))
}

#[rstest]
#[case::ordinary(130, "History claim 🥟".to_owned())]
#[case::beyond_live_window(300, "Earlier history".to_owned())]
#[case::large_control_text(70, "\n\"".repeat(8_000))]
fn provider_pages_visit_every_item_without_overwriting_live_state(
    #[case] count: usize,
    #[case] text: String,
    #[values(false, true)] legacy: bool,
) {
    let root = TempDir::new().unwrap();
    if legacy {
        fs::write(root.path().join("legacy-history"), "").unwrap();
    }
    let items = (0..count)
        .map(|index| {
            json!({
                "turnId": format!("turn-{index}"),
                "item": {"id":format!("item-{index}"), "type":"agentMessage", "text":text},
                "startedAtMs":index.checked_mul(1_000).unwrap(), "completedAtMs":index.checked_mul(1_000).unwrap().checked_add(100).unwrap(),
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        root.path().join("paged-history.json"),
        serde_json::to_vec(&items).unwrap(),
    )
    .unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    let live = session.snapshot();
    let mut page = session.read_history("latest").unwrap();
    assert!(page.at_latest && !page.has_newer);
    let mut visited = std::collections::BTreeSet::new();
    loop {
        assert!(serde_json::to_vec(&page).unwrap().len() < 600 * 1024);
        for item in &page.transcript {
            assert!(
                visited.insert(item.id.clone()),
                "history cursor skipped or repeated a page"
            );
            let index = item
                .id
                .strip_prefix("item-")
                .unwrap()
                .parse::<i64>()
                .unwrap();
            assert_eq!(item.created_at, Some(index.checked_mul(1_000).unwrap()));
            assert_eq!(
                item.updated_at,
                Some(index.checked_mul(1_000).unwrap().checked_add(100).unwrap())
            );
        }
        assert!(
            page.transcript
                .windows(2)
                .all(|pair| pair[0].created_at < pair[1].created_at)
        );
        if !page.has_older {
            break;
        }
        page = session.read_history("older").unwrap();
    }
    assert_eq!(
        visited,
        (0..count).map(|index| format!("item-{index}")).collect()
    );
    assert!(session.read_history("older").is_err());
    while page.has_newer {
        page = session.read_history("newer").unwrap();
    }
    assert!(page.at_latest);
    assert_eq!(
        page.transcript.last().unwrap().id,
        format!("item-{}", count.checked_sub(1).unwrap())
    );
    assert_eq!(
        session.snapshot(),
        live,
        "history reads preserve the live transcript, clock and turn receipts"
    );
    fs::write(root.path().join("malformed-history"), "").unwrap();
    assert!(session.read_history("latest").is_err());
    assert_eq!(session.snapshot(), live);
}

#[rstest]
#[case::paged(false)]
#[case::legacy(true)]
fn side_chat_from_an_older_provider_page_keeps_context_and_the_displayed_page(
    #[case] legacy: bool,
) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("unique-threads"), "").unwrap();
    if legacy {
        fs::write(root.path().join("legacy-history"), "").unwrap();
    }
    let items = (0..150).map(|index| json!({
        "turnId":format!("turn-{index}"),
        "item":if index % 2 == 0 {
            json!({"id":format!("item-{index}"),"type":"userMessage","content":[{"type":"text","text":format!("Question {index}")}]})
        } else {
            json!({"id":format!("item-{index}"),"type":"agentMessage","text":format!("Claim {index}")})
        },
    })).collect::<Vec<_>>();
    fs::write(
        root.path().join("paged-history.json"),
        serde_json::to_vec(&items).unwrap(),
    )
    .unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let parent = service
        .create_for_task("binding", "task", "Parent", config(root.path()).unwrap())
        .unwrap();
    let session = service.resolve(&parent.target()).unwrap();
    let before = session.snapshot();
    service.read_history(&parent.target(), "latest").unwrap();
    let older = service.read_history(&parent.target(), "older").unwrap();
    assert!(
        service
            .fork_side_chat(&parent.target(), Some("unavailable-response"), None)
            .is_err()
    );
    assert_eq!(service.sessions().len(), 1);
    let boundary = older
        .transcript
        .iter()
        .find(|item| item.role == "assistant")
        .unwrap();
    let child = service
        .fork_side_chat(&parent.target(), Some(&boundary.id), None)
        .unwrap();
    let copied = &child.side_chat.as_ref().unwrap().transcript;
    assert_eq!(copied.first().unwrap().text, "Question 0");
    assert_eq!(copied.last().unwrap().text, boundary.text);
    assert_eq!(copied.len(), 24);
    assert_eq!(session.snapshot(), before);
    let next = service.read_history(&parent.target(), "newer").unwrap();
    assert_eq!(next.transcript.first().unwrap().text, "Question 86");
    assert!(next.at_latest);
    service
        .prompt(&child.target(), "Continue the earlier response")
        .unwrap();
    let identity = child.snapshot.session_id.as_ref().unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join(format!("{identity}-1.json"))).unwrap())
            .unwrap();
    let text = wire["input"][0]["text"].as_str().unwrap();
    assert!(text.contains("Question 0"));
    assert!(text.contains("Claim 23"));
    assert!(!text.contains("Question 24"));
    service.shutdown().unwrap();
}

#[rstest]
#[case::context_limit(320, "x".repeat(8192), "item-319", "context limit")]
#[case::lookback_limit(4200, "Claim".into(), "item-1", "lookback limit")]
fn unsupported_older_forks_leave_the_catalog_and_displayed_history_unchanged(
    #[case] count: usize,
    #[case] text: String,
    #[case] response: &str,
    #[case] reason: &str,
) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("unique-threads"), "").unwrap();
    let items = (0..count)
        .map(|index| {
            json!({
                "turnId":format!("turn-{index}"),
                "item":{"id":format!("item-{index}"),"type":"agentMessage","text":text},
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        root.path().join("paged-history.json"),
        serde_json::to_vec(&items).unwrap(),
    )
    .unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let parent = service
        .create_for_task("binding", "task", "Parent", config(root.path()).unwrap())
        .unwrap();
    let session = service.resolve(&parent.target()).unwrap();
    let before = session.snapshot();
    let latest = service.read_history(&parent.target(), "latest").unwrap();
    service.read_history(&parent.target(), "older").unwrap();
    let error = service
        .fork_side_chat(&parent.target(), Some(response), None)
        .unwrap_err();
    assert!(error.contains(reason), "{error}");
    assert_eq!(service.sessions().len(), 1);
    assert_eq!(session.snapshot(), before);
    let newer = service.read_history(&parent.target(), "newer").unwrap();
    assert_eq!(newer.transcript, latest.transcript);
    assert!(newer.at_latest);
    service.shutdown().unwrap();
}

#[rstest]
fn quoting_an_earlier_page_keeps_the_exact_live_conversation() {
    let root = TempDir::new().unwrap();
    let items = (0..150).map(|index| json!({
        "turnId":format!("turn-{index}"),
        "item":{"id":format!("item-{index}"),"type":"agentMessage","text":format!("Claim {index}")},
    })).collect::<Vec<_>>();
    fs::write(
        root.path().join("paged-history.json"),
        serde_json::to_vec(&items).unwrap(),
    )
    .unwrap();
    let service = NativeAgentService::open(root.path().join("catalog.json")).unwrap();
    let record = service
        .create("captured-binding", "History", config(root.path()).unwrap())
        .unwrap();
    let target = record.target();
    let live = service.resolve(&target).unwrap();
    let initial = live.snapshot();
    service.read_history(&target, "latest").unwrap();
    let older = service.read_history(&target, "older").unwrap();
    let source = older.transcript.first().unwrap();
    let citation = bootty_agents::NativeResponseCitation {
        message_id: source.id.clone(),
        source_range: 0..source.text.len(),
        prompt_range: None,
        quote: source.text.clone(),
        comment: "Clarify".into(),
    };
    service.read_history(&target, "older").unwrap();
    assert_eq!(live.snapshot(), initial);
    let prompt = bootty_agents::NativePrompt::new_with_context(
        "Clarify the quote".into(),
        vec![],
        bootty_agents::NativePromptAttachments::default(),
        vec![citation.clone()],
    )
    .unwrap();
    service.prompt_input(&target, &prompt).unwrap();
    barrier(&live).unwrap();
    assert_eq!(live.snapshot().session_id, initial.session_id);
    assert!(
        live.snapshot()
            .transcript
            .iter()
            .any(|item| item.role == "user" && item.citations == [citation.clone()])
    );
    let mut stale = target;
    stale.generation = stale.generation.checked_add(1).unwrap();
    assert!(service.read_history(&stale, "latest").is_err());
}

#[rstest]
#[case::completed("tool-completed", NativeToolStatus::Completed)]
#[case::failed("tool-failed", NativeToolStatus::Failed)]
#[case::declined("tool-declined", NativeToolStatus::Declined)]
fn codex_tool_projection_keeps_command_streams_and_actual_tool_status(
    #[case] prompt: &str,
    #[case] status: NativeToolStatus,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(prompt).unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    for (id, name, input, output) in [
        (
            "typed-command",
            "commandExecution",
            "echo typed".to_owned(),
            "streamed output",
        ),
        (
            "typed-mcp",
            "inspect",
            json!({"path":"source.rs"}).to_string(),
            "inspection output",
        ),
    ] {
        let item = snapshot
            .transcript
            .iter()
            .find(|item| item.id == id)
            .unwrap();
        let tool = item.tool.as_ref().unwrap();
        assert!(item.complete);
        assert_eq!(item.text, output);
        assert_eq!(tool.name, name);
        assert_eq!(tool.input, input);
        assert_eq!(
            tool.status,
            if id == "typed-mcp" && status == NativeToolStatus::Declined {
                NativeToolStatus::Failed
            } else {
                status
            }
        );
    }
    session.stop();
}

#[rstest]
#[case(
    "completed",
    NativeSessionStatus::Idle,
    true,
    NativeTurnOutcome::Succeeded
)]
#[case("failed", NativeSessionStatus::Error, false, NativeTurnOutcome::Failed)]
#[case(
    "interrupted",
    NativeSessionStatus::Idle,
    false,
    NativeTurnOutcome::Interrupted
)]
#[case(
    "unknown",
    NativeSessionStatus::Idle,
    false,
    NativeTurnOutcome::Running
)]
fn completion_requires_an_observed_successful_turn(
    #[case] prompt: &str,
    #[case] status: NativeSessionStatus,
    #[case] completed: bool,
    #[case] outcome: NativeTurnOutcome,
) {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create("binding-captured", "Codex", config(root.path()).unwrap())
        .unwrap();
    assert_eq!(record.snapshot.completed_turn, false);
    assert_eq!(record.snapshot.first_turn, None);
    assert_eq!(service.activities()[0].completed_turn, false);
    service.prompt(&record.target(), prompt).unwrap();
    assert_eq!(service.activities()[0].status, status);
    assert_eq!(service.activities()[0].completed_turn, completed);
    assert_eq!(
        service.activities()[0].first_turn,
        Some(NativeTurnReceipt {
            id: "turn-1".to_owned(),
            outcome,
        })
    );
}

#[rstest]
fn legacy_accountless_history_remains_readable_without_resuming_under_another_account() {
    let root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service
        .create("binding", "Saved Codex", config(root.path()).unwrap())
        .unwrap();
    service.prompt(&record.target(), "completed").unwrap();
    service.stop(&record.target()).unwrap();
    let transcript = service.sessions()[0].snapshot.transcript.clone();
    service.shutdown().unwrap();
    drop(service);
    let mut legacy: Value = serde_json::from_slice(&fs::read(&catalog).unwrap()).unwrap();
    legacy["records"][0]["config"]
        .as_object_mut()
        .unwrap()
        .remove("account_directory");
    fs::write(&catalog, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let service = NativeAgentService::open(&catalog).unwrap();
    let retained = service.sessions().remove(0);
    assert_eq!(retained.snapshot.transcript, transcript);
    assert!(
        retained
            .snapshot
            .error
            .as_deref()
            .unwrap()
            .contains("captured accounts")
    );
    assert!(
        service
            .resume(&retained.target())
            .unwrap_err()
            .contains("captured absolute account")
    );
    assert_eq!(service.sessions()[0].target(), retained.target());
    service
        .create("binding", "New Codex", config(root.path()).unwrap())
        .unwrap();
    assert_eq!(service.sessions().len(), 2);
    service.shutdown().unwrap();
}

#[rstest]
#[case(false)]
#[case(true)]
fn completion_survives_attachment_resume_without_inference_from_history(#[case] completed: bool) {
    let root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service
        .create("binding-captured", "Codex", config(root.path()).unwrap())
        .unwrap();
    if completed {
        service.prompt(&record.target(), "completed").unwrap();
    }
    service.stop(&record.target()).unwrap();
    assert_eq!(service.activities()[0].completed_turn, completed);
    service.shutdown().unwrap();
    drop(service);
    if !completed {
        // Existing catalogs predate the completion fact; loading them defaults to unknown.
        let mut legacy: Value = serde_json::from_slice(&fs::read(&catalog).unwrap()).unwrap();
        legacy["records"][0]["snapshot"]
            .as_object_mut()
            .unwrap()
            .remove("completed_turn");
        legacy["records"][0]["snapshot"]
            .as_object_mut()
            .unwrap()
            .remove("first_turn");
        fs::write(&catalog, serde_json::to_vec(&legacy).unwrap()).unwrap();
    }
    let service = NativeAgentService::open(&catalog).unwrap();
    assert_eq!(service.activities()[0].completed_turn, completed);
    let restored = service.sessions().remove(0);
    assert_eq!(
        restored
            .snapshot
            .first_turn
            .as_ref()
            .map(|receipt| receipt.outcome),
        completed.then_some(NativeTurnOutcome::Succeeded)
    );
    let resumed = service.resume(&restored.target()).unwrap();
    assert_eq!(resumed.snapshot.first_turn, None);
    assert_eq!(resumed.snapshot.status, NativeSessionStatus::Idle);
    assert_eq!(resumed.snapshot.transcript[0].text, "resumed history");
    assert_eq!(resumed.snapshot.completed_turn, completed);
    service.checkpoint().unwrap();
    assert_eq!(service.activities()[0].completed_turn, completed);
    // Rejected prompts leave the prior observed completion intact.
    assert!(service.prompt(&resumed.target(), "").is_err());
    assert_eq!(service.activities()[0].completed_turn, completed);
}

#[rstest]
fn captures_account_and_profile_and_resumes_exact_thread() {
    let root = TempDir::new().unwrap();
    let launch = AgentLaunch {
        program: fixture(root.path()).unwrap(),
        cwd: Some(root.path().to_string_lossy().into_owned()),
        arguments: vec![
            "--profile".to_owned(),
            "personal".to_owned(),
            "--model".to_owned(),
            "chosen-model".to_owned(),
        ],
        ephemeral: false,
        account_directory: Some(
            root.path()
                .join("exact-account")
                .to_string_lossy()
                .into_owned(),
        ),
    };
    let config = NativeSessionConfig::from_launch(AgentKind::Codex, launch).unwrap();
    let account = config.account_directory.clone();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service.create("binding-captured", "Codex", config).unwrap();
    assert_eq!(record.snapshot.session_id.as_deref(), Some("native-thread"));
    let launched: Value =
        serde_json::from_slice(&fs::read(root.path().join("launch.json")).unwrap()).unwrap();
    assert_eq!(launched["home"].as_str(), account.as_deref());
    let argv = launched["argv"].as_array().unwrap();
    assert!(argv.contains(&json!("profile=\"personal\"")));
    assert!(argv.contains(&json!("model=\"chosen-model\"")));
    let target = record.target();
    service.prompt(&target, "hello").unwrap();
    service.stop(&target).unwrap();
    assert!(
        service.sessions()[0]
            .snapshot
            .transcript
            .iter()
            .any(|item| item.text == "hello")
    );
    service.shutdown().unwrap();
    drop(service);
    let restored = NativeAgentService::open(&catalog).unwrap();
    let stopped = restored.sessions().remove(0);
    assert_eq!(stopped.snapshot.status, NativeSessionStatus::Stopped);
    assert_eq!(stopped.config.account_directory, account);
    let resumed = restored.resume(&stopped.target()).unwrap();
    assert_eq!(resumed.id, record.id);
    assert_ne!(resumed.generation, record.generation);
    assert_eq!(resumed.snapshot.session_id, record.snapshot.session_id);
    assert_eq!(resumed.snapshot.transcript[0].text, "resumed history");
    assert!(restored.prompt(&target, "stale").is_err());
    assert!(restored.resume(&target).is_err());
    assert_eq!(
        restored
            .resolve(&resumed.target())
            .unwrap()
            .snapshot()
            .status,
        NativeSessionStatus::Idle
    );
}

#[rstest]
fn resume_publishes_starting_then_idle_with_monotonic_generation_revisions() {
    let root = TempDir::new().unwrap();
    let service = Arc::new(NativeAgentService::open(root.path().join("native.json")).unwrap());
    let record = service
        .create_for_task(
            "binding-captured",
            "task-captured",
            "Codex",
            config(root.path()).unwrap(),
        )
        .unwrap();
    service.prompt(&record.target(), "first history").unwrap();
    service.prompt(&record.target(), "second history").unwrap();
    service.stop(&record.target()).unwrap();
    let stopped = service.sessions().remove(0);
    let (published, publications) = mpsc::channel();
    let weak = Arc::downgrade(&service);
    let identity = stopped.id.clone();
    service.set_change_handler(Arc::new(move || {
        if let Some(service) = weak.upgrade()
            && let Some(record) = service
                .sessions()
                .into_iter()
                .find(|record| record.id == identity)
        {
            let _ = published.send(record);
        }
    }));
    let resumed = service.resume(&stopped.target()).unwrap();
    let observed = publications
        .try_iter()
        .filter(|record| record.generation == resumed.generation)
        .collect::<Vec<_>>();
    let starting = observed.first().unwrap();
    assert_eq!(starting.snapshot.status, NativeSessionStatus::Starting);
    assert_eq!(starting.snapshot.revision, 0);
    assert_eq!(starting.snapshot.transcript, stopped.snapshot.transcript);
    assert_eq!(starting.snapshot.usage, stopped.snapshot.usage);
    assert_eq!(starting.config.session_id, stopped.config.session_id);
    assert_eq!(
        starting.config.account_directory,
        stopped.config.account_directory
    );
    assert_eq!(starting.config.arguments, stopped.config.arguments);
    assert_eq!(starting.snapshot.turn_id, None);
    assert_eq!(starting.snapshot.requests, []);
    assert_eq!(starting.snapshot.error, None);
    assert!(
        observed
            .iter()
            .skip(1)
            .any(|record| record.snapshot.status == NativeSessionStatus::Idle)
    );
    assert!(
        observed
            .windows(2)
            .all(|records| records[0].snapshot.revision <= records[1].snapshot.revision)
    );
    assert_eq!(resumed.snapshot.status, NativeSessionStatus::Idle);
    assert!(
        stopped.snapshot.revision > resumed.snapshot.revision,
        "fixture must exercise an older process with a larger revision"
    );
    assert!(resumed.generation > stopped.generation);
}

#[rstest]
fn retains_exact_task_membership_when_reopened() {
    let root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service
        .create_for_task(
            "binding-captured",
            "task-captured",
            "Codex",
            config(root.path()).unwrap(),
        )
        .unwrap();
    assert_eq!(record.task_identity.as_deref(), Some("task-captured"));
    service.shutdown().unwrap();
    drop(service);
    let restored = NativeAgentService::open(&catalog).unwrap();
    let record = restored.sessions().remove(0);
    assert_eq!(record.binding_id, "binding-captured");
    assert_eq!(record.task_identity.as_deref(), Some("task-captured"));
    let resumed = restored.resume(&record.target()).unwrap();
    assert_eq!(resumed.task_identity, record.task_identity);
}

#[rstest]
fn stopping_session_reaps_provider_and_closes_descendant_transport_pipes() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    let descendants = session.rpc("__descendant", json!({})).unwrap();
    let provider_pid = descendants["providerPid"].as_u64().unwrap();
    assert!(descendants["descendantPid"].as_u64().is_some());
    session.stop();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Stopped);
    // A surviving descendant would retain stdout/stderr and make owned transport teardown fail.
    assert_eq!(session.snapshot().error, None);
    let still_exists = std::process::Command::new("/bin/kill")
        .args(["-0", &provider_pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!still_exists.success(), "provider was not reaped");
}

#[rstest]
fn protocol_callback_can_stop_its_own_session_without_joining_itself() {
    let root = TempDir::new().unwrap();
    let session = Arc::new(NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap());
    let weak = Arc::downgrade(&session);
    let (done, completed) = mpsc::channel();
    session.set_change_handler(Arc::new(move || {
        if let Some(session) = weak.upgrade() {
            session.stop();
            let _ = done.send(());
        }
    }));
    assert!(session.rpc("__stop_callback", json!({})).is_err());
    completed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Stopped);
    assert_eq!(session.snapshot().error, None);
}

#[rstest]
fn streams_command_output_and_filters_exact_thread_and_turn() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("same").unwrap();
    barrier(&session).unwrap();
    session.send_prompt("same").unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Idle);
    assert_eq!(
        snapshot
            .transcript
            .iter()
            .filter(|item| item.role == "user")
            .count(),
        2
    );
    assert!(
        snapshot
            .transcript
            .iter()
            .any(|item| item.role == "tool" && item.text.contains("probe output"))
    );
    assert!(
        !snapshot
            .transcript
            .iter()
            .any(|item| item.text.contains("foreign"))
    );
}

#[rstest]
fn active_followup_steers_the_existing_turn_and_rejection_preserves_work() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("active").unwrap();
    barrier(&session).unwrap();
    let active = session.snapshot();
    session.send_prompt("change course").unwrap();
    barrier(&session).unwrap();
    let steered = session.snapshot();
    assert_eq!(steered.status, NativeSessionStatus::Working);
    assert_eq!(steered.turn_id, active.turn_id);
    assert_eq!(steered.first_turn, active.first_turn);
    assert!(
        steered
            .transcript
            .iter()
            .any(|item| item.role == "user" && item.text == "change course")
    );
    assert!(session.send_prompt("reject-steer").is_err());
    let rejected = session.snapshot();
    assert_eq!(rejected.status, NativeSessionStatus::Working);
    assert_eq!(rejected.turn_id, active.turn_id);
    assert_eq!(rejected.first_turn, active.first_turn);
    assert!(
        !rejected
            .transcript
            .iter()
            .any(|item| item.text == "reject-steer")
    );
    session.interrupt().unwrap();
}

#[rstest]
#[case("wait", true)]
#[case("defer", false)]
fn interrupt_does_not_wait_for_prompt_acknowledgement(
    #[case] prompt: &'static str,
    #[case] turn_started: bool,
) {
    let root = TempDir::new().unwrap();
    let seconds = Arc::new(AtomicU64::new(100));
    let origin = Instant::now();
    let clock = Arc::clone(&seconds);
    let service = Arc::new(
        NativeAgentService::open_with_clock(
            root.path().join("native.json"),
            Arc::new(move || {
                origin
                    .checked_add(Duration::from_secs(clock.load(Ordering::Acquire)))
                    .unwrap()
            }),
        )
        .unwrap(),
    );
    let record = service
        .create("captured-binding", "Codex", config(root.path()).unwrap())
        .unwrap();
    service.prompt(&record.target(), "completed").unwrap();
    assert_eq!(service.activities()[0].completed_turn, true);
    let session = service.resolve(&record.target()).unwrap();
    let (changed, changes) = mpsc::channel();
    session.set_change_handler(Arc::new(move || {
        let _ = changed.send(());
    }));
    let target = record.target();
    let sender = Arc::clone(&service);
    let worker = std::thread::spawn(move || sender.prompt(&target, prompt));
    loop {
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        let snapshot = session.snapshot();
        if snapshot.status == NativeSessionStatus::Working
            && snapshot.turn_id.is_some() == turn_started
        {
            assert_eq!(snapshot.completed_turn, false);
            break;
        }
    }
    barrier(&session).unwrap();
    service.checkpoint().unwrap();
    assert_eq!(
        service.activities()[0].working_elapsed,
        Some(Duration::ZERO)
    );
    seconds.store(107, Ordering::Release);
    let revision = service.revision();
    assert_eq!(
        service.activities()[0].working_elapsed,
        Some(Duration::from_secs(7))
    );
    assert_eq!(service.revision(), revision);
    if turn_started {
        session.rpc("__repeat_started", json!({})).unwrap();
        service.checkpoint().unwrap();
        assert_eq!(
            service.activities()[0].working_elapsed,
            Some(Duration::from_secs(7))
        );
    }
    service.interrupt(&record.target()).unwrap();
    if !turn_started {
        session.rpc("__release", json!({})).unwrap();
    }
    worker.join().unwrap().unwrap();
    barrier(&session).unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    assert_eq!(session.snapshot().turn_id, None);
    assert_eq!(session.snapshot().completed_turn, false);
    service.checkpoint().unwrap();
    assert_eq!(service.activities()[0].completed_turn, false);
    assert_eq!(service.activities()[0].working_elapsed, None);
}

#[rstest]
#[case("completed")]
#[case("failed")]
#[case("interrupted")]
#[case("stop")]
fn working_duration_keeps_one_turn_origin_through_approval_and_clears_when_inactive(
    #[case] ending: &str,
) {
    let root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let seconds = Arc::new(AtomicU64::new(100));
    let origin = Instant::now();
    let clock = Arc::clone(&seconds);
    let service = NativeAgentService::open_with_clock(
        &catalog,
        Arc::new(move || {
            origin
                .checked_add(Duration::from_secs(clock.load(Ordering::Acquire)))
                .unwrap()
        }),
    )
    .unwrap();
    let record = service
        .create("binding", "Codex", config(root.path()).unwrap())
        .unwrap();
    let session = service.resolve(&record.target()).unwrap();
    assert_eq!(service.activities()[0].working_elapsed, None);
    service.prompt(&record.target(), "approval-hold").unwrap();
    assert_eq!(service.activities()[0].status, NativeSessionStatus::Waiting);
    assert_eq!(service.activities()[0].working_elapsed, None);
    seconds.store(107, Ordering::Release);
    let request = session.snapshot().requests.remove(0);
    service
        .approve(&record.target(), &request.id, true)
        .unwrap();
    barrier(&session).unwrap();
    service.checkpoint().unwrap();
    assert_eq!(
        service.activities()[0].working_elapsed,
        Some(Duration::from_secs(7))
    );
    session.rpc("__repeat_started", json!({})).unwrap();
    service.checkpoint().unwrap();
    assert_eq!(
        service.activities()[0].working_elapsed,
        Some(Duration::from_secs(7))
    );
    seconds.store(400, Ordering::Release);
    assert_eq!(
        service.activities()[0].working_elapsed,
        Some(Duration::from_secs(300))
    );
    let saved: Value = serde_json::from_slice(&fs::read(&catalog).unwrap()).unwrap();
    assert_eq!(saved["records"][0]["snapshot"].get("working_since"), None);
    if ending != "stop" {
        session.rpc("__finish", json!({"status":ending})).unwrap();
        service.checkpoint().unwrap();
        assert_eq!(service.activities()[0].working_elapsed, None);
    }
    service.stop(&record.target()).unwrap();
    assert_eq!(service.activities()[0].working_elapsed, None);
    let resumed = service.resume(&record.target()).unwrap();
    assert!(resumed.generation > record.generation);
    assert_eq!(service.activities()[0].working_elapsed, None);
    assert_eq!(
        resumed.config.account_directory,
        record.config.account_directory
    );
    service.shutdown().unwrap();
    drop(service);
    let restored = NativeAgentService::open(catalog).unwrap();
    assert_eq!(restored.activities()[0].working_elapsed, None);
    assert_eq!(
        restored.activities()[0].status,
        NativeSessionStatus::Stopped
    );
}

#[rstest]
fn approves_only_first_hand_current_requests_without_durable_grants() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("approval").unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Waiting);
    assert_eq!(snapshot.requests.len(), 1);
    let id = snapshot.requests[0].id.clone();
    assert!(session.approve("900", true).is_err());
    assert!(
        session
            .respond(&id, json!({"decision":"acceptWithExecpolicyAmendment"}))
            .is_err()
    );
    session.approve(&id, false).unwrap();
    let result = barrier(&session).unwrap();
    assert_eq!(result["approvals"], json!([{"decision":"decline"}]));
    assert!(session.approve(&id, true).is_err());
    session.send_prompt("approval").unwrap();
    barrier(&session).unwrap();
    assert_ne!(session.snapshot().requests[0].id, id);
    assert!(session.approve(&id, true).is_err());
}

#[rstest]
#[case(bootty_agents::NativeApprovalDecision::AllowOnce, json!({"decision":"accept"}))]
#[case(bootty_agents::NativeApprovalDecision::Deny, json!({"decision":"decline"}))]
#[case(bootty_agents::NativeApprovalDecision::AllowSession, json!({"decision":"acceptForSession"}))]
#[case(bootty_agents::NativeApprovalDecision::AlwaysAllow, json!({"decision":{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["echo"]}}}))]
fn approval_scopes_use_only_the_exact_advertised_provider_grant(
    #[case] decision: bootty_agents::NativeApprovalDecision,
    #[case] expected: Value,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("approval-scopes").unwrap();
    barrier(&session).unwrap();
    let request = session.snapshot().requests.remove(0);
    assert_eq!(request.approval_response(decision), Some(expected.clone()));
    assert!(session.respond(&request.id, json!({"decision":{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["sh"]}}})).is_err());
    session.approve_decision(&request.id, decision).unwrap();
    assert_eq!(barrier(&session).unwrap()["approvals"], json!([expected]));
    assert!(session.approve_decision(&request.id, decision).is_err());
    session.send_prompt("approval-once-only").unwrap();
    barrier(&session).unwrap();
    let request = session.snapshot().requests.remove(0);
    for reusable in [
        bootty_agents::NativeApprovalDecision::AllowSession,
        bootty_agents::NativeApprovalDecision::AlwaysAllow,
    ] {
        assert_eq!(request.approval_response(reusable), None);
        assert!(session.approve_decision(&request.id, reusable).is_err());
    }
    session.approve(&request.id, false).unwrap();
}

#[rstest]
#[case::accept(true, json!({"action":"accept","content":{}}))]
#[case::decline(false, json!({"action":"decline","content":null}))]
fn mcp_confirmations_are_first_hand_once_only_decisions(
    #[case] allow: bool,
    #[case] expected: Value,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("mcp-approval").unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Waiting);
    assert_eq!(snapshot.error, None);
    assert_eq!(snapshot.requests.len(), 1);
    let request = &snapshot.requests[0];
    assert!(request.is_mcp_approval());
    assert!(session.approve("mcp-decision", allow).is_err());
    for response in [
        json!({"decision":"accept"}),
        json!({"action":"accept","content":{"invented":true}}),
        json!({"action":"acceptForSession"}),
        json!({"action":"decline","content":{}}),
        json!({"action":"accept","content":{},"_meta":{"grant":"always"}}),
    ] {
        assert!(session.respond(&request.id, response).is_err());
    }
    session.approve(&request.id, allow).unwrap();
    assert_eq!(barrier(&session).unwrap()["approvals"], json!([expected]));
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    assert!(session.approve(&request.id, allow).is_err());
}

#[rstest]
#[case::form(json!({"mode":"form","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}}))]
#[case::url(json!({"mode":"url","url":"https://example.com/authorize","elicitationId":"exact-identity"}))]
#[case::device(json!({"mode":"openai/userVerification","challenge":"device-proof"}))]
#[case::malformed(json!({"mode":"form","requestedSchema":{"type":"object","properties":{},"required":["unknown"]}}))]
#[case::invalid_required(json!({"mode":"form","requestedSchema":{"type":"object","properties":{},"required":"unknown"}}))]
fn mcp_input_requests_remain_pending_without_fabricating_content(
    #[case] parameters: Value,
    #[values("decline", "cancel")] action: &str,
    #[values("active", "complete")] initial_prompt: &str,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(initial_prompt).unwrap();
    let mut parameters = parameters;
    parameters["threadId"] = session.snapshot().session_id.into();
    parameters["turnId"] = session.snapshot().turn_id.into();
    fs::write(
        root.path().join("events.json"),
        json!([{"id":"mcp-input","method":"mcpServer/elicitation/request","params":parameters}])
            .to_string(),
    )
    .unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Waiting);
    assert_eq!(snapshot.error, None);
    let request = &snapshot.requests[0];
    assert!(!request.is_mcp_approval());
    assert!(session.approve(&request.id, true).is_err());
    assert_eq!(session.snapshot().requests.len(), 1);
    session
        .respond(&request.id, json!({"action":action}))
        .unwrap();
    assert_eq!(
        barrier(&session).unwrap()["approvals"],
        json!([{"action":action}])
    );
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
}

#[rstest]
fn mcp_approval_is_reported_as_attention_without_a_provider_failure() {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task("binding", "task", "Codex", config(root.path()).unwrap())
        .unwrap();
    service.prompt(&record.target(), "mcp-approval").unwrap();
    let activity = service.activity(&record.target()).unwrap();
    assert_eq!(activity.status, NativeSessionStatus::Waiting);
    assert!(activity.approval);
    assert!(!activity.input);
    service.stop(&record.target()).unwrap();
    assert_eq!(service.sessions()[0].snapshot.error, None);
}

#[rstest]
fn activity_reports_first_hand_attention_and_clears_it_when_stopped() {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task(
            "binding-captured",
            "task-captured",
            "Codex",
            config(root.path()).unwrap(),
        )
        .unwrap();
    service.prompt(&record.target(), "approval").unwrap();
    let activity = service.activities().remove(0);
    assert_eq!(activity.id, record.id);
    assert_eq!(activity.binding_id, "binding-captured");
    assert_eq!(activity.task_identity.as_deref(), Some("task-captured"));
    assert_eq!(activity.provider, AgentKind::Codex);
    assert_eq!(activity.status, NativeSessionStatus::Waiting);
    assert!(activity.approval);
    assert!(!activity.input);
    service.stop(&record.target()).unwrap();
    let stopped = service.activities().remove(0);
    assert_eq!(stopped.status, NativeSessionStatus::Stopped);
    assert!(!stopped.approval);
    assert!(!stopped.input);
    assert_eq!(service.sessions()[0].snapshot.error, None);
}

#[rstest]
#[case(1)]
#[case(16)]
#[case(32)]
fn recent_activity_is_bounded_and_remains_readable_after_stop(#[case] limit: usize) {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let record = service
        .create("captured", "Bounded activity", config(root.path()).unwrap())
        .unwrap();
    service.prompt(&record.target(), "bounded").unwrap();
    let observed = service.sessions().remove(0);
    let page = service.recent_activity(&record.target(), limit).unwrap();
    assert_eq!(page.id, record.id);
    assert_eq!(page.total, observed.snapshot.transcript.len());
    assert_eq!(page.items.len(), limit);
    assert_eq!(
        page.items.iter().map(|item| &item.id).collect::<Vec<_>>(),
        observed
            .snapshot
            .transcript
            .iter()
            .rev()
            .take(limit)
            .map(|item| &item.id)
            .collect::<Vec<_>>()
    );
    assert!(
        page.items
            .iter()
            .all(|item| item.text.len() <= 8192 && item.text_truncated)
    );
    let text = serde_json::to_string(&page).unwrap();
    assert!(!text.contains(root.path().to_str().unwrap()));
    assert!(text.len() < bootty_agents::MAX_TOOL_MESSAGE_BYTES);
    assert!(service.recent_activity(&record.target(), 0).is_err());
    assert!(service.recent_activity(&record.target(), 33).is_err());
    let mut stale = record.target();
    stale.generation = stale.generation.checked_add(1).unwrap();
    assert!(service.recent_activity(&stale, limit).is_err());
    service.stop(&record.target()).unwrap();
    drop(service);
    let restored = NativeAgentService::open(&path).unwrap();
    assert_eq!(
        restored
            .recent_activity(&record.target(), limit)
            .unwrap()
            .items[0]
            .text,
        page.items[0].text
    );
}

#[rstest]
fn failed_launch_retries_same_host_identity_and_preserves_provider_selector() {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    fs::write(root.path().join("fail-launch"), "").unwrap();
    let mut config = config(root.path()).unwrap();
    config.session_id = Some("retained-provider-thread".to_owned());
    assert!(service.create("captured-binding", "Codex", config).is_err());
    let failed = service.sessions().remove(0);
    assert_eq!(failed.snapshot.status, NativeSessionStatus::Error);
    assert_eq!(
        failed.config.session_id.as_deref(),
        Some("retained-provider-thread")
    );
    fs::remove_file(root.path().join("fail-launch")).unwrap();
    fs::write(root.path().join("wrong-thread"), "").unwrap();
    assert!(service.resume(&failed.target()).is_err());
    let wrong = service.sessions().remove(0);
    assert_eq!(wrong.id, failed.id);
    assert_eq!(wrong.config.session_id, failed.config.session_id);
    fs::remove_file(root.path().join("wrong-thread")).unwrap();
    let resumed = service.resume(&wrong.target()).unwrap();
    assert_eq!(resumed.id, failed.id);
    assert_eq!(
        resumed.snapshot.session_id.as_deref(),
        Some("retained-provider-thread")
    );
}

#[rstest]
fn failed_catalog_commit_keeps_prior_publication() {
    let root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service
        .create("binding", "Original", config(root.path()).unwrap())
        .unwrap();
    // A directory is a deterministic atomic-replace failure, without changing permissions.
    fs::remove_file(&catalog).unwrap();
    fs::create_dir(&catalog).unwrap();
    let revision = service.revision();
    assert!(service.rename(&record.target(), "Changed").is_err());
    assert_eq!(service.sessions()[0].title, "Original");
    assert_eq!(service.revision(), revision);
    assert!(
        service
            .configure_permissions(&record.target(), "supervised".parse().unwrap())
            .is_err()
    );
    assert_eq!(
        service.sessions()[0].config.permissions.id(),
        "provider-default"
    );
    assert_eq!(
        service.sessions()[0].snapshot.status,
        NativeSessionStatus::Idle
    );
    fs::remove_dir(&catalog).unwrap();
    service.checkpoint().unwrap();
}

#[rstest]
#[case::generated("Initial", "Generated title 🥟", true)]
#[case::manual_rename("Renamed by user", "Renamed by user", false)]
fn generated_titles_preserve_explicit_names_and_exact_conversation_membership(
    #[case] current: &str,
    #[case] expected: &str,
    #[case] applied: bool,
) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("unique-threads"), "").unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let first = service
        .create_for_task("binding", "task", "Initial", config(root.path()).unwrap())
        .unwrap();
    let sibling = service
        .create_for_task("binding", "task", "Sibling", config(root.path()).unwrap())
        .unwrap();
    service.rename(&first.target(), current).unwrap();
    assert_eq!(
        service
            .rename_if_unchanged(&first.target(), "Initial", "Generated title 🥟")
            .unwrap(),
        applied
    );
    service
        .prompt(&first.target(), "A different first prompt")
        .unwrap();
    let titles = |service: &NativeAgentService| {
        service
            .sessions()
            .into_iter()
            .map(|record| (record.id, record.title))
            .collect::<Vec<_>>()
    };
    let expected = vec![
        (first.id.clone(), expected.to_owned()),
        (sibling.id, "Sibling".to_owned()),
    ];
    assert_eq!(titles(&service), expected);
    let mut stale = first.target();
    stale.generation = 0;
    assert!(
        service
            .rename_if_unchanged(&stale, current, "Stale title")
            .is_err()
    );
    service.shutdown().unwrap();
    drop(service);
    let reopened = NativeAgentService::open(&catalog).unwrap();
    assert_eq!(titles(&reopened), expected);
}

#[rstest]
fn bounds_recent_transcript_across_all_stream_items() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("bounded").unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    assert!(snapshot.transcript.len() <= 256);
    assert!(
        snapshot
            .transcript
            .iter()
            .map(|item| item.text.len())
            .sum::<usize>()
            <= 1024 * 1024
    );
    assert!(snapshot.transcript.iter().any(|item| item.id == "long-95"));
    assert!(
        snapshot
            .transcript
            .iter()
            .all(|item| item.text.is_char_boundary(item.text.len()))
    );
}

proptest! {
    #[test]
    fn invalid_captured_profile_ids_are_rejected_before_provider_launch(
        prefix in "[a-zA-Z0-9_-]{0,10}",
        invalid in "[^a-zA-Z0-9_-]",
    ) {
        let root = TempDir::new().unwrap();
        let mut config = NativeSessionConfig::new(AgentKind::Codex, root.path());
        config.program = root.path().join("missing-provider").to_string_lossy().into_owned();
        config.account_directory = Some(root.path().to_string_lossy().into_owned());
        config.profile = Some(format!("{prefix}{invalid}"));
        prop_assert_eq!(
            NativeAgentSession::spawn(config).err().unwrap(),
            "Captured profile requires a simple bounded ID",
        );
    }

    #[test]
    fn native_launch_rejects_relative_account_stores(directory in "[a-zA-Z][a-zA-Z0-9_-]{0,30}") {
        let launch = AgentLaunch {program:"codex".to_owned(),cwd:Some("/tmp".to_owned()),arguments:Vec::new(),ephemeral:false,account_directory:Some(directory)};
        prop_assert!(NativeSessionConfig::from_launch(AgentKind::Codex,launch).is_err());
    }
}

#[rstest]
#[case::empty(String::new())]
#[case::oversized("a".repeat(65))]
fn captured_profile_ids_are_nonempty_and_bounded(#[case] profile: String) {
    let root = TempDir::new().unwrap();
    let mut config = NativeSessionConfig::new(AgentKind::Codex, root.path());
    config.program = root
        .path()
        .join("missing-provider")
        .to_string_lossy()
        .into_owned();
    config.account_directory = Some(root.path().to_string_lossy().into_owned());
    config.profile = Some(profile);
    assert_eq!(
        NativeAgentSession::spawn(config).err().unwrap(),
        "Captured profile requires a simple bounded ID",
    );
}

#[rstest]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn invalid_captured_provider_configuration_does_not_launch_a_terminal_fallback(
    #[case] provider: AgentKind,
) {
    let root = TempDir::new().unwrap();
    let mut config = config(root.path()).unwrap();
    config.provider = provider;
    assert!(NativeAgentSession::spawn(config).is_err());
    assert!(!root.path().join("launch.json").exists());
}

#[rstest]
fn interrupted_first_turn_cannot_be_released_by_later_success_or_resume_history() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let record = service
        .create_for_task("binding", "task", "Codex", config(root.path()).unwrap())
        .unwrap();
    assert_eq!(record.snapshot.first_turn, None);
    service.prompt(&record.target(), "interrupted").unwrap();
    let first = Some(NativeTurnReceipt {
        id: "turn-1".to_owned(),
        outcome: NativeTurnOutcome::Interrupted,
    });
    assert_eq!(service.activities()[0].first_turn, first);
    service.prompt(&record.target(), "completed").unwrap();
    let session = service.resolve(&record.target()).unwrap();
    // Provider events for old or foreign turns cannot rewrite the retained first outcome.
    session
        .rpc("__finish", json!({"status":"completed"}))
        .unwrap();
    barrier(&session).unwrap();
    service.checkpoint().unwrap();
    assert_eq!(service.activities()[0].completed_turn, true);
    assert_eq!(service.activities()[0].first_turn, first);
    service.stop(&record.target()).unwrap();
    service.shutdown().unwrap();
    drop(service);
    let service = NativeAgentService::open(&path).unwrap();
    let restored = service.sessions().remove(0);
    assert_eq!(restored.snapshot.first_turn, first);
    let resumed = service.resume(&restored.target()).unwrap();
    assert!(resumed.generation > restored.generation);
    assert_eq!(resumed.snapshot.first_turn, None);
    assert!(service.prompt(&record.target(), "completed").is_err());
    assert_eq!(service.activities()[0].first_turn, None);
    service.prompt(&resumed.target(), "completed").unwrap();
    assert_eq!(
        service.activities()[0]
            .first_turn
            .as_ref()
            .map(|receipt| receipt.outcome),
        Some(NativeTurnOutcome::Succeeded)
    );
}

#[rstest]
fn completed_first_turn_events_cannot_hide_a_mismatched_acceptance_acknowledgement() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    let result = session.send_prompt("mismatched-ack");
    assert!(result.is_err());
    assert_eq!(session.snapshot().status, NativeSessionStatus::Error);
    assert_eq!(
        session
            .snapshot()
            .first_turn
            .as_ref()
            .map(|receipt| receipt.id.as_str()),
        Some("turn-1")
    );
    session.stop();
}

#[rstest]
#[case::provider_rejection("reject-first", NativeSessionStatus::Error)]
#[case::validation_rejection("", NativeSessionStatus::Idle)]
fn prompt_failure_persists_observed_state_without_inventing_an_accepted_turn(
    #[case] prompt: &str,
    #[case] expected: NativeSessionStatus,
) {
    let root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service
        .create_for_task(
            "binding",
            "task",
            "Captured title",
            config(root.path()).unwrap(),
        )
        .unwrap();
    let error = service.prompt(&record.target(), prompt).unwrap_err();
    let observed = service.resolve(&record.target()).unwrap().snapshot();
    let published = service.sessions().remove(0);
    assert_eq!(published.target(), record.target());
    assert_eq!(published.title, "Captured title");
    assert_eq!(published.snapshot, observed);
    assert_eq!(published.snapshot.status, expected);
    assert_eq!(published.snapshot.first_turn, None);
    assert!(!published.snapshot.completed_turn);
    if expected == NativeSessionStatus::Error {
        assert!(error.contains("captured first prompt rejected"));
        assert_eq!(published.snapshot.error.as_deref(), Some(error.as_str()));
    } else {
        assert_eq!(published.snapshot.error, None);
    }
    let durable: Value = serde_json::from_slice(&fs::read(&catalog).unwrap()).unwrap();
    assert_eq!(
        durable["records"][0]["snapshot"],
        serde_json::to_value(&published.snapshot).unwrap()
    );
    assert_eq!(
        durable["records"][0]["config"]["session_id"],
        "native-thread"
    );
    drop(service);
    let reopened = NativeAgentService::open(&catalog).unwrap();
    let saved = reopened.sessions().remove(0);
    assert_eq!(saved.target(), record.target());
    assert_eq!(saved.snapshot.error, published.snapshot.error);
    assert_eq!(saved.snapshot.first_turn, None);
    assert!(!saved.snapshot.completed_turn);
    assert_eq!(saved.task_identity.as_deref(), Some("task"));
    assert_eq!(saved.config.session_id.as_deref(), Some("native-thread"));
    reopened.shutdown().unwrap();
}

#[rstest]
#[case::known(
    Some(1_791_224_700),
    Some(1_791_224_710),
    Some(1_791_224_700_000),
    Some(1_791_224_710_000)
)]
#[case::unknown(None, None, None, None)]
#[case::overflow(Some(i64::MAX), Some(i64::MAX), None, None)]
fn restored_codex_times_come_from_the_saved_turn(
    #[case] started: Option<i64>,
    #[case] completed: Option<i64>,
    #[case] created_at: Option<i64>,
    #[case] updated_at: Option<i64>,
) {
    let root = TempDir::new().unwrap();
    let history = json!([{
        "id":"prior","startedAt":started,"completedAt":completed,
        "items":[{"id":"saved-answer","type":"agentMessage","text":"Earlier answer"}]
    }]);
    fs::write(
        root.path().join("history-timestamps.json"),
        history.to_string(),
    )
    .unwrap();
    let mut config = config(root.path()).unwrap();
    config.session_id = Some("native-thread".to_owned());
    let session = NativeAgentSession::spawn(config).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.transcript.len(), 1);
    assert_eq!(
        (
            snapshot.transcript[0].created_at,
            snapshot.transcript[0].updated_at
        ),
        (created_at, updated_at)
    );
    session.stop();
}

#[rstest]
#[case(true)]
#[case(false)]
fn side_chat_placement_precedes_provider_start_and_retains_failed_identity(
    #[case] reject_placement: bool,
) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("unique-threads"), "").unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let parent = service
        .create_for_task("binding", "task", "Parent", config(root.path()).unwrap())
        .unwrap();
    service
        .prompt(&parent.target(), "Completed response")
        .unwrap();
    let published = std::cell::RefCell::new(None);
    let result = service.fork_side_chat_placed(&parent.target(), None, None, |child| {
        *published.borrow_mut() = Some(child.target());
        assert_eq!(child.snapshot.status, NativeSessionStatus::Starting);
        if reject_placement {
            Err("native placement rejected".into())
        } else {
            fs::write(root.path().join("fail-launch"), "").unwrap();
            Ok(())
        }
    });
    assert!(result.is_err());
    let target = published.into_inner().unwrap();
    let child = service
        .sessions()
        .into_iter()
        .find(|record| record.target() == target)
        .unwrap();
    assert_eq!(child.snapshot.status, NativeSessionStatus::Error);
    assert_eq!(child.side_chat.unwrap().source_id, parent.id);
    assert_eq!(child.task_identity, parent.task_identity);
    assert!(
        child
            .snapshot
            .transcript
            .iter()
            .any(|item| item.text == "Completed response")
    );
    assert!(service.resolve(&target).is_err());
    assert_eq!(
        service.resolve(&parent.target()).unwrap().snapshot().status,
        NativeSessionStatus::Idle
    );
}

#[rstest]
fn side_chat_copies_a_completed_response_without_reusing_the_parent_provider_session() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("unique-threads"), "").unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let parent = service
        .create_for_task("binding", "task", "Parent", config(root.path()).unwrap())
        .unwrap();
    assert!(
        service
            .fork_side_chat(&parent.target(), None, None)
            .is_err()
    );
    assert_eq!(service.sessions().len(), 1);
    let completed = service.prompt(&parent.target(), "First response").unwrap();
    let response = completed
        .transcript
        .iter()
        .find(|item| item.role == "assistant" && item.complete)
        .unwrap()
        .id
        .clone();
    service.prompt(&parent.target(), "Later response").unwrap();
    let before = service
        .sessions()
        .into_iter()
        .find(|record| record.id == parent.id)
        .unwrap();
    let child = service
        .fork_side_chat_placed(&parent.target(), Some(&response), None, |reserved| {
            assert_eq!(reserved.snapshot.status, NativeSessionStatus::Starting);
            assert!(reserved.snapshot.session_id.is_none());
            assert!(service.resolve(&reserved.target()).is_err());
            assert!(
                service
                    .sessions()
                    .iter()
                    .any(|record| record.target() == reserved.target())
            );
            Ok(())
        })
        .unwrap();
    assert_ne!(child.snapshot.session_id, parent.snapshot.session_id);
    assert_eq!(child.task_identity, parent.task_identity);
    assert_eq!(
        child.config.account_directory,
        parent.config.account_directory
    );
    assert_eq!(child.side_chat.as_ref().unwrap().source_id, parent.id);
    assert_eq!(
        child.snapshot.requests,
        Vec::<bootty_agents::NativeAgentRequest>::new()
    );
    assert!(
        !child
            .snapshot
            .transcript
            .iter()
            .any(|item| item.text == "Later response")
    );
    service
        .prompt(&child.target(), "Independent question")
        .unwrap();
    let identity = child.snapshot.session_id.as_ref().unwrap();
    let first: Value =
        serde_json::from_slice(&fs::read(root.path().join(format!("{identity}-1.json"))).unwrap())
            .unwrap();
    let text = first["input"][0]["text"].as_str().unwrap();
    assert!(text.contains("First response"));
    assert!(!text.contains("Later response"));
    service.prompt(&child.target(), "Follow up").unwrap();
    let second: Value =
        serde_json::from_slice(&fs::read(root.path().join(format!("{identity}-2.json"))).unwrap())
            .unwrap();
    assert_eq!(second["input"][0]["text"], "Follow up");
    let copied = child.side_chat.as_ref().unwrap().transcript.clone();
    let long = service.prompt(&child.target(), "bounded").unwrap();
    assert!(
        long.transcript
            .iter()
            .all(|item| !item.id.starts_with("fork:"))
    );
    assert_eq!(
        service
            .sessions()
            .into_iter()
            .find(|record| record.id == child.id)
            .unwrap()
            .side_chat
            .unwrap()
            .transcript,
        copied
    );
    let after = service
        .sessions()
        .into_iter()
        .find(|record| record.id == parent.id)
        .unwrap();
    assert_eq!(after.snapshot.transcript, before.snapshot.transcript);
    assert_eq!(after.snapshot.session_id, before.snapshot.session_id);
    service.shutdown().unwrap();
    let restored = NativeAgentService::open(&path).unwrap();
    let saved = restored
        .sessions()
        .into_iter()
        .find(|record| record.id == child.id)
        .unwrap();
    assert_eq!(
        saved.side_chat.as_ref().unwrap().seeded_identity.as_deref(),
        Some(identity.as_str())
    );
    assert_eq!(saved.side_chat.as_ref().unwrap().transcript, copied);
    restored.resume(&child.target()).unwrap();
    let resumed = restored
        .sessions()
        .into_iter()
        .find(|record| record.id == child.id)
        .unwrap();
    assert_eq!(resumed.side_chat.as_ref().unwrap().transcript, copied);
    restored
        .prompt(&resumed.target(), "After relaunch")
        .unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join(format!("{identity}-1.json"))).unwrap())
            .unwrap();
    assert_eq!(wire["input"][0]["text"], "After relaunch");
    restored.shutdown().unwrap();
}

#[rstest]
#[case::copied_response(false, "exact")]
#[case::recent_response(true, "exact")]
#[case::altered_echo(true, "altered")]
#[case::foreign_echo(true, "foreign")]
#[case::unowned_metadata(true, "metadata")]
fn nested_side_chat_retains_copied_context_after_live_history_eviction(
    #[case] recent: bool,
    #[case] echo_mode: &str,
) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("unique-threads"), "").unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let parent = service
        .create_for_task("binding", "task", "Parent", config(root.path()).unwrap())
        .unwrap();
    service
        .prompt(&parent.target(), "Original context")
        .unwrap();
    let child = service
        .fork_side_chat(&parent.target(), None, None)
        .unwrap();
    let copied = child.side_chat.as_ref().unwrap().transcript.clone();
    service
        .prompt(&child.target(), "Seed copied history")
        .unwrap();
    let evicted = service.prompt(&child.target(), "bounded").unwrap();
    assert!(
        evicted
            .transcript
            .iter()
            .all(|item| !item.id.starts_with("fork:"))
    );
    let recent_history = service.prompt(&child.target(), "Recent context").unwrap();
    let boundary = if recent {
        recent_history
            .transcript
            .iter()
            .rev()
            .find(|item| item.role == "assistant" && item.complete)
            .unwrap()
    } else {
        copied.last().unwrap()
    };
    let grandchild = service
        .fork_side_chat(&child.target(), Some(&boundary.id), None)
        .unwrap();
    let history = &grandchild.side_chat.as_ref().unwrap().transcript;
    assert_eq!(history.last().unwrap().text, boundary.text);
    assert!(history.iter().any(|item| item.text == "Original context"));
    assert_eq!(
        history.iter().any(|item| item.text == "Recent context"),
        recent
    );
    fs::write(root.path().join("history-echo-mode"), echo_mode).unwrap();
    let result = service.prompt(&grandchild.target(), "Grandchild question");
    if echo_mode != "exact" {
        assert!(result.is_err());
        assert!(
            service
                .resolve(&grandchild.target())
                .unwrap()
                .snapshot()
                .error
                .is_some()
        );
        service.shutdown().unwrap();
        return;
    }
    result.unwrap();
    let identity = grandchild.snapshot.session_id.as_ref().unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join(format!("{identity}-1.json"))).unwrap())
            .unwrap();
    let text = wire["input"][0]["text"].as_str().unwrap();
    assert!(text.contains("Original context"));
    assert_eq!(text.contains("Recent context"), recent);
    service.shutdown().unwrap();
}

#[rstest]
#[case::exact("exact", true)]
#[case::altered("altered", false)]
#[case::foreign("foreign", false)]
fn copied_response_quotes_validate_after_live_history_eviction(
    #[case] kind: &str,
    #[case] accepted: bool,
) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("unique-threads"), "").unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let parent = service
        .create_for_task("binding", "task", "Parent", config(root.path()).unwrap())
        .unwrap();
    service
        .prompt(&parent.target(), "Original context")
        .unwrap();
    let child = service
        .fork_side_chat(&parent.target(), None, None)
        .unwrap();
    let source = child.side_chat.as_ref().unwrap().transcript.last().unwrap();
    service
        .prompt(&child.target(), "Seed copied history")
        .unwrap();
    let evicted = service.prompt(&child.target(), "bounded").unwrap();
    assert!(evicted.transcript.iter().all(|item| item.id != source.id));
    let citation = bootty_agents::NativeResponseCitation {
        message_id: if kind == "foreign" {
            "another-conversation".into()
        } else {
            source.id.clone()
        },
        source_range: 0..source.text.len(),
        prompt_range: None,
        quote: if kind == "altered" {
            "wrong".into()
        } else {
            source.text.clone()
        },
        comment: "Clarify".into(),
    };
    let prompt = bootty_agents::NativePrompt::new_with_context(
        "Clarify the quote".into(),
        vec![],
        bootty_agents::NativePromptAttachments::default(),
        vec![citation.clone()],
    )
    .unwrap();
    let result = service.prompt_input(&child.target(), &prompt);
    assert_eq!(result.is_ok(), accepted);
    if let Ok(snapshot) = result {
        assert!(
            snapshot
                .transcript
                .iter()
                .any(|item| item.role == "user" && item.citations == [citation.clone()])
        );
    }
    service.shutdown().unwrap();
}

#[rstest]
fn codex_child_lifecycle_outlives_parent_turn_and_history_reads_do_not_take_a_writer() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("active").unwrap();
    let event = |thread: &str, kind: &str, item: Value| json!({"method":kind,"params":{"threadId":thread,"turnId":"turn-1","item":item}});
    fs::write(root.path().join("events.json"), json!([
        event("foreign","item/started",json!({"type":"collabAgentToolCall","tool":"spawnAgent","receiverThreadIds":["foreign-child"],"prompt":"Wrong"})),
        event("native-thread","item/started",json!({"type":"collabAgentToolCall","tool":"spawnAgent","receiverThreadIds":["child-thread"],"prompt":"Review this change","model":"gpt-6-sol","agentsStates":{"child-thread":{"status":"running"}}})),
    ]).to_string()).unwrap();
    session.rpc("__barrier", json!({})).unwrap();
    let running = session.snapshot();
    let agents = running
        .transcript
        .iter()
        .filter_map(|item| item.subagent.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].status, NativeToolStatus::Running);
    session
        .rpc("__finish", json!({"status":"completed"}))
        .unwrap();
    fs::write(root.path().join("events.json"), json!([
        event("native-thread","item/completed",json!({"type":"collabAgentToolCall","tool":"wait","receiverThreadIds":["child-thread","unreported-child"],"agentsStates":{"child-thread":{"status":"completed","message":"Reviewed"},"unreported-child":{"status":"running"}}})),
        event("native-thread","item/completed",json!({"type":"subAgentActivity","kind":"interacted","agentThreadId":"child-thread"})),
    ]).to_string()).unwrap();
    session.rpc("__barrier", json!({})).unwrap();
    let before = session.snapshot();
    let child = before
        .transcript
        .iter()
        .find(|item| item.subagent.is_some())
        .unwrap();
    assert_eq!(child.text, "Reviewed");
    assert_eq!(
        child.subagent.as_ref().unwrap().status,
        NativeToolStatus::Completed
    );
    assert!(session.read_subagent("unreported-child").is_err());
    let detail = session.read_subagent("child-thread").unwrap();
    assert_eq!(detail.transcript.last().unwrap().text, "Child review");
    assert_eq!(session.snapshot().transcript, before.transcript);
    assert!(!root.path().join("child-thread.writer").exists());
    let request: Value =
        serde_json::from_slice(&fs::read(root.path().join("read.json")).unwrap()).unwrap();
    assert_eq!(request["method"], "thread/read");
    assert_eq!(request["params"]["threadId"], "child-thread");
    session.stop();
}

#[rstest]
fn codex_compaction_uses_the_provider_operation_and_waits_for_completion() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("/compact").unwrap();
    barrier(&session).unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Working);
    assert_eq!(
        session.snapshot().transcript,
        Vec::<bootty_agents::NativeTranscriptItem>::new()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(
            &fs::read(root.path().join("compact-request.json")).unwrap()
        )
        .unwrap(),
        json!({"threadId":"native-thread"})
    );
    assert!(
        session
            .send_prompt("/compact")
            .unwrap_err()
            .contains("Wait")
    );
    session.rpc("__finish_compaction", json!({})).unwrap();
    barrier(&session).unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    session.stop();
}

#[rstest]
fn codex_compaction_rejects_additional_instructions_without_starting_work() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    assert!(
        session
            .send_prompt("/compact focus on tests")
            .unwrap_err()
            .contains("additional instructions")
    );
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    assert!(!root.path().join("compact-request.json").exists());
    session.stop();
}

#[rstest]
#[case(false, false)]
#[case(true, false)]
#[case(false, true)]
#[case(true, true)]
fn missing_codex_rollouts_recreate_only_empty_reservations(
    #[case] fork: bool,
    #[case] accepted_prompt: bool,
) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("unique-threads"), "").unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let parent = service
        .create_for_task("binding", "task", "Parent", config(root.path()).unwrap())
        .unwrap();
    let selected = if fork {
        service.prompt(&parent.target(), "Source context").unwrap();
        service
            .fork_side_chat(&parent.target(), None, None)
            .unwrap()
    } else {
        parent
    };
    if accepted_prompt {
        service
            .prompt(&selected.target(), "Accepted prompt")
            .unwrap();
    }
    service.shutdown().unwrap();
    fs::write(root.path().join("missing-rollout"), "").unwrap();
    let reopened = NativeAgentService::open(&path).unwrap();
    let result = reopened.resume(&selected.target());
    let saved = reopened
        .sessions()
        .into_iter()
        .find(|record| record.id == selected.id)
        .unwrap();
    assert_eq!(result.is_ok(), !accepted_prompt);
    assert_eq!(saved.id, selected.id);
    if accepted_prompt {
        assert_eq!(saved.config.session_id, selected.config.session_id);
        assert!(
            saved
                .snapshot
                .transcript
                .iter()
                .any(|item| item.role == "user" && !item.id.starts_with("fork:"))
        );
    } else {
        assert_ne!(saved.config.session_id, selected.config.session_id);
        assert_eq!(saved.snapshot.transcript, selected.snapshot.transcript);
        assert_eq!(saved.snapshot.status, NativeSessionStatus::Idle);
    }
    reopened.shutdown().unwrap();
}

#[rstest]
#[case("provider-default", json!({}))]
#[case("supervised", json!({"approvalPolicy":"untrusted","approvalsReviewer":"user","sandboxPolicy":{"type":"readOnly"}}))]
#[case("auto-accept-edits", json!({"approvalPolicy":"on-request","approvalsReviewer":"user","sandboxPolicy":{"type":"workspaceWrite"}}))]
#[case("auto", json!({"approvalPolicy":"on-request","approvalsReviewer":"auto_review","sandboxPolicy":{"type":"workspaceWrite"}}))]
#[case("full-access", json!({"approvalPolicy":"never","approvalsReviewer":"user","sandboxPolicy":{"type":"dangerFullAccess"}}))]
fn codex_receives_the_selected_permission_policy(#[case] mode: &str, #[case] expected: Value) {
    let root = TempDir::new().unwrap();
    let mut config = config(root.path()).unwrap();
    config.permissions = mode.parse().unwrap();
    let session = NativeAgentSession::spawn(config).unwrap();
    session.send_prompt("hello").unwrap();
    barrier(&session).unwrap();
    let parameters: Value =
        serde_json::from_slice(&fs::read(root.path().join("turn-policy.json")).unwrap()).unwrap();
    for field in ["approvalPolicy", "approvalsReviewer", "sandboxPolicy"] {
        assert_eq!(parameters.get(field), expected.get(field));
    }
}

#[rstest]
fn changing_permissions_preserves_identity_without_replaying_a_prompt() {
    let root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service
        .create("binding", "Task", config(root.path()).unwrap())
        .unwrap();
    service.prompt(&record.target(), "active").unwrap();
    let before = service.sessions().remove(0);
    service
        .configure_permissions(&record.target(), "supervised".parse().unwrap())
        .unwrap();
    let queued = service.sessions().remove(0);
    assert_eq!(queued.target(), before.target());
    assert_eq!(queued.snapshot.status, before.snapshot.status);
    assert_eq!(queued.snapshot.requests, before.snapshot.requests);
    assert!(queued.permissions_pending);
    assert_eq!(queued.config.permissions.id(), "supervised");
    service.interrupt(&record.target()).unwrap();
    service.stop(&record.target()).unwrap();
    service
        .configure_permissions(&record.target(), "supervised".parse().unwrap())
        .unwrap();
    let saved = service.sessions().remove(0);
    let before = fs::read(root.path().join("turn-policy.json")).unwrap();
    let resumed = service.resume(&saved.target()).unwrap();
    assert_eq!(resumed.id, saved.id);
    assert_eq!(resumed.config.session_id, saved.config.session_id);
    assert_eq!(resumed.config.permissions.id(), "supervised");
    assert!(resumed.generation > saved.generation);
    assert_eq!(
        fs::read(root.path().join("turn-policy.json")).unwrap(),
        before
    );
    assert!(
        service
            .configure_permissions(&saved.target(), "full-access".parse().unwrap())
            .is_err()
    );
    service.stop(&resumed.target()).unwrap();
    let reopened = NativeAgentService::open(&catalog).unwrap();
    assert_eq!(reopened.sessions()[0].config.permissions.id(), "supervised");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4))]
    #[test]
    fn file_changes_preserve_large_unicode_patch_json(payload in "[a-z界🦀]{18000,22000}") {
        let root = TempDir::new().unwrap();
        let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
        session.send_prompt("active").unwrap();
        let snapshot = session.snapshot();
        let changes = json!([{
            "path": "source.txt", "kind": { "type": "update", "move_path": null },
            "diff": format!("@@ -1,1 +1,1 @@\n-before\n+{payload}\n"),
        }]);
        let item = json!({ "id": "large-file-change", "type": "fileChange", "changes": changes, "status": "completed" });
        fs::write(root.path().join("events.json"), serde_json::to_vec(&json!([
            { "method": "item/completed", "params": {
                "threadId": snapshot.session_id, "turnId": snapshot.turn_id, "item": item,
            }},
        ])).unwrap()).unwrap();
        barrier(&session).unwrap();
        let snapshot = session.snapshot();
        let input = &snapshot.transcript.iter().find(|item| item.id == "large-file-change")
            .unwrap().tool.as_ref().unwrap().input;
        let actual: Value = serde_json::from_str(input).expect("the public transcript keeps valid patch JSON");
        assert_eq!(actual, changes);
        prop_assert!(snapshot.transcript.iter().map(|item| item.text.len().saturating_add(
            item.tool.as_ref().map_or(0, |tool| tool.input.len().saturating_add(tool.name.len()))
        )).fold(0_usize, usize::saturating_add) <= 1024 * 1024);
    }
}
