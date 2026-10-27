#![cfg(test)]

use std::{fs, path::Path, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_agents::{AgentKind, NativeLaunch, NativeLauncher};
use serde_json::{Value, json};

fn save(root: &Path, name: &str, value: &Value) {
    fs::write(root.join(name), super::native_peer::ascii_json(value)).unwrap();
}

fn read(root: &Path, name: &str, default: Value) -> Value {
    fs::read(root.join(name))
        .ok()
        .map_or(default, |bytes| serde_json::from_slice(&bytes).unwrap())
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep the three providers byte-for-byte echo sequences together"
)]
pub(crate) fn launcher() -> NativeLauncher {
    Arc::new(|launch: &NativeLaunch<'_>| {
        if launch.config.program.ends_with("capture-provider.py") {
            return Ok(capture(launch));
        }
        let config = launch.config.clone();
        let root = config.cwd.canonicalize().unwrap();
        let claude = config.provider == AgentKind::Claude;
        let pi = config.provider == AgentKind::Pi;
        let history_text = fs::read_to_string(root.join("history-mode")).ok();
        let history_mode = history_text.is_some();
        let mut session = config
            .session_id
            .or(config.fresh_session_id)
            .unwrap_or_else(|| "image-session".into());
        let file = if history_mode && !claude {
            let generation = fs::read_to_string(root.join("provider-counter"))
                .unwrap_or_else(|_| "0".into())
                .parse::<u64>()
                .unwrap()
                .checked_add(1)
                .unwrap();
            fs::write(root.join("provider-counter"), generation.to_string()).unwrap();
            session = format!("image-session-{generation}");
            config
                .session_file
                .map_or_else(|| root.join(format!("{session}.jsonl")), Into::into)
        } else {
            root.join("image-session.jsonl")
        };
        let mut messages = Vec::<Value>::new();
        let mut items = Vec::<Value>::new();
        let mut turn = 0_u64;
        Ok(super::native_peer::peer(move |value| {
            let reply = |result: Value| {
                if claude {
                    json!({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":result}})
                } else if pi {
                    json!({"id":value["id"],"type":"response","command":value["type"],"success":true,"data":result})
                } else {
                    json!({"id":value["id"],"result":result})
                }
            };
            let notice = |method: &str, params: Value| {
                let mut base = json!({"threadId":session,"turnId":"turn-image"});
                base.as_object_mut()
                    .unwrap()
                    .extend(params.as_object().unwrap().clone());
                json!({"method":method,"params":base})
            };
            let kind = value["method"]
                .as_str()
                .or_else(|| value["type"].as_str())
                .unwrap();
            match kind {
                "control_request" | "initialize" => vec![reply(json!({}))],
                "initialized" => vec![],
                "thread/start" | "thread/resume" | "thread/read" => vec![reply(
                    json!({"thread":{"id":session,"turns":[{"id":"turn-image","items":items}]}}),
                )],
                "get_state" => vec![reply(
                    json!({"sessionId":session,"sessionFile":file,"isStreaming":false,"isCompacting":false}),
                )],
                "get_messages" => vec![reply(json!({"messages":messages}))],
                "user" | "prompt" | "turn/start" => {
                    turn = turn.checked_add(1).unwrap();
                    let blocks = if claude {
                        value["message"]["content"].as_str().map_or_else(
                            || value["message"]["content"].as_array().unwrap().clone(),
                            |text| vec![json!({"type":"text","text":text})],
                        )
                    } else if pi {
                        let mut blocks = Vec::new();
                        if value["message"] != "" {
                            blocks.push(json!({"type":"text","text":value["message"]}));
                        }
                        blocks.extend(value["images"].as_array().into_iter().flatten().cloned());
                        blocks
                    } else {
                        value["params"]["input"].as_array().unwrap().clone()
                    };
                    let mut images = Vec::new();
                    for block in &blocks {
                        if block["type"] == "image" {
                            let data = if claude {
                                assert_eq!(block["source"]["type"], "base64");
                                assert_eq!(block["source"]["media_type"], "image/png");
                                block["source"]["data"].as_str().unwrap()
                            } else if pi {
                                assert_eq!(block["mimeType"], "image/png");
                                block["data"].as_str().unwrap()
                            } else {
                                block["url"]
                                    .as_str()
                                    .unwrap()
                                    .strip_prefix("data:image/png;base64,")
                                    .unwrap()
                            };
                            images.push(data);
                        }
                    }
                    if let Some(image) = images.first() {
                        fs::write(root.join("received.png"), STANDARD.decode(image).unwrap())
                            .unwrap();
                    }
                    save(&root, "prompt.json", &value);
                    if claude {
                        let mut echo = value.clone();
                        echo["isReplay"] = json!(true);
                        if root.join("jpeg-echo").exists() {
                            let mut blocks = blocks.clone();
                            for block in &mut blocks {
                                if block["type"] == "image" {
                                    block["source"]["media_type"] = json!("image/jpeg");
                                }
                            }
                            echo["message"]["content"] = json!(blocks);
                        }
                        let mut events = vec![
                            json!({"type":"system","subtype":"init","session_id":session,"cwd":root}),
                            echo,
                        ];
                        if history_mode {
                            events.push(json!({"type":"assistant","uuid":format!("answer-{turn}"),"session_id":session,"parent_tool_use_id":null,"message":{"id":format!("answer-{turn}"),"content":[{"type":"text","text":history_text.as_deref().unwrap()}]}}));
                        }
                        events.push(json!({"type":"result","subtype":"success","session_id":session,"uuid":"result-not-prompt","is_error":false,"terminal_reason":"completed","result":"Image seen"}));
                        events
                    } else if pi {
                        let user = json!({"role":"user","content":blocks,"timestamp":turn.checked_mul(2).unwrap()});
                        if !history_mode {
                            messages.clear();
                        }
                        messages.push(user.clone());
                        let mut events = vec![
                            json!({"type":"agent_start"}),
                            json!({"type":"message_start","message":user}),
                            json!({"type":"message_end","message":user}),
                        ];
                        if history_mode {
                            let assistant = json!({"role":"assistant","content":[{"type":"text","text":history_text.as_deref().unwrap()}],"timestamp":turn.checked_mul(2).unwrap().checked_add(1).unwrap(),"stopReason":"stop"});
                            events.extend([
                                json!({"type":"message_end","message":assistant}),
                                json!({"type":"agent_end","messages":[user,assistant]}),
                            ]);
                            messages.push(assistant);
                        }
                        events.extend([
                            reply(json!({"disposition":"started"})),
                            json!({"type":"agent_settled"}),
                        ]);
                        events
                    } else {
                        let user = json!({"id":format!("user-image-{turn}"),"type":"userMessage","content":blocks});
                        if !history_mode {
                            items.clear();
                        }
                        items.push(user.clone());
                        let mut events = vec![
                            notice(
                                "turn/started",
                                json!({"turn":{"id":"turn-image","status":"inProgress"}}),
                            ),
                            notice("item/completed", json!({"item":user})),
                        ];
                        let mut completed = json!({"id":"turn-image","status":"completed"});
                        if history_mode {
                            let assistant = json!({"id":format!("answer-{turn}"),"type":"agentMessage","text":history_text.as_deref().unwrap()});
                            events.push(notice("item/completed", json!({"item":assistant})));
                            items.push(assistant.clone());
                            completed["items"] = json!([user, assistant]);
                        }
                        events.extend([reply(json!({"turn":{"id":"turn-image","items":if history_mode{vec![]}else{items.clone()}}})),notice("turn/completed",json!({"turn":completed}))]);
                        events
                    }
                }
                kind => panic!("unexpected image peer request {kind}"),
            }
        }))
    })
}

fn pi_name(server: &str, tool: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let name = format!("mcp__{server}__{tool}");
    if name.len() <= 64 {
        name
    } else {
        let hash = Sha256::digest(format!("{server}\0{tool}"));
        format!(
            "{}_{}",
            name.get(..55).unwrap(),
            hash.iter().take(4).fold(String::new(), |mut text, byte| {
                use std::fmt::Write as _;
                write!(text, "{byte:02x}").unwrap();
                text
            })
        )
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep nested capture fault cases in their wire order"
)]
fn capture(launch: &NativeLaunch<'_>) -> bootty_agents::NativeChild {
    let config = launch.config.clone();
    let root = config.cwd.canonicalize().unwrap();
    let pi = config.provider == AgentKind::Pi;
    let mut session = "capture-session".to_owned();
    let command = launch.command().unwrap();
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut server = String::new();
    for pair in args.windows(2) {
        if pair[0] == "--config" && pair[1].starts_with("mcp_servers.") {
            server = pair[1].split('.').nth(1).unwrap().into();
        }
        if pi && pair[0] == "--extension" {
            let extension = fs::read_to_string(&pair[1]).unwrap();
            let value = extension.split("const server = ").nth(1).unwrap();
            server = serde_json::Deserializer::from_str(value)
                .into_iter::<String>()
                .next()
                .unwrap()
                .unwrap();
        }
    }
    let fault = fs::read_to_string(root.join("capture-fault")).unwrap_or_default();
    let mut history = read(&root, "capture-history.json", json!([]));
    let content = json!([{"type":"image","mimeType":"image/png","data":STANDARD.encode(fs::read(root.join("capture.png")).unwrap())},{"type":"text","text":"Capture 1x1"}]);
    super::native_peer::peer(move |value| {
        let reply = |mut data: Value| {
            if pi && value["type"] == "prompt" && fault == "legacy_ack" {
                data = json!({});
            }
            if pi {
                json!({"id":value["id"],"type":"response","command":value["type"],"success":true,"data":data})
            } else {
                json!({"id":value["id"],"result":data})
            }
        };
        let notice = |method: &str, params: Value| {
            let mut base = json!({"threadId":session,"turnId":"capture-turn"});
            base.as_object_mut()
                .unwrap()
                .extend(params.as_object().unwrap().clone());
            json!({"method":method,"params":base})
        };
        let kind = value["method"]
            .as_str()
            .or_else(|| value["type"].as_str())
            .unwrap();
        match kind {
            "initialized" => vec![],
            "initialize" | "__barrier" => vec![reply(json!({}))],
            "thread/start" | "thread/resume" | "thread/read" => vec![reply(
                json!({"thread":{"id":session,"turns":if history.as_array().unwrap().is_empty(){json!([])}else{json!([{"id":"capture-turn","items":history}])}}}),
            )],
            "get_state" => vec![reply(
                json!({"sessionId":session,"sessionFile":root.join("capture.jsonl"),"isStreaming":false}),
            )],
            "get_messages" => vec![reply(json!({"messages":history}))],
            "get_commands" => vec![reply(
                json!({"commands":if fault=="checkpoint_missing"{json!([])}else{json!([{"name":"__bootty_checkpoint","source":"extension"}])}}),
            )],
            "switch_session" => {
                if fault == "checkpoint_identity" {
                    session = "foreign-session".into();
                }
                vec![reply(json!({"cancelled":fault=="checkpoint_cancelled"}))]
            }
            "prompt" if value["message"] == "/__bootty_checkpoint" => {
                vec![reply(json!({"disposition":"handled"}))]
            }
            "turn/start" | "prompt" => {
                let tool = if fault == "name" {
                    "unrelated_tool"
                } else {
                    "computer_snapshot"
                };
                let mut final_id = if fault == "id" {
                    "unobserved-tool"
                } else {
                    "capture-id"
                };
                if pi {
                    let nested = fault.starts_with("nested");
                    let tool_server = if fault == "server" {
                        format!("bootty_{}", "0".repeat(64))
                    } else {
                        server.clone()
                    };
                    let mut details = json!({"server":tool_server,"tool":tool});
                    let mut name = if fault == "plain" {
                        tool.into()
                    } else {
                        pi_name(&tool_server, tool)
                    };
                    let mut content = content.clone();
                    let mut events = vec![json!({"type":"agent_start"})];
                    if nested {
                        events.push(json!({"type":"tool_execution_start","toolCallId":"codemode-id","toolName":"codemode"}));
                    }
                    let mut start = json!({"type":"tool_execution_start","toolCallId":"capture-id","toolName":name});
                    if nested {
                        start["parentToolCallId"] = json!("codemode-id");
                    }
                    events.push(start);
                    let mut end = json!({"type":"tool_execution_end","toolCallId":final_id,"toolName":name,"result":{"content":content,"details":details},"isError":false});
                    if nested {
                        end["parentToolCallId"] = json!(if fault == "nested_capture_parent" {
                            "foreign-codemode"
                        } else {
                            "codemode-id"
                        });
                    }
                    events.push(end);
                    if nested {
                        name = "codemode".into();
                        final_id = if fault == "nested_parent" {
                            "foreign-codemode"
                        } else {
                            "codemode-id"
                        };
                        if fault == "nested_changed_image" {
                            content = json!([{"type":"image","mimeType":"image/png","data":STANDARD.encode(fs::read(root.join("changed.png")).unwrap())}]);
                        }
                        details = json!({"calls":[{"id":"capture-id","name":pi_name(&server,"computer_snapshot"),"args":"{}","status":"ok"}]});
                        events.push(json!({"type":"tool_execution_end","toolCallId":final_id,"toolName":name,"result":{"content":content,"details":details},"isError":false}));
                    }
                    let message = json!({"role":"toolResult","toolCallId":final_id,"toolName":name,"content":content,"details":details,"timestamp":1});
                    history = json!([message]);
                    save(&root, "capture-history.json", &history);
                    events.extend([
                        json!({"type":"message_start","message":message}),
                        json!({"type":"message_end","message":message}),
                    ]);
                    let mut final_message = message;
                    if fault == "completion_image" {
                        final_message["content"] = json!([{"type":"image","mimeType":"image/png","data":STANDARD.encode(fs::read(root.join("changed.png")).unwrap())}]);
                    }
                    if fault == "completion_id" || fault == "nested_completion_parent" {
                        final_message["toolCallId"] = json!("foreign-tool");
                    }
                    events.extend([json!({"type":"turn_end","message":{"role":"assistant","content":[]},"toolResults":[final_message]}),json!({"type":"agent_end","messages":[final_message]}),reply(json!({"disposition":"started"})),json!({"type":"agent_settled"})]);
                    events
                } else {
                    let mut item = json!({"id":"capture-id","type":"mcpToolCall","server":if fault=="server"{"foreign-server"}else{&server},"tool":tool,"status":"inProgress","arguments":{},"result":null});
                    let mut events = vec![
                        notice(
                            "turn/started",
                            json!({"turn":{"id":"capture-turn","status":"inProgress"}}),
                        ),
                        notice("item/started", json!({"item":item})),
                    ];
                    item["id"] = json!(final_id);
                    item["result"] = json!({"content":content});
                    item["status"] = json!("completed");
                    history = json!([item]);
                    save(&root, "capture-history.json", &history);
                    events.extend([notice("item/completed",json!({"item":item})),notice("turn/completed",json!({"turn":{"id":"capture-turn","status":"completed","items":[item]}})),reply(json!({"turn":{"id":"capture-turn"}}))]);
                    events
                }
            }
            kind => panic!("unexpected capture peer request {kind}"),
        }
    })
}
