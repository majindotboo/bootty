#![cfg(test)]

use std::{fs, io::Write as _, path::Path, sync::Arc};

use bootty_agents::{AgentKind, NativeLaunch, NativeLauncher};
use serde_json::{Value, json};

fn read(root: &Path, name: &str, default: Value) -> Value {
    fs::read(root.join(name))
        .ok()
        .map_or(default, |bytes| serde_json::from_slice(&bytes).unwrap())
}

fn save(root: &Path, name: &str, value: &Value) {
    fs::write(root.join(name), super::native_peer::ascii_json(value)).unwrap();
}

#[expect(
    clippy::too_many_lines,
    reason = "One catalog fixture mirrors all three provider protocols"
)]
pub(crate) fn launcher() -> NativeLauncher {
    Arc::new(|launch: &NativeLaunch<'_>| {
        let config = launch.config.clone();
        let root = config.cwd.canonicalize().unwrap();
        let claude = config.provider == AgentKind::Claude;
        let pi = config.provider == AgentKind::Pi;
        let identity = config
            .session_id
            .clone()
            .or_else(|| config.fresh_session_id.clone());
        let mut model = json!({"provider":"configured","id":"available","name":"Available","reasoning":true,"thinkingLevelMap":{"xhigh":null,"max":"high"}});
        let mut effort = config
            .reasoning_effort
            .clone()
            .unwrap_or_else(|| "medium".into());
        let mut fast = config.fast_mode;
        let mut goal = Value::Null;
        if claude {
            let command = launch.command()?;
            let arguments = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            save(&root, "argv.json", &json!(arguments));
        }
        Ok(super::native_peer::peer(move |value| {
            let reply = |data: Value| {
                if claude {
                    json!({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":data}})
                } else if pi {
                    json!({"id":value["id"],"type":"response","command":value["type"],"success":true,"data":data})
                } else {
                    json!({"id":value["id"],"result":data})
                }
            };
            let finish = || json!({"type":"result","subtype":"success","session_id":identity,"uuid":"result-uuid","is_error":false,"terminal_reason":"completed","result":"Done","errors":[]});
            if claude && value["type"] == "user" {
                save(
                    &root,
                    "prompt.json",
                    &json!({"params":value["message"],"model":model,"effort":effort,"fast":fast}),
                );
                let mut echo = value.clone();
                echo["isReplay"] = json!(true);
                let mut events = vec![
                    json!({"type":"system","subtype":"init","session_id":identity,"cwd":root}),
                    echo,
                ];
                if value["message"]["content"] == "plan" {
                    events.push(json!({"type":"control_request","request_id":"plan-id","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","tool_use_id":"plan-tool","input":{"plan":"1. Write it"},"requires_user_interaction":true}}));
                } else if value["message"]["content"] == "finish" {
                    events.push(finish());
                }
                return events;
            }
            if claude && value["type"] == "control_response" {
                return vec![finish()];
            }
            let method = if claude {
                &value["request"]["subtype"]
            } else if pi {
                &value["type"]
            } else {
                &value["method"]
            };
            let params = if claude {
                &value["request"]
            } else if pi {
                &value
            } else {
                &value["params"]
            };
            let mut events = Vec::new();
            let data = match method.as_str().unwrap() {
                "initialized" => return events,
                "initialize" => {
                    if claude {
                        read(
                            &root,
                            "claude-catalog.json",
                            json!({"models":[{"value":"available","resolvedModel":"claude-test","displayName":"Available","supportsFastMode":true,"supportsEffort":true,"supportedEffortLevels":["medium","high","max"]}]}),
                        )
                    } else {
                        json!({})
                    }
                }
                "thread/start" | "thread/resume" => {
                    fs::write(root.join("thread-started"), method.as_str().unwrap()).unwrap();
                    json!({"thread":{"id":"thread","turns":[]}})
                }
                "skills/list" => {
                    json!({"data":[{"cwd":root,"skills":[{"name":"project-skill","path":root.join("SKILL.md"),"enabled":true,"description":"Project work","scope":"project"},{"name":"disabled","path":"/disabled","enabled":false}]}]})
                }
                "app/installed" => {
                    json!({"apps":read(&root,"apps.json",json!([{"id":"mail","runtimeName":"Mail","enabled":true,"callable":true},{"id":"disabled-app","enabled":false,"callable":false},{"id":"uncallable","enabled":true,"callable":false}]))})
                }
                "app/read" => {
                    json!({"apps":[{"id":"mail","name":"Mail","description":"Read and manage mail"}]})
                }
                "app/list" => {
                    json!({"data":read(&root,"apps.json",json!([{"id":"mail","name":"Mail","isAccessible":true,"isEnabled":true},{"id":"disabled-app","name":"Disabled","isAccessible":true,"isEnabled":false},{"id":"inaccessible","name":"Private","isAccessible":false,"isEnabled":true}])) ,"nextCursor":null})
                }
                "plugin/installed" => {
                    json!({"marketplaces":[{"name":"market","plugins":[{"id":"review@market","name":"review","installed":true,"enabled":true,"interface":{"displayName":"Code review","shortDescription":"Review changes"}},{"id":"disabled-plugin","installed":true,"enabled":false},{"id":"uninstalled","installed":false,"enabled":true},{"id":"admin-blocked","installed":true,"enabled":true,"availability":"DISABLED_BY_ADMIN"}]}]})
                }
                "collaborationMode/list" => json!({"data":[{"name":"Plan","mode":"plan"}]}),
                "experimentalFeature/list" => json!({"data":[{"name":"goals","enabled":true}]}),
                "thread/goal/get" => json!({"goal":goal}),
                "thread/goal/set" => {
                    assert_eq!(params["threadId"], "thread");
                    assert!(params.get("origin").is_none());
                    if goal.is_null() {
                        goal = json!({"tokensUsed":1200,"tokenBudget":50_000,"timeUsedSeconds":90});
                    }
                    for key in ["objective", "status"] {
                        if let Some(value) = params.get(key) {
                            goal[key] = value.clone();
                        }
                    }
                    json!({"goal":goal})
                }
                "thread/goal/clear" => {
                    assert_eq!(params, &json!({"threadId":"thread"}));
                    goal = Value::Null;
                    json!({"cleared":true})
                }
                "get_commands" => {
                    let mut commands = vec![
                        json!({"name":"review","description":"Review work","source":"extension"}),
                        json!({"name":"skill:project-skill","source":"skill","description":"Project work","sourceInfo":{"path":root.join("SKILL.md"),"scope":"project"}}),
                    ];
                    if !root.join("no-plan").exists() {
                        commands.push(json!({"name":"__bootty_plan","source":"extension"}));
                    }
                    json!({"commands":commands})
                }
                "model/list" => {
                    let mut advertised = json!({"id":"catalog-id","model":"available","displayName":"Available","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],"defaultReasoningEffort":"medium","isDefault":true,"serviceTiers":[{"id":"fast"}]});
                    if root.join("speed-capabilities.json").exists() {
                        advertised.as_object_mut().unwrap().remove("serviceTiers");
                        advertised.as_object_mut().unwrap().extend(
                            read(&root, "speed-capabilities.json", json!({}))
                                .as_object()
                                .unwrap()
                                .clone(),
                        );
                    }
                    json!({"data":[advertised],"nextCursor":null})
                }
                "config/read" => {
                    let mut settings = json!({"model":"available","model_reasoning_effort":"high"});
                    settings.as_object_mut().unwrap().extend(
                        read(&root, "codex-config.json", json!({}))
                            .as_object()
                            .unwrap()
                            .clone(),
                    );
                    json!({"config":settings})
                }
                "get_available_models" => json!({"models":[model]}),
                "get_state" => {
                    json!({"sessionId":"thread","sessionFile":root.join("session.jsonl"),"model":model,"thinkingLevel":effort,"isStreaming":false,"isCompacting":false})
                }
                "get_messages" => json!({"messages":[]}),
                "set_model" => {
                    assert_eq!(
                        params[if claude { "model" } else { "modelId" }],
                        model["id"]
                    );
                    if !claude {
                        assert_eq!(params["provider"], model["provider"]);
                    }
                    model["contextWindow"] = json!(32_768);
                    model.clone()
                }
                "set_thinking_level" => {
                    effort = params["level"].as_str().unwrap().to_owned();
                    json!({})
                }
                "apply_flag_settings" => {
                    if let Some(level) = params["settings"]["effortLevel"].as_str() {
                        effort = level.into();
                    }
                    if let Some(value) = params["settings"]["fastMode"].as_bool() {
                        fast = value;
                    }
                    json!({})
                }
                "interrupt" => json!({}),
                "set_permission_mode" => {
                    append(&root, "modes", params["mode"].as_str().unwrap());
                    json!({"mode":params["mode"]})
                }
                "get_settings" => read(
                    &root,
                    "claude-settings.json",
                    json!({"effective":{"effortLevel":effort,"fastMode":fast}}),
                ),
                "prompt"
                    if pi
                        && params["message"]
                            .as_str()
                            .unwrap()
                            .starts_with("/__bootty_plan ") =>
                {
                    append(&root, "modes", params["message"].as_str().unwrap());
                    json!({"disposition":"handled"})
                }
                "turn/start" | "turn/steer" | "prompt" => {
                    if method == "turn/start" {
                        events.extend([json!({"method":"turn/started","params":{"threadId":"thread","turn":{"id":"turn","status":"inProgress","items":[]}}}),json!({"method":"item/completed","params":{"threadId":"thread","turnId":"turn","item":{"id":"provider-user","type":"userMessage","content":params["input"]}}})]);
                    }
                    save(
                        &root,
                        "prompt.json",
                        &json!({"params":params,"model":model,"effort":effort,"fast":fast}),
                    );
                    if pi {
                        json!({"disposition":"handled"})
                    } else {
                        json!({"turn":{"id":"turn"}})
                    }
                }
                method => panic!("unexpected model peer request {method}: {value}"),
            };
            events.push(reply(data));
            events
        }))
    })
}

fn append(root: &Path, name: &str, text: &str) {
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join(name))
            .unwrap(),
        "{text}"
    )
    .unwrap();
}
