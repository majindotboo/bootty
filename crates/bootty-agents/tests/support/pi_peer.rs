#![cfg(test)]

use super::native_peer::Frame;
use bootty_agents::{NativeLaunch, NativeLauncher};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

fn save(root: &Path, name: &str, value: &Value) {
    fs::write(root.join(name), super::native_peer::ascii_json(value)).unwrap();
}
static NEXT: AtomicU64 = AtomicU64::new(1);

fn read(root: &Path, name: &str) -> Value {
    serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
}

pub(crate) fn launcher() -> NativeLauncher {
    Arc::new(|launch: &NativeLaunch<'_>| {
        let command = launch.command()?;
        if Path::new(&launch.config.program)
            .file_name()
            .is_none_or(|name| name != "pi-rpc.py")
        {
            return Err("Fixture refuses to launch an unexpected provider".into());
        }
        let config = launch.config.clone();
        let root = config.cwd.canonicalize().unwrap();
        let argv = arguments(&config.program, &command);
        assert!(
            argv.ends_with(&["--mode".into(), "rpc".into()])
                || argv.iter().any(|arg| arg == "--session")
        );
        let fresh = root.join("unique-sessions").exists() && config.session_file.is_none();
        let session = if fresh {
            format!("owned-pi-{}", NEXT.fetch_add(1, Ordering::Relaxed))
        } else {
            "owned-pi-id".into()
        };
        let file = if fresh {
            root.join("account/sessions")
                .join(format!("{session}.jsonl"))
        } else {
            config
                .session_file
                .map_or_else(|| root.join("saved.jsonl"), Into::into)
        };
        if fresh {
            fs::write(
                &file,
                format!("{}\n", json!({"type":"session","id":session,"version":3})),
            )
            .unwrap();
        }
        save(
            &root,
            "launch.json",
            &json!({"argv":argv,"account":config.account_directory}),
        );
        let mut working = false;
        let mut waiting = None::<Value>;
        let mut timestamp = 2_i64;
        let mut messages = messages(&root, fresh);
        Ok(super::native_peer::raw_peer(move |value| {
            let reply = |request: &Value, mut data: Value, success: bool, command: Option<&str>| {
                if request["type"] == "prompt" && root.join("legacy-ack").exists() {
                    data = json!({});
                }
                json!({"id":request["id"],"type":"response","command":command.map_or_else(||request["type"].clone(),|command|json!(command)),"success":success,"data":data})
            };
            let mut events = Vec::<Value>::new();
            match value["type"].as_str().unwrap(){
                "get_state"=>{
                    if root.join("events.json").exists(){let added=read(&root,"events.json").as_array().unwrap().clone();fs::remove_file(root.join("events.json")).unwrap();for event in &added{if event["type"]=="message_end"{messages.push(event["message"].clone());}}events.extend(added);}
                    let state=json!({"sessionId":session,"sessionFile":if root.join("wrong-file").exists(){format!("{}.wrong",file.display())}else{file.to_string_lossy().into_owned()},"isStreaming":working,"isCompacting":false,"messageCount":messages.len()});
                    if root.join("fault").exists(){let fault=fs::read_to_string(root.join("fault")).unwrap();if fault=="reply"{events.push(reply(&value,state,true,Some("prompt")));}else{let mut frames=events.into_iter().map(Frame::Json).collect::<Vec<_>>();if fault=="partial"{frames.extend([Frame::Raw(b"{\"type\":\"agent_start\"}".to_vec()),Frame::End]);}else{frames.push(Frame::Raw(format!("{}\n",if fault=="oversize"{"x".repeat(1_048_577)}else{fault}).into_bytes()));}return frames;}return events.into_iter().map(Frame::Json).collect();}
                    if root.join("retry").exists(){events.extend([json!({"type":"agent_end","willRetry":true}),json!({"type":"agent_start"})]);fs::remove_file(root.join("retry")).unwrap();}
                    if root.join("settle").exists(){events.push(json!({"type":"agent_settled"}));working=false;fs::remove_file(root.join("settle")).unwrap();}
                    events.push(json!({"id":"foreign","type":"response","command":"get_state","success":true,"data":{"sessionId":"foreign","sessionFile":"/foreign"}}));
                    let mut state=state;state["isStreaming"]=json!(working);events.push(reply(&value,state,true,None));
                }
                "get_messages"=>events.push(reply(&value,json!({"messages":messages}),true,None)),
                "get_entries"=>{let entries=fs::read_to_string(&file).unwrap().lines().filter(|line|!line.trim().is_empty()).map(|line|serde_json::from_str::<Value>(line).unwrap()).collect::<Vec<_>>();assert_eq!(value["since"],entries.last().unwrap()["id"]);events.push(reply(&value,json!({"entries":[],"leafId":fs::read_to_string(root.join("history-leaf")).unwrap()}),true,None));}
                "get_commands"=>events.push(reply(&value,json!({"commands":if root.join("no-rewind").exists(){json!([])}else{json!([{"name":"__bootty_rewind","source":"extension"}])}}),true,None)),
                "compact"=>{save(&root,"compact.json",&value);events.push(reply(&value,json!({"summary":"Observed summary"}),true,None));}
                "prompt"=>{
                    save(&root,"prompt.json",&value);let text=value["message"].as_str().unwrap();
                    if text=="handled"{events.push(reply(&value,json!({"disposition":"handled"}),true,None));}
                    else if let Some(at)=text.strip_prefix("/__bootty_rewind "){let at=at.parse::<i64>().unwrap();if let Some(index)=messages.iter().position(|message|message["role"]=="user"&&message["timestamp"]==at).filter(|_|!root.join("keep-rewind").exists()){messages.truncate(index);}else{events.push(json!({"type":"extension_error","extensionPath":"command:__bootty_rewind","event":"command","error":"Pi no longer has that message in this conversation"}));}events.push(reply(&value,json!({"disposition":"handled"}),true,None));}
                    else if text=="/editor"{waiting=Some(value.clone());events.push(json!({"type":"extension_ui_request","id":"dialog","method":"editor","title":"Question","prefill":"First line\nSecond line"}));}
                    else{
                        events.push(json!({"type":"agent_start"}));working=true;
                        if text=="oversized-editor"{events.push(json!({"type":"extension_ui_request","id":"dialog","method":"editor","title":"Question","prefill":"x".repeat(2*1024*1024)}));}
                        else if ["confirm","select","input","editor"].contains(&text){for index in 0..40{events.push(json!({"type":"extension_ui_request","id":format!("notice-{index}"),"method":"notify","message":"Information"}));}events.push(json!({"type":"extension_ui_request","id":"dialog","method":text,"title":"Question","options":["Allow","Block"]}));}
                        else{
                            let user=json!({"role":"user","content":text,"timestamp":timestamp});let mut assistant=json!({"role":"assistant","content":[],"timestamp":timestamp.checked_add(1).unwrap(),"stopReason":"pending"});timestamp=timestamp.checked_add(2).unwrap();
                            events.extend([json!({"type":"message_start","message":user}),json!({"type":"message_end","message":user}),json!({"type":"message_start","message":assistant})]);
                            for (text,delta) in [("Hello ","Hello "),("Hello world","world")]{assistant["content"]=json!([{"type":"text","text":text}]);events.push(json!({"type":"message_update","message":assistant,"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":delta}}));}
                            assistant["stopReason"]=json!(if ["error","aborted"].contains(&text){text}else{"stop"});events.push(json!({"type":"message_end","message":assistant}));
                            let mut end=json!({"type":"agent_end","messages":[assistant],"willRetry":true});
                            if root.join("image-end-case").exists(){let case=fs::read_to_string(root.join("image-end-case")).unwrap();let mut images=value["images"].clone();if case=="wrong-mime"{images[0]["mimeType"]=json!("image/jpeg");}let mut content=vec![json!({"type":"text","text":text})];content.extend(images.as_array().unwrap().clone());end["messages"]=json!([{"role":"user","content":content},assistant]);if case=="foreign-event"{end["type"]=json!("unrelated_event");}
if case=="control-overflow"{end["extra"]=json!("x".repeat(1_048_577));}}
                            events.push(end);messages.extend([user,assistant]);
                        }
                        events.push(reply(&value,json!({"disposition":"started"}),true,None));
                    }
                }
                "steer"=>{assert!(working);save(&root,"steer.json",&value);if value["message"]=="reject"{events.push(reply(&value,json!({}),false,None));}else{let prior=messages.iter().rev().find(|message|message["role"]=="user").unwrap();events.push(json!({"type":"message_end","message":prior}));let user=json!({"role":"user","content":value["message"],"timestamp":timestamp});timestamp=timestamp.checked_add(1).unwrap();events.push(json!({"type":"message_end","message":user}));messages.push(user);events.push(reply(&value,json!({"disposition":"queued"}),true,None));}}
                "extension_ui_response"=>{save(&root,"answer.json",&value);if let Some(request)=waiting.take(){events.push(reply(&request,json!({"disposition":"handled"}),!root.join("reject-editor").exists(),None));}else{events.push(json!({"type":"agent_settled"}));working=false;}}
                "abort"=>{if root.join("abort-needs-answer").exists()&&working{events.push(reply(&value,json!({}),false,None));}else{events.push(json!({"type":"agent_settled"}));working=false;events.push(reply(&value,json!({}),true,None));}}
                _=>events.push(reply(&value,json!({}),false,None)),
            }
            events.into_iter().map(Frame::Json).collect()
        }))
    })
}

fn arguments(program: &str, command: &std::process::Command) -> Vec<String> {
    std::iter::once(program.to_owned())
        .chain(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned()),
        )
        .collect()
}

fn messages(root: &Path, fresh: bool) -> Vec<Value> {
    if fresh {
        vec![]
    } else if root.join("history.json").exists() {
        read(root, "history.json").as_array().unwrap().clone()
    } else {
        vec![json!({"role":"user","content":"Saved history","timestamp":1})]
    }
}
