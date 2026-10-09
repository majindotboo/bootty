#![cfg(unix)]

use std::{fs, io::Cursor, os::unix::fs::PermissionsExt as _, path::Path};

use assert_fs::TempDir;
use bootty_agents::{
    AgentKind, NativeAgentService, NativeAgentSession, NativeSessionConfig, NativeSessionStatus,
    NativeToolStatus, NativeTranscriptItem, NativeTurnOutcome,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

const PI_FIXTURE: &str = r#"#!/usr/bin/env python3
import json,os,signal,sys
assert sys.argv[-2:] == ['--mode','rpc'] or '--session' in sys.argv,sys.argv
file=sys.argv[sys.argv.index('--session')+1] if '--session' in sys.argv else os.path.join(os.getcwd(),'saved.jsonl')
session='owned-pi-id'
fresh=os.path.exists('unique-sessions') and '--session' not in sys.argv
if fresh:
 session='owned-pi-'+str(os.getpid())
 file=os.path.join(os.getcwd(),'account','sessions',session+'.jsonl')
 with open(file,'w') as output: output.write(json.dumps({'type':'session','id':session,'version':3})+'\n')
with open('launch.json','w') as output: json.dump({'argv':sys.argv,'account':os.environ.get('PI_CODING_AGENT_DIR'),'pid':os.getpid()},output)
def closing(*args):
 with open('exited','w') as output: output.write('closed')
 sys.exit(0)
signal.signal(signal.SIGTERM,closing)
def emit(value): print(json.dumps(value),flush=True)
def reply(value,data=None,command=None,success=True):
 if value['type']=='prompt' and os.path.exists('legacy-ack'): data={}
 emit({'id':value['id'],'type':'response','command':command or value['type'],'success':success,'data':data or {}})
def state():
 return {'sessionId':session,'sessionFile':file if not os.path.exists('wrong-file') else file+'.wrong','isStreaming':working,'isCompacting':False}
messages=[] if fresh else [{'role':'user','content':'Saved history','timestamp':1}]
if os.path.exists('history.json') and not fresh:
 with open('history.json') as source: messages=json.load(source)
working=False
waiting_command=None
message_timestamp=2
for line in sys.stdin:
 value=json.loads(line)
 kind=value['type']
 if kind=='get_state':
  if os.path.exists('events.json'):
   with open('events.json') as source: events=json.load(source)
   os.unlink('events.json')
   for event in events:
    emit(event)
    if event.get('type')=='message_end': messages.append(event['message'])
  if os.path.exists('fault'):
   fault=open('fault').read()
   if fault=='reply': reply(value,state(),'prompt');continue
   if fault=='oversize': sys.stdout.write('x'*(1024*1024+1)+'\n');sys.stdout.flush();continue
   if fault=='partial': sys.stdout.write('{"type":"agent_start"}');sys.stdout.flush();break
   sys.stdout.write(fault+'\n');sys.stdout.flush();continue
  if os.path.exists('retry'):
   emit({'type':'agent_end','willRetry':True});emit({'type':'agent_start'});os.unlink('retry')
  if os.path.exists('settle'):
   emit({'type':'agent_settled'});working=False;os.unlink('settle')
  emit({'id':'foreign','type':'response','command':'get_state','success':True,'data':{'sessionId':'foreign','sessionFile':'/foreign'}})
  reply(value,state())
 elif kind=='get_messages': reply(value,{'messages':messages})
 elif kind=='get_entries':
  with open(file) as source: entries=[json.loads(line) for line in source if line.strip()]
  assert value.get('since')==entries[-1]['id']
  reply(value,{'entries':[],'leafId':open('history-leaf').read()})
 elif kind=='get_commands': reply(value,{'commands':[]})
 elif kind=='compact':
  with open('compact.json','w') as output: json.dump(value,output)
  reply(value,{'summary':'Observed summary'})
 elif kind=='prompt':
  with open('prompt.json','w') as output: json.dump(value,output)
  if value['message']=='handled': reply(value,{'disposition':'handled'});continue
  if value['message']=='/editor':
   waiting_command=value
   emit({'type':'extension_ui_request','id':'dialog','method':'editor','title':'Question','prefill':'First line\nSecond line'})
   continue
  emit({'type':'agent_start'});working=True
  if value['message'] in ['confirm','select','input','editor']:
   for index in range(40): emit({'type':'extension_ui_request','id':'notice-'+str(index),'method':'notify','message':'Information'})
   emit({'type':'extension_ui_request','id':'dialog','method':value['message'],'title':'Question','options':['Allow','Block']})
   reply(value,{'disposition':'started'});continue
  user={'role':'user','content':value['message'],'timestamp':message_timestamp}
  assistant={'role':'assistant','content':[],'timestamp':message_timestamp+1,'stopReason':'pending'}
  message_timestamp+=2
  emit({'type':'message_start','message':user});emit({'type':'message_end','message':user})
  emit({'type':'message_start','message':assistant})
  assistant['content']=[{'type':'text','text':'Hello '}]
  emit({'type':'message_update','message':assistant,'assistantMessageEvent':{'type':'text_delta','contentIndex':0,'delta':'Hello '}})
  assistant['content']=[{'type':'text','text':'Hello world'}]
  emit({'type':'message_update','message':assistant,'assistantMessageEvent':{'type':'text_delta','contentIndex':0,'delta':'world'}})
  assistant['content']=[{'type':'text','text':'Hello world'}];assistant['stopReason']=value['message'] if value['message'] in ['error','aborted'] else 'stop'
  emit({'type':'message_end','message':assistant})
  end={'type':'agent_end','messages':[assistant],'willRetry':True}
  if os.path.exists('image-end-case'):
   case=open('image-end-case').read()
   images=value['images']
   if case=='wrong-mime': images[0]['mimeType']='image/jpeg'
   end['messages']=[{'role':'user','content':[{'type':'text','text':value['message']},*images]},assistant]
   if case=='foreign-event': end['type']='unrelated_event'
   if case=='control-overflow': end['extra']='x'*(1024*1024+1)
  emit(end)
  messages.extend([user,assistant]);reply(value,{'disposition':'started'})
 elif kind=='steer':
  assert working
  with open('steer.json','w') as output: json.dump(value,output)
  if value['message']=='reject': reply(value,success=False);continue
  prior=next(message for message in reversed(messages) if message['role']=='user')
  emit({'type':'message_end','message':prior})
  user={'role':'user','content':value['message'],'timestamp':message_timestamp}
  message_timestamp+=1
  emit({'type':'message_end','message':user});messages.append(user)
  reply(value,{'disposition':'queued'})
 elif kind=='extension_ui_response':
  with open('answer.json','w') as output: json.dump(value,output)
  if waiting_command is not None:
   reply(waiting_command,{'disposition':'handled'},success=not os.path.exists('reject-editor'));waiting_command=None;continue
  emit({'type':'agent_settled'});working=False
 elif kind=='abort':
  if os.path.exists('abort-needs-answer') and working:
   reply(value,success=False);continue
  emit({'type':'agent_settled'});working=False;reply(value)
 else: reply(value,success=False)
closing()
"#;

fn fixture(root: &Path) -> std::io::Result<String> {
    let path = root.join("pi-rpc.py");
    fs::write(&path, PI_FIXTURE)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path.to_string_lossy().into_owned())
}

fn config(root: &Path) -> std::io::Result<NativeSessionConfig> {
    let mut config = NativeSessionConfig::new(AgentKind::Pi, root);
    config.program = fixture(root)?;
    config.account_directory = Some(root.join("account").to_string_lossy().into_owned());
    config.arguments = vec![
        "--provider".to_owned(),
        "openai".to_owned(),
        "--model".to_owned(),
        "fixture-model".to_owned(),
    ];
    Ok(config)
}

#[rstest]
#[case::owned_image("valid", true)]
#[case::foreign_event("foreign-event", false)]
#[case::wrong_mime("wrong-mime", false)]
#[case::nonimage_budget("control-overflow", false)]
fn pi_large_completion_keeps_the_owned_image_budget(#[case] event: &str, #[case] accepted: bool) {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create("binding", "Image", config(root.path()).unwrap())
        .unwrap();
    let mut seed = 17_u32;
    let pixels = image::RgbaImage::from_fn(500, 500, |_, _| {
        let mut pixel = [0; 4];
        for channel in &mut pixel {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *channel = seed.to_le_bytes()[3];
        }
        image::Rgba(pixel)
    });
    let mut png = Cursor::new(Vec::new());
    pixels.write_to(&mut png, image::ImageFormat::Png).unwrap();
    assert!(
        png.get_ref().len() > 900_000,
        "completion envelope exceeds the control budget"
    );
    let path = root.path().join("image.png");
    fs::write(&path, png.get_ref()).unwrap();
    let attachment = service.import_attachment(&record.target(), &path).unwrap();
    let admitted = service
        .resolve_prompt_attachments(&record.target(), &[attachment.id])
        .unwrap();
    let prompt = bootty_agents::NativePrompt::new_with_context(
        "Describe the image".into(),
        Vec::new(),
        admitted,
        Vec::new(),
    )
    .unwrap();
    fs::write(root.path().join("image-end-case"), event).unwrap();
    let result = service.prompt_input(&record.target(), &prompt);
    assert_eq!(
        result.is_ok(),
        accepted,
        "completion case {event}: {result:?}"
    );
    if accepted {
        let session = service.resolve(&record.target()).unwrap();
        assert_eq!(
            session.snapshot().status,
            NativeSessionStatus::Working,
            "agent_end alone does not finish an owned run"
        );
        fs::write(root.path().join("settle"), "").unwrap();
        session.rpc("get_state", json!({})).unwrap();
        assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
        assert!(session.snapshot().completed_turn);
    }
    service.shutdown().unwrap();
}

#[rstest]
fn pi_side_chat_from_older_history_keeps_its_branch_and_displayed_page() {
    let root = TempDir::new().unwrap();
    let directory = root.path().join("account/sessions/project");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("owned.jsonl");
    let mut entries = vec![json!({"type":"session","id":"owned-pi-id","version":3})];
    for index in 0_usize..150 {
        entries.push(json!({
            "type":"message","id":format!("e{index}"),
            "parentId":index.checked_sub(1).map(|previous| format!("e{previous}")),
            "message":{"role":if index % 2 == 0 { "user" } else { "assistant" },
                "timestamp":index.checked_add(1).unwrap(),"stopReason":"stop",
                "content":[{"type":"text","text":format!("Context {index}")}]},
        }));
    }
    entries.push(json!({"type":"message","id":"detached","parentId":null,
        "message":{"role":"assistant","timestamp":9999,"stopReason":"stop","content":[{"type":"text","text":"Other branch"}]}}));
    let mut saved = entries
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    saved.push('\n');
    fs::write(&path, &saved).unwrap();
    fs::write(root.path().join("history-leaf"), "e149").unwrap();
    fs::write(root.path().join("unique-sessions"), "").unwrap();
    fs::write(
        root.path().join("history.json"),
        json!([{
            "role":"assistant","timestamp":150,"stopReason":"stop",
            "content":[{"type":"text","text":"Context 149"}],
        }])
        .to_string(),
    )
    .unwrap();
    let mut config = config(root.path()).unwrap();
    config.session_id = Some("owned-pi-id".into());
    config.session_file = Some(path.to_string_lossy().into_owned());
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let parent = service
        .create_for_task("binding", "task", "Parent", config)
        .unwrap();
    let session = service.resolve(&parent.target()).unwrap();
    let before = session.snapshot();
    service.read_history(&parent.target(), "latest").unwrap();
    let older = service.read_history(&parent.target(), "older").unwrap();
    let boundary = older
        .transcript
        .iter()
        .find(|item| item.role == "assistant")
        .unwrap();
    let child = service
        .fork_side_chat(&parent.target(), Some(&boundary.id), None)
        .unwrap();
    let copied = &child.side_chat.as_ref().unwrap().transcript;
    assert_eq!(copied.first().unwrap().text, "Context 0");
    assert_eq!(copied.last().unwrap().text, "Context 23");
    assert_eq!(copied.len(), 24);
    assert!(copied.iter().all(|item| item.text != "Other branch"));
    assert_ne!(child.snapshot.session_id, before.session_id);
    assert_eq!(session.snapshot(), before);
    let newer = service.read_history(&parent.target(), "newer").unwrap();
    assert_eq!(newer.transcript.first().unwrap().text, "Context 86");
    assert!(newer.at_latest);
    service
        .prompt(&child.target(), "Continue the earlier response")
        .unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    let text = wire["message"].as_str().unwrap();
    assert!(text.contains("Context 0"));
    assert!(text.contains("Context 23"));
    assert!(!text.contains("Context 24"));
    assert!(!text.contains("Other branch"));
    assert_eq!(fs::read_to_string(&path).unwrap(), saved);
    service.shutdown().unwrap();
}

#[rstest]
#[case::before_compaction(130, "Earlier claim 🥟".to_owned())]
#[case::past_live_window(300, "Earlier claim".to_owned())]
#[case::large_records(70, "\n\"".repeat(8_000))]
fn pi_history_pages_follow_the_live_branch_through_compaction(
    #[case] count: usize,
    #[case] text: String,
) {
    let root = TempDir::new().unwrap();
    let directory = root.path().join("account/sessions/project");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("owned.jsonl");
    let mut entries = vec![json!({"type":"session","id":"owned-pi-id","version":3})];
    for index in 0..count {
        entries.push(json!({
            "type":"message","id":format!("e{index}"),
            "parentId":index.checked_sub(1).map(|previous| format!("e{previous}")),
            "message":{"role":"assistant","timestamp":index.checked_add(1).unwrap(),"content":[{"type":"text","text":text}]},
        }));
    }
    entries.push(json!({"type":"compaction","id":"compact","parentId":format!("e{}",count.checked_sub(1).unwrap()),"timestamp":"2026-10-06T00:00:00Z","summary":"Observed summary"}));
    entries.push(json!({"type":"message","id":"latest","parentId":"compact","message":{"role":"assistant","timestamp":count.checked_add(1).unwrap(),"content":[{"type":"text","text":"Latest claim"}]}}));
    // The file's tail belongs to another branch; the provider's live leaf selects the real one.
    entries.push(json!({"type":"message","id":"detached","parentId":null,"message":{"role":"assistant","timestamp":9999,"content":[{"type":"text","text":"Other branch"}]}}));
    let mut saved = String::new();
    for entry in &entries {
        saved.push_str(&entry.to_string());
        saved.push('\n');
    }
    fs::write(&path, &saved).unwrap();
    fs::write(root.path().join("history-leaf"), "latest").unwrap();
    let mut config = config(root.path()).unwrap();
    config.session_id = Some("owned-pi-id".into());
    config.session_file = Some(path.to_string_lossy().into_owned());
    let session = NativeAgentSession::spawn(config).unwrap();
    let live = session.snapshot();
    let mut page = session.read_history("latest").unwrap();
    let mut ids = std::collections::BTreeSet::new();
    loop {
        assert!(serde_json::to_vec(&page).unwrap().len() < 600 * 1024);
        for item in &page.transcript {
            assert!(
                ids.insert(item.id.clone()),
                "Pi pagination repeated a message"
            );
            assert_ne!(item.text, "Other branch");
        }
        if !page.has_older {
            break;
        }
        page = session.read_history("older").unwrap();
    }
    let expected = (1..=count.checked_add(1).unwrap())
        .map(|timestamp| format!("pi-assistant-{timestamp}"))
        .chain(["pi-entry-compact".into()])
        .collect();
    assert_eq!(ids, expected);
    assert_eq!(session.snapshot(), live);
    while page.has_newer {
        page = session.read_history("newer").unwrap();
    }
    assert!(page.at_latest);
    fs::write(root.path().join("history-leaf"), "detached").unwrap();
    assert!(session.read_history("older").is_err());
    let detached = session.read_history("latest").unwrap();
    assert_eq!(detached.transcript.len(), 1);
    assert_eq!(detached.transcript[0].text, "Other branch");
    fs::write(&path, saved.replacen("owned-pi-id", "foreign-id", 1)).unwrap();
    assert!(session.read_history("latest").is_err());
    assert_eq!(session.snapshot(), live);
}

#[rstest]
fn native_attachments_are_private_session_scoped_and_survive_restart() {
    let root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service
        .create("binding", "Conversation", config(root.path()).unwrap())
        .unwrap();
    let other = service
        .create(
            "other-binding",
            "Other",
            config(second_root.path()).unwrap(),
        )
        .unwrap();

    let markdown = root.path().join("notes.md");
    fs::write(&markdown, "private attachment body").unwrap();
    let file = service
        .import_attachment(&record.target(), &markdown)
        .unwrap();
    assert_eq!(file.kind, bootty_agents::NativeAttachmentKind::File);
    assert_eq!(file.mime_type, "text/markdown");
    assert_ne!(file.id, markdown.to_string_lossy());

    let image_path = root.path().join("screenshot.png");
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([10, 20, 30, 255]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    fs::write(&image_path, png.get_ref()).unwrap();
    let image = service
        .import_attachment(&record.target(), &image_path)
        .unwrap();
    assert_eq!(image.kind, bootty_agents::NativeAttachmentKind::Image);
    assert_eq!(image.pixel_width, Some(1));
    assert_eq!(image.pixel_height, Some(1));

    assert!(
        service
            .resolve_prompt_attachments(&other.target(), std::slice::from_ref(&file.id))
            .is_err()
    );
    let ids = vec![file.id.clone(), image.id.clone()];
    let prompt_attachments = service
        .resolve_prompt_attachments(&record.target(), &ids)
        .unwrap();
    let prompt = bootty_agents::NativePrompt::new_with_context(
        "Review these files.".to_owned(),
        Vec::new(),
        prompt_attachments,
        Vec::new(),
    )
    .unwrap();
    let snapshot = service.prompt_input(&record.target(), &prompt).unwrap();
    let user = snapshot
        .transcript
        .iter()
        .find(|item| item.role == "user" && !item.attachments.is_empty())
        .unwrap();
    assert_eq!(user.text, "Review these files.");
    assert_eq!(user.attachments, vec![file.clone(), image.clone()]);
    assert_eq!(user.images.len(), 1);

    let received: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    let provider_message = received["message"].as_str().unwrap();
    assert!(provider_message.contains("[Attached file \"notes.md\" is saved at:"));
    assert!(provider_message.contains("native-attachments"));
    assert!(!provider_message.contains(&markdown.to_string_lossy().to_string()));
    assert_eq!(received["images"].as_array().unwrap().len(), 1);

    fs::write(root.path().join("settle"), "").unwrap();
    service
        .resolve(&record.target())
        .unwrap()
        .rpc("get_state", json!({}))
        .unwrap();

    let assistant = snapshot
        .transcript
        .iter()
        .find(|item| item.role == "assistant")
        .unwrap();
    let citation = bootty_agents::NativeResponseCitation {
        message_id: assistant.id.clone(),
        source_range: 6..11,
        prompt_range: None,
        quote: "world".to_owned(),
        comment: "Clarify this claim.".to_owned(),
    };
    let citation_prompt = bootty_agents::NativePrompt::new_with_context(
        "Please revise.".to_owned(),
        Vec::new(),
        bootty_agents::NativePromptAttachments::default(),
        vec![citation.clone()],
    )
    .unwrap();
    let cited_snapshot = service
        .prompt_input(&record.target(), &citation_prompt)
        .unwrap();
    let cited_user = cited_snapshot
        .transcript
        .iter()
        .find(|item| item.role == "user" && item.text == "Please revise.")
        .unwrap();
    assert_eq!(cited_user.citations, vec![citation]);
    let citation_wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    let citation_message = citation_wire["message"].as_str().unwrap();
    assert!(citation_message.contains("<assistant_citations>"));
    assert!(citation_message.contains("\"source_range\":{\"start\":6,\"end\":11}"));
    assert!(
        service
            .resolve(&record.target())
            .unwrap()
            .snapshot()
            .transcript
            .iter()
            .any(|item| item.role == "user" && item.text == "Please revise.")
    );

    fs::write(root.path().join("settle"), "").unwrap();
    service
        .resolve(&record.target())
        .unwrap()
        .rpc("get_state", json!({}))
        .unwrap();
    service
        .resolve(&record.target())
        .unwrap()
        .refresh_history()
        .unwrap();
    let refreshed = service.resolve(&record.target()).unwrap().snapshot();
    let refreshed_user = refreshed
        .transcript
        .iter()
        .find(|item| item.role == "user" && item.text == "Please revise.")
        .unwrap();
    assert_eq!(refreshed_user.citations, cited_user.citations);
    assert_eq!(refreshed_user.text, "Please revise.");

    let large_comment = "c".repeat(8_000);
    let oversized_context = bootty_agents::NativePrompt::new_with_context(
        String::new(),
        Vec::new(),
        bootty_agents::NativePromptAttachments::default(),
        vec![
            bootty_agents::NativeResponseCitation {
                message_id: assistant.id.clone(),
                source_range: 6..11,
                prompt_range: None,
                quote: "world".to_owned(),
                comment: large_comment,
            };
            9
        ],
    );
    assert!(oversized_context.is_err());
    assert_eq!(
        service.resolve(&record.target()).unwrap().snapshot().status,
        NativeSessionStatus::Idle,
        "oversized provider context must fail before the session starts"
    );

    let serialized = fs::read_to_string(&catalog).unwrap();
    assert!(!serialized.contains(&markdown.to_string_lossy().to_string()));
    service.stop(&record.target()).unwrap();
    service.stop(&other.target()).unwrap();
    service.shutdown().unwrap();
    drop(service);

    let restored = NativeAgentService::open(&catalog).unwrap();
    let restored_record = restored
        .sessions()
        .into_iter()
        .find(|candidate| candidate.id == record.id)
        .unwrap();
    assert_eq!(restored_record.attachments, vec![file, image.clone()]);
    let restored_citation = restored_record
        .snapshot
        .transcript
        .iter()
        .find(|item| item.role == "user" && item.text == "Please revise.")
        .unwrap();
    assert_eq!(restored_citation.citations.len(), 1);
    assert_eq!(restored_citation.text, "Please revise.");
    let preview = restored
        .preview_attachment(&restored_record.target(), &image.id)
        .unwrap();
    let reader = png::Decoder::new(Cursor::new(preview)).read_info().unwrap();
    assert!(reader.info().width <= 1600);
    assert!(reader.info().height <= 1600);
    assert!(
        restored
            .resolve_prompt_attachments(&restored_record.target(), &ids)
            .is_ok()
    );
    restored.shutdown().unwrap();
}

#[rstest]
#[case(false)]
#[case(true)]
fn exact_process_identity_streaming_history_and_abort_share_one_rpc_owner(
    #[case] legacy_ack: bool,
) {
    let root = TempDir::new().unwrap();
    if legacy_ack {
        fs::write(root.path().join("legacy-ack"), "").unwrap();
    }
    let config = config(root.path()).unwrap();
    let session = NativeAgentSession::spawn(config.clone()).unwrap();
    assert_eq!(
        session.snapshot().session_id.as_deref(),
        Some("owned-pi-id")
    );
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    assert_eq!(session.snapshot().transcript[0].text, "Saved history");
    let prompt = "literal 'quotes'\nnext\tline\u{2028}continues";
    session.send_prompt(prompt).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Working);
    assert!(
        !snapshot.completed_turn,
        "agent_end is not a session completion"
    );
    assert_eq!(
        snapshot
            .transcript
            .iter()
            .filter(|item| item.role == "user" && item.text == prompt)
            .count(),
        1
    );
    assert_eq!(
        snapshot
            .transcript
            .iter()
            .find(|item| item.role == "assistant")
            .unwrap()
            .text,
        "Hello world"
    );
    let received: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(received["message"], prompt);
    assert!(received.get("method").is_none());
    fs::write(root.path().join("settle"), "").unwrap();
    session.rpc("get_state", json!({})).unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    assert!(session.snapshot().completed_turn);
    assert_eq!(session.refresh_history().unwrap().transcript.len(), 3);
    session.send_prompt("next").unwrap();
    session.interrupt().unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    assert!(!session.snapshot().completed_turn);
    session.stop();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Stopped);
    assert!(session.rpc("get_state", json!({})).is_err());
    let launch: Value =
        serde_json::from_slice(&fs::read(root.path().join("launch.json")).unwrap()).unwrap();
    assert_eq!(launch["account"], config.account_directory.unwrap());
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &launch["pid"].to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success(),
        "the owned provider was reaped"
    );
    assert_eq!(
        launch["argv"].as_array().unwrap().last(),
        Some(&json!("rpc"))
    );
}

#[rstest]
#[case("confirm", json!({"confirmed":true}), json!({"value":true}))]
#[case("select", json!({"value":"Allow"}), json!({"value":"unlisted"}))]
#[case("input", json!({"value":"typed"}), json!({"value":12}))]
#[case("editor", json!({"cancelled":true}), json!({"cancelled":false}))]
fn only_pending_dialogs_accept_their_native_answer_schema(
    #[case] method: &str,
    #[case] answer: Value,
    #[case] invalid: Value,
) {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create("captured-binding", "Dialog", config(root.path()).unwrap())
        .unwrap();
    let session = service.resolve(&record.target()).unwrap();
    service.prompt(&record.target(), method).unwrap();
    service.checkpoint().unwrap();
    let activity = service.activities().remove(0);
    assert_eq!(activity.id, record.id);
    assert_eq!(activity.generation, record.generation);
    assert_eq!(activity.approval, method == "confirm");
    assert_eq!(activity.input, method != "confirm");
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Waiting);
    assert_eq!(
        snapshot.requests.len(),
        1,
        "notifications never accumulate pending replies"
    );
    assert_eq!(
        snapshot
            .transcript
            .iter()
            .filter(|item| item.role == "notice")
            .count(),
        40
    );
    let request = &snapshot.requests[0];
    assert!(session.respond("unknown", answer.clone()).is_err());
    assert!(session.respond(&request.id, invalid).is_err());
    session.respond(&request.id, answer.clone()).unwrap();
    session.rpc("get_state", json!({})).unwrap();
    let response: Value =
        serde_json::from_slice(&fs::read(root.path().join("answer.json")).unwrap()).unwrap();
    assert_eq!(response["id"], "dialog");
    assert_eq!(response["type"], "extension_ui_response");
    for (key, value) in answer.as_object().unwrap() {
        assert_eq!(response.get(key), Some(value));
    }
    assert_eq!(session.snapshot().requests, []);
    assert!(session.respond(&request.id, answer).is_err());
    service.checkpoint().unwrap();
    let activity = service.activities().remove(0);
    assert!(!activity.approval);
    assert!(!activity.input);
}

#[rstest]
#[case::multiline_answer(Some(json!({"value":"First edited line\nSecond edited line"})), NativeSessionStatus::Idle)]
#[case::cancelled(Some(json!({"cancelled":true})), NativeSessionStatus::Idle)]
#[case::interrupted(None, NativeSessionStatus::Idle)]
#[case::stopped(None, NativeSessionStatus::Stopped)]
#[case::provider_rejected(Some(json!({"value":"Rejected answer"})), NativeSessionStatus::Error)]
fn pi_extension_editor_can_wait_for_input_before_acknowledging_its_prompt(
    #[case] answer: Option<Value>,
    #[case] expected: NativeSessionStatus,
) {
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    let root = TempDir::new().unwrap();
    if expected == NativeSessionStatus::Error {
        fs::write(root.path().join("reject-editor"), "").unwrap();
    }
    let session = Arc::new(NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap());
    let (wake, changes) = mpsc::channel();
    session.set_change_handler(Arc::new(move || {
        let _ = wake.send(());
    }));
    let worker = Arc::clone(&session);
    let (reply, replies) = mpsc::channel();
    let prompt = std::thread::spawn(move || reply.send(worker.send_prompt("/editor")).unwrap());
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .expect("fixture deadline");
    while session.snapshot().requests.is_empty() {
        changes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
    }
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Waiting);
    assert_eq!(snapshot.first_turn, None);
    replies
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    prompt.join().unwrap();
    if expected == NativeSessionStatus::Stopped {
        session.stop();
    } else if let Some(answer) = &answer {
        session
            .respond(&snapshot.requests[0].id, answer.clone())
            .unwrap();
    } else {
        session.interrupt().unwrap();
    }
    while session.snapshot().status != expected {
        changes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
    }
    if expected != NativeSessionStatus::Stopped {
        let sent: Value =
            serde_json::from_slice(&fs::read(root.path().join("answer.json")).unwrap()).unwrap();
        for (key, value) in answer
            .unwrap_or(json!({"cancelled":true}))
            .as_object()
            .unwrap()
        {
            assert_eq!(sent[key], *value);
        }
    }
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, expected);
    assert_eq!(snapshot.first_turn, None);
    assert_eq!(
        snapshot.error.is_some(),
        expected == NativeSessionStatus::Error
    );
    assert_eq!(snapshot.requests, []);
    session.stop();
}

#[rstest]
fn resume_uses_the_exact_saved_file_account_and_task_identity() {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task("binding", "task", "Pi", config(root.path()).unwrap())
        .unwrap();
    assert!(record.id.starts_with("native:pi:"));
    assert_eq!(record.snapshot.session_id.as_deref(), Some("owned-pi-id"));
    assert_eq!(record.config.session_id, record.snapshot.session_id);
    assert_eq!(record.config.session_file, record.snapshot.session_file);
    service.prompt(&record.target(), "saved prompt").unwrap();
    service.stop(&record.target()).unwrap();
    let resumed = service.resume(&record.target()).unwrap();
    assert_eq!(resumed.binding_id, record.binding_id);
    assert_eq!(resumed.task_identity, record.task_identity);
    assert_eq!(
        resumed.config.account_directory,
        record.config.account_directory
    );
    assert_eq!(resumed.snapshot.session_id, record.snapshot.session_id);
    assert_eq!(resumed.snapshot.session_file, record.snapshot.session_file);
    assert!(service.prompt(&record.target(), "stale").is_err());
    let launch: Value =
        serde_json::from_slice(&fs::read(root.path().join("launch.json")).unwrap()).unwrap();
    assert!(
        launch["argv"]
            .as_array()
            .unwrap()
            .windows(2)
            .any(|pair| pair == [json!("--session"), json!(resumed.snapshot.session_file)])
    );
    service.shutdown().unwrap();
}

#[rstest]
#[case("reply")]
#[case("oversize")]
#[case("partial")]
#[case("[]")]
#[case("{")]
fn invalid_or_mismatched_protocol_cannot_initialize_a_live_session(#[case] fault: &str) {
    let root = TempDir::new().unwrap();
    let config = config(root.path()).unwrap();
    fs::write(root.path().join("fault"), fault).unwrap();
    assert!(NativeAgentSession::spawn(config).is_err());
}

#[rstest]
fn provider_identity_changes_and_handled_prompts_do_not_invent_turn_success() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("handled").unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    assert!(!session.snapshot().completed_turn);
    fs::write(root.path().join("wrong-file"), "").unwrap();
    assert!(session.send_prompt("wrong identity").is_err());
    assert!(session.refresh_history().is_err());
    assert_eq!(
        session.snapshot().session_id.as_deref(),
        Some("owned-pi-id")
    );
}

proptest! {
    #[test]
    fn relative_resume_selectors_are_rejected_before_launch(selector in "[a-zA-Z0-9_-]{1,32}") {
        let root = TempDir::new().unwrap();
        let mut config = config(root.path()).unwrap();
        config.session_id = Some("captured-uuid".to_owned());
        config.session_file = Some(selector);
        prop_assert!(NativeAgentSession::spawn(config).is_err());
        prop_assert!(!root.path().join("launch.json").exists());
    }
}

#[rstest]
fn pi_first_dispatch_keeps_accepted_abort_outcome_after_manual_success() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    assert_eq!(session.snapshot().first_turn, None);
    session.send_prompt("handled").unwrap();
    assert_eq!(session.snapshot().first_turn, None);
    session.send_prompt("first work").unwrap();
    let mut first = session.snapshot().first_turn.unwrap();
    assert_eq!(first.outcome, NativeTurnOutcome::Running);
    // agent_end has already arrived but automatic work remains active until settled.
    session.interrupt().unwrap();
    first.outcome = NativeTurnOutcome::Interrupted;
    assert_eq!(session.snapshot().first_turn, Some(first.clone()));
    session.send_prompt("later work").unwrap();
    fs::write(root.path().join("settle"), "").unwrap();
    session.rpc("get_state", json!({})).unwrap();
    assert_eq!(session.snapshot().completed_turn, true);
    assert_eq!(session.snapshot().first_turn, Some(first));
    session.stop();
}

#[rstest]
fn pi_active_followup_uses_steer_without_replacing_the_original_run() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("first work").unwrap();
    session.rpc("get_state", json!({})).unwrap();
    let active = session.snapshot();
    session.send_prompt("change course").unwrap();
    session.rpc("get_state", json!({})).unwrap();
    let steered = session.snapshot();
    assert_eq!(steered.status, NativeSessionStatus::Working);
    assert_eq!(steered.turn_id, active.turn_id);
    assert_eq!(steered.first_turn, active.first_turn);
    let received: Value =
        serde_json::from_slice(&fs::read(root.path().join("steer.json")).unwrap()).unwrap();
    assert_eq!(received["type"], "steer");
    assert_eq!(received["message"], "change course");
    assert!(
        steered
            .transcript
            .iter()
            .any(|item| item.role == "user" && item.text == "change course")
    );
    assert!(
        steered
            .transcript
            .iter()
            .any(|item| item.role == "user" && item.text == "first work")
    );
    assert_eq!(
        steered
            .transcript
            .iter()
            .map(|item| &item.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        steered.transcript.len()
    );
    assert!(session.send_prompt("reject").is_err());
    assert_eq!(session.snapshot().status, NativeSessionStatus::Working);
    assert!(
        !session
            .snapshot()
            .transcript
            .iter()
            .any(|item| item.text == "reject")
    );
    session.interrupt().unwrap();
}

#[rstest]
#[case("normal", NativeTurnOutcome::Succeeded)]
#[case("error", NativeTurnOutcome::Failed)]
#[case("aborted", NativeTurnOutcome::Interrupted)]
fn pi_first_receipt_uses_fixed_admission_identity_and_actual_session_outcome(
    #[case] prompt: &str,
    #[case] outcome: NativeTurnOutcome,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(prompt).unwrap();
    let first_id = session.snapshot().first_turn.unwrap().id;
    if prompt == "normal" {
        fs::write(root.path().join("retry"), "").unwrap();
        session.rpc("get_state", json!({})).unwrap();
        assert_eq!(
            session
                .snapshot()
                .first_turn
                .as_ref()
                .map(|receipt| receipt.id.as_str()),
            Some(first_id.as_str())
        );
        assert_eq!(
            session
                .snapshot()
                .first_turn
                .as_ref()
                .map(|receipt| receipt.outcome),
            Some(NativeTurnOutcome::Running)
        );
    }
    fs::write(root.path().join("settle"), "").unwrap();
    session.rpc("get_state", json!({})).unwrap();
    let first = session.snapshot().first_turn.unwrap();
    assert_eq!(first.id, first_id);
    assert_eq!(first.outcome, outcome);
    session.stop();
}

#[rstest]
fn pi_projects_messages_without_turning_extension_ui_state_into_chat() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    let assistant = json!({"role":"assistant","timestamp":10,"content":[{"type":"text","text":"\u{001b}[38;5;109mHello world\u{001b}[39m"}]});
    let thinking = json!({"role":"assistant","timestamp":11,"content":[{"type":"thinking","thinking":"Planning"},{"type":"toolCall","id":"call","name":"read","arguments":{}}]});
    let custom = json!({"role":"custom","timestamp":12,"customType":"extension-note","display":true,"content":"A custom message"});
    let hidden = json!({"role":"custom","timestamp":13,"customType":"private-context","display":false,"content":"Hidden context"});
    let tool = json!({"role":"toolResult","timestamp":14,"toolCallId":"call","content":[{"type":"text","text":"\u{001b}[2Ktool\n\toutput\u{001b}]8;;https://example.com\u{0007} link\u{001b}]8;;\u{0007}"}]});
    let events = json!([
        {"type":"extension_ui_request","id":"status","method":"setStatus","statusKey":"codex-native-context","statusText":"\u{001b}[38;5;109mBalanced (272k)\u{001b}[39m"},
        {"type":"extension_ui_request","id":"cache","method":"setStatus","statusKey":"codex-native-cache","statusText":"Codex Cache"},
        {"type":"extension_ui_request","id":"clear","method":"setStatus","statusKey":"codex-native-cache"},
        {"type":"extension_ui_request","id":"widget","method":"setWidget","widgetLines":["Codex Cache"]},
        {"type":"extension_ui_request","id":"title","method":"setTitle","title":"Pi title"},
        {"type":"extension_ui_request","id":"editor","method":"set_editor_text","text":"Editor draft"},
        {"type":"extension_ui_request","id":"notify","method":"notify","message":"\u{001b}[31mVisible notice\u{001b}[0m"},
        {"type":"extension_ui_request","id":"empty-notify","method":"notify","message":""},
        {"type":"message_start","message":{"role":"assistant","timestamp":10,"content":[]}},
        {"type":"message_update","message":{"role":"assistant","timestamp":10,"content":[{"type":"text","text":"\u{001b}[38;5;109mHello "}]},"assistantMessageEvent":{"type":"text_delta","delta":"Hello "}},
        {"type":"message_update","message":assistant,"assistantMessageEvent":{"type":"text_delta","delta":"world"}},
        {"type":"message_end","message":assistant},
        {"type":"message_start","message":{"role":"assistant","timestamp":11,"content":[]}},
        {"type":"message_update","message":thinking,"assistantMessageEvent":{"type":"thinking_delta","delta":"Planning"}},
        {"type":"message_end","message":thinking},
        {"type":"message_end","message":custom},
        {"type":"message_end","message":hidden},
        {"type":"message_end","message":tool}
    ]);
    fs::write(root.path().join("events.json"), events.to_string()).unwrap();
    session.rpc("get_state", json!({})).unwrap();
    let snapshot = session.snapshot();
    let transcript = snapshot
        .transcript
        .iter()
        .map(|item| (item.role.as_str(), item.text.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        transcript,
        vec![
            ("user", "Saved history"),
            ("notice", "Visible notice"),
            ("assistant", "Hello world"),
            ("thinking", "Planning"),
            ("tool", "tool\n\toutput link"),
            ("custom", "A custom message"),
        ]
    );
    assert_eq!(snapshot.requests, Vec::new());
    assert!(snapshot.transcript.iter().all(|item| item.complete));
    let tool = snapshot
        .transcript
        .iter()
        .find_map(|item| item.tool.as_ref())
        .unwrap();
    assert_eq!(tool.name, "read");
    assert_eq!(tool.input, "{}");
    assert_eq!(tool.status, NativeToolStatus::Completed);
    let history = session.refresh_history().unwrap();
    assert_eq!(
        history
            .transcript
            .iter()
            .map(|item| (item.role.as_str(), item.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("user", "Saved history"),
            ("assistant", "Hello world"),
            ("thinking", "Planning"),
            ("tool", "tool\n\toutput link"),
            ("custom", "A custom message"),
        ]
    );
    session.stop();
}

#[rstest]
#[case::success(false, NativeToolStatus::Completed)]
#[case::failure(true, NativeToolStatus::Failed)]
fn pi_tool_results_keep_the_observed_name_input_and_failure_across_history(
    #[case] is_error: bool,
    #[case] status: NativeToolStatus,
) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    let arguments = json!({"cmd":"printf 'tool input'"});
    fs::write(
        root.path().join("events.json"),
        json!([
            {"type":"message_end","message":{"role":"assistant","timestamp":2,"content":[{"type":"toolCall","id":"exec","name":"exec_command","arguments":arguments}]}},
            {"type":"tool_execution_start","toolCallId":"exec","toolName":"exec_command","args":arguments},
        ]).to_string(),
    ).unwrap();
    session.rpc("get_state", json!({})).unwrap();
    let started = session.snapshot();
    let item = started
        .transcript
        .iter()
        .find(|item| item.id == "tool-exec")
        .unwrap();
    assert!(!item.complete);
    assert_eq!(
        item.tool.as_ref().unwrap().status,
        NativeToolStatus::Running
    );
    fs::write(
        root.path().join("events.json"),
        json!([
            {"type":"tool_execution_end","toolCallId":"exec","toolName":"exec_command","result":{"content":[{"type":"text","text":"actual result"}]},"isError":is_error},
            {"type":"message_end","message":{"role":"toolResult","timestamp":3,"toolCallId":"exec","toolName":"exec_command","content":[{"type":"text","text":"actual result"}],"isError":is_error}},
        ]).to_string(),
    ).unwrap();
    session.rpc("get_state", json!({})).unwrap();
    for snapshot in [session.snapshot(), session.refresh_history().unwrap()] {
        let item = snapshot
            .transcript
            .iter()
            .find(|item| item.id == "tool-exec")
            .unwrap();
        let tool = item.tool.as_ref().unwrap();
        assert!(item.complete);
        assert_eq!(item.text, "actual result");
        assert_eq!(tool.name, "exec_command");
        assert_eq!(tool.input, arguments.to_string());
        assert_eq!(tool.status, status);
    }
    session.stop();
}

#[rstest]
#[case::builtin_completed("succeeded", NativeToolStatus::Completed, false)]
#[case::builtin_failed("failed", NativeToolStatus::Failed, false)]
#[case::mcp_completed("succeeded", NativeToolStatus::Completed, true)]
#[case::mcp_failed("failed", NativeToolStatus::Failed, true)]
fn nested_tool_results_survive_history_refresh_and_process_restart(
    #[case] status: &str,
    #[case] expected: NativeToolStatus,
    #[case] mcp: bool,
) {
    let root = TempDir::new().unwrap();
    let launch = config(root.path()).unwrap();
    let args = json!({"cmd":"printf 'nested output'"});
    // Identifier and metadata observed from the installed Pi MCP client.
    let (name, expected_name, details) = if mcp {
        (
            "mcp__bootty_81f1d166be8d1bd895a84b735b886251b59a0b5f3dc_0fe6f294",
            "terminal_read",
            json!({"server":"bootty_81f1d166be8d1bd895a84b735b886251b59a0b5f3dcc4ec396ad4eea9af26f0f", "tool":"terminal_read"}),
        )
    } else {
        ("exec_command", "exec_command", json!({}))
    };
    let message = json!({
        "role":"toolResult", "timestamp":2, "toolCallId":"parent", "toolName":"codemode",
        "content":[{"type":"text","text":"Parent output"}],
        "details":{"libtuiNestedCalls":{"version":1,"omitted":0,"calls":[
            {"id":"parent/1","name":name,"args":args,"status":status,
             "result":{"content":[{"type":"text","text":"Nested output"}],"details":details}},
            {"id":"foreign/1","name":"exec_command","args":{},"status":"succeeded"},
            {"id":"parent/2","name":"exec_command","args":{},"status":"unknown"}
        ]}}
    });
    fs::write(
        root.path().join("history.json"),
        json!([message]).to_string(),
    )
    .unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service.create("binding", "Conversation", launch).unwrap();
    let initial = service
        .resolve(&record.target())
        .unwrap()
        .snapshot()
        .transcript;
    assert_eq!(
        initial
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["tool-parent", "tool-parent/1"]
    );
    let child = initial
        .iter()
        .find(|item| item.id == "tool-parent/1")
        .unwrap();
    assert_eq!(child.text, "Nested output");
    let tool = child.tool.as_ref().unwrap();
    assert_eq!(tool.name, expected_name);
    assert_eq!(tool.input, args.to_string());
    assert_eq!(tool.status, expected);
    let refreshed = service
        .resolve(&record.target())
        .unwrap()
        .refresh_history()
        .unwrap();
    assert_eq!(refreshed.transcript, initial);
    service.shutdown().unwrap();
    drop(service);
    let reopened = NativeAgentService::open(&catalog).unwrap();
    let saved = reopened
        .sessions()
        .into_iter()
        .find(|saved| saved.id == record.id)
        .unwrap();
    let resumed = reopened.resume(&saved.target()).unwrap();
    assert_eq!(resumed.snapshot.transcript, initial);
    reopened.shutdown().unwrap();
}

#[rstest]
fn old_saved_tool_rows_deserialize_without_invented_metadata() {
    let item: NativeTranscriptItem = serde_json::from_value(json!({
        "id":"old-tool","role":"tool","text":"saved result","complete":true
    }))
    .unwrap();
    assert_eq!(item.tool, None);
    assert_eq!(item.display_text(), "saved result");
}

#[rstest]
#[case::empty(json!([]))]
#[case::hidden_custom(json!([{"role":"custom","timestamp":2,"display":false,"content":"Private context"}]))]
fn pi_resume_keeps_an_empty_provider_projection_instead_of_restoring_cached_notices(
    #[case] messages: Value,
) {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create("binding", "Pi", config(root.path()).unwrap())
        .unwrap();
    let session = service.resolve(&record.target()).unwrap();
    fs::write(
        root.path().join("events.json"),
        json!([{"type":"extension_ui_request","id":"recent","method":"notify","message":"Cached notice"}]).to_string(),
    ).unwrap();
    session.rpc("get_state", json!({})).unwrap();
    service.checkpoint().unwrap();
    assert!(service.sessions().iter().any(|record| {
        record
            .snapshot
            .transcript
            .iter()
            .any(|item| item.role == "notice" && item.text == "Cached notice")
    }));
    service.stop(&record.target()).unwrap();
    fs::write(root.path().join("history.json"), messages.to_string()).unwrap();
    let resumed = service.resume(&record.target()).unwrap();
    assert_eq!(resumed.snapshot.transcript, Vec::new());
    assert_eq!(
        service
            .resolve(&resumed.target())
            .unwrap()
            .snapshot()
            .transcript,
        Vec::new()
    );
    service.shutdown().unwrap();
}

#[rstest]
#[case::styles("\x1b[38;5;109mBalanced (272k)\x1b[39m", "Balanced (272k)")]
#[case::unicode("🥟\n\t🧠", "🥟\n\t🧠")]
#[case::cursor("before\x1b[2Kafter", "beforeafter")]
#[case::osc("before\x1b]52;c;YWJj\x07after", "beforeafter")]
#[case::dcs("before\x1bP$qm\x1b\\after", "beforeafter")]
#[case::incomplete("before\x1b[38;5", "before")]
fn saved_native_transcript_presents_plain_text_without_rewriting_its_source(
    #[case] saved: &str,
    #[case] plain: &str,
) {
    let item = NativeTranscriptItem {
        id: "saved".to_owned(),
        role: "notice".to_owned(),
        text: saved.to_owned(),
        complete: true,
        created_at: None,
        updated_at: None,
        tool: None,
        subagent: None,
        images: Vec::new(),
        attachments: Vec::new(),
        citations: Vec::new(),
    };
    assert_eq!(item.display_text(), plain);
    assert_eq!(item.text, saved);
}

#[rstest]
#[case(1)]
#[case(1_791_224_700_000)]
fn provider_message_times_survive_history_refresh_and_catalog_reload(#[case] timestamp: i64) {
    let root = TempDir::new().unwrap();
    let completed = timestamp.checked_add(1000).unwrap();
    let history = json!([
        {"role":"user","timestamp":timestamp,"content":"Earlier prompt"},
        {"role":"assistant","timestamp":completed,"content":[{"type":"text","text":"Saved answer"}]}
    ]);
    fs::write(root.path().join("history.json"), history.to_string()).unwrap();
    let catalog = root.path().join("native.json");
    let service = NativeAgentService::open(&catalog).unwrap();
    let record = service
        .create("binding-captured", "Pi", config(root.path()).unwrap())
        .unwrap();
    let session = service.resolve(&record.target()).unwrap();
    let expected = vec![
        (Some(timestamp), Some(timestamp)),
        (Some(completed), Some(completed)),
    ];
    let times = |items: &[NativeTranscriptItem]| {
        items
            .iter()
            .map(|item| (item.created_at, item.updated_at))
            .collect::<Vec<_>>()
    };
    assert_eq!(times(&session.snapshot().transcript), expected);
    assert_eq!(
        times(&session.refresh_history().unwrap().transcript),
        expected
    );
    service.stop(&record.target()).unwrap();
    service.shutdown().unwrap();
    drop(service);
    let restored = NativeAgentService::open(&catalog).unwrap();
    assert_eq!(times(&restored.sessions()[0].snapshot.transcript), expected);
    restored.shutdown().unwrap();
}

#[rstest]
fn legacy_transcript_without_times_stays_unknown() {
    let item: NativeTranscriptItem = serde_json::from_value(json!({
        "id":"saved","role":"assistant","text":"Earlier answer","complete":true
    }))
    .unwrap();
    assert_eq!((item.created_at, item.updated_at), (None, None));
}

#[rstest]
#[case("")]
#[case("preserve the debugging context")]
fn pi_compact_completion_routes_to_compaction_without_an_llm_turn(#[case] instructions: &str) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    assert!(
        session
            .completions()
            .unwrap()
            .options
            .iter()
            .any(|option| option.name == "compact")
    );
    session
        .send_prompt(&format!("/compact {instructions}"))
        .unwrap();
    let request: Value =
        serde_json::from_slice(&fs::read(root.path().join("compact.json")).unwrap()).unwrap();
    assert_eq!(request["type"], "compact");
    assert_eq!(
        request["customInstructions"].as_str().unwrap_or_default(),
        instructions
    );
    assert!(!root.path().join("prompt.json").exists());
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    assert_eq!(session.snapshot().first_turn, None);
    session.stop();
}

#[rstest]
fn pi_subagent_lifecycle_and_results_survive_history_without_inventing_threads() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    let result = json!({"role":"toolResult","timestamp":2,"toolCallId":"spawn","toolName":"spawn_agent","content":[{"type":"text","text":"Started reviewer"}],"details":{"version":1,"tool":"spawn_agent","agent":{"id":"/root/reviewer","description":"Review the patch","model":"gpt-6-sol","status":"running","startedAt":1000}}});
    fs::write(
        root.path().join("events.json"),
        json!([{"type":"message_end","message":result}]).to_string(),
    )
    .unwrap();
    session.rpc("get_state", json!({})).unwrap();
    for snapshot in [session.snapshot(), session.refresh_history().unwrap()] {
        let agent = snapshot
            .transcript
            .iter()
            .find_map(|item| item.subagent.as_ref())
            .unwrap();
        assert_eq!(agent.status, NativeToolStatus::Running);
        assert_eq!(agent.prompt, "Review the patch");
        assert_eq!(agent.thread_id, None);
        assert_eq!(agent.owner_session.as_deref(), Some("owned-pi-id"));
    }
    let result = json!({"role":"toolResult","timestamp":3,"toolCallId":"wait","toolName":"wait_agent","content":[{"type":"text","text":"Reviewer finished"}],"details":{"version":1,"tool":"wait_agent","status":"updated","update":{"target":"/root/reviewer","agentStatus":"idle"}}});
    fs::write(
        root.path().join("events.json"),
        json!([{"type":"message_end","message":result}]).to_string(),
    )
    .unwrap();
    session.rpc("get_state", json!({})).unwrap();
    for snapshot in [session.snapshot(), session.refresh_history().unwrap()] {
        let agent = snapshot
            .transcript
            .iter()
            .find_map(|item| item.subagent.as_ref())
            .unwrap();
        assert_eq!(agent.status, NativeToolStatus::Completed);
        assert!(agent.completed_at.is_some());
    }
    assert!(
        session
            .read_subagent("/root/reviewer")
            .unwrap_err()
            .contains("task output")
    );
    assert!(session.read_subagent("unknown").is_err());
    session.stop();
}

#[rstest]
#[case::exact(std::iter::once(0..10).collect(), true)]
#[case::literal_suffix(std::iter::once(11..21).collect(), false)]
#[case::invalid_boundary(std::iter::once(1..10).collect(), false)]
#[case::overlap(vec![0..10, 0..10], false)]
fn inline_attachment_ranges_require_exact_admitted_tokens(
    #[case] ranges: Vec<std::ops::Range<usize>>,
    #[case] accepted: bool,
) {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create("binding", "Inline references", config(root.path()).unwrap())
        .unwrap();
    let path = root.path().join("notes.md");
    fs::write(&path, "admitted file").unwrap();
    let reference = service.import_attachment(&record.target(), &path).unwrap();
    let attachments = service
        .resolve_prompt_attachments(&record.target(), std::slice::from_ref(&reference.id))
        .unwrap();
    let prompt = bootty_agents::NativePrompt::new_with_context(
        "[notes.md] literal".into(),
        vec![],
        attachments,
        vec![],
    )
    .unwrap();
    assert!(
        prompt
            .clone()
            .with_attachment_ranges(std::collections::BTreeMap::from([(
                "foreign-id".into(),
                std::iter::once(0..10).collect()
            ),]))
            .is_err()
    );
    let result = prompt.with_attachment_ranges(std::collections::BTreeMap::from([(
        reference.id,
        ranges.clone(),
    )]));
    assert_eq!(result.is_ok(), accepted);
    if let Ok(prompt) = result {
        assert_eq!(prompt.message(), "[notes.md] literal");
        assert_eq!(
            prompt
                .attachment_references()
                .first()
                .unwrap()
                .prompt_ranges,
            ranges
        );
    }
    service.stop(&record.target()).unwrap();
}

#[rstest]
fn pi_rejects_an_unavailable_automatic_review_policy_before_launching() {
    let root = TempDir::new().unwrap();
    let mut config = config(root.path()).unwrap();
    config.permissions = "auto".parse().unwrap();
    assert!(NativeAgentSession::spawn(config).is_err());
    assert!(!root.path().join("launch.json").exists());
}

#[rstest]
fn pi_permission_extension_is_private_and_owned_by_the_provider_lifetime() {
    let root = TempDir::new().unwrap();
    let mut config = config(root.path()).unwrap();
    config.permissions = "supervised".parse().unwrap();
    let session = NativeAgentSession::spawn(config).unwrap();
    let launch: Value =
        serde_json::from_slice(&fs::read(root.path().join("launch.json")).unwrap()).unwrap();
    let args = launch["argv"].as_array().unwrap();
    let position = args.iter().position(|arg| arg == "--extension").unwrap();
    let extension =
        std::path::PathBuf::from(args[position.checked_add(1).unwrap()].as_str().unwrap());
    assert_eq!(
        fs::metadata(&extension).unwrap().permissions().mode() & 0o777,
        0o600
    );
    session.stop();
    drop(session);
    assert!(!extension.exists());
}

#[rstest]
#[case::stream_start("message_start")]
#[case::stream_update("message_update")]
#[case::interrupted_end("message_end")]
fn pi_unreported_stream_usage_preserves_the_last_observed_context(#[case] event: &str) {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    let observed = json!({"input":81,"output":4,"cacheRead":22,"cacheWrite":3,"totalTokens":110});
    let empty = json!({"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0});
    for (kind, usage, timestamp) in [("message_end", observed.clone(), 900), (event, empty, 901)] {
        fs::write(root.path().join("events.json"), json!([
            {"type":kind,"usage":usage,"message":{"role":"assistant","timestamp":timestamp,"content":[],"usage":usage,"stopReason":if timestamp == 901 { "aborted" } else { "stop" }}}
        ]).to_string()).unwrap();
        session.rpc("get_state", json!({})).unwrap();
        assert_eq!(session.snapshot().usage.as_ref(), Some(&observed));
    }
    let reported = json!({"input":43,"output":2,"cacheRead":7,"cacheWrite":0,"totalTokens":52});
    fs::write(root.path().join("events.json"), json!([
        {"type":"message_end","message":{"role":"assistant","timestamp":902,"content":[],"usage":reported,"stopReason":"stop"}}
    ]).to_string()).unwrap();
    session.rpc("get_state", json!({})).unwrap();
    assert_eq!(session.snapshot().usage.as_ref(), Some(&reported));
    session.stop();
}

#[rstest]
fn pi_cancel_returns_to_idle_without_a_provider_abort_failure() {
    let root = TempDir::new().unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt("aborted").unwrap();
    session.interrupt().unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Idle);
    assert_eq!(snapshot.error, None);
    assert!(!snapshot.completed_turn);
    assert_eq!(
        snapshot.first_turn.unwrap().outcome,
        NativeTurnOutcome::Interrupted
    );
    session.stop();
}

#[rstest]
#[case("confirm")]
#[case("select")]
#[case("input")]
#[case("editor")]
fn pi_interrupt_cancels_pending_ui_before_waiting_for_abort(#[case] method: &str) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("abort-needs-answer"), "").unwrap();
    let session = NativeAgentSession::spawn(config(root.path()).unwrap()).unwrap();
    session.send_prompt(method).unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Waiting);
    session.interrupt().unwrap();
    let answer: Value =
        serde_json::from_slice(&fs::read(root.path().join("answer.json")).unwrap()).unwrap();
    assert_eq!(
        answer,
        json!({"type":"extension_ui_response","id":"dialog","cancelled":true})
    );
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Idle);
    assert_eq!(snapshot.error, None);
    assert_eq!(snapshot.requests, []);
    assert_eq!(
        snapshot.first_turn.unwrap().outcome,
        NativeTurnOutcome::Interrupted
    );
    session.send_prompt("next message").unwrap();
    fs::write(root.path().join("settle"), "").unwrap();
    session.rpc("get_state", json!({})).unwrap();
    assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
    session.stop();
}
