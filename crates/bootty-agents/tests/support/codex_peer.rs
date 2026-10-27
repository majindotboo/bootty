#![cfg(test)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use bootty_agents::{NativeLaunch, NativeLauncher};
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn read(root: &Path, name: &str) -> Value {
    serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
}

fn save(root: &Path, name: &str, value: &Value) {
    fs::write(root.join(name), super::native_peer::ascii_json(value)).unwrap();
}

pub(crate) fn launcher() -> NativeLauncher {
    Arc::new(|launch: &NativeLaunch<'_>| {
        let command = launch.command()?;
        if Path::new(&launch.config.program)
            .file_name()
            .is_none_or(|name| name != "provider.py")
        {
            return Err("Fixture refuses to launch an unexpected provider".into());
        }
        if launch.config.provider != bootty_agents::AgentKind::Codex {
            return Err("fixture expects Codex app-server arguments".into());
        }
        let root = launch.config.cwd.canonicalize().unwrap();
        let argv = std::iter::once(launch.config.program.clone())
            .chain(
                command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned()),
            )
            .collect::<Vec<_>>();
        assert_eq!(&argv[1..4], ["app-server", "--listen", "stdio://"]);
        save(
            &root,
            "launch.json",
            &json!({"argv":argv,"home":launch.config.account_directory}),
        );
        let mut state = Codex {
            root,
            thread: "native-thread".into(),
            turn: 0,
            waiting: None,
            deferred: false,
            release_pending: false,
            approvals: vec![],
            declined: vec![],
            hold_approval: false,
            voice_waiting: false,
            voice_waiter: None,
            writers: vec![],
            history_items: None,
            history_limits: Vec::new(),
        };
        Ok(super::native_peer::peer(move |value| state.handle(value)))
    })
}

struct Codex {
    root: PathBuf,
    thread: String,
    turn: u64,
    waiting: Option<Value>,
    deferred: bool,
    release_pending: bool,
    approvals: Vec<Value>,
    declined: Vec<Value>,
    hold_approval: bool,
    voice_waiting: bool,
    voice_waiter: Option<Value>,
    writers: Vec<fs::File>,
    history_items: Option<Arc<Value>>,
    history_limits: Vec<usize>,
}

impl Codex {
    fn exists(&self, name: &str) -> bool {
        self.root.join(name).exists()
    }
    fn notice(&self, method: &str, params: &Value) -> Value {
        let mut base = json!({"threadId":self.thread,"turnId":format!("turn-{}",self.turn)});
        base.as_object_mut()
            .unwrap()
            .extend(params.as_object().unwrap().clone());
        json!({"method":method,"params":base})
    }
    fn started(&self) -> Value {
        self.notice(
            "turn/started",
            &json!({"turn":{"id":format!("turn-{}",self.turn),"status":"inProgress","items":[]}}),
        )
    }
    fn completed(&self, status: &str) -> Value {
        let mut turn = json!({"id":format!("turn-{}",self.turn),"status":status,"items":[]});
        if status == "failed" {
            turn["error"] = json!({"message":"stream disconnected"});
        }
        self.notice("turn/completed", &json!({"turn":turn}))
    }
    fn handle(&mut self, value: Value) -> Vec<Value> {
        let id = &value["id"];
        let Some(method) = value["method"].as_str() else {
            if let Some(result) = value.get("result") {
                self.approvals.push(result.clone());
                if !self.hold_approval {
                    return vec![self.completed("completed")];
                }
            }
            if value.get("error").is_some() {
                self.declined.push(value);
            }
            return vec![];
        };
        match method {
            "initialized" => vec![],
            "review/start" | "thread/unarchive" | "thread/start" | "thread/resume" => {
                self.startup(&value)
            }
            "mcpServer/resource/read"
            | "mcpServerStatus/list"
            | "mcpServer/tool/call"
            | "thread/compact/start"
            | "__finish_compaction" => self.tools(&value),
            "turn/start" => self.prompt(&value),
            "turn/steer" | "turn/interrupt" | "__release" | "__repeat_started" | "__finish"
            | "__stop_callback" | "__barrier" => self.steering(&value),
            "thread/realtime/start"
            | "__await_voice_start"
            | "thread/realtime/stop"
            | "__finish_voice_stop" => self.voice(&value),
            "thread/read" | "thread/items/list" => self.history(&value),
            _ => vec![json!({"id":id,"result":{}})],
        }
    }
    fn startup(&mut self, value: &Value) -> Vec<Value> {
        let id = &value["id"];
        let params = &value["params"];
        let method = value["method"].as_str().unwrap();
        let reply = |data: Value| json!({"id":id,"result":data});
        let error = |message: String| json!({"id":id,"error":{"code":-32600,"message":message}});
        match method {
            "review/start" => {
                self.turn = self.turn.checked_add(1).unwrap();
                save(&self.root, "review.json", params);
                vec![reply(json!({"turn":{"id":format!("turn-{}",self.turn),"status":"inProgress","items":[]},"reviewThreadId":self.thread})),self.started(),self.notice("item/started",&json!({"item":{"type":"enteredReviewMode","id":format!("turn-{}",self.turn),"review":"current changes"}})),self.notice("item/started",&json!({"item":{"type":"exitedReviewMode","id":format!("exit-{}",self.turn),"review":"Looks solid."}})),self.notice("item/completed",&json!({"item":{"type":"exitedReviewMode","id":format!("exit-{}",self.turn),"review":"Looks solid."}})),self.notice("item/completed",&json!({"item":{"type":"agentMessage","id":format!("review-reply-{}",self.turn),"text":"Looks solid."}})),self.completed("completed")]
            }
            "thread/unarchive" => {
                save(&self.root, "unarchive-request.json", params);
                if self.exists("unarchive-fail") {
                    return vec![
                        json!({"id":id,"error":{"code":-32603,"message":"Restore service unavailable"}}),
                    ];
                }
                fs::remove_file(self.root.join("archived")).unwrap();
                vec![reply(json!({"thread":{"id":params["threadId"]}}))]
            }
            "thread/start" | "thread/resume" => {
                if method == "thread/resume" {
                    if self.exists("writer-blocked") {
                        return vec![error(format!(
                            "thread {} already has an active writer",
                            params["threadId"].as_str().unwrap()
                        ))];
                    }
                    if self.exists("archived") {
                        return vec![error(format!(
                            "session {} is archived. Run `codex unarchive {}` to unarchive it first.",
                            params["threadId"].as_str().unwrap(),
                            params["threadId"].as_str().unwrap()
                        ))];
                    }
                    if self.exists("missing-rollout") {
                        return vec![error(format!(
                            "no rollout found for thread id {}",
                            params["threadId"].as_str().unwrap()
                        ))];
                    }
                }
                if self.exists("fail-launch") {
                    return vec![error("captured provider launch failed".into())];
                }
                self.thread = params["threadId"].as_str().map_or_else(
                    || {
                        if self.exists("unique-threads") {
                            format!("native-thread-{}", NEXT.fetch_add(1, Ordering::Relaxed))
                        } else {
                            "native-thread".into()
                        }
                    },
                    str::to_owned,
                );
                if self.exists("wrong-thread") {
                    self.thread = "different-thread".into();
                }
                let writer = fs::OpenOptions::new()
                    .read(true)
                    .append(true)
                    .create(true)
                    .open(self.root.join(format!("{}.writer", self.thread)))
                    .unwrap();
                if writer.try_lock().is_err() {
                    return vec![error("thread has an active writer".into())];
                }
                self.writers.push(writer);
                let history = if method == "thread/start" {
                    json!([])
                } else if self.exists("history-timestamps.json") {
                    read(&self.root, "history-timestamps.json")
                } else {
                    json!([{"id":"prior","items":[{"id":"history","type":"agentMessage","text":"resumed history"}]}])
                };
                vec![reply(json!({"thread":{"id":self.thread,"turns":history}}))]
            }
            _ => panic!("unexpected routed native fixture request"),
        }
    }
    fn tools(&mut self, value: &Value) -> Vec<Value> {
        let id = &value["id"];
        let params = &value["params"];
        let reply = |data: Value| json!({"id":id,"result":data});
        match value["method"].as_str().unwrap() {
            "mcpServer/resource/read" => {
                assert_eq!(params["threadId"], self.thread);
                assert_eq!(params["server"], "fixture");
                vec![reply(
                    json!({"contents":[{"uri":params["uri"],"mimeType":"text/html;profile=mcp-app","text":format!("<p>{}</p>",params["originCallId"].as_str().unwrap())}]}),
                )]
            }
            "mcpServerStatus/list" => {
                assert_eq!(params["threadId"], self.thread);
                assert_eq!(params["serverName"], "fixture");
                vec![reply(
                    json!({"data":[{"name":"fixture","authStatus":"unsupported","resources":[],"resourceTemplates":[],"tools":{"look":{"name":"look","inputSchema":{},"annotations":{"readOnlyHint":true}},"change":{"name":"change","title":"Change it","inputSchema":{}},"hidden":{"name":"hidden","inputSchema":{},"_meta":{"ui":{"visibility":["model"]}}}}}]}),
                )]
            }
            "mcpServer/tool/call" => {
                assert_eq!(params["threadId"], self.thread);
                assert_eq!(params["server"], "fixture");
                vec![reply(
                    json!({"content":[{"type":"text","text":format!("{} {}",params["tool"].as_str().unwrap(),super::native_peer::ascii_json(&params["arguments"]).replace(':', ": ").replace(',', ", "))}]}),
                )]
            }
            "thread/compact/start" => {
                assert_eq!(params, &json!({"threadId":self.thread}));
                save(&self.root, "compact-request.json", params);
                self.turn = self.turn.checked_add(1).unwrap();
                vec![self.started(),self.notice("item/started",&json!({"item":{"id":format!("compaction-{}",self.turn),"type":"contextCompaction"}})),reply(json!({}))]
            }
            "__finish_compaction" => {
                let status = params["status"].as_str().unwrap_or("completed");
                let mut events = vec![];
                if status == "completed" {
                    events.push(self.notice("item/completed",&json!({"item":{"id":format!("compaction-{}",self.turn),"type":"contextCompaction"}})));
                }
                events.extend([self.completed(status), reply(json!({}))]);
                events
            }
            _ => panic!("unexpected routed native fixture request"),
        }
    }
    fn prompt(&mut self, value: &Value) -> Vec<Value> {
        let id = &value["id"];
        let params = &value["params"];
        let reply = |data: Value| json!({"id":id,"result":data});
        let error = |message: String| json!({"id":id,"error":{"code":-32600,"message":message}});
        {
            assert_eq!(params["threadId"], self.thread);
            save(&self.root, "turn-policy.json", params);
            self.turn = self.turn.checked_add(1).unwrap();
            let text = params["input"][0]["text"].as_str().unwrap();
            if self.exists("unique-threads") {
                save(
                    &self.root,
                    &format!("{}-{}.json", self.thread, self.turn),
                    params,
                );
            }
            if text == "reject-first" {
                return vec![error("captured first prompt rejected".into())];
            }
            if text == "defer" {
                self.waiting = Some(id.clone());
                self.deferred = !self.release_pending;
                return if self.release_pending {
                    vec![self.started()]
                } else {
                    vec![]
                };
            }
            let mut events = vec![self.started()];
            let mut user = json!({"id":format!("user-{}",self.turn),"type":"userMessage","content":params["input"]});
            let mode = fs::read_to_string(self.root.join("history-echo-mode"))
                .unwrap_or_else(|_| "exact".into());
            if mode == "altered" {
                user["content"][0]["text"] = json!(format!(
                    "{}altered",
                    user["content"][0]["text"].as_str().unwrap()
                ));
            }
            if mode == "metadata" {
                user["unowned"] = json!("x".repeat(1024 * 1024));
            }
            events.push(if mode=="foreign"{json!({"method":"item/completed","params":{"threadId":"foreign","turnId":format!("turn-{}",self.turn),"item":user}})}else{self.notice("item/completed",&json!({"item":user}))});
            if text == "active" {
                events.push(reply(json!({"turn":{"id":format!("turn-{}",self.turn)}})));
                return events;
            }
            if text == "wait" {
                self.waiting = Some(id.clone());
                return events;
            }
            if [
                "approval",
                "approval-hold",
                "approval-scopes",
                "approval-once-only",
            ]
            .contains(&text)
            {
                self.hold_approval = text == "approval-hold";
                events.extend([json!({"id":"wrong-thread","method":"item/commandExecution/requestApproval","params":{"threadId":"other","turnId":format!("turn-{}",self.turn)}}),json!({"id":"wrong-turn","method":"item/commandExecution/requestApproval","params":{"threadId":self.thread,"turnId":"other"}})]);
                let mut scope = json!({"threadId":self.thread,"turnId":format!("turn-{}",self.turn),"itemId":"command","command":"echo approved"});
                if text == "approval-scopes" {
                    scope["proposedExecpolicyAmendment"] = json!(["echo"]);
                    scope["availableDecisions"] = json!(["accept","decline","acceptForSession",{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["echo"]}},"cancel"]);
                }
                if text == "approval-once-only" {
                    scope["availableDecisions"] = json!(["accept", "decline"]);
                }
                events.extend([json!({"id":900,"method":"item/commandExecution/requestApproval","params":scope}),reply(json!({"turn":{"id":format!("turn-{}",self.turn)}}))]);
                return events;
            }
            if text == "unknown-request" {
                events.push(json!({"id":"permissions-1","method":"item/permissions/requestApproval","params":{"threadId":self.thread,"turnId":format!("turn-{}",self.turn)}}));
            }
            if text == "mcp-approval" {
                events.extend([json!({"id":"foreign-mcp","method":"mcpServer/elicitation/request","params":{"threadId":"foreign","turnId":null}}),json!({"id":"stale-mcp","method":"mcpServer/elicitation/request","params":{"threadId":self.thread,"turnId":"other"}}),json!({"id":"mcp-decision","method":"mcpServer/elicitation/request","params":{"threadId":self.thread,"turnId":null,"serverName":"Bootty tools","mode":"form","message":"Allow stop_spawned_agent?","requestedSchema":{"type":"object","properties":{}}}}),reply(json!({"turn":{"id":format!("turn-{}",self.turn)}}))]);
                return events;
            }
            if ["tool-completed", "tool-failed", "tool-declined"].contains(&text) {
                let status = text.strip_prefix("tool-").unwrap();
                events.extend([self.notice("item/started",&json!({"item":{"id":"typed-command","type":"commandExecution","command":"echo typed","status":"inProgress"}})),self.notice("item/commandExecution/outputDelta",&json!({"itemId":"typed-command","delta":"streamed output"})),self.notice("item/completed",&json!({"item":{"id":"typed-command","type":"commandExecution","command":"echo typed","status":status,"aggregatedOutput":""}})),self.notice("item/completed",&json!({"item":{"id":"typed-mcp","type":"mcpToolCall","server":"fixture","tool":"inspect","arguments":{"path":"source.rs"},"status":if status=="declined"{"failed"}else{status},"result":{"content":[{"type":"text","text":"inspection output"}]},"mcpAppUi":{"resourceUri":"ui://fixture/inspect.html","preferredModelDisplayMode":"inline"}}}))]);
            }
            if ["retried-error", "fatal-error"].contains(&text) {
                events.push(self.notice("error",&json!({"error":{"message":"stream disconnected"},"willRetry":text=="retried-error"})));
            }
            if text == "bounded" {
                for i in 0..96 {
                    events.push(self.notice(
                        "item/agentMessage/delta",
                        &json!({"itemId":format!("long-{i}"),"delta":"界".repeat(6000)}),
                    ));
                }
            } else {
                events.extend([json!({"method":"item/agentMessage/delta","params":{"threadId":"other","turnId":format!("turn-{}",self.turn),"itemId":"foreign","delta":"foreign thread"}}),json!({"method":"item/agentMessage/delta","params":{"threadId":self.thread,"turnId":"old","itemId":"foreign-turn","delta":"foreign turn"}}),self.notice("item/started",&json!({"item":{"id":format!("command-{}",self.turn),"type":"commandExecution","command":"echo probe","aggregatedOutput":""}})),self.notice("item/commandExecution/outputDelta",&json!({"itemId":format!("command-{}",self.turn),"delta":"probe output"})),self.notice("item/agentMessage/delta",&json!({"itemId":format!("answer-{}",self.turn),"delta":"hello"})),self.notice("item/completed",&json!({"item":{"id":format!("answer-{}",self.turn),"type":"agentMessage","text":"hello"}}))]);
            }
            events.extend([json!({"method":"turn/completed","params":{"threadId":"other","turn":{"id":format!("turn-{}",self.turn),"status":"completed"}}}),json!({"method":"turn/completed","params":{"threadId":self.thread,"turn":{"id":"old","status":"completed"}}}),self.completed(if text=="fatal-error"{"failed"}else if ["failed","interrupted","unknown"].contains(&text){text}else{"completed"}),reply(json!({"turn":{"id":if text=="mismatched-ack"{"different-ack".into()}else{format!("turn-{}",self.turn)}}}))]);
            events
        }
    }
    fn steering(&mut self, value: &Value) -> Vec<Value> {
        let id = &value["id"];
        let params = &value["params"];
        let reply = |data: Value| json!({"id":id,"result":data});
        let error = |message: String| json!({"id":id,"error":{"code":-32600,"message":message}});
        match value["method"].as_str().unwrap() {
            "turn/steer" => {
                assert_eq!(params["threadId"], self.thread);
                assert_eq!(params["expectedTurnId"], format!("turn-{}", self.turn));
                let text = params["input"][0]["text"].as_str().unwrap();
                if text == "reject-steer" {
                    return vec![error("steer rejected".into())];
                }
                let item = json!({"id":format!("steered-user-{}",self.turn),"type":"userMessage","content":params["input"]});
                let mut events = vec![self.notice("item/started", &json!({"item":item}))];
                if text != "pending-steer" {
                    events.push(self.notice("item/completed", &json!({"item":item})));
                }
                events.push(reply(json!({"turnId":format!("turn-{}",self.turn)})));
                events
            }
            "turn/interrupt" => {
                if params["threadId"] == "child-thread" {
                    save(&self.root, "child-interrupt.json", params);
                    return vec![reply(json!({}))];
                }
                assert_eq!(params["threadId"], self.thread);
                assert_eq!(params["turnId"], format!("turn-{}", self.turn));
                let mut events = vec![self.completed("interrupted")];
                if let Some(waiting) = self.waiting.take() {
                    events.push(
                        json!({"id":waiting,"result":{"turn":{"id":format!("turn-{}",self.turn)}}}),
                    );
                }
                events.push(reply(json!({})));
                events
            }
            "__release" => {
                self.release_pending = true;
                let mut events = vec![];
                if self.deferred {
                    events.push(self.started());
                    self.deferred = false;
                }
                events.push(reply(json!({})));
                events
            }
            "__repeat_started" => vec![self.started(), reply(json!({}))],
            "__finish" => vec![
                self.completed(params["status"].as_str().unwrap()),
                reply(json!({})),
            ],
            "__stop_callback" => vec![
                self.notice("thread/tokenUsage/updated", &json!({"tokenUsage":{}})),
                reply(json!({})),
            ],
            "__barrier" => {
                let mut events = vec![];
                if self.exists("events.json") {
                    events = read(&self.root, "events.json").as_array().unwrap().clone();
                    fs::remove_file(self.root.join("events.json")).unwrap();
                }
                events.push(reply(
                    json!({"approvals":self.approvals,"declined":self.declined,"historyLimits":self.history_limits}),
                ));
                events
            }
            _ => panic!("unexpected routed native fixture request"),
        }
    }
    fn voice(&mut self, value: &Value) -> Vec<Value> {
        let id = &value["id"];
        let params = &value["params"];
        let method = value["method"].as_str().unwrap();
        let reply = |data: Value| json!({"id":id,"result":data});
        match method {
            "thread/realtime/start" => {
                save(&self.root, "realtime.json", params);
                let mut events = vec![reply(json!({}))];
                match params["transport"]["sdp"].as_str().unwrap(){"hold"=>{self.voice_waiting=true;if let Some(waiter)=self.voice_waiter.take(){events.push(json!({"id":waiter,"result":{}}));}},"refuse"=>events.push(json!({"method":"thread/realtime/error","params":{"threadId":self.thread,"message":"realtime conversation requires API key auth"}})),_=>events.extend([json!({"method":"thread/realtime/started","params":{"threadId":self.thread,"version":"v3"}}),json!({"method":"thread/realtime/sdp","params":{"threadId":self.thread,"sdp":"answer-sdp"}})])}
                events
            }
            "__await_voice_start" => {
                if self.voice_waiting {
                    vec![reply(json!({}))]
                } else {
                    self.voice_waiter = Some(id.clone());
                    vec![]
                }
            }
            "thread/realtime/stop" => {
                fs::write(self.root.join("realtime-stopped"), "").unwrap();
                vec![
                    reply(json!({})),
                    if self.exists("defer-voice-close") {
                        json!({"method":"thread/realtime/sdp","params":{"threadId":self.thread,"sdp":"stale-answer"}})
                    } else {
                        json!({"method":"thread/realtime/closed","params":{"threadId":self.thread,"reason":"requested"}})
                    },
                ]
            }
            "__finish_voice_stop" => vec![
                json!({"method":"thread/realtime/closed","params":{"threadId":self.thread,"reason":"requested"}}),
                reply(json!({})),
            ],
            _ => panic!("unexpected routed native fixture request"),
        }
    }
    fn history_items(&mut self) -> Arc<Value> {
        Arc::clone(
            self.history_items
                .get_or_insert_with(|| Arc::new(read(&self.root, "paged-history.json"))),
        )
    }
    fn history(&mut self, value: &Value) -> Vec<Value> {
        let id = &value["id"];
        let params = &value["params"];
        let reply = |data: Value| json!({"id":id,"result":data});
        match value["method"].as_str().unwrap() {
            "thread/read" => {
                save(&self.root, "read.json", value);
                let child = params["threadId"]
                    .as_str()
                    .unwrap_or(&self.thread)
                    .to_owned();
                if child == "child-thread" && self.exists("child-read.json") {
                    return vec![reply(json!({"thread":read(&self.root,"child-read.json")}))];
                }
                let turns = if self.exists("legacy-history") {
                    let mut items = self.history_items().as_array().unwrap().clone();
                    if self.exists("malformed-history") && items.len() > 1 {
                        let n = items.len();
                        items[n.checked_sub(1).unwrap()] = items[n.checked_sub(2).unwrap()].clone();
                    }
                    json!(items.iter().map(|entry|json!({"id":entry["turnId"],"items":[entry["item"]],"startedAtMs":entry["startedAtMs"],"completedAtMs":entry["completedAtMs"]})).collect::<Vec<_>>())
                } else if child == "child-thread" {
                    json!([{"id":"child-turn","items":[{"id":"child-answer","type":"agentMessage","text":"Child review"}]}])
                } else {
                    json!([])
                };
                vec![reply(json!({"thread":{"id":child,"turns":turns}}))]
            }
            "thread/items/list" => {
                assert_eq!(params["threadId"], self.thread);
                if self.exists("legacy-history") {
                    return vec![
                        json!({"id":id,"error":{"code":-32601,"message":"thread/items/list is not supported yet"}}),
                    ];
                }
                let items = self.history_items();
                let items = items.as_array().unwrap();
                let limit = usize::try_from(params["limit"].as_u64().unwrap()).unwrap();
                self.history_limits.push(limit);
                let cursor = params["cursor"]
                    .as_str()
                    .map(|cursor| cursor.parse::<usize>().unwrap());
                let (mut data, next, backwards) = if params["sortDirection"] == "desc" {
                    let end = cursor.unwrap_or(items.len());
                    let start = end.saturating_sub(limit);
                    (
                        items[start..end].iter().rev().cloned().collect::<Vec<_>>(),
                        (start > 0).then(|| start.to_string()),
                        (start < end).then(|| end.to_string()),
                    )
                } else {
                    let start = cursor.unwrap();
                    let end = items.len().min(start.checked_add(limit).unwrap());
                    (
                        items[start..end].to_vec(),
                        (end < items.len()).then(|| end.to_string()),
                        (start > 0).then(|| start.to_string()),
                    )
                };
                if self.exists("malformed-history") && data.len() > 1 {
                    let n = data.len();
                    data[n.checked_sub(1).unwrap()] = data[0].clone();
                }
                vec![reply(
                    json!({"data":data,"nextCursor":next,"backwardsCursor":backwards}),
                )]
            }
            _ => panic!("unexpected routed native fixture request"),
        }
    }
}
