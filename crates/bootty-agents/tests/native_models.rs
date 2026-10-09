#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use assert_fs::TempDir;
use bootty_agents::{AgentKind, NativeAgentService, NativeModelSelection, NativeSessionConfig};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

fn config(root: &Path, provider: AgentKind) -> std::io::Result<NativeSessionConfig> {
    let script = root.join("provider.py");
    fs::write(
        &script,
        r"#!/usr/bin/env python3
import json,os,sys
pi='--mode' in sys.argv
claude='--input-format' in sys.argv
identity=sys.argv[sys.argv.index('--resume' if '--resume' in sys.argv else '--session-id')+1] if claude else None
model={'provider':'configured','id':'available','name':'Available','reasoning':True,'thinkingLevelMap':{'xhigh':None,'max':'high'}}
effort=sys.argv[sys.argv.index('--effort')+1] if '--effort' in sys.argv else 'medium'
def emit(value): print(json.dumps(value),flush=True)
def reply(value,data):
 if claude: emit({'type':'control_response','response':{'subtype':'success','request_id':value['request_id'],'response':data}})
 elif pi: emit({'id':value['id'],'type':'response','command':value['type'],'success':True,'data':data})
 else: emit({'id':value['id'],'result':data})
for line in sys.stdin:
 value=json.loads(line)
 if claude and value['type']=='user':
  with open('prompt.json','w') as output: json.dump({'params':value['message'],'model':model,'effort':effort},output)
  emit({'type':'system','subtype':'init','session_id':identity,'cwd':os.getcwd()})
  emit(dict(value,isReplay=True))
  continue
 method=value['request']['subtype'] if claude else value.get('type') if pi else value.get('method')
 params=value['request'] if claude else value if pi else value.get('params',{})
 if method=='initialized': continue
 if method=='initialize':
  catalog={'models':[{'value':'available','resolvedModel':'claude-test','displayName':'Available','supportsEffort':True,'supportedEffortLevels':['medium','high','max']}]}
  if claude and os.path.exists('claude-catalog.json'):
   with open('claude-catalog.json') as source: catalog=json.load(source)
  reply(value,catalog if claude else {})
 elif method in ['thread/start','thread/resume']:
  with open('thread-started','w') as output: output.write(method)
  reply(value,{'thread':{'id':'thread','turns':[]}})
 elif method=='skills/list': reply(value,{'data':[{'cwd':os.getcwd(),'skills':[{'name':'project-skill','path':os.path.join(os.getcwd(),'SKILL.md'),'enabled':True,'description':'Project work','scope':'project'},{'name':'disabled','path':'/disabled','enabled':False}]}]})
 elif method=='get_commands': reply(value,{'commands':[{'name':'review','description':'Review work','source':'extension'},{'name':'skill:project-skill','source':'skill','description':'Project work','sourceInfo':{'path':os.path.join(os.getcwd(),'SKILL.md'),'scope':'project'}}]})
 elif method=='model/list': reply(value,{'data':[{'id':'catalog-id','model':'available','displayName':'Available','supportedReasoningEfforts':[{'reasoningEffort':'medium'},{'reasoningEffort':'high'}],'defaultReasoningEffort':'medium','isDefault':True}],'nextCursor':None})
 elif method=='config/read':
  settings={'model':'available','model_reasoning_effort':'high'}
  if os.path.exists('codex-config.json'):
   with open('codex-config.json') as source: settings.update(json.load(source))
  reply(value,{'config':settings})
 elif method=='get_available_models': reply(value,{'models':[model]})
 elif method=='get_state': reply(value,{'sessionId':'thread','sessionFile':os.path.join(os.getcwd(),'session.jsonl'),'model':model,'thinkingLevel':effort,'isStreaming':False,'isCompacting':False})
 elif method=='get_messages': reply(value,{'messages':[]})
 elif method=='set_model':
  if claude: assert params['model']==model['id']
  else: assert params['provider']==model['provider'] and params['modelId']==model['id']
  model['contextWindow']=32768
  reply(value,model)
 elif method=='set_thinking_level': effort=params['level'];reply(value,{})
 elif method=='apply_flag_settings': effort=params['settings']['effortLevel'];reply(value,{})
 elif method=='interrupt': reply(value,{})
 elif method=='get_settings':
  settings={'effective':{'effortLevel':effort}}
  if os.path.exists('claude-settings.json'):
   with open('claude-settings.json') as source: settings=json.load(source)
  reply(value,settings)
 elif method in ['turn/start','prompt']:
  with open('prompt.json','w') as output: json.dump({'params':params,'model':model,'effort':effort},output)
  if pi: reply(value,{'disposition':'handled'})
  else: reply(value,{'turn':{'id':'turn'}})
 else: raise Exception(method)
",
    )?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700))?;
    let mut config = NativeSessionConfig::new(provider, root);
    config.program = script.to_string_lossy().into_owned();
    config.account_directory = Some(root.join("account").to_string_lossy().into_owned());
    Ok(config)
}

#[rstest]
#[case(false)]
#[case(true)]
fn codex_fast_mode_is_sent_with_the_turn(#[case] fast: bool) {
    let root = TempDir::new().unwrap();
    let mut config = config(root.path(), AgentKind::Codex).unwrap();
    config.fast_mode = fast;
    let session = bootty_agents::NativeAgentSession::spawn(config).unwrap();
    session.send_prompt("hello").unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(
        wire["params"].get("serviceTier").cloned(),
        fast.then(|| json!("fast"))
    );
    session.stop();
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Pi)]
#[case(AgentKind::Claude)]
fn discovery_opens_no_conversation_and_submits_no_turn(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    let options =
        bootty_agents::NativeAgentSession::discover_models(config(root.path(), provider).unwrap())
            .unwrap();
    assert_eq!(options.len(), 1);
    assert!(!root.path().join("thread-started").exists());
    assert!(!root.path().join("prompt.json").exists());
    assert!(!root.path().join("session.jsonl").exists());
}

#[rstest]
#[case(AgentKind::Codex, json!({"approval_policy":"never","sandbox_mode":"danger-full-access"}), Some(bootty_agents::NativePermissionMode::FullAccess))]
#[case(AgentKind::Codex, json!({"approval_policy":"untrusted","sandbox_mode":"read-only"}), Some(bootty_agents::NativePermissionMode::Supervised))]
#[case(AgentKind::Codex, json!({"approval_policy":"on-request","sandbox_mode":"workspace-write","approvals_reviewer":"user"}), Some(bootty_agents::NativePermissionMode::AutoAcceptEdits))]
#[case(AgentKind::Codex, json!({"approval_policy":"on-request","sandbox_mode":"workspace-write","approvals_reviewer":"auto_review"}), Some(bootty_agents::NativePermissionMode::Auto))]
#[case(AgentKind::Codex, json!({"approval_policy":"never","sandbox_mode":"workspace-write"}), None)]
#[case(AgentKind::Claude, json!({"effective":{"permissions":{"defaultMode":"bypassPermissions"}}}), Some(bootty_agents::NativePermissionMode::FullAccess))]
#[case(AgentKind::Claude, json!({"effective":{"permissions":{"defaultMode":"acceptEdits"}}}), Some(bootty_agents::NativePermissionMode::AutoAcceptEdits))]
#[case(AgentKind::Claude, json!({"effective":{"permissions":{"defaultMode":"plan"}}}), None)]
#[case(AgentKind::Pi, json!({}), None)]
fn discovery_reports_the_captured_provider_policy_without_starting_a_conversation(
    #[case] provider: AgentKind,
    #[case] settings: Value,
    #[case] expected: Option<bootty_agents::NativePermissionMode>,
) {
    let root = TempDir::new().unwrap();
    let filename = if provider == AgentKind::Codex {
        "codex-config.json"
    } else {
        "claude-settings.json"
    };
    fs::write(
        root.path().join(filename),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    let mut config = config(root.path(), provider).unwrap();
    config.permissions = bootty_agents::NativePermissionMode::Supervised;
    let catalog = bootty_agents::NativeAgentSession::discover_catalog(config).unwrap();
    assert_eq!(catalog.permissions, expected);
    assert_eq!(catalog.models.len(), 1);
    assert!(!root.path().join("thread-started").exists());
    assert!(!root.path().join("prompt.json").exists());
}

#[rstest]
#[case(AgentKind::Codex, "available")]
#[case(AgentKind::Pi, "configured/available")]
#[case(AgentKind::Claude, "available")]
fn committed_selection_drives_the_next_provider_prompt(
    #[case] provider: AgentKind,
    #[case] model: &str,
) {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let record = service
        .create("binding", "Session", config(root.path(), provider).unwrap())
        .unwrap();
    let models = service.models(&record.target()).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, model);
    assert!(models[0].reasoning_efforts.contains(&"high".to_owned()));
    if provider == AgentKind::Pi {
        assert!(models[0].reasoning_efforts.contains(&"max".to_owned()));
        assert!(!models[0].reasoning_efforts.contains(&"xhigh".to_owned()));
    }
    let selection = NativeModelSelection {
        model: model.to_owned(),
        reasoning_effort: Some("high".to_owned()),
    };
    service.configure(&record.target(), &selection).unwrap();
    let stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["records"][0]["config"]["model"], model);
    assert_eq!(stored["records"][0]["config"]["reasoning_effort"], "high");
    assert!(!root.path().join("prompt.json").exists());
    service.prompt(&record.target(), "hello").unwrap();
    if provider == AgentKind::Claude {
        // The correlated control reply follows the prompt on Claude's input stream.
        service.interrupt(&record.target()).unwrap();
    }
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    if provider == AgentKind::Codex {
        assert_eq!(wire["params"]["model"], model);
        assert_eq!(wire["params"]["effort"], "high");
        assert_eq!(wire["params"].get("approvalPolicy"), None);
        assert_eq!(wire["params"].get("sandboxPolicy"), None);
    } else {
        assert_eq!(wire["effort"], "high");
        assert_eq!(wire["model"]["id"], "available");
    }
    if provider == AgentKind::Pi {
        let snapshot = service
            .sessions()
            .into_iter()
            .find(|session| session.id == record.id)
            .expect("configured conversation")
            .snapshot;
        assert_eq!(
            snapshot
                .usage
                .as_ref()
                .and_then(|usage| usage.get("modelContextWindow")),
            Some(&json!(32768)),
            "context capacity follows the accepted provider model"
        );
    }
    service.shutdown().unwrap();
    let reopened = NativeAgentService::open(&path).unwrap();
    let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["records"][0]["config"]["model"], model);
    drop(reopened);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]
    #[test]
    fn unadvertised_selection_leaves_durable_configuration_unchanged(selector in "unavailable-[a-z]{1,8}", invalid_model in any::<bool>()) {
        let root = TempDir::new().unwrap();
        let path = root.path().join("native.json");
        let service = NativeAgentService::open(&path).unwrap();
        let record = service.create("binding", "Session", config(root.path(), AgentKind::Codex).unwrap()).unwrap();
        let before: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let selection = NativeModelSelection {
            model: if invalid_model { selector.clone() } else { "available".to_owned() },
            reasoning_effort: if invalid_model { None } else { Some(selector) },
        };
        prop_assert!(service.configure(&record.target(), &selection).is_err());
        let after: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(before["records"][0]["config"], after["records"][0]["config"]);
        prop_assert!(!root.path().join("prompt.json").exists());
    }
}

#[rstest]
fn failed_configuration_write_keeps_the_prior_prompt_settings() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let record = service
        .create(
            "binding",
            "Session",
            config(root.path(), AgentKind::Codex).unwrap(),
        )
        .unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        service
            .configure(
                &record.target(),
                &NativeModelSelection {
                    model: "available".to_owned(),
                    reasoning_effort: Some("high".to_owned())
                }
            )
            .is_err()
    );
    fs::remove_dir(&path).unwrap();
    service.prompt(&record.target(), "hello").unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(wire["params"]["model"], Value::Null);
    assert_eq!(wire["params"]["effort"], Value::Null);
    assert_eq!(
        wire["params"]["input"],
        json!([{"type":"text","text":"hello"}])
    );
}

#[rstest]
#[case(AgentKind::Codex, "available")]
#[case(AgentKind::Pi, "configured/available")]
fn favorites_persist_per_account_without_changing_prompt_configuration(
    #[case] provider: AgentKind,
    #[case] model: &str,
) {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let config = config(root.path(), provider).unwrap();
    let record = service
        .create("binding", "Session", config.clone())
        .unwrap();
    let before = serde_json::to_value(&record.config).unwrap();
    let models = service.favorite_model(&record.target(), model).unwrap();
    assert!(models[0].is_favorite);
    assert_eq!(
        serde_json::to_value(&service.sessions()[0].config).unwrap(),
        before
    );
    assert!(!root.path().join("prompt.json").exists());
    service.shutdown().unwrap();
    drop(service);
    let reopened = NativeAgentService::open(&path).unwrap();
    let mut models = bootty_agents::NativeAgentSession::discover_models(config.clone()).unwrap();
    reopened.mark_model_favorites(&config, &mut models);
    assert!(models[0].is_favorite);
    let mut other_account = config.clone();
    other_account.account_directory = Some(
        root.path()
            .join("other-account")
            .to_string_lossy()
            .into_owned(),
    );
    reopened.mark_model_favorites(&other_account, &mut models);
    assert!(!models[0].is_favorite);
    let mut remote_account = config.clone();
    remote_account.remote = Some(bootty_agents::NativeRemote {
        host: bootty_config::config::RemoteConfig::Ssh(
            bootty_config::config::SshRemoteConfig::for_host("first-host"),
        ),
        daemon: "/captured/bootty-daemon".into(),
    });
    reopened.mark_model_favorites(&remote_account, &mut models);
    assert!(!models[0].is_favorite);
    reopened
        .toggle_model_favorite(&remote_account, model)
        .unwrap();
    let mut other_host = remote_account.clone();
    other_host.remote.as_mut().unwrap().host = bootty_config::config::RemoteConfig::Ssh(
        bootty_config::config::SshRemoteConfig::for_host("second-host"),
    );
    reopened.mark_model_favorites(&other_host, &mut models);
    assert!(!models[0].is_favorite);
    reopened.mark_model_favorites(&remote_account, &mut models);
    assert!(models[0].is_favorite);
    reopened.toggle_model_favorite(&config, model).unwrap();
    reopened.mark_model_favorites(&config, &mut models);
    assert!(!models[0].is_favorite);
}

#[rstest]
fn a_failed_favorite_commit_does_not_publish_the_star() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let config = config(root.path(), AgentKind::Codex).unwrap();
    let record = service
        .create("binding", "Session", config.clone())
        .unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        service
            .favorite_model(&record.target(), "available")
            .is_err()
    );
    let mut models = service.models(&record.target()).unwrap();
    service.mark_model_favorites(&config, &mut models);
    assert!(!models[0].is_favorite);
    fs::remove_dir(&path).unwrap();
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Pi)]
fn completion_discovery_reads_the_project_catalog_without_a_turn(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    let catalog = bootty_agents::NativeAgentSession::discover_completions(
        config(root.path(), provider).unwrap(),
    )
    .unwrap();
    let skill = catalog
        .options
        .iter()
        .find(|option| option.kind == bootty_agents::NativeCompletionKind::Skill)
        .unwrap();
    assert_eq!(skill.name, "project-skill");
    assert_eq!(skill.scope.as_deref(), Some("project"));
    assert_eq!(
        skill.path.as_deref(),
        fs::canonicalize(root.path())
            .unwrap()
            .join("SKILL.md")
            .to_str()
    );
    assert!(
        !catalog
            .options
            .iter()
            .any(|option| option.name == "disabled")
    );
    assert!(catalog.options.iter().any(|option| option.name == "compact"
        && option.kind == bootty_agents::NativeCompletionKind::Command));
    if provider == AgentKind::Pi {
        assert!(catalog.options.iter().any(|option| option.name == "review"
            && option.kind == bootty_agents::NativeCompletionKind::Command));
    }
    assert!(!root.path().join("thread-started").exists());
    assert!(!root.path().join("prompt.json").exists());
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Pi)]
fn advertised_skill_chips_use_the_provider_protocol(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create("binding", "Session", config(root.path(), provider).unwrap())
        .unwrap();
    service
        .prompt(
            &record.target(),
            "Use $project-skill\nKeep $unknown literal. $project-skill",
        )
        .unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    if provider == AgentKind::Codex {
        let input = wire["params"]["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[1]["type"], "skill");
        assert_eq!(input[1]["name"], "project-skill");
        assert_eq!(
            input[1]["path"],
            fs::canonicalize(root.path())
                .unwrap()
                .join("SKILL.md")
                .to_string_lossy()
                .as_ref()
        );
    } else {
        assert_eq!(
            wire["params"]["message"],
            "/skill:project-skill Use \nKeep $unknown literal."
        );
    }
    service.shutdown().unwrap();
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Pi)]
fn skill_text_in_attached_reference_context_does_not_execute(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create("binding", "Session", config(root.path(), provider).unwrap())
        .unwrap();
    let authored = "Read this quote";
    let message = format!("{authored}\n\nQuoted content: $project-skill");
    let prompt = bootty_agents::NativePrompt::text(&message)
        .unwrap()
        .with_authored_prefix(authored.len())
        .unwrap();
    service.prompt_input(&record.target(), &prompt).unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    if provider == AgentKind::Codex {
        assert_eq!(wire["params"]["input"].as_array().unwrap().len(), 1);
        assert_eq!(wire["params"]["input"][0]["text"], message);
    } else {
        assert_eq!(wire["params"]["message"], message);
    }
    service.shutdown().unwrap();
}

#[rstest]
fn claude_discovery_keeps_launch_efforts_and_live_controls_offer_only_supported_efforts() {
    let root = TempDir::new().unwrap();
    let config = config(root.path(), AgentKind::Claude).unwrap();
    let launch = bootty_agents::NativeAgentSession::discover_models(config.clone()).unwrap();
    assert!(launch[0].reasoning_efforts.contains(&"max".to_owned()));
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service.create("binding", "Session", config).unwrap();
    assert!(
        !service.models(&record.target()).unwrap()[0]
            .reasoning_efforts
            .contains(&"max".to_owned())
    );
    assert!(
        service
            .configure(
                &record.target(),
                &NativeModelSelection {
                    model: "available".to_owned(),
                    reasoning_effort: Some("max".to_owned()),
                }
            )
            .is_err()
    );
    assert!(!root.path().join("prompt.json").exists());
}

#[rstest]
#[case("medium")]
#[case("high")]
#[case("max")]
fn claude_creation_effort_reaches_the_first_prompt(#[case] effort: &str) {
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let mut config = config(root.path(), AgentKind::Claude).unwrap();
    config.model = Some("available".into());
    config.reasoning_effort = Some(effort.into());
    let record = service.create("binding", "Session", config).unwrap();
    service.prompt(&record.target(), "hello").unwrap();
    service.interrupt(&record.target()).unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(wire["effort"], effort);
    assert_eq!(wire["model"]["id"], "available");
}

#[rstest]
fn codex_catalog_resolves_configured_effort_before_starting_a_conversation() {
    let root = TempDir::new().unwrap();
    let options = bootty_agents::NativeAgentSession::discover_models(
        config(root.path(), AgentKind::Codex).unwrap(),
    )
    .unwrap();
    assert!(options[0].is_default);
    assert_eq!(options[0].default_reasoning_effort.as_deref(), Some("high"));
    assert!(!root.path().join("thread-started").exists());
}

#[rstest]
#[case("high", Some("high"))]
#[case("xhigh", Some("xhigh"))]
#[case("unsupported", None)]
fn claude_catalog_resolves_model_and_account_effort_without_a_placeholder(
    #[case] effort: &str,
    #[case] expected: Option<&str>,
) {
    let root = TempDir::new().unwrap();
    let config = config(root.path(), AgentKind::Claude).unwrap();
    fs::write(root.path().join("claude-catalog.json"), serde_json::to_vec(&json!({"models":[
        {"value":"default","resolvedModel":"claude-opus-5-5","displayName":"Default (recommended)"},
        {"value":"opus","resolvedModel":"claude-opus-5-5","displayName":"Opus","description":"Opus 5.5 · Best for everyday tasks","supportsEffort":true,"supportedEffortLevels":["high","xhigh"]}
    ]})).unwrap()).unwrap();
    fs::write(
        root.path().join("claude-settings.json"),
        serde_json::to_vec(
            &json!({"effective":{"modelSettings":{"claude-opus-5-5":{"effortLevel":effort}}}}),
        )
        .unwrap(),
    )
    .unwrap();
    let models = bootty_agents::NativeAgentSession::discover_models(config).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "opus");
    assert_eq!(models[0].display_name, "Opus 5.5");
    assert!(models[0].is_default);
    assert_eq!(models[0].default_reasoning_effort.as_deref(), expected);
    assert!(!root.path().join("prompt.json").exists());
}
