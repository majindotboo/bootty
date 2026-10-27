#![cfg(test)]

use super::native_peer::Frame;
use bootty_agents::{NativeLaunch, NativeLauncher};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn save(root: &Path, name: &str, value: &Value) {
    fs::write(root.join(name), super::native_peer::ascii_json(value)).unwrap();
}

pub(crate) fn launcher() -> NativeLauncher {
    Arc::new(|launch: &NativeLaunch<'_>| {
        let command = launch.command()?;
        if Path::new(&launch.config.program)
            .file_name()
            .is_none_or(|name| name != "claude-stream.py")
        {
            return Err("Fixture refuses to launch an unexpected provider".into());
        }
        let root = launch.config.cwd.canonicalize().unwrap();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for (flag, value) in [
            ("--input-format", "stream-json"),
            ("--output-format", "stream-json"),
            ("--permission-prompt-tool", "stdio"),
        ] {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == flag && pair[1] == value)
            );
        }
        assert!(args.iter().any(|arg| arg == "--replay-user-messages"));
        let session = launch
            .config
            .session_id
            .clone()
            .or_else(|| launch.config.fresh_session_id.clone())
            .unwrap();
        save(
            &root,
            "launch.json",
            &json!({"argv":args,"account":launch.config.account_directory}),
        );
        let mut state = Claude {
            root,
            session,
            running: None,
            compacting: None,
            unconfirmed: None,
            failing: false,
        };
        Ok(super::native_peer::raw_peer(move |value| {
            state.handle(value)
        }))
    })
}

struct Claude {
    root: PathBuf,
    session: String,
    running: Option<String>,
    compacting: Option<Value>,
    unconfirmed: Option<Value>,
    failing: bool,
}
impl Claude {
    fn exists(&self, name: &str) -> bool {
        self.root.join(name).exists()
    }
    fn control(value: &Value) -> Value {
        Self::control_data(
            value,
            &json!({"models":[],"account":{},"commands":[{"name":"review","description":"Review changes","argumentHint":"[path]"}]}),
        )
    }
    fn control_data(value: &Value, data: &Value) -> Value {
        json!({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":data}})
    }
    fn result(&self, reason: &str, subtype: &str) -> Value {
        json!({"type":"result","subtype":subtype,"session_id":self.session,"uuid":"result-uuid-not-user-uuid","is_error":subtype!="success","terminal_reason":reason,"result":"Hello world","errors":[]})
    }
    fn event(&self, event: &Value) -> Value {
        json!({"type":"stream_event","session_id":self.session,"parent_tool_use_id":null,"event":event})
    }
    fn tool(&self, id: &str, name: &str, input: &Value) -> Value {
        json!({"type":"assistant","uuid":format!("block-{id}"),"session_id":self.session,"parent_tool_use_id":null,"message":{"id":format!("message-{id}"),"content":[{"type":"tool_use","id":id,"name":name,"input":input}]}})
    }
    fn user(&self, content: &Value, extra: &Value) -> Value {
        let mut value = json!({"type":"user","session_id":self.session,"parent_tool_use_id":null,"message":{"role":"user","content":content}});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        value
    }
    fn echo(value: &Value) -> Value {
        let mut echo = value.clone();
        echo["isReplay"] = json!(true);
        echo["message"] =
            json!({"role":"user","content":[{"type":"text","text":value["message"]["content"]}]});
        echo
    }
    fn release_pending(&mut self, events: &mut Vec<Value>) {
        if self.exists("ack")
            && let Some(value) = self.unconfirmed.take()
        {
            events.push(Self::echo(&value));
            self.running = Some("bash-call".into());
            events.push(self.tool("bash-call", "Bash", &json!({"command":"sleep 60"})));
        }
        if self.failing && self.exists("fail") {
            events.push(self.result("model_error", "error_during_execution"));
            self.failing = false;
        }
    }
    fn handle(&mut self, value: Value) -> Vec<Frame> {
        let mut events = Vec::<Value>::new();
        self.release_pending(&mut events);
        match value["type"].as_str().unwrap() {
            "control_request" => {
                if self.compacting.is_some() && self.exists("compact-done") {
                    events.extend([json!({"type":"system","subtype":"status","status":null,"session_id":self.session}),json!({"type":"system","subtype":"init","session_id":self.session,"cwd":self.root}),json!({"type":"assistant","uuid":"local-answer","session_id":self.session,"parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Not enough messages to compact."}]}})]);
                    let mut echo = self.compacting.take().unwrap();
                    echo["isReplay"] = json!(true);
                    echo["message"] =
                        json!({"role":"user","content":"<command-name>/compact</command-name>"});
                    events.push(echo);
                    let mut result = self.result("completed", "success");
                    result["result"] = json!("Not enough messages to compact.");
                    events.push(result);
                }
                match value["request"]["subtype"].as_str().unwrap(){
                    "rewind_conversation"=>{save(&self.root,"rewind.json",&value["request"]);let outcome=if self.exists("refuse-rewind"){json!({"rewound":false,"error":"stale target","reason":"stale_target"})}else{json!({"rewound":true,"prefillText":"first","precedingAssistantUuid":null})};events.push(Self::control_data(&value,&outcome));}
                    "stop_task"=>events.extend([Self::control(&value),json!({"type":"system","subtype":"task_notification","session_id":self.session,"task_id":value["request"]["task_id"],"status":"stopped","summary":"Stopped"})]),
                    "interrupt"=>{
                        if self.exists("ignore-interrupt"){events.push(Self::control(&value));}
                        else if let Some(running)=self.running.take(){events.extend([Self::control(&value),self.user(&json!([{"type":"tool_result","tool_use_id":running,"content":"The user doesn't want to proceed with this tool use.","is_error":true}]),&json!({"tool_result_meta":[{"id":running,"non_execution_kind":"user-rejected"}]})),self.user(&json!([{"type":"text","text":"[Request interrupted by user]"}]),&json!({})),self.result("aborted_streaming","error_during_execution")]);}
                        else{events.extend([self.result("aborted_streaming","success"),Self::control(&value)]);}
                    }
                    _=>events.push(Self::control(&value)),
                }
            }
            "control_response" => {
                save(&self.root, "answer.json", &value);
                events.push(self.result("completed", "success"));
            }
            "user" => {
                let prompt = &value["message"]["content"];
                save(&self.root, "prompt.json", &value);
                if value["priority"] == "next"
                    && let Some(running) = &self.running
                {
                    events.push(self.user(&json!([{"type":"tool_result","tool_use_id":running,"content":"one","is_error":false}]),&json!({})));
                    let mut echo = value.clone();
                    echo["isReplay"] = json!(true);
                    if let Some(blocks) = prompt.as_array() {
                        let mut blocks = blocks.clone();
                        for block in &mut blocks {
                            if block["type"] == "image" {
                                block["source"]["media_type"] = json!("image/jpeg");
                            }
                        }
                        echo["message"]["content"] = json!(blocks);
                    }
                    events.push(echo);
                    self.running = Some("steer-call".into());
                    events.push(self.tool("steer-call", "Bash", &json!({"command":"sleep 20"})));
                    return events.into_iter().map(Frame::Json).collect();
                }
                let prompt = prompt.as_str().unwrap();
                if prompt == "oversize" {
                    return vec![Frame::Raw(
                        format!("{}\n", "x".repeat(1024 * 1024 + 1)).into_bytes(),
                    )];
                }
                if prompt == "partial" {
                    return vec![Frame::Raw(b"{\"type\":\"result\"}".to_vec()), Frame::End];
                }
                if prompt == "/compact" {
                    events.push(json!({"type":"system","subtype":"status","status":"compacting","session_id":self.session}));
                    self.compacting = Some(value);
                    return events.into_iter().map(Frame::Json).collect();
                }
                events.push(json!({"type":"system","subtype":"init","session_id":if prompt=="wrong-session"{"foreign"}else{&self.session},"cwd":if prompt=="wrong-cwd"{Path::new("/foreign")}else{&self.root}}));
                let mut foreign = self.result("completed", "success");
                foreign["session_id"] = json!("foreign");
                events.extend([foreign, self.result("completed", "success")]);
                if prompt == "no-ack" {
                    return events.into_iter().map(Frame::Json).collect();
                }
                if prompt == "slow-ack" {
                    self.unconfirmed = Some(value);
                    return events.into_iter().map(Frame::Json).collect();
                }
                events.push(Self::echo(&value));
                match prompt{
                    "permission-rules"|"permission-mode"=>{let suggestion=if prompt=="permission-rules"{json!({"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"npm view *"}],"behavior":"allow","destination":"localSettings"})}else{json!({"type":"setMode","mode":"acceptEdits","destination":"session"})};events.push(json!({"type":"control_request","request_id":"permission-id","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool-id","input":{"command":"npm view left-pad version"},"permission_suggestions":[suggestion]}}));}
                    "permission"|"cancel"|"question"=>{events.push(json!({"type":"control_request","request_id":"permission-id","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool-id","input":{"command":"echo hello"}}}));if prompt=="question"||prompt=="cancel"{events.push(json!({"type":"control_cancel_request","request_id":"permission-id"}));}
if prompt=="question"{events.push(json!({"type":"control_request","request_id":"question-id","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","tool_use_id":"question-tool","input":{"questions":[{"question":"Choose color","options":[{"label":"Red"},{"label":"Blue"}],"multiSelect":true}],"marker":"preserve"}}}));}}
                    "hold"=>{},
                    "fail-later"=>self.failing=true,
                    text if text.starts_with("write-")=>events.push(json!({"type":"control_request","request_id":"write-id","request":{"subtype":"can_use_tool","tool_name":"Write","tool_use_id":"write-tool","input":{"file_path":"/tmp/large","content":"x".repeat(text.strip_prefix("write-").unwrap().parse().unwrap())}}})),
                    "edit"=>events.extend([json!({"type":"assistant","uuid":"block-edit","session_id":self.session,"parent_tool_use_id":null,"message":{"id":"message-edit","content":[{"type":"tool_use","id":"edit-call","name":"Edit","input":{"file_path":"/tmp/project/notes.txt","old_string":"line two","new_string":"line 2"}}]}}),self.user(&json!([{"type":"tool_result","tool_use_id":"edit-call","content":"Edited"}]),&json!({"tool_use_result":{"filePath":"/tmp/project/notes.txt","oldString":"line two","newString":"line 2","structuredPatch":[{"oldStart":1,"oldLines":3,"newStart":1,"newLines":3,"lines":[" line one","-line two","+line 2"," line three"]}]}})),self.result("completed","success")]),
                    "tool-hold"=>{self.running=Some("bash-call".into());events.push(self.tool("bash-call","Bash",&json!({"command":"sleep 60"})));}
                    "tool-stream"=>{events.extend([self.event(&json!({"type":"message_start","message":{"id":"stream-tool-message"}})),self.event(&json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"stream-call","name":"Read","input":{}}}))]);for part in ["","{\"file_path\": \"/private/t","mp/notes"]{events.push(self.event(&json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":part}})));}}
                    _=>self.finish_prompt(prompt,&mut events),
                }
            }
            kind => panic!("unexpected Claude peer frame {kind}"),
        }
        events.into_iter().map(Frame::Json).collect()
    }
    fn finish_prompt(&self, prompt: &str, events: &mut Vec<Value>) {
        if prompt == "flood" {
            for i in 0..300 {
                events.extend([json!({"type":"assistant","uuid":format!("think-{i}"),"session_id":self.session,"parent_tool_use_id":null,"message":{"id":format!("think-message-{i}"),"content":[{"type":"thinking","thinking":""}]}}),json!({"type":"assistant","uuid":format!("flood-{i}"),"session_id":self.session,"parent_tool_use_id":null,"message":{"id":format!("flood-message-{i}"),"content":[{"type":"tool_use","id":format!("flood-call-{i}"),"name":"Bash","input":{"command":"true"}}]}}),self.user(&json!([{"type":"tool_result","tool_use_id":format!("flood-call-{i}"),"content":[{"type":"text","text":"ok"}],"is_error":false}]),&json!({}))]);
            }
        }
        events.extend([self.event(&json!({"type":"message_start","message":{"id":"assistant-id"}})),self.event(&json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})),self.event(&json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}})),self.event(&json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"world"}})),json!({"type":"assistant","uuid":"block-id","session_id":self.session,"parent_tool_use_id":null,"message":{"id":"assistant-id","content":[{"type":"text","text":"Hello world"}]}}),self.event(&json!({"type":"content_block_stop","index":0}))]);
        match prompt{
            "usage"=>{events.push(json!({"type":"assistant","uuid":"usage-block","session_id":self.session,"parent_tool_use_id":null,"message":{"id":"usage-message","content":[{"type":"text","text":"Counted"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":900,"cache_read_input_tokens":24_000,"output_tokens":40}}}));let mut result=self.result("completed","success");result["modelUsage"]=json!({"claude-haiku-5-5":{"contextWindow":200_000},"claude-opus-5-5":{"contextWindow":1_000_000}});result["usage"]=json!({"input_tokens":5,"cache_creation_input_tokens":900,"cache_read_input_tokens":48_000,"output_tokens":95});result["total_cost_usd"]=json!(0.0421);events.push(result);return;}
            "read-image"=>{
                use base64::{Engine as _,engine::general_purpose::STANDARD};
                let mut bytes=Vec::new();{let mut encoder=png::Encoder::new(&mut bytes,1,1);encoder.set_color(png::ColorType::Rgba);encoder.set_depth(png::BitDepth::Eight);encoder.add_text_chunk("pad".into(),"x".repeat(500_000)).unwrap();let mut writer=encoder.write_header().unwrap();writer.write_image_data(&[1,2,3,255]).unwrap();writer.finish().unwrap();}let data=STANDARD.encode(bytes);
                events.extend([json!({"type":"assistant","uuid":"read-block","session_id":self.session,"parent_tool_use_id":null,"message":{"id":"read-message","content":[{"type":"tool_use","id":"read-call","name":"Read","input":{"file_path":"shot.png"}}]}}),self.user(&json!([{"type":"tool_result","tool_use_id":"read-call","content":[{"type":"image","source":{"type":"base64","data":data,"media_type":"image/png"}}]}]),&json!({"tool_use_result":{"type":"image","file":{"base64":data,"type":"image/png"}}}))]);
            }
            "big-output"=>events.extend([json!({"type":"assistant","uuid":"big-block","session_id":self.session,"parent_tool_use_id":null,"message":{"id":"big-message","content":[{"type":"tool_use","id":"read-call","name":"Bash","input":{"command":"cat big.log"}}]}}),self.user(&json!([{"type":"tool_result","tool_use_id":"read-call","content":[{"type":"text","text":"z".repeat(2*1024*1024)}]}]),&json!({}))]),
            "huge-line"=>events.push(json!({"type":"assistant","uuid":"huge-block","session_id":self.session,"parent_tool_use_id":null,"message":{"id":"huge-message","content":[{"type":"text","text":"y".repeat(17*1024*1024)}]}})),
            "tool-success"|"tool-failure"=>events.extend([json!({"type":"assistant","uuid":"tool-block","session_id":self.session,"parent_tool_use_id":null,"message":{"id":"tool-message","content":[{"type":"tool_use","id":"bash-call","name":"Bash","input":{"command":"echo hello"}}]}}),self.user(&json!([{"type":"tool_result","tool_use_id":"bash-call","content":[{"type":"text","text":"observed output"}],"is_error":prompt=="tool-failure"}]),&json!({}))]),
            "limits"=>events.extend([json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","rateLimitType":"five_hour","resetsAt":1_760_000_000,"utilization":0.91},"uuid":"limit-1","session_id":self.session}),json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"seven_day_opus","resetsAt":1_760_500_000},"uuid":"limit-2","session_id":self.session})]),
            "compact"=>events.extend([json!({"type":"system","subtype":"status","status":"compacting","session_id":self.session}),json!({"type":"system","subtype":"status","status":"compacting","session_id":self.session}),json!({"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":180_000,"post_tokens":12_000},"uuid":"boundary","session_id":self.session})]),
            "todo"=>events.extend([self.tool("todo-call","TodoWrite",&json!({"todos":[{"content":"Read the code","status":"completed","activeForm":"Reading"},{"content":"Fix the bug","status":"in_progress","activeForm":"Fixing"},{"content":"Ship it","status":"pending","activeForm":"Shipping"}]})),self.user(&json!([{"type":"tool_result","tool_use_id":"todo-call","content":"ok"}]),&json!({}))]),
            "tasks"=>{for (number,subject) in [("1","Read the code"),("2","Fix the bug"),("3","Drop this")]{events.extend([self.tool(&format!("task-{number}"),"TaskCreate",&json!({"subject":subject})),self.user(&json!([{"type":"tool_result","tool_use_id":format!("task-{number}"),"content":"created"}]),&json!({"tool_use_result":{"task":{"id":number,"subject":subject}}}))]);}for (number,change) in [("1",json!({"status":"completed"})),("2",json!({"status":"in_progress","subject":"Fix the real bug"})),("3",json!({"status":"deleted"}))]{let mut input=change;input["taskId"]=json!(number);events.extend([self.tool(&format!("update-{number}"),"TaskUpdate",&input),self.user(&json!([{"type":"tool_result","tool_use_id":format!("update-{number}"),"content":"updated"}]),&json!({}))]);}}
            text if text.starts_with("subagent-")=>events.extend([json!({"type":"assistant","uuid":"agent-block","session_id":self.session,"parent_tool_use_id":null,"message":{"id":"agent-message","content":[{"type":"tool_use","id":"agent-call","name":"Agent","input":{"description":"Review source","prompt":"Review the changes","model":"sonnet"}}]}}),json!({"type":"system","subtype":"task_started","session_id":"foreign","task_id":"foreign-child","tool_use_id":"agent-call"}),json!({"type":"system","subtype":"task_started","session_id":self.session,"task_id":"child","tool_use_id":"agent-call","description":"Review source"}),json!({"type":"system","subtype":"task_notification","session_id":self.session,"task_id":"child","status":text.strip_prefix("subagent-").unwrap(),"summary":"Observed result"}),json!({"type":"system","subtype":"task_progress","session_id":self.session,"task_id":"child"})]),
            "background-shell"=>{events.extend([self.tool("bg-call","Bash",&json!({"command":"cargo build"})),self.tool("fg-call","Bash",&json!({"command":"ls"})),json!({"type":"system","subtype":"task_started","session_id":self.session,"task_id":"fg-task","tool_use_id":"fg-call","description":"List files","task_type":"local_bash"}),json!({"type":"system","subtype":"task_started","session_id":self.session,"task_id":"bg-task","tool_use_id":"bg-call","description":"Watch the build","is_backgrounded":true,"task_type":"local_bash"})]);fs::create_dir_all(self.root.join("tasks")).unwrap();fs::write(self.root.join("tasks/bg-task.output"),"Compiling bootty\n").unwrap();events.push(self.user(&json!([{"type":"tool_result","tool_use_id":"bg-call","content":format!("Command running in background with ID: bg-task. Output is being written to: {}",self.root.join("tasks/bg-task.output").display())}]),&json!({"tool_use_result":{"stdout":"","stderr":"","interrupted":false,"backgroundTaskId":"bg-task"}})));}
            "background-done"=>events.push(json!({"type":"system","subtype":"task_updated","session_id":self.session,"task_id":"bg-task","patch":{"status":"completed"}})),
            "self-turn"=>{events.extend([self.result("completed","success"),json!({"type":"assistant","uuid":"self-block","session_id":self.session,"parent_tool_use_id":null,"message":{"id":"self-message","content":[{"type":"text","text":"Background job finished"}]}}),self.result("completed","success")]);return;}
            _=>{},
        }
        events.push(match prompt {
            "failed" => self.result("model_error", "error_during_execution"),
            "failed-reason" => {
                let mut result = self.result("model_error", "error_during_execution");
                result["errors"] = json!(["API Error: 529 overloaded"]);
                result
            }
            "failed-turns" => self.result("max_turns", "error_max_turns"),
            _ => self.result("completed", "success"),
        });
    }
}
