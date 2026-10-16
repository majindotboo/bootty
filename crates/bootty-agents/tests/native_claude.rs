#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use assert_fs::TempDir;
use bootty_agents::{
    AgentKind, AgentLaunch, NativeAgentService, NativeAgentSession, NativeApprovalDecision,
    NativeSessionConfig, NativeSessionStatus, NativeTodoStatus, NativeToolStatus,
    NativeTurnOutcome,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

const SESSION: &str = "01234567-89ab-4cde-8123-456789abcdef";

#[expect(
    clippy::too_many_lines,
    reason = "One provider script replays every observed Claude sequence"
)]
fn fixture(root: &Path) -> std::io::Result<String> {
    let path = root.join("claude-stream.py");
    fs::write(
        &path,
        r#"#!/usr/bin/env python3
import json,os,sys,time
assert sys.argv[sys.argv.index('--input-format')+1]=='stream-json'
assert sys.argv[sys.argv.index('--output-format')+1]=='stream-json'
assert sys.argv[sys.argv.index('--permission-prompt-tool')+1]=='stdio'
assert '--replay-user-messages' in sys.argv
session=sys.argv[sys.argv.index('--resume' if '--resume' in sys.argv else '--session-id')+1]
with open('launch.json','w') as output: json.dump({'argv':sys.argv[1:],'account':os.environ.get('CLAUDE_CONFIG_DIR'),'pid':os.getpid()},output)
def emit(value): print(json.dumps(value),flush=True)
def control(value): emit({'type':'control_response','response':{'subtype':'success','request_id':value['request_id'],'response':{'models':[],'account':{},'commands':[{'name':'review','description':'Review changes','argumentHint':'[path]'}]}}})
def result(reason='completed',subtype='success',identity=None,errors=None,**extra):
 emit({'type':'result','subtype':subtype,'session_id':identity or session,'uuid':'result-uuid-not-user-uuid','is_error':subtype!='success','terminal_reason':reason,'result':'Hello world','errors':errors or [],**extra})
def event(value): emit({'type':'stream_event','session_id':session,'parent_tool_use_id':None,'event':value})
def tool_use(ident,command): emit({'type':'assistant','uuid':'block-'+ident,'session_id':session,'parent_tool_use_id':None,'message':{'id':'message-'+ident,'content':[{'type':'tool_use','id':ident,'name':'Bash','input':{'command':command}}]}})
def tool_call(ident,name,arguments): emit({'type':'assistant','uuid':'block-'+ident,'session_id':session,'parent_tool_use_id':None,'message':{'id':'message-'+ident,'content':[{'type':'tool_use','id':ident,'name':name,'input':arguments}]}})
def user_event(content,**extra): emit({'type':'user','session_id':session,'parent_tool_use_id':None,'message':{'role':'user','content':content},**extra})
running=None
for line in sys.stdin:
 value=json.loads(line)
 if value['type']=='control_request':
  if value['request']['subtype']=='stop_task':
   # Claude acknowledges a stop, then reports the task stopped.
   control(value)
   emit({'type':'system','subtype':'task_notification','session_id':session,'task_id':value['request']['task_id'],'status':'stopped','summary':'Stopped'})
   continue
  if value['request']['subtype']=='interrupt':
   # An idle Claude acknowledges an interrupt and reports nothing else.
   if os.path.exists('ignore-interrupt'): control(value);continue
   if running:
    # Claude 2.1.296's observed order: acknowledge, reject the running tool, end the turn.
    control(value)
    user_event([{'type':'tool_result','tool_use_id':running,'content':"The user doesn't want to proceed with this tool use.",'is_error':True}],tool_result_meta=[{'id':running,'non_execution_kind':'user-rejected'}])
    user_event([{'type':'text','text':'[Request interrupted by user]'}])
    result('aborted_streaming','error_during_execution');running=None;continue
   result('aborted_streaming')
  control(value)
 elif value['type']=='user':
  prompt=value['message']['content']
  steer=value.get('priority')=='next'
  if steer and running:
   with open('prompt.json','w') as output: json.dump(value,output)
   # Observed from Claude 2.1.300: the running tool finishes, Claude replays the steer right
   # after its result, and the same turn continues to one result.
   user_event([{'type':'tool_result','tool_use_id':running,'content':'one','is_error':False}])
   # Claude downscales large images and echoes them re-encoded as JPEG.
   echo=dict(value);echo['isReplay']=True
   if isinstance(prompt,list): echo['message']=dict(value['message'],content=[dict(block,source=dict(block['source'],media_type='image/jpeg')) if block['type']=='image' else block for block in prompt])
   emit(echo)
   running='steer-call';tool_use(running,'sleep 20');continue
  with open('prompt.json','w') as output: json.dump(value,output)
  if prompt=='oversize': sys.stdout.write('x'*(1024*1024+1)+'\n');sys.stdout.flush();continue
  if prompt=='partial': sys.stdout.write('{"type":"result"}');sys.stdout.flush();break
  emit({'type':'system','subtype':'init','session_id':'foreign' if prompt=='wrong-session' else session,'cwd':'/foreign' if prompt=='wrong-cwd' else os.getcwd()})
  result(identity='foreign')
  result() # No user ACK yet: never a completed turn.
  if prompt=='no-ack': continue
  if prompt=='slow-ack':
   # The test lets Claude confirm this message only after it sends another.
   while not os.path.exists('ack'): time.sleep(0.01)
  echo=dict(value);echo['isReplay']=True;echo['message']={'role':'user','content':[{'type':'text','text':prompt}]};emit(echo)
  if prompt in ['permission-rules','permission-mode']:
   # Observed from Claude 2.1.299: Bash suggests a reusable rule; Write suggests accepting edits.
   suggestion={'type':'addRules','rules':[{'toolName':'Bash','ruleContent':'npm view *'}],'behavior':'allow','destination':'localSettings'} if prompt=='permission-rules' else {'type':'setMode','mode':'acceptEdits','destination':'session'}
   emit({'type':'control_request','request_id':'permission-id','request':{'subtype':'can_use_tool','tool_name':'Bash','tool_use_id':'tool-id','input':{'command':'npm view left-pad version'},'permission_suggestions':[suggestion]}})
   continue
  if prompt in ['permission','cancel','question']:
   emit({'type':'control_request','request_id':'permission-id','request':{'subtype':'can_use_tool','tool_name':'Bash','tool_use_id':'tool-id','input':{'command':'echo hello'}}})
   if prompt=='question':
    emit({'type':'control_cancel_request','request_id':'permission-id'})
    emit({'type':'control_request','request_id':'question-id','request':{'subtype':'can_use_tool','tool_name':'AskUserQuestion','tool_use_id':'question-tool','input':{'questions':[{'question':'Choose color','options':[{'label':'Red'},{'label':'Blue'}],'multiSelect':True}],'marker':'preserve'}}})
   if prompt=='cancel': emit({'type':'control_cancel_request','request_id':'permission-id'})
   continue
  if prompt=='hold': continue
  if prompt=='fail-later':
   # The test fails this turn once it has queued messages behind it.
   while not os.path.exists('fail'): time.sleep(0.01)
   result('model_error','error_during_execution');continue
  if prompt.startswith('write-'):
   emit({'type':'control_request','request_id':'write-id','request':{'subtype':'can_use_tool','tool_name':'Write','tool_use_id':'write-tool','input':{'file_path':'/tmp/large','content':'x'*int(prompt[6:])}}})
   continue
  if prompt=='flood':
   for i in range(300):
    emit({'type':'assistant','uuid':'think-'+str(i),'session_id':session,'parent_tool_use_id':None,'message':{'id':'think-message-'+str(i),'content':[{'type':'thinking','thinking':''}]}})
    emit({'type':'assistant','uuid':'flood-'+str(i),'session_id':session,'parent_tool_use_id':None,'message':{'id':'flood-message-'+str(i),'content':[{'type':'tool_use','id':'flood-call-'+str(i),'name':'Bash','input':{'command':'true'}}]}})
    emit({'type':'user','session_id':session,'parent_tool_use_id':None,'message':{'role':'user','content':[{'type':'tool_result','tool_use_id':'flood-call-'+str(i),'content':[{'type':'text','text':'ok'}],'is_error':False}]}})
  if prompt=='edit':
   # Observed from Claude 2.1.299: an Edit result carries Claude's own patch.
   emit({'type':'assistant','uuid':'block-edit','session_id':session,'parent_tool_use_id':None,'message':{'id':'message-edit','content':[{'type':'tool_use','id':'edit-call','name':'Edit','input':{'file_path':'/tmp/project/notes.txt','old_string':'line two','new_string':'line 2'}}]}})
   user_event([{'type':'tool_result','tool_use_id':'edit-call','content':'Edited'}],tool_use_result={'filePath':'/tmp/project/notes.txt','oldString':'line two','newString':'line 2','structuredPatch':[{'oldStart':1,'oldLines':3,'newStart':1,'newLines':3,'lines':[' line one','-line two','+line 2',' line three']}]})
   result();continue
  if prompt in ['tool-hold','slow-ack']:
   running='bash-call';tool_use(running,'sleep 60');continue
  if prompt=='tool-stream':
   # Observed from Claude 2.1.299: arguments arrive as partial JSON after an empty input.
   event({'type':'message_start','message':{'id':'stream-tool-message'}})
   event({'type':'content_block_start','index':0,'content_block':{'type':'tool_use','id':'stream-call','name':'Read','input':{}}})
   for part in ['','{"file_path": "/private/t','mp/notes']:
    event({'type':'content_block_delta','index':0,'delta':{'type':'input_json_delta','partial_json':part}})
   continue
  event({'type':'message_start','message':{'id':'assistant-id'}})
  event({'type':'content_block_start','index':0,'content_block':{'type':'text','text':''}})
  event({'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'Hello '}})
  event({'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'world'}})
  emit({'type':'assistant','uuid':'block-id','session_id':session,'parent_tool_use_id':None,'message':{'id':'assistant-id','content':[{'type':'text','text':'Hello world'}]}})
  event({'type':'content_block_stop','index':0})
  if prompt=='usage':
   # Observed from Claude 2.1.299: each model call reports its usage; the result reports the window.
   emit({'type':'assistant','uuid':'usage-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'usage-message','content':[{'type':'text','text':'Counted'}],'usage':{'input_tokens':2,'cache_creation_input_tokens':900,'cache_read_input_tokens':24000,'output_tokens':40}}})
   result(modelUsage={'claude-haiku-5-5':{'contextWindow':200000},'claude-opus-5-5':{'contextWindow':1000000}},usage={'input_tokens':5,'cache_creation_input_tokens':900,'cache_read_input_tokens':48000,'output_tokens':95},total_cost_usd=0.0421)
   continue
  if prompt=='read-image':
   # Observed from Claude 2.1.299: a Read of an image repeats its base64 in tool_use_result.
   import base64,struct,zlib
   def chunk(kind,body): return struct.pack('>I',len(body))+kind+body+struct.pack('>I',zlib.crc32(kind+body))
   png=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',1,1,8,6,0,0,0))+chunk(b'tEXt',b'pad\0'+b'x'*500000)+chunk(b'IDAT',zlib.compress(b'\0\1\2\3\377'))+chunk(b'IEND',b'')
   data=base64.b64encode(png).decode()
   emit({'type':'assistant','uuid':'read-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'read-message','content':[{'type':'tool_use','id':'read-call','name':'Read','input':{'file_path':'shot.png'}}]}})
   emit({'type':'user','session_id':session,'parent_tool_use_id':None,'message':{'role':'user','content':[{'type':'tool_result','tool_use_id':'read-call','content':[{'type':'image','source':{'type':'base64','data':data,'media_type':'image/png'}}]}]},'tool_use_result':{'type':'image','file':{'base64':data,'type':'image/png'}}})
  if prompt=='big-output':
   emit({'type':'assistant','uuid':'big-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'big-message','content':[{'type':'tool_use','id':'read-call','name':'Bash','input':{'command':'cat big.log'}}]}})
   emit({'type':'user','session_id':session,'parent_tool_use_id':None,'message':{'role':'user','content':[{'type':'tool_result','tool_use_id':'read-call','content':[{'type':'text','text':'z'*(2*1024*1024)}]}]}})
  if prompt=='huge-line':
   emit({'type':'assistant','uuid':'huge-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'huge-message','content':[{'type':'text','text':'y'*(17*1024*1024)}]}})
  if prompt in ['tool-success','tool-failure']:
   emit({'type':'assistant','uuid':'tool-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'tool-message','content':[{'type':'tool_use','id':'bash-call','name':'Bash','input':{'command':'echo hello'}}]}})
   emit({'type':'user','session_id':session,'parent_tool_use_id':None,'message':{'role':'user','content':[{'type':'tool_result','tool_use_id':'bash-call','content':[{'type':'text','text':'observed output'}],'is_error':prompt=='tool-failure'}]}})
  if prompt=='limits':
   emit({'type':'rate_limit_event','rate_limit_info':{'status':'allowed_warning','rateLimitType':'five_hour','resetsAt':1760000000,'utilization':0.91},'uuid':'limit-1','session_id':session})
   emit({'type':'rate_limit_event','rate_limit_info':{'status':'rejected','rateLimitType':'seven_day_opus','resetsAt':1760500000},'uuid':'limit-2','session_id':session})
  if prompt=='compact':
   # Observed from Claude: `compacting` repeats while it works, then a boundary reports the sizes.
   emit({'type':'system','subtype':'status','status':'compacting','session_id':session})
   emit({'type':'system','subtype':'status','status':'compacting','session_id':session})
   emit({'type':'system','subtype':'compact_boundary','compact_metadata':{'trigger':'manual','pre_tokens':180000,'post_tokens':12000},'uuid':'boundary','session_id':session})
  if prompt=='todo':
   tool_call('todo-call','TodoWrite',{'todos':[{'content':'Read the code','status':'completed','activeForm':'Reading'},{'content':'Fix the bug','status':'in_progress','activeForm':'Fixing'},{'content':'Ship it','status':'pending','activeForm':'Shipping'}]})
   user_event([{'type':'tool_result','tool_use_id':'todo-call','content':'ok'}])
  if prompt=='tasks':
   for number,subject in [('1','Read the code'),('2','Fix the bug'),('3','Drop this')]:
    tool_call('task-'+number,'TaskCreate',{'subject':subject})
    user_event([{'type':'tool_result','tool_use_id':'task-'+number,'content':'created'}],tool_use_result={'task':{'id':number,'subject':subject}})
   for number,change in [('1',{'status':'completed'}),('2',{'status':'in_progress','subject':'Fix the real bug'}),('3',{'status':'deleted'})]:
    tool_call('update-'+number,'TaskUpdate',{'taskId':number,**change})
    user_event([{'type':'tool_result','tool_use_id':'update-'+number,'content':'updated'}])
  if prompt.startswith('subagent-'):
   status=prompt.removeprefix('subagent-')
   emit({'type':'assistant','uuid':'agent-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'agent-message','content':[{'type':'tool_use','id':'agent-call','name':'Agent','input':{'description':'Review source','prompt':'Review the changes','model':'sonnet'}}]}})
   emit({'type':'system','subtype':'task_started','session_id':'foreign','task_id':'foreign-child','tool_use_id':'agent-call'})
   emit({'type':'system','subtype':'task_started','session_id':session,'task_id':'child','tool_use_id':'agent-call','description':'Review source'})
   emit({'type':'system','subtype':'task_notification','session_id':session,'task_id':'child','status':status,'summary':'Observed result'})
   emit({'type':'system','subtype':'task_progress','session_id':session,'task_id':'child'})
  if prompt=='background-shell':
   # Observed from Claude 2.1.296: every Bash run is a task; only a backgrounded one outlives its turn.
   tool_use('bg-call','cargo build');tool_use('fg-call','ls')
   emit({'type':'system','subtype':'task_started','session_id':session,'task_id':'fg-task','tool_use_id':'fg-call','description':'List files','task_type':'local_bash'})
   emit({'type':'system','subtype':'task_started','session_id':session,'task_id':'bg-task','tool_use_id':'bg-call','description':'Watch the build','is_backgrounded':True,'task_type':'local_bash'})
  if prompt=='background-done':
   emit({'type':'system','subtype':'task_updated','session_id':session,'task_id':'bg-task','patch':{'status':'completed'}})
  if prompt=='self-turn':
   result()
   # Claude reports a finished background task in a turn nobody sent.
   emit({'type':'assistant','uuid':'self-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'self-message','content':[{'type':'text','text':'Background job finished'}]}})
   result()
   continue
  if prompt=='failed': result('model_error','error_during_execution')
  elif prompt=='failed-reason': result('model_error','error_during_execution',errors=['API Error: 529 overloaded'])
  elif prompt=='failed-turns': result('max_turns','error_max_turns')
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

/// A Claude conversation reads earlier turns from its session file, with the rows the live view
/// would show: the main thread only, and a tool's result on its call. A record Claude is still
/// writing isn't read yet.
#[rstest]
fn claude_history_reads_the_session_file() {
    let root = TempDir::new().unwrap();
    let folder = fs::canonicalize(root.path())
        .unwrap()
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let directory = root.path().join("account/projects").join(folder);
    fs::create_dir_all(&directory).unwrap();
    // Observed from Claude 2.1.299: one entry per content block, bookkeeping without a uuid,
    // attachments beside the chain, and sidechain entries for subagents.
    let entries = [
        json!({"type":"queue-operation","operation":"enqueue"}),
        json!({"type":"user","uuid":"u1","parentUuid":null,"isSidechain":false,
            "message":{"role":"user","content":"Find the bug"}}),
        json!({"type":"assistant","uuid":"a1","parentUuid":"u1","isSidechain":false,
            "message":{"id":"msg-1","role":"assistant","content":[
                {"type":"tool_use","id":"toolu-1","name":"Bash","input":{"command":"cargo test"}}]}}),
        json!({"type":"attachment","uuid":"x1","parentUuid":"a1","isSidechain":false}),
        json!({"type":"user","uuid":"r1","parentUuid":"a1","isSidechain":false,
            "message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"toolu-1","content":"1 failed"}]}}),
        json!({"type":"user","uuid":"s1","parentUuid":"r1","isSidechain":true,
            "message":{"role":"user","content":"Subagent prompt"}}),
        json!({"type":"assistant","uuid":"a2","parentUuid":"r1","isSidechain":false,
            "message":{"id":"msg-2","role":"assistant","content":[
                {"type":"text","text":"The test fails on overflow."}]}}),
        json!({"type":"last-prompt","lastPrompt":"Find the bug"}),
    ];
    // Claude is still writing its next record, so the file ends partway through it.
    fs::write(
        directory.join(format!("{SESSION}.jsonl")),
        entries
            .iter()
            .map(|entry| entry.to_string() + "\n")
            .collect::<String>()
            + r#"{"type":"assistant","uuid":"a3","parentU"#,
    )
    .unwrap();
    let mut config = config(root.path()).unwrap();
    config.fresh_session_id = None;
    config.session_id = Some(SESSION.to_owned());
    let session = NativeAgentSession::spawn(config).unwrap();
    barrier(&session).unwrap();
    let page = session.read_history("latest").unwrap();
    let rows = page
        .transcript
        .iter()
        .map(|item| (item.id.as_str(), item.role.as_str(), item.text.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            ("u1", "user", "Find the bug"),
            ("claude-tool-toolu-1", "tool", "1 failed"),
            ("claude-msg-2-0", "assistant", "The test fails on overflow."),
        ]
    );
    assert_eq!((page.has_older, page.at_latest), (false, true));
    session.stop();
}

/// A conversation Claude hasn't written to its session file yet has no history, rather than a
/// failed read.
#[rstest]
fn claude_history_before_the_first_record_is_empty() {
    let root = TempDir::new().unwrap();
    let mut config = config(root.path()).unwrap();
    config.fresh_session_id = None;
    config.session_id = Some(SESSION.to_owned());
    let session = NativeAgentSession::spawn(config).unwrap();
    barrier(&session).unwrap();
    let page = session.read_history("latest").unwrap();
    assert_eq!(
        (page.transcript.len(), page.has_older, page.at_latest),
        (0, false, true)
    );
    session.stop();
}

/// Claude's edits read as file changes built from Claude's own patch, live and from the session
/// file, and a new file reads as all added.
#[rstest]
fn claude_edits_become_file_changes_from_its_patch() {
    let root = TempDir::new().unwrap();
    let edited = json!([{"path": "/tmp/project/notes.txt",
        "diff": "@@ -1,3 +1,3 @@\n line one\n-line two\n+line 2\n line three\n"}]);
    let created = json!([{"path": "/tmp/project/hello.txt", "diff": "@@ -0,0 +1,1 @@\n+hi\n"}]);
    let change = |transcript: &[bootty_agents::NativeTranscriptItem], id: &str| {
        transcript
            .iter()
            .find(|item| item.id == id)
            .and_then(|item| item.tool.as_ref())
            .filter(|tool| tool.name == "fileChange")
            .map(|tool| serde_json::from_str::<Value>(&tool.input).unwrap())
    };
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("edit").unwrap();
    barrier(&session).unwrap();
    assert_eq!(
        change(&session.snapshot().transcript, "claude-tool-edit-call"),
        Some(edited.clone())
    );
    session.stop();

    let folder = fs::canonicalize(root.path())
        .unwrap()
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let directory = root.path().join("account/projects").join(folder);
    fs::create_dir_all(&directory).unwrap();
    // Observed from Claude 2.1.299: the session file calls the result `toolUseResult`.
    let entries = [
        json!({"type":"user","uuid":"u1","parentUuid":null,"isSidechain":false,
            "message":{"role":"user","content":"Edit it"}}),
        json!({"type":"assistant","uuid":"a1","parentUuid":"u1","isSidechain":false,
            "message":{"id":"msg-1","role":"assistant","content":[
                {"type":"tool_use","id":"toolu-edit","name":"Edit","input":{}},
                {"type":"tool_use","id":"toolu-write","name":"Write","input":{}}]}}),
        json!({"type":"user","uuid":"r1","parentUuid":"a1","isSidechain":false,
            "message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"toolu-edit","content":"Edited"}]},
            "toolUseResult":{"filePath":"/tmp/project/notes.txt","structuredPatch":[
                {"oldStart":1,"oldLines":3,"newStart":1,"newLines":3,
                 "lines":[" line one","-line two","+line 2"," line three"]}]}}),
        json!({"type":"user","uuid":"r2","parentUuid":"r1","isSidechain":false,
            "message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"toolu-write","content":"Created"}]},
            "toolUseResult":{"type":"create","filePath":"/tmp/project/hello.txt",
                "content":"hi","structuredPatch":[]}}),
    ];
    fs::write(
        directory.join(format!("{SESSION}.jsonl")),
        entries
            .iter()
            .map(|entry| entry.to_string() + "\n")
            .collect::<String>(),
    )
    .unwrap();
    let mut config = config(root.path()).unwrap();
    config.fresh_session_id = None;
    config.session_id = Some(SESSION.to_owned());
    let session = NativeAgentSession::spawn(config).unwrap();
    barrier(&session).unwrap();
    let page = session.read_history("latest").unwrap();
    assert_eq!(
        (
            change(&page.transcript, "claude-tool-toolu-edit"),
            change(&page.transcript, "claude-tool-toolu-write")
        ),
        (Some(edited), Some(created))
    );
    session.stop();
}

/// A Claude subagent's transcript comes from its own file, named for the task Claude reported.
#[rstest]
fn claude_subagent_transcripts_read_their_own_file() {
    let root = TempDir::new().unwrap();
    let folder = fs::canonicalize(root.path())
        .unwrap()
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let directory = root
        .path()
        .join("account/projects")
        .join(folder)
        .join(SESSION)
        .join("subagents");
    fs::create_dir_all(&directory).unwrap();
    // Observed from Claude 2.1.299: a subagent file holds only sidechain entries.
    let entries = [
        json!({"type":"user","uuid":"c1","parentUuid":null,"isSidechain":true,"agentId":"child",
            "message":{"role":"user","content":"Review the changes"}}),
        json!({"type":"assistant","uuid":"c2","parentUuid":"c1","isSidechain":true,"agentId":"child",
            "message":{"id":"msg-child","role":"assistant","content":[
                {"type":"text","text":"Two files changed."}]}}),
    ];
    fs::write(
        directory.join("agent-child.jsonl"),
        entries
            .iter()
            .map(|entry| entry.to_string() + "\n")
            .collect::<String>(),
    )
    .unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("subagent-completed").unwrap();
    barrier(&session).unwrap();
    let detail = session.read_subagent("child").unwrap();
    let rows = detail
        .transcript
        .iter()
        .map(|item| (item.role.as_str(), item.text.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            ("user", "Review the changes"),
            ("assistant", "Two files changed.")
        ]
    );
    session.stop();
}

/// Stopping a running Claude subagent stops its task, and Claude's report settles it.
#[rstest]
fn stopping_a_claude_subagent_stops_its_task() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("subagent-running").unwrap();
    barrier(&session).unwrap();
    let status = |session: &NativeAgentSession| {
        session
            .snapshot()
            .transcript
            .iter()
            .find_map(|item| item.subagent.clone())
            .map(|agent| agent.status)
    };
    assert_eq!(status(&session), Some(NativeToolStatus::Running));
    session.stop_subagent("child").unwrap();
    barrier(&session).unwrap();
    assert_eq!(status(&session), Some(NativeToolStatus::Interrupted));
    assert!(session.stop_subagent("child").is_err());
    session.stop();
}

/// A running tool shows its arguments while Claude streams them.
#[rstest]
fn claude_tool_arguments_stream_into_the_running_row() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("tool-stream").unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    let tool = snapshot
        .transcript
        .iter()
        .find(|item| item.id == "claude-tool-stream-call")
        .and_then(|item| item.tool.clone())
        .unwrap();
    assert_eq!(
        (
            tool.name.as_str(),
            tool.status,
            serde_json::from_str::<Value>(&tool.input).unwrap()
        ),
        (
            "Read",
            NativeToolStatus::Running,
            json!({"file_path": "/private/tmp/notes"})
        )
    );
    session.stop();
}

/// The context meter reads the latest model call's tokens against the turn's largest window,
/// and the turn's reply keeps what the whole turn used and cost.
#[rstest]
fn claude_usage_fills_the_context_meter() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("usage").unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(
        snapshot.usage,
        Some(json!({
            "input": 2, "output": 40, "cacheRead": 24000, "cacheWrite": 900,
            "modelContextWindow": 1_000_000
        }))
    );
    assert_eq!(
        snapshot
            .transcript
            .iter()
            .filter_map(|item| item.usage.map(|usage| (item.role.as_str(), usage)))
            .collect::<Vec<_>>(),
        [(
            "assistant",
            bootty_agents::NativeTurnUsage {
                input: 48_905,
                output: 95,
                cache_read: 48_000,
                cache_write: 900,
                reasoning: 0,
                cost_micros: Some(42_100),
            }
        )]
    );
    session.stop();
}

/// A large message never ends the conversation. A tool's image fits the budget; anything else
/// past it is cut down or skipped, with a notice, and the turn still finishes.
#[rstest]
#[case::tool_image("read-image", None)]
#[case::large_output("big-output", Some("Omitted 2 MiB of oversized output from Claude."))]
#[case::past_the_envelope("huge-line", Some("Omitted 18 MiB of oversized output from Claude."))]
fn large_messages_never_end_the_conversation(#[case] prompt: &str, #[case] notice: Option<&str>) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(prompt).unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(
        (snapshot.status, snapshot.completed_turn, &snapshot.error),
        (NativeSessionStatus::Idle, true, &None)
    );
    assert_eq!(
        snapshot
            .transcript
            .iter()
            .find(|item| item.role == "notice")
            .map(|item| item.text.split(" that").next().unwrap_or_default()),
        notice
    );
    if prompt != "huge-line" {
        let item = snapshot
            .transcript
            .iter()
            .find(|item| item.id == "claude-tool-read-call")
            .unwrap();
        assert_eq!(
            item.tool.as_ref().unwrap().status,
            NativeToolStatus::Completed
        );
        // The image the tool read shows on its row.
        assert_eq!(
            item.attachments
                .iter()
                .map(|image| (image.name.as_str(), image.pixel_width))
                .collect::<Vec<_>>(),
            if prompt == "read-image" {
                vec![("Read image 1.png", Some(1))]
            } else {
                Vec::new()
            }
        );
    }
    session.stop();
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

/// Reusable approvals apply only what Claude suggested: for the session, every suggestion moves to
/// the session; Always allow keeps a suggestion where Claude proposed it.
#[rstest]
#[case::rule_for_session("permission-rules", NativeApprovalDecision::AllowSession, Some(json!([{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"npm view *"}],"behavior":"allow","destination":"session"}])))]
#[case::rule_always("permission-rules", NativeApprovalDecision::AlwaysAllow, Some(json!([{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"npm view *"}],"behavior":"allow","destination":"localSettings"}])))]
#[case::mode_for_session("permission-mode", NativeApprovalDecision::AllowSession, Some(json!([{"type":"setMode","mode":"acceptEdits","destination":"session"}])))]
#[case::session_mode_is_not_always("permission-mode", NativeApprovalDecision::AlwaysAllow, None)]
#[case::nothing_suggested("permission", NativeApprovalDecision::AllowSession, None)]
fn reusable_approvals_apply_only_claudes_suggestions(
    #[case] prompt: &str,
    #[case] decision: NativeApprovalDecision,
    #[case] permissions: Option<Value>,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(prompt).unwrap();
    barrier(&session).unwrap();
    let request = session.snapshot().requests.remove(0);
    let result = session.approve_decision(&request.id, decision);
    assert_eq!(result.is_ok(), permissions.is_some(), "{result:?}");
    if let Some(permissions) = permissions {
        barrier(&session).unwrap();
        let answer: Value =
            serde_json::from_slice(&fs::read(root.path().join("answer.json")).unwrap()).unwrap();
        let response = &answer["response"]["response"];
        assert_eq!(
            (&response["behavior"], &response["updatedPermissions"]),
            (&json!("allow"), &permissions)
        );
    }
    session.stop();
}

/// Deny and stop tells Claude to stop the turn, and a note tells it why it was denied.
#[rstest]
#[case::stop(Some(NativeApprovalDecision::Cancel), None, json!({"behavior":"deny",
    "message":"Denied by the user","toolUseID":"tool-id","interrupt":true}))]
#[case::note(None, Some("Use the staging bucket"), json!({"behavior":"deny",
    "message":"Use the staging bucket","toolUseID":"tool-id"}))]
fn denials_can_stop_the_turn_or_say_why(
    #[case] decision: Option<NativeApprovalDecision>,
    #[case] note: Option<&str>,
    #[case] expected: Value,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("permission").unwrap();
    barrier(&session).unwrap();
    let request = session.snapshot().requests.remove(0);
    if let Some(decision) = decision {
        session.approve_decision(&request.id, decision).unwrap();
    }
    if let Some(note) = note {
        assert!(session.deny_with_note(&request.id, note).unwrap());
    }
    barrier(&session).unwrap();
    let answer: Value =
        serde_json::from_slice(&fs::read(root.path().join("answer.json")).unwrap()).unwrap();
    assert_eq!(answer["response"]["response"], expected);
    session.stop();
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

/// A turn Claude starts by itself, such as a background task report, is shown and settles.
#[test]
fn self_started_turns_appear_and_settle() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("self-turn").unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    assert!(
        snapshot
            .transcript
            .iter()
            .any(|item| item.role == "assistant" && item.text == "Background job finished")
    );
    assert_eq!(
        (snapshot.status, snapshot.completed_turn),
        (NativeSessionStatus::Idle, true)
    );
    session.stop();
}

/// A long turn keeps the prompt that started it, and empty thinking blocks take no slots in the
/// bounded live view.
#[test]
fn long_turns_keep_their_prompt_and_skip_empty_thinking() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("flood").unwrap();
    barrier(&session).unwrap();
    let transcript = session.snapshot().transcript;
    assert!(transcript.len() <= 256, "{} items", transcript.len());
    assert!(
        transcript
            .iter()
            .any(|item| item.role == "user" && item.text == "flood")
    );
    assert!(
        !transcript
            .iter()
            .any(|item| item.role == "thinking" && item.text.is_empty())
    );
    session.stop();
}

/// A failed turn keeps Claude's reported reason; the fixed sentence is only a last resort.
#[rstest]
#[case::reported("failed-reason", "API Error: 529 overloaded")]
#[case::turn_limit("failed-turns", "Claude stopped at its turn limit")]
#[case::unexplained("failed", "Claude reported a failed turn")]
fn failed_turns_keep_claudes_reason(#[case] prompt: &str, #[case] reason: &str) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(prompt).unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(
        (snapshot.status, snapshot.error.as_deref()),
        (NativeSessionStatus::Error, Some(reason))
    );
    session.stop();
}

/// An interrupted turn settles the tool it was running instead of leaving it Running, and the
/// transcript keeps a mark that the user stopped it.
#[test]
fn interrupted_turn_settles_its_running_tool() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    let tool_status = || {
        session
            .snapshot()
            .transcript
            .iter()
            .find_map(|item| item.tool.as_ref().map(|tool| tool.status))
    };
    session.send_prompt("tool-hold").unwrap();
    barrier(&session).unwrap();
    assert_eq!(tool_status(), Some(NativeToolStatus::Running));
    session.interrupt().unwrap();
    barrier(&session).unwrap();
    assert_eq!(tool_status(), Some(NativeToolStatus::Interrupted));
    assert!(
        session
            .snapshot()
            .transcript
            .iter()
            .any(|item| item.role == "notice" && item.text == "Interrupted")
    );
    session.stop();
}

/// Subagents keep a conversation working after its own turn ends. A stopped process has none,
/// and a relaunch interrupts what the previous process left running.
#[test]
fn running_subagents_outlive_the_turn_but_not_the_process() {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let session = service.resolve(&record.target()).unwrap();
    service
        .prompt(&record.target(), "subagent-running")
        .unwrap();
    barrier(&session).unwrap();
    service.checkpoint().unwrap();
    let activity = service.activities().remove(0);
    assert_eq!(
        (activity.status, activity.running_subagents),
        (NativeSessionStatus::Idle, 1)
    );
    service.stop(&record.target()).unwrap();
    assert_eq!(service.activities()[0].running_subagents, 0);
    let resumed = service.resume(&record.target()).unwrap();
    let child = resumed
        .snapshot
        .transcript
        .iter()
        .find_map(|item| item.subagent.as_ref())
        .unwrap();
    assert_eq!(child.status, NativeToolStatus::Interrupted);
    service.shutdown().unwrap();
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

#[rstest]
fn active_follow_up_uses_claudes_live_stream_without_restarting() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("hold").unwrap();
    barrier(&session).unwrap();
    let initial = session.snapshot();
    assert_eq!(initial.status, NativeSessionStatus::Working);
    let launch = fs::read(root.path().join("launch.json")).unwrap();
    session
        .send_prompt("Follow this updated instruction")
        .unwrap();
    barrier(&session).unwrap();
    let sent: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(sent["priority"], "next");
    assert_eq!(sent["session_id"], SESSION);
    assert_eq!(fs::read(root.path().join("launch.json")).unwrap(), launch);
    let final_state = session.snapshot();
    assert_eq!(final_state.status, NativeSessionStatus::Idle);
    assert!(final_state.completed_turn);
    assert!(final_state.error.is_none());
    assert!(final_state.transcript.iter().any(|item| item.role == "user"
        && item.text == "Follow this updated instruction"
        && item.complete));
    session.stop();
}

#[rstest]
fn queued_followups_wait_for_idle_and_survive_a_conversation_owner_restart() {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = Arc::new(NativeAgentService::open(&path).unwrap());
    service.start_followups().unwrap();
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let target = record.target();
    service.prompt(&target, "hold").unwrap();
    barrier(&service.resolve(&target).unwrap()).unwrap();
    let first = service
        .queue_input(
            &target,
            &bootty_agents::NativePrompt::text("Queued first").unwrap(),
        )
        .unwrap();
    let discarded = service
        .queue_input(
            &target,
            &bootty_agents::NativePrompt::text("Remove this").unwrap(),
        )
        .unwrap();
    service.cancel_followup(&target, &discarded.id).unwrap();
    let second = service
        .queue_input(
            &target,
            &bootty_agents::NativePrompt::text("Queued second").unwrap(),
        )
        .unwrap();
    let queued = service.sessions().remove(0).queued_messages;
    assert_eq!(
        queued.iter().map(|message| &message.id).collect::<Vec<_>>(),
        vec![&first.id, &second.id]
    );
    assert_eq!(
        service
            .resolve(&target)
            .unwrap()
            .snapshot()
            .transcript
            .iter()
            .filter(|item| item.role == "user")
            .count(),
        1
    );
    service.shutdown().unwrap();
    drop(service);
    let service = Arc::new(NativeAgentService::open(&path).unwrap());
    assert_eq!(service.sessions()[0].queued_messages.len(), 2);
    let (sender, receiver) = mpsc::channel();
    service.set_change_handler(Arc::new(move || {
        let _ = sender.send(());
    }));
    service.start_followups().unwrap();
    let stopped = service.sessions().remove(0);
    let resumed = service.resume(&stopped.target()).unwrap();
    assert_eq!(
        resumed.config.account_directory,
        stopped.config.account_directory
    );
    loop {
        let current = service.sessions().remove(0);
        if current.queued_messages.is_empty()
            && current.snapshot.status == NativeSessionStatus::Idle
        {
            break;
        }
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("provider publication wakes the follow-up owner");
    }
    let current = service.sessions().remove(0);
    let messages = current
        .snapshot
        .transcript
        .iter()
        .filter(|item| item.role == "user")
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(messages, vec!["hold", "Queued first", "Queued second"]);
    let sent: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert!(
        sent.get("priority").is_none(),
        "queued follow-ups must not steer"
    );
    service.shutdown().unwrap();
}

/// Each image format people attach becomes a PNG image the provider can read.
#[rstest]
#[case("photo.png")]
#[case("photo.jpg")]
#[case("photo.webp")]
#[case("animation.gif")]
fn image_files_attach_as_png_images(#[case] name: &str) {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let file = root.path().join(name);
    image::RgbImage::from_pixel(2, 1, image::Rgb([0, 0, 0]))
        .save(&file)
        .unwrap();
    let reference = service.import_attachment(&record.target(), &file).unwrap();
    assert_eq!(
        (
            reference.kind,
            reference.mime_type.as_str(),
            reference.pixel_width,
            reference.pixel_height
        ),
        (
            bootty_agents::NativeAttachmentKind::Image,
            "image/png",
            Some(2),
            Some(1)
        )
    );
    service.shutdown().unwrap();
}

/// A finished turn is unread until the conversation is seen. Mark unread brings the last turn
/// back, and what was seen survives a restart.
#[rstest]
fn finished_turns_stay_unread_until_seen() {
    use std::sync::{Arc, mpsc};
    let root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let service = Arc::new(NativeAgentService::open(&catalog).unwrap());
    let (sender, changes) = mpsc::channel();
    service.set_change_handler(Arc::new(move || {
        let _ = sender.send(());
    }));
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let target = record.target();
    let unread = |service: &NativeAgentService| service.activities()[0].unread;
    assert!(!unread(&service));
    service.prompt(&target, "hello").unwrap();
    while !unread(&service) {
        changes
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
    }
    service.mark_seen(&target, true).unwrap();
    assert!(!unread(&service));
    service.mark_seen(&target, false).unwrap();
    assert!(unread(&service));
    service.shutdown().unwrap();
    drop(service);

    let restored = NativeAgentService::open(&catalog).unwrap();
    assert!(unread(&restored));
    restored
        .mark_seen(&restored.sessions()[0].target(), true)
        .unwrap();
    assert!(!unread(&restored));
    restored.shutdown().unwrap();
}

/// A conversation's unsent draft survives a restart. Its attachment names and sizes come from the
/// conversation's own copies, an attachment it doesn't hold is refused, and an empty draft clears.
#[rstest]
fn drafts_survive_a_restart_and_only_name_held_attachments() {
    let root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let file = root.path().join("notes.md");
    fs::write(&file, "Review this note").unwrap();
    let held = service.import_attachment(&record.target(), &file).unwrap();
    let mut placed = held.clone();
    placed.name = "renamed by the client".to_owned();
    placed.prompt_ranges = std::iter::once(0..10).collect();
    let draft = bootty_agents::NativeDraft {
        text: "[notes.md] then this".to_owned(),
        attachments: vec![placed.clone()],
    };
    service.save_draft(&record.target(), draft.clone()).unwrap();
    let mut foreign = placed;
    foreign.id = "native-att-foreign".to_owned();
    assert!(
        service
            .save_draft(
                &record.target(),
                bootty_agents::NativeDraft {
                    text: String::new(),
                    attachments: vec![foreign],
                },
            )
            .is_err()
    );
    service.shutdown().unwrap();
    drop(service);

    let restored = NativeAgentService::open(&catalog).unwrap();
    let mut expected = held;
    expected.prompt_ranges = std::iter::once(0..10).collect();
    assert_eq!(
        restored.sessions()[0].draft,
        Some(bootty_agents::NativeDraft {
            text: draft.text,
            attachments: vec![expected],
        })
    );
    restored
        .save_draft(&record.target(), bootty_agents::NativeDraft::default())
        .unwrap();
    assert_eq!(restored.sessions()[0].draft, None);
    restored.shutdown().unwrap();
}

#[rstest]
#[case("notes.md", false)]
#[case("screenshot.png", true)]
fn queued_preview_retains_attachments_and_hides_only_image_tokens(
    #[case] name: &str,
    #[case] image: bool,
) {
    let root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let file = root.path().join(name);
    if image {
        image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]))
            .save(&file)
            .unwrap();
    } else {
        fs::write(&file, "Review this note").unwrap();
    }
    let reference = service.import_attachment(&record.target(), &file).unwrap();
    let attachments = service
        .resolve_prompt_attachments(&record.target(), std::slice::from_ref(&reference.id))
        .unwrap();
    let token = format!("[{name}]");
    let text = format!("{token} Review this; literal {token}");
    let prompt =
        bootty_agents::NativePrompt::new_with_context(text.clone(), vec![], attachments, vec![])
            .unwrap()
            .with_attachment_ranges(std::collections::BTreeMap::from([(
                reference.id.clone(),
                std::iter::once(0..token.len()).collect(),
            )]))
            .unwrap();
    let queued = service.queue_input(&record.target(), &prompt).unwrap();
    assert_eq!(queued.text, text);
    assert_eq!(queued.attachments.len(), 1);
    assert_eq!(queued.attachments[0].id, reference.id);
    assert_eq!(
        queued.preview_text,
        if image {
            format!(" Review this; literal {token}")
        } else {
            format!("{name} Review this; literal {token}")
        }
    );
    assert_eq!(service.sessions()[0].queued_messages, vec![queued.clone()]);
    service.shutdown().unwrap();
    drop(service);
    let restored = NativeAgentService::open(&catalog).unwrap();
    assert_eq!(restored.sessions()[0].queued_messages, vec![queued]);
    restored.shutdown().unwrap();
}

#[rstest]
fn an_early_queued_steer_waits_and_sends_with_the_next_turn() {
    use bootty_agents::{NativeAgentService, NativePrompt, NativeQueuedMessageStatus};
    use std::{
        sync::{Arc, mpsc},
        time::Duration,
    };
    let root = TempDir::new().unwrap();
    let service = Arc::new(NativeAgentService::open(root.path().join("native.json")).unwrap());
    let (sender, receiver) = mpsc::channel();
    service.set_change_handler(Arc::new(move || {
        let _ = sender.send(());
    }));
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let queued = service
        .queue_input(
            &record.target(),
            &NativePrompt::text("Retain this instruction").unwrap(),
        )
        .unwrap();
    assert_eq!(service.steer_followup(&record.target(), &queued.id), Ok(()));
    assert_eq!(
        service.sessions()[0].queued_messages[0].status,
        NativeQueuedMessageStatus::Queued,
        "no turn can take it, so the message keeps its place"
    );
    service.start_followups().unwrap();
    loop {
        let current = service.sessions().remove(0);
        if current.queued_messages.is_empty()
            && current.snapshot.status == NativeSessionStatus::Idle
        {
            break;
        }
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the queue sends on the next idle turn");
    }
    let current = service.sessions().remove(0);
    assert_eq!(
        current
            .snapshot
            .transcript
            .iter()
            .filter(|item| item.role == "user")
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Retain this instruction"]
    );
    service.shutdown().unwrap();
}

/// A message sent while the previous one is still on its way to Claude waits in the queue, then
/// steers the same turn once Claude confirms the first. Timing never refuses it.
#[test]
fn a_message_sent_before_the_previous_one_lands_follows_it_into_the_turn() {
    use bootty_agents::NativePrompt;
    use std::{
        sync::{Arc, mpsc},
        time::Duration,
    };
    let root = TempDir::new().unwrap();
    let service = Arc::new(NativeAgentService::open(root.path().join("native.json")).unwrap());
    let (sender, changes) = mpsc::channel();
    service.set_change_handler(Arc::new(move || {
        let _ = sender.send(());
    }));
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let target = record.target();
    service.start_followups().unwrap();
    service.prompt(&target, "slow-ack").unwrap();
    service
        .prompt_input(
            &target,
            &NativePrompt::text("Also check the tests").unwrap(),
        )
        .unwrap();
    let waiting = service.sessions().remove(0);
    assert_eq!(
        waiting.queued_messages.len(),
        1,
        "the message waits for the first to land"
    );
    let sending = |record: &bootty_agents::NativeSessionRecord| {
        record
            .snapshot
            .transcript
            .iter()
            .filter(|item| record.snapshot.sending(item))
            .map(|item| item.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(sending(&waiting), ["slow-ack"]);
    fs::write(root.path().join("ack"), "").unwrap();
    loop {
        let current = service.sessions().remove(0);
        let users = current
            .snapshot
            .transcript
            .iter()
            .filter(|item| item.role == "user")
            .map(|item| (item.text.clone(), item.complete))
            .collect::<Vec<_>>();
        // A sent message completes when Claude replays it.
        if current.queued_messages.is_empty()
            && users.len() == 2
            && users.iter().all(|(_, complete)| *complete)
        {
            assert_eq!(
                users,
                vec![
                    ("slow-ack".to_owned(), true),
                    ("Also check the tests".to_owned(), true)
                ]
            );
            assert_eq!(current.snapshot.status, NativeSessionStatus::Working);
            assert_eq!(sending(&current), Vec::<String>::new());
            break;
        }
        changes
            .recv_timeout(Duration::from_secs(5))
            .expect("the queued message steers the turn");
    }
    service.shutdown().unwrap();
}

/// Interrupt and send stops the running turn and sends its message next, ahead of messages
/// already waiting, without pausing the queue as a plain stop does.
#[test]
fn interrupt_and_send_goes_ahead_of_the_queue() {
    use bootty_agents::NativePrompt;
    use std::{
        sync::{Arc, mpsc},
        time::Duration,
    };
    let root = TempDir::new().unwrap();
    let service = Arc::new(NativeAgentService::open(root.path().join("native.json")).unwrap());
    let (sender, changes) = mpsc::channel();
    service.set_change_handler(Arc::new(move || {
        let _ = sender.send(());
    }));
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let target = record.target();
    service.start_followups().unwrap();
    service.prompt(&target, "tool-hold").unwrap();
    barrier(&service.resolve(&target).unwrap()).unwrap();
    service
        .queue_input(&target, &NativePrompt::text("Later").unwrap())
        .unwrap();
    service
        .interrupt_and_send(&target, &NativePrompt::text("Now").unwrap())
        .unwrap();
    loop {
        let current = service.sessions().remove(0);
        let users = current
            .snapshot
            .transcript
            .iter()
            .filter(|item| item.role == "user" && item.complete)
            .map(|item| item.text.clone())
            .collect::<Vec<_>>();
        if current.queued_messages.is_empty()
            && current.snapshot.status == NativeSessionStatus::Idle
        {
            assert_eq!(users, ["tool-hold", "Now", "Later"]);
            break;
        }
        changes
            .recv_timeout(Duration::from_secs(5))
            .expect("both messages go out after the stop");
    }
    service.shutdown().unwrap();
}

/// Waiting messages trade places. A failed turn holds the queue instead of sending into the
/// failure; resuming sends the rest to the same Claude, in their new order, without a restart.
#[test]
fn a_failed_turn_holds_the_queue_until_resumed_and_messages_trade_places() {
    use bootty_agents::{NativePrompt, NativeQueuedMessageStatus};
    use std::sync::{Arc, mpsc};
    let root = TempDir::new().unwrap();
    let service = Arc::new(NativeAgentService::open(root.path().join("native.json")).unwrap());
    let (sender, changes) = mpsc::channel();
    service.set_change_handler(Arc::new(move || {
        let _ = sender.send(());
    }));
    service.start_followups().unwrap();
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let target = record.target();
    service.prompt(&target, "fail-later").unwrap();
    let ids: Vec<String> = ["First", "Second", "Third"]
        .into_iter()
        .map(|text| {
            service
                .queue_input(&target, &NativePrompt::text(text).unwrap())
                .unwrap()
                .id
        })
        .collect();
    let queued = |service: &NativeAgentService| {
        service.sessions()[0]
            .queued_messages
            .iter()
            .map(|message| (message.text.clone(), message.status))
            .collect::<Vec<_>>()
    };
    service.move_followup(&target, &ids[2], true).unwrap();
    service.move_followup(&target, &ids[2], true).unwrap();
    assert!(
        service.move_followup(&target, &ids[2], true).is_err(),
        "first already"
    );
    assert!(
        service.move_followup(&target, &ids[1], false).is_err(),
        "last already"
    );
    fs::write(root.path().join("fail"), "").unwrap();
    loop {
        if queued(&service)
            .iter()
            .all(|(_, status)| *status == NativeQueuedMessageStatus::Paused)
        {
            break;
        }
        changes
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the failed turn holds the queue");
    }
    assert_eq!(
        service.resolve(&target).unwrap().snapshot().status,
        NativeSessionStatus::Error
    );
    service.resume_followups(&target).unwrap();
    let current = wait_for_queue(&service, &changes).unwrap();
    let users = current
        .snapshot
        .transcript
        .iter()
        .filter(|item| item.role == "user")
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(users, ["fail-later", "Third", "First", "Second"]);
    service.shutdown().unwrap();
}

/// A message whose delivery a crash left unknown waits for Retry, and the messages behind it
/// still go out.
#[test]
fn a_failed_message_waits_without_holding_the_rest() {
    use bootty_agents::{NativePrompt, NativeQueuedMessageStatus};
    use std::sync::{Arc, mpsc};
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = Arc::new(NativeAgentService::open(&path).unwrap());
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let target = record.target();
    service.prompt(&target, "hold").unwrap();
    barrier(&service.resolve(&target).unwrap()).unwrap();
    for text in ["Unknown", "Behind it"] {
        service
            .queue_input(&target, &NativePrompt::text(text).unwrap())
            .unwrap();
    }
    service.shutdown().unwrap();
    drop(service);
    // The first message was on its way when the app ended.
    let mut catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    catalog["followups"][0]["status"] = json!("sending");
    fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let service = Arc::new(NativeAgentService::open(&path).unwrap());
    let (sender, changes) = mpsc::channel();
    service.set_change_handler(Arc::new(move || {
        let _ = sender.send(());
    }));
    service.start_followups().unwrap();
    let stopped = service.sessions().remove(0);
    service.resume(&stopped.target()).unwrap();
    loop {
        let current = service.sessions().remove(0);
        let delivered = current
            .snapshot
            .transcript
            .iter()
            .any(|item| item.role == "user" && item.text == "Behind it" && item.complete);
        let waiting = current
            .queued_messages
            .iter()
            .map(|message| (message.text.as_str(), message.status))
            .collect::<Vec<_>>();
        if delivered && waiting == [("Unknown", NativeQueuedMessageStatus::Failed)] {
            break;
        }
        changes
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the message behind the failed one goes out");
    }
    service.shutdown().unwrap();
}

fn screenshot() -> Result<bootty_agents::NativePromptImage, Box<dyn std::error::Error>> {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&[1, 2, 3, 255])?;
    }
    Ok(bootty_agents::NativePromptImage::from_host_png(
        bootty_agents::NativeImageReference {
            id: "screenshot".to_owned(),
            pixel_width: 1,
            pixel_height: 1,
        },
        png,
    )?)
}

/// Provider events wake the follow-up owner; each wait is bounded for a broken fixture.
fn wait_for_queue(
    service: &NativeAgentService,
    changes: &std::sync::mpsc::Receiver<()>,
) -> Result<bootty_agents::NativeSessionRecord, std::sync::mpsc::RecvTimeoutError> {
    loop {
        let current = service.sessions().remove(0);
        if current.queued_messages.is_empty()
            && current.snapshot.status == NativeSessionStatus::Idle
        {
            return Ok(current);
        }
        changes.recv_timeout(std::time::Duration::from_secs(5))?;
    }
}

/// Replays Claude's observed events for a screenshot steered into a running tool, then Stop.
/// The stopped turn ends on Claude's own result, the queue waits instead of sending at once, and
/// resuming it sends the queued message.
#[test]
fn stopping_a_steered_screenshot_turn_settles_and_pauses_the_queue() {
    use bootty_agents::NativePrompt;
    use std::sync::{Arc, mpsc};
    let root = TempDir::new().unwrap();
    let service = Arc::new(NativeAgentService::open(root.path().join("native.json")).unwrap());
    let (sender, changes) = mpsc::channel();
    service.set_change_handler(Arc::new(move || {
        let _ = sender.send(());
    }));
    service.start_followups().unwrap();
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let target = record.target();
    service.prompt(&target, "tool-hold").unwrap();
    let session = service.resolve(&target).unwrap();
    barrier(&session).unwrap();
    let steer = NativePrompt::new("Look at this".to_owned(), vec![screenshot().unwrap()]).unwrap();
    service.prompt_input(&target, &steer).unwrap();
    barrier(&session).unwrap();
    service
        .queue_input(&target, &NativePrompt::text("Queued after stop").unwrap())
        .unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Working);

    service.interrupt(&target).unwrap();
    loop {
        let current = service.sessions().remove(0);
        if current.snapshot.status == NativeSessionStatus::Idle {
            assert_eq!(
                current.queued_messages[0].status,
                bootty_agents::NativeQueuedMessageStatus::Paused,
                "stopping holds the queue"
            );
            break;
        }
        changes
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the stopped turn settles");
    }
    service.resume_followups(&target).unwrap();
    let current = wait_for_queue(&service, &changes).unwrap();
    let users = current
        .snapshot
        .transcript
        .iter()
        .filter(|item| item.role == "user")
        .map(|item| (item.text.as_str(), item.complete))
        .collect::<Vec<_>>();
    assert_eq!(
        users,
        vec![
            ("tool-hold", true),
            ("Look at this", true),
            ("Queued after stop", true)
        ]
    );
    let tools = current
        .snapshot
        .transcript
        .iter()
        .filter_map(|item| Some((item.id.as_str(), item.tool.as_ref()?.status)))
        .collect::<Vec<_>>();
    assert_eq!(
        tools,
        vec![
            ("claude-tool-bash-call", NativeToolStatus::Completed),
            ("claude-tool-steer-call", NativeToolStatus::Interrupted)
        ]
    );
    let sent: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(sent["message"]["content"], "Queued after stop");
    assert!(sent.get("priority").is_none());
    service.shutdown().unwrap();
}

/// Claude acknowledges an interrupt without a result when no turn is running. The session stays
/// visibly stopping, and a steered queued message keeps its place instead of joining the turn.
#[test]
fn an_unanswered_stop_keeps_the_session_stopping_and_the_queue_intact() {
    use bootty_agents::{NativePrompt, NativeQueuedMessageStatus};
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("ignore-interrupt"), "").unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let target = record.target();
    service.prompt(&target, "tool-hold").unwrap();
    let session = service.resolve(&target).unwrap();
    barrier(&session).unwrap();
    let queued = service
        .queue_input(&target, &NativePrompt::text("Wait for me").unwrap())
        .unwrap();

    service.interrupt(&target).unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Working);
    assert!(snapshot.stopping());
    assert_eq!(service.steer_followup(&target, &queued.id), Ok(()));
    assert_eq!(
        service.sessions()[0].queued_messages[0].status,
        NativeQueuedMessageStatus::Paused,
        "stopping holds the queue"
    );
    service.shutdown().unwrap();
}

/// Claude reports background shells as tasks. One stays visible and keeps its conversation active
/// until Claude reports it finished; foreground commands stay plain tools.
#[test]
fn background_shells_stay_visible_until_they_finish() {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task("binding", "task", "Claude", config(root.path()).unwrap())
        .unwrap();
    let target = record.target();
    let session = service.resolve(&target).unwrap();
    let tasks = || {
        session
            .snapshot()
            .transcript
            .iter()
            .filter_map(|item| item.subagent.as_ref())
            .map(|task| {
                (
                    task.id.clone(),
                    task.title.clone(),
                    task.prompt.clone(),
                    task.status,
                )
            })
            .collect::<Vec<_>>()
    };
    service.prompt(&target, "background-shell").unwrap();
    barrier(&session).unwrap();
    assert_eq!(
        tasks(),
        vec![(
            "bg-task".to_owned(),
            "Watch the build".to_owned(),
            "cargo build".to_owned(),
            NativeToolStatus::Running
        )]
    );
    service.checkpoint().unwrap();
    let activity = service.activities().remove(0);
    assert_eq!(
        (activity.status, activity.running_subagents),
        (NativeSessionStatus::Idle, 1)
    );

    service.prompt(&target, "background-done").unwrap();
    barrier(&session).unwrap();
    assert_eq!(tasks()[0].3, NativeToolStatus::Completed);
    service.checkpoint().unwrap();
    assert_eq!(service.activities()[0].running_subagents, 0);
    service.shutdown().unwrap();
}

/// An approval stays whole, because allowing it sends Claude the exact input back. One too large
/// to hold is declined through Claude's protocol and the conversation continues.
#[rstest]
#[case::large_write(64 * 1024, true)]
#[case::oversized_write(2 * 1024 * 1024, false)]
fn large_claude_approvals_are_held_and_oversized_ones_declined(
    #[case] bytes: usize,
    #[case] held: bool,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(&format!("write-{bytes}")).unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    if held {
        assert_eq!(snapshot.status, NativeSessionStatus::Waiting);
        let request = snapshot.requests.first().unwrap().id.clone();
        session
            .respond(&request, json!({"decision":"accept"}))
            .unwrap();
    } else {
        assert_eq!(snapshot.requests, []);
        assert!(snapshot.transcript.iter().any(|item| item.role == "notice"
            && item.text == "Bootty declined a provider request that is too large to show."));
    }
    // An answer or decline is already queued for Claude; this barrier follows it.
    barrier(&session).unwrap();
    let answer: Value =
        serde_json::from_slice(&fs::read(root.path().join("answer.json")).unwrap()).unwrap();
    if held {
        assert_eq!(
            answer["response"]["response"]["updatedInput"]["content"]
                .as_str()
                .map(str::len),
            Some(bytes)
        );
    } else {
        assert_eq!(answer["response"]["subtype"], "error");
    }
    // Either answer lets Claude finish the turn; the conversation is still alive.
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    session.stop();
}

/// A message Claude never confirmed is in flight while Claude runs, and undelivered once the
/// process that could confirm it has ended.
#[test]
fn an_unconfirmed_message_is_undelivered_once_its_provider_ends() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("no-ack").unwrap();
    barrier(&session).unwrap();
    let undelivered = || {
        let snapshot = session.snapshot();
        snapshot
            .transcript
            .iter()
            .filter(|item| snapshot.undelivered(item))
            .map(|item| item.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(undelivered(), Vec::<String>::new());
    session.stop();
    assert_eq!(undelivered(), vec!["no-ack".to_owned()]);
}

/// Claude's to-do list fills the plan: `TodoWrite` writes it whole, and the task tools add, change
/// and remove tasks one at a time.
#[rstest]
#[case::todo_write("todo", &[
    ("Read the code", NativeTodoStatus::Completed),
    ("Fix the bug", NativeTodoStatus::InProgress),
    ("Ship it", NativeTodoStatus::Pending),
])]
#[case::task_tools("tasks", &[
    ("Read the code", NativeTodoStatus::Completed),
    ("Fix the real bug", NativeTodoStatus::InProgress),
])]
fn claude_to_do_lists_fill_the_plan(
    #[case] prompt: &str,
    #[case] expected: &[(&str, NativeTodoStatus)],
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(prompt).unwrap();
    barrier(&session).unwrap();
    assert_eq!(
        session
            .snapshot()
            .todos
            .iter()
            .map(|todo| (todo.text.as_str(), todo.status))
            .collect::<Vec<_>>(),
        expected
    );
    session.stop();
}

/// A compaction shows as one row while Claude works on it, then says how far it shrank the
/// context, which the meter reads.
#[rstest]
fn claude_compaction_shows_and_resets_the_context_meter() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("compact").unwrap();
    barrier(&session).unwrap();
    let snapshot = session.snapshot();
    let rows = snapshot
        .transcript
        .iter()
        .filter_map(|item| item.tool.as_ref())
        .filter(|tool| tool.name == "contextCompaction")
        .map(|tool| (tool.status, tool.input.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [(
            NativeToolStatus::Completed,
            json!({"preTokens": 180_000, "postTokens": 12_000}).to_string()
        )]
    );
    assert_eq!(snapshot.usage.unwrap()["input"], 12_000);
    session.stop();
}

/// Claude reports each limit window on its own; a rejected one stopped work until it resets.
#[rstest]
fn claude_rate_limits_keep_each_window() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("limits").unwrap();
    barrier(&session).unwrap();
    let limits = session.snapshot().rate_limits.unwrap();
    assert_eq!(
        limits
            .windows
            .iter()
            .map(|window| (window.label.as_str(), window.used_percent, window.resets_at))
            .collect::<Vec<_>>(),
        [
            ("5h", 91, Some(1_760_000_000)),
            ("7d Opus", 100, Some(1_760_500_000))
        ]
    );
    assert_eq!(limits.reached_until(), Some(1_760_500_000));
    session.stop();
}
