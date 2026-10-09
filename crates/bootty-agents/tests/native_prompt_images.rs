#![cfg(unix)]

use std::{
    error::Error, fs, os::unix::fs::PermissionsExt as _, path::Path, sync::Arc, time::Instant,
};

use assert_fs::TempDir;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_agents::{
    AgentKind, MAX_NATIVE_PROMPT_IMAGE_BYTES, MAX_NATIVE_PROMPT_TEXT_BYTES, NativeAgentService,
    NativeImageReference, NativePrompt, NativePromptImage, NativeSessionConfig,
    NativeSessionStatus, NativeTurnOutcome, ToolBridge, ToolBridgeContext, ToolCapture,
    ToolCapturedCommand, ToolPolicy, ToolScope,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

const SESSION: &str = "01234567-89ab-4cde-8123-456789abcdef";

fn png(metadata_bytes: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        if metadata_bytes > 0 {
            encoder.add_text_chunk("fixture".to_owned(), "x".repeat(metadata_bytes))?;
        }
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&[1, 2, 3, 255])?;
        writer.finish()?;
    }
    Ok(bytes)
}

fn reference(id: &str) -> NativeImageReference {
    NativeImageReference {
        id: id.to_owned(),
        pixel_width: 1,
        pixel_height: 1,
    }
}

fn fixture(root: &Path) -> std::io::Result<String> {
    let path = root.join("image-provider.py");
    fs::write(
        &path,
        r"#!/usr/bin/env python3
import base64,fcntl,json,os,sys
claude='--input-format' in sys.argv
pi='--mode' in sys.argv
session=sys.argv[sys.argv.index('--resume' if '--resume' in sys.argv else '--session-id')+1] if claude else 'image-session'
file=os.path.join(os.getcwd(),'image-session.jsonl')
history_mode=os.path.exists('history-mode')
if history_mode and not claude:
 with open('provider-counter','a+') as counter:
  fcntl.flock(counter,fcntl.LOCK_EX);counter.seek(0)
  generation=int(counter.read() or '0')+1
  counter.seek(0);counter.truncate();counter.write(str(generation));counter.flush()
 session='image-session-'+str(generation)
 file=sys.argv[sys.argv.index('--session')+1] if '--session' in sys.argv else os.path.join(os.getcwd(),session+'.jsonl')
messages=[]
items=[]
turn=0
def emit(value): print(json.dumps(value),flush=True)
def reply(value,result):
 if claude: emit({'type':'control_response','response':{'subtype':'success','request_id':value['request_id'],'response':result}})
 elif pi: emit({'id':value['id'],'type':'response','command':value['type'],'success':True,'data':result})
 else: emit({'id':value['id'],'result':result})
def notice(method,**params): emit({'method':method,'params':{'threadId':session,'turnId':'turn-image',**params}})
for line in sys.stdin:
 value=json.loads(line)
 kind=value.get('method',value.get('type'))
 if claude and kind=='control_request': reply(value,{})
 elif kind=='initialize': reply(value,{})
 elif kind in ['thread/start','thread/resume']: reply(value,{'thread':{'id':session,'turns':[{'id':'turn-image','items':items}]}})
 elif kind=='get_state': reply(value,{'sessionId':session,'sessionFile':file,'isStreaming':False,'isCompacting':False})
 elif kind=='get_messages': reply(value,{'messages':messages})
 elif kind=='thread/read': reply(value,{'thread':{'id':session,'turns':[{'id':'turn-image','items':items}]}})
 elif kind in ['user','prompt','turn/start']:
  turn+=1
  if claude:
   blocks=value['message']['content']
   if isinstance(blocks,str): blocks=[{'type':'text','text':blocks}]
   encoded=[block['source']['data'] for block in blocks if block['type']=='image']
   assert all(block['source']['type']=='base64' and block['source']['media_type']=='image/png' for block in blocks if block['type']=='image')
  elif pi:
   blocks=([{'type':'text','text':value['message']}] if value['message'] else [])+value.get('images',[])
   encoded=[block['data'] for block in value.get('images',[])]
   assert all(block['mimeType']=='image/png' for block in value.get('images',[]))
  else:
   blocks=value['params']['input']
   encoded=[block['url'].removeprefix('data:image/png;base64,') for block in blocks if block['type']=='image']
   assert all(block['url'].startswith('data:image/png;base64,') for block in blocks if block['type']=='image')
  if encoded:
   with open('received.png','wb') as output: output.write(base64.b64decode(encoded[0],validate=True))
  with open('prompt.json','w') as output: json.dump(value,output)
  if claude:
   emit({'type':'system','subtype':'init','session_id':session,'cwd':os.getcwd()})
   echo=dict(value);echo['isReplay']=True;emit(echo)
   if history_mode:
    emit({'type':'assistant','uuid':'answer-'+str(turn),'session_id':session,'parent_tool_use_id':None,'message':{'id':'answer-'+str(turn),'content':[{'type':'text','text':'History seen'}]}})
   emit({'type':'result','subtype':'success','session_id':session,'uuid':'result-not-prompt','is_error':False,'terminal_reason':'completed','result':'Image seen'})
  elif pi:
   emit({'type':'agent_start'})
   user={'role':'user','content':blocks,'timestamp':turn*2}
   messages=[*messages,user] if history_mode else [user]
   emit({'type':'message_start','message':user});emit({'type':'message_end','message':user})
   if history_mode:
    assistant={'role':'assistant','content':[{'type':'text','text':'History seen'}],'timestamp':turn*2+1,'stopReason':'stop'}
    emit({'type':'message_end','message':assistant});messages.append(assistant)
    emit({'type':'agent_end','messages':[user,assistant]})
   reply(value,{'disposition':'started'})
   emit({'type':'agent_settled'})
  else:
   notice('turn/started',turn={'id':'turn-image','status':'inProgress'})
   user={'id':'user-image-'+str(turn),'type':'userMessage','content':blocks}
   items=[*items,user] if history_mode else [user]
   notice('item/completed',item=user)
   if history_mode:
    assistant={'id':'answer-'+str(turn),'type':'agentMessage','text':'History seen'}
    notice('item/completed',item=assistant);items.append(assistant)
   reply(value,{'turn':{'id':'turn-image','items':[] if history_mode else items}})
   notice('turn/completed',turn={'id':'turn-image','status':'completed',**({'items':[user,assistant]} if history_mode else {})})
",
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path.to_string_lossy().into_owned())
}

#[rstest]
#[case::codex(AgentKind::Codex, false)]
#[case::codex_with_image(AgentKind::Codex, true)]
#[case::pi(AgentKind::Pi, false)]
#[case::pi_with_image(AgentKind::Pi, true)]
#[case::claude(AgentKind::Claude, false)]
#[case::claude_with_image(AgentKind::Claude, true)]
fn large_fork_context_echoes_preserve_turn_ownership_and_image_delivery(
    #[case] provider: AgentKind,
    #[case] image: bool,
) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("history-mode"), "").unwrap();
    let mut config = NativeSessionConfig::new(provider, root.path());
    config.program = fixture(root.path()).unwrap();
    config.fresh_session_id = (provider == AgentKind::Claude).then(|| SESSION.to_owned());
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let parent = service
        .create_for_task("binding", "task", "Parent", config)
        .unwrap();
    let session = service.resolve(&parent.target()).unwrap();
    let barrier = match provider {
        AgentKind::Codex => "thread/read",
        AgentKind::Pi => "get_state",
        AgentKind::Claude => "initialize",
    };
    for _ in 0..40 {
        service
            .prompt(&parent.target(), &"界".repeat(5000))
            .unwrap();
        session
            .rpc(
                barrier,
                if provider == AgentKind::Codex {
                    json!({"threadId":session.snapshot().session_id})
                } else {
                    json!({})
                },
            )
            .unwrap();
    }
    service.checkpoint().unwrap();
    let parent_identity = session.snapshot().session_id;
    let child = service
        .fork_side_chat(&parent.target(), None, None)
        .unwrap();
    let bytes = png(1024 * 1024).unwrap();
    let images = if image {
        vec![NativePromptImage::from_host_png(reference("fork-image"), bytes.clone()).unwrap()]
    } else {
        Vec::new()
    };
    let prompt = NativePrompt::new("Continue copied history".into(), images).unwrap();
    service.prompt_input(&child.target(), &prompt).unwrap();
    let session = service.resolve(&child.target()).unwrap();
    session
        .rpc(
            barrier,
            if provider == AgentKind::Codex {
                json!({"threadId":session.snapshot().session_id})
            } else {
                json!({})
            },
        )
        .unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    assert_eq!(session.snapshot().error, None);
    assert!(session.snapshot().session_id.is_some());
    assert_ne!(session.snapshot().session_id, parent_identity);
    assert_eq!(
        session.snapshot().first_turn.unwrap().outcome,
        NativeTurnOutcome::Succeeded
    );
    // This fixture escapes Unicode, exercising a genuinely oversized provider echo.
    assert!(fs::metadata(root.path().join("prompt.json")).unwrap().len() > 1024 * 1024);
    if image {
        assert_eq!(fs::read(root.path().join("received.png")).unwrap(), bytes);
    }
    service.shutdown().unwrap();
}

#[rstest]
#[case::codex_text(AgentKind::Codex, "Read this annotation", 0)]
#[case::pi_text(AgentKind::Pi, "Read this annotation", 0)]
#[case::claude_text(AgentKind::Claude, "Read this annotation", 0)]
#[case::codex_large_image_only(AgentKind::Codex, "", 1024 * 1024)]
#[case::pi_large_image_only(AgentKind::Pi, "", 1024 * 1024)]
#[case::claude_large_image_only(AgentKind::Claude, "", 1024 * 1024)]
fn provider_images_are_exact_transport_bytes_and_durable_safe_references(
    #[case] provider: AgentKind,
    #[case] text: &str,
    #[case] metadata_bytes: usize,
) {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let mut config = NativeSessionConfig::new(provider, root.path());
    config.program = fixture(root.path()).unwrap();
    config.fresh_session_id = (provider == AgentKind::Claude).then(|| SESSION.to_owned());
    config.account_directory = Some(root.path().join("account").to_string_lossy().into_owned());
    let bytes = png(metadata_bytes).unwrap();
    let image =
        NativePromptImage::from_host_png(reference("annotation_123"), bytes.clone()).unwrap();
    let prompt = NativePrompt::new(text.to_owned(), vec![image]).unwrap();
    let service = NativeAgentService::open(&path).unwrap();
    let record = service
        .create_for_task("binding", "task", "Image annotation", config)
        .unwrap();
    service.prompt_input(&record.target(), &prompt).unwrap();
    let session = service.resolve(&record.target()).unwrap();
    let barrier = match provider {
        AgentKind::Codex => "thread/read",
        AgentKind::Pi => "get_state",
        AgentKind::Claude => "initialize",
    };
    session
        .rpc(
            barrier,
            if provider == AgentKind::Codex {
                json!({"threadId":"image-session"})
            } else {
                json!({})
            },
        )
        .unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Idle);
    assert_eq!(
        snapshot.first_turn.as_ref().unwrap().outcome,
        NativeTurnOutcome::Succeeded
    );
    let users = snapshot
        .transcript
        .iter()
        .filter(|item| item.role == "user")
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 1);
    let user = users.first().unwrap();
    assert_eq!(user.text, text);
    assert_eq!(user.images, vec![reference("annotation_123")]);
    assert_eq!(fs::read(root.path().join("received.png")).unwrap(), bytes);
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(prompt.image_count(), 1);
    assert!(wire.to_string().contains(&STANDARD.encode(&bytes)));
    let refreshed = session.refresh_history().unwrap();
    let refreshed_user = refreshed
        .transcript
        .iter()
        .find(|item| item.role == "user")
        .unwrap();
    assert_eq!(refreshed_user.images, user.images);
    assert_eq!(refreshed_user.text, text);
    service.checkpoint().unwrap();
    service.stop(&record.target()).unwrap();
    let saved = fs::read_to_string(&path).unwrap();
    assert!(!saved.contains(&STANDARD.encode(&bytes)));
    assert!(!saved.contains("data:image/png"));
    service.shutdown().unwrap();
    let restored = NativeAgentService::open(&path).unwrap();
    let records = restored.sessions();
    let record = records.first().unwrap();
    assert_eq!(record.binding_id, "binding");
    assert_eq!(record.task_identity.as_deref(), Some("task"));
    let user = record
        .snapshot
        .transcript
        .iter()
        .find(|item| item.role == "user")
        .unwrap();
    assert_eq!(user.images, vec![reference("annotation_123")]);
    assert_eq!(user.text, text);
    restored.shutdown().unwrap();
}

#[rstest]
#[case::path("/private/image.png")]
#[case::url("https://image.test/a.png")]
#[case::empty("")]
fn image_identity_never_admits_paths_or_urls(#[case] id: &str) {
    assert!(NativePromptImage::from_host_png(reference(id), png(0).unwrap()).is_err());
}

#[rstest]
fn image_admission_checks_pixels_dimensions_and_submission_budgets() {
    let bytes = png(0).unwrap();
    assert!(NativePromptImage::from_host_png(reference("valid"), b"not a PNG".to_vec()).is_err());
    let mut truncated = bytes.clone();
    truncated.truncate(truncated.len().saturating_sub(10));
    assert!(NativePromptImage::from_host_png(reference("valid"), truncated).is_err());
    let mut wrong = reference("valid");
    wrong.pixel_width = 2;
    assert!(NativePromptImage::from_host_png(wrong, bytes.clone()).is_err());
    assert!(
        NativePromptImage::from_host_png(
            reference("valid"),
            vec![0; MAX_NATIVE_PROMPT_IMAGE_BYTES + 1]
        )
        .is_err()
    );
    let image = NativePromptImage::from_host_png(reference("valid"), bytes).unwrap();
    assert!(NativePrompt::new(String::new(), vec![image.clone(); 5]).is_err());
    assert!(NativePrompt::text(&"x".repeat(MAX_NATIVE_PROMPT_TEXT_BYTES + 1)).is_err());
    assert!(NativePrompt::text("").is_err());
    assert!(NativePrompt::new(String::new(), vec![image]).is_ok());
    let big = NativePromptImage::from_host_png(
        reference("large"),
        png(MAX_NATIVE_PROMPT_IMAGE_BYTES / 2).unwrap(),
    )
    .unwrap();
    assert!(NativePrompt::new(String::new(), vec![big; 2]).is_err());
}

proptest! {
    #[test]
    fn text_wrappers_keep_literal_content(text in "[a-zA-Z0-9 '\"\\n\\t]{1,256}") {
        let prompt = NativePrompt::text(&text).unwrap();
        prop_assert_eq!(prompt.message(), text);
        prop_assert_eq!(prompt.image_count(), 0);
        prop_assert_eq!(prompt.image_references(), Vec::<NativeImageReference>::new());
    }
}

fn capture_provider(root: &Path) -> std::io::Result<String> {
    let path = root.join("capture-provider.py");
    fs::write(
        &path,
        r"#!/usr/bin/env python3
import base64,hashlib,json,os,sys
pi='--mode' in sys.argv
session='capture-session'
server=None
for i,arg in enumerate(sys.argv):
 if arg=='--config' and sys.argv[i+1].startswith('mcp_servers.'):
  server=sys.argv[i+1].split('.')[1]
 if pi and arg=='--extension':
  extension=open(sys.argv[i+1]).read()
  server=json.JSONDecoder().raw_decode(extension.split('const server = ',1)[1])[0]
def pi_name(server,tool):
 name='mcp__'+server+'__'+tool
 return name if len(name)<=64 else name[:55]+'_'+hashlib.sha256((server+'\0'+tool).encode()).hexdigest()[:8]
fault=open('capture-fault').read() if os.path.exists('capture-fault') else ''
history=json.load(open('capture-history.json')) if os.path.exists('capture-history.json') else []
image=base64.b64encode(open('capture.png','rb').read()).decode()
content=[{'type':'image','mimeType':'image/png','data':image},{'type':'text','text':'Capture 1x1'}]
def emit(value): print(json.dumps(value),flush=True)
def reply(value,data):
 if pi and value['type']=='prompt' and fault=='legacy_ack': data={}
 if pi: emit({'id':value['id'],'type':'response','command':value['type'],'success':True,'data':data})
 else: emit({'id':value['id'],'result':data})
def notice(method,**params): emit({'method':method,'params':{'threadId':session,'turnId':'capture-turn',**params}})
for line in sys.stdin:
 value=json.loads(line)
 kind=value.get('method',value.get('type'))
 if kind=='initialize': reply(value,{})
 elif kind=='initialized': continue
 elif kind in ['thread/start','thread/resume','thread/read']: reply(value,{'thread':{'id':session,'turns':[{'id':'capture-turn','items':history}] if history else []}})
 elif kind=='get_state': reply(value,{'sessionId':session,'sessionFile':os.path.join(os.getcwd(),'capture.jsonl'),'isStreaming':False})
 elif kind=='get_messages': reply(value,{'messages':history})
 elif kind=='get_commands': reply(value,{'commands':[] if fault=='checkpoint_missing' else [{'name':'__bootty_checkpoint','source':'extension'}]})
 elif kind=='switch_session':
  if fault=='checkpoint_identity': session='foreign-session'
  reply(value,{'cancelled':fault=='checkpoint_cancelled'})
 elif kind=='prompt' and value.get('message')=='/__bootty_checkpoint': reply(value,{'disposition':'handled'})
 elif kind in ['turn/start','prompt']:
  name='unrelated_tool' if fault=='name' else 'computer_snapshot'
  final_id='unobserved-tool' if fault=='id' else 'capture-id'
  if pi:
   nested=fault.startswith('nested')
   tool_server='bootty_'+'0'*64 if fault=='server' else server
   details={'server':tool_server,'tool':name}
   name=pi_name(tool_server,name) if fault!='plain' else name
   emit({'type':'agent_start'})
   parent={'parentToolCallId':'codemode-id'} if nested else {}
   if nested: emit({'type':'tool_execution_start','toolCallId':'codemode-id','toolName':'codemode'})
   emit({'type':'tool_execution_start','toolCallId':'capture-id','toolName':name,**parent})
   if fault=='nested_capture_parent': parent={'parentToolCallId':'foreign-codemode'}
   emit({'type':'tool_execution_end','toolCallId':final_id,'toolName':name,'result':{'content':content,'details':details},'isError':False,**parent})
   if nested:
    name='codemode'
    final_id='foreign-codemode' if fault=='nested_parent' else 'codemode-id'
    if fault=='nested_changed_image': content=[{'type':'image','mimeType':'image/png','data':base64.b64encode(open('changed.png','rb').read()).decode()}]
    details={'calls':[{'id':'capture-id','name':pi_name(server,'computer_snapshot'),'args':'{}','status':'ok'}]}
    emit({'type':'tool_execution_end','toolCallId':final_id,'toolName':name,'result':{'content':content,'details':details},'isError':False})
   message={'role':'toolResult','toolCallId':final_id,'toolName':name,'content':content,'details':details,'timestamp':1}
   history=[message]
   with open('capture-history.json','w') as output: json.dump(history,output)
   emit({'type':'message_start','message':message});emit({'type':'message_end','message':message})
   final=json.loads(json.dumps(message))
   if fault=='completion_image': final['content']=[{'type':'image','mimeType':'image/png','data':base64.b64encode(open('changed.png','rb').read()).decode()}]
   if fault in ['completion_id','nested_completion_parent']: final['toolCallId']='foreign-tool'
   emit({'type':'turn_end','message':{'role':'assistant','content':[]},'toolResults':[final]})
   emit({'type':'agent_end','messages':[final]})
   reply(value,{'disposition':'started'});emit({'type':'agent_settled'})
  else:
   notice('turn/started',turn={'id':'capture-turn','status':'inProgress'})
   item={'id':'capture-id','type':'mcpToolCall','server':'foreign-server' if fault=='server' else server,'tool':name,'status':'inProgress','arguments':{},'result':None}
   notice('item/started',item=item)
   item['id']=final_id;item['result']={'content':content};item['status']='completed'
   history=[item]
   with open('capture-history.json','w') as output: json.dump(history,output)
   notice('item/completed',item=item)
   notice('turn/completed',turn={'id':'capture-turn','status':'completed','items':[item]})
   reply(value,{'turn':{'id':'capture-turn'}})
 elif kind=='__barrier': reply(value,{})
",
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path.to_string_lossy().into_owned())
}

fn target(kind: ResourceKind) -> CommandTarget {
    CommandTarget {
        kind,
        handle: format!("captured-{kind:?}"),
        generation: 17,
    }
}

#[rstest]
#[case::cancelled("checkpoint_cancelled")]
#[case::identity_changed("checkpoint_identity")]
#[case::missing_registration("checkpoint_missing")]
fn fresh_pi_conversations_require_an_exact_confirmed_checkpoint(#[case] fault: &str) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("capture-fault"), fault).unwrap();
    fs::write(root.path().join("capture.png"), png(0).unwrap()).unwrap();
    let mut config = NativeSessionConfig::new(AgentKind::Pi, root.path());
    config.program = capture_provider(root.path()).unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    assert!(
        service
            .create_for_task_with_tools(
                "binding",
                "task",
                "Capture",
                config,
                capture_tools(AgentKind::Pi, false).unwrap(),
            )
            .is_err()
    );
    assert!(
        service
            .sessions()
            .iter()
            .all(|record| record.snapshot.status == NativeSessionStatus::Error)
    );
    service.shutdown().unwrap();
}

fn capture_tools(provider: AgentKind, computer: bool) -> Result<Arc<ToolBridge>, String> {
    let mut invocation = CommandInvocation::new("computer.capture", Vec::new(), Caller::Internal);
    invocation.target = Some(target(ResourceKind::ApplicationWindow));
    let tools = ToolBridge::prepare(
        ToolBridgeContext {
            scope: ToolScope {
                provider,
                binding: target(ResourceKind::Binding),
            },
            caller: Caller::Socket,
            policy: ToolPolicy {
                computer_capture: computer,
                ..ToolPolicy::own_terminal()
            },
            captures: if computer {
                vec![ToolCapturedCommand {
                    capture: ToolCapture::Computer,
                    invocation,
                }]
            } else {
                Vec::new()
            },
            spawn: None,
        },
        &std::env::current_exe().map_err(|error| error.to_string())?,
        Arc::new(|_: CommandInvocation, _: Instant, _: CommandCancellation| {
            CommandOutcome::Success {
                value: json!({}),
                warnings: Vec::new(),
            }
        }),
    )?;
    tools.lease().bind(
        &target(ResourceKind::Binding),
        target(ResourceKind::Terminal),
    )?;
    Ok(Arc::new(tools))
}

#[rstest]
#[case::codex(AgentKind::Codex, false, false)]
#[case::pi(AgentKind::Pi, false, false)]
#[case::pi_nested(AgentKind::Pi, true, false)]
#[case::pi_legacy(AgentKind::Pi, false, true)]
fn exact_attached_computer_results_cross_the_control_budget_without_persisting_pixels(
    #[case] provider: AgentKind,
    #[case] nested: bool,
    #[case] legacy_ack: bool,
) {
    let root = TempDir::new().unwrap();
    let bytes = png(1024 * 1024).unwrap();
    fs::write(root.path().join("capture.png"), &bytes).unwrap();
    if nested {
        fs::write(root.path().join("capture-fault"), "nested").unwrap();
    }
    if legacy_ack {
        fs::write(root.path().join("capture-fault"), "legacy_ack").unwrap();
    }
    let mut config = NativeSessionConfig::new(provider, root.path());
    config.program = capture_provider(root.path()).unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task_with_tools(
            "binding",
            "task",
            "Capture",
            config,
            capture_tools(provider, true).unwrap(),
        )
        .unwrap();
    service
        .prompt(&record.target(), "Capture the exact attached window")
        .unwrap();
    let session = service.resolve(&record.target()).unwrap();
    session
        .rpc(
            if provider == AgentKind::Pi {
                "get_state"
            } else {
                "__barrier"
            },
            json!({}),
        )
        .unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Idle);
    if provider == AgentKind::Pi {
        assert_eq!(
            snapshot
                .transcript
                .iter()
                .find_map(|item| item.tool.as_ref())
                .unwrap()
                .name,
            if nested {
                "codemode"
            } else {
                "computer_snapshot"
            }
        );
    }
    assert_eq!(
        snapshot.first_turn.as_ref().unwrap().outcome,
        NativeTurnOutcome::Succeeded
    );
    service.checkpoint().unwrap();
    let journal = fs::read_to_string(root.path().join("native.json")).unwrap();
    assert!(!journal.contains(&STANDARD.encode(bytes)));
    assert_eq!(
        session.refresh_history().unwrap().status,
        NativeSessionStatus::Idle
    );
    service.checkpoint().unwrap();
    service.stop(&record.target()).unwrap();
    service.shutdown().unwrap();
    let restored = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let records = restored.sessions();
    let previous = records.first().unwrap();
    let resumed = restored
        .resume_with_tools(&previous.target(), capture_tools(provider, true).unwrap())
        .unwrap();
    assert_eq!(
        resumed.config.session_id.as_deref(),
        Some("capture-session")
    );
    assert_eq!(resumed.binding_id, "binding");
    assert_eq!(resumed.task_identity.as_deref(), Some("task"));
    assert_eq!(resumed.snapshot.status, NativeSessionStatus::Idle);
    assert_eq!(resumed.snapshot.first_turn, None);
    if provider == AgentKind::Pi {
        assert_eq!(
            resumed
                .snapshot
                .transcript
                .iter()
                .find_map(|item| item.tool.as_ref())
                .unwrap()
                .name,
            if nested {
                "codemode"
            } else {
                "computer_snapshot"
            }
        );
    }
    restored.shutdown().unwrap();
}

#[rstest]
#[case::codex_foreign_id(AgentKind::Codex, "id")]
#[case::codex_unrelated_name(AgentKind::Codex, "name")]
#[case::codex_foreign_server(AgentKind::Codex, "server")]
#[case::pi_foreign_id(AgentKind::Pi, "id")]
#[case::pi_unrelated_name(AgentKind::Pi, "name")]
#[case::pi_foreign_server(AgentKind::Pi, "server")]
#[case::pi_unqualified_name(AgentKind::Pi, "plain")]
#[case::pi_no_computer_grant(AgentKind::Pi, "permission")]
#[case::codex_no_computer_grant(AgentKind::Codex, "permission")]
#[case::codex_invalid_png(AgentKind::Codex, "png")]
#[case::pi_invalid_png(AgentKind::Pi, "png")]
#[case::pi_foreign_nested_parent(AgentKind::Pi, "nested_parent")]
#[case::pi_changed_nested_pixels(AgentKind::Pi, "nested_changed_image")]
#[case::pi_changed_capture_parent(AgentKind::Pi, "nested_capture_parent")]
#[case::pi_changed_completion_pixels(AgentKind::Pi, "completion_image")]
#[case::pi_foreign_completion_id(AgentKind::Pi, "completion_id")]
#[case::pi_foreign_completion_parent(AgentKind::Pi, "nested_completion_parent")]
fn large_tool_records_require_the_exact_start_and_valid_bounded_png(
    #[case] provider: AgentKind,
    #[case] fault: &str,
) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("capture-fault"), fault).unwrap();
    let bytes = if fault == "png" {
        vec![0; 1024 * 1024]
    } else {
        png(1024 * 1024).unwrap()
    };
    fs::write(root.path().join("capture.png"), bytes).unwrap();
    if matches!(fault, "nested_changed_image" | "completion_image") {
        fs::write(
            root.path().join("changed.png"),
            png(1024 * 1024 + 1).unwrap(),
        )
        .unwrap();
    }
    let mut config = NativeSessionConfig::new(provider, root.path());
    config.program = capture_provider(root.path()).unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task_with_tools(
            "binding",
            "task",
            "Capture",
            config,
            capture_tools(provider, fault != "permission").unwrap(),
        )
        .unwrap();
    assert!(
        service
            .prompt(&record.target(), "Capture the exact attached window")
            .is_err()
    );
    let snapshot = service.resolve(&record.target()).unwrap().snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Error);
    assert!(!snapshot.completed_turn);
    assert_ne!(
        snapshot.first_turn.as_ref().unwrap().outcome,
        NativeTurnOutcome::Succeeded
    );
    service.shutdown().unwrap();
}
