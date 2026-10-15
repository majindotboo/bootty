#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use assert_fs::TempDir;
use bootty_agents::{AgentKind, NativeAgentService, NativeModelSelection, NativeSessionConfig};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

/// One fake for Codex, Claude and Pi, picked by the arguments each is launched with.
const PROVIDER: &str = r"#!/usr/bin/env python3
import json,os,sys
pi='--mode' in sys.argv
claude='--input-format' in sys.argv
identity=sys.argv[sys.argv.index('--resume' if '--resume' in sys.argv else '--session-id')+1] if claude else None
model={'provider':'configured','id':'available','name':'Available','reasoning':True,'thinkingLevelMap':{'xhigh':None,'max':'high'}}
effort=sys.argv[sys.argv.index('--effort')+1] if '--effort' in sys.argv else 'medium'
goal=None
fast=any(json.loads(sys.argv[i+1]).get('fastMode',False) for i,arg in enumerate(sys.argv[:-1]) if arg=='--settings')
if claude:
 with open('argv.json','w') as output: json.dump(sys.argv[1:],output)
def emit(value): print(json.dumps(value),flush=True)
def finish(): emit({'type':'result','subtype':'success','session_id':identity,'uuid':'result-uuid','is_error':False,'terminal_reason':'completed','result':'Done','errors':[]})
def reply(value,data):
 if claude: emit({'type':'control_response','response':{'subtype':'success','request_id':value['request_id'],'response':data}})
 elif pi: emit({'id':value['id'],'type':'response','command':value['type'],'success':True,'data':data})
 else: emit({'id':value['id'],'result':data})
for line in sys.stdin:
 value=json.loads(line)
 if claude and value['type']=='user':
  with open('prompt.json','w') as output: json.dump({'params':value['message'],'model':model,'effort':effort,'fast':fast},output)
  emit({'type':'system','subtype':'init','session_id':identity,'cwd':os.getcwd()})
  emit(dict(value,isReplay=True))
  # Observed from Claude 2.1.299: a plan arrives as an ExitPlanMode approval.
  if value['message']['content']=='plan': emit({'type':'control_request','request_id':'plan-id','request':{'subtype':'can_use_tool','tool_name':'ExitPlanMode','tool_use_id':'plan-tool','input':{'plan':'1. Write it'},'requires_user_interaction':True}})
  elif value['message']['content']=='finish': finish()
  continue
 if claude and value['type']=='control_response':
  finish()
  continue
 method=value['request']['subtype'] if claude else value.get('type') if pi else value.get('method')
 params=value['request'] if claude else value if pi else value.get('params',{})
 if method=='initialized': continue
 if method=='initialize':
  catalog={'models':[{'value':'available','resolvedModel':'claude-test','displayName':'Available','supportsFastMode':True,'supportsEffort':True,'supportedEffortLevels':['medium','high','max']}]}
  if claude and os.path.exists('claude-catalog.json'):
   with open('claude-catalog.json') as source: catalog=json.load(source)
  reply(value,catalog if claude else {})
 elif method in ['thread/start','thread/resume']:
  with open('thread-started','w') as output: output.write(method)
  reply(value,{'thread':{'id':'thread','turns':[]}})
 elif method=='skills/list': reply(value,{'data':[{'cwd':os.getcwd(),'skills':[{'name':'project-skill','path':os.path.join(os.getcwd(),'SKILL.md'),'enabled':True,'description':'Project work','scope':'project'},{'name':'disabled','path':'/disabled','enabled':False}]}]})
 elif method=='app/installed':
  apps=[{'id':'mail','runtimeName':'Mail','enabled':True,'callable':True},{'id':'disabled-app','enabled':False,'callable':False},{'id':'uncallable','enabled':True,'callable':False}]
  if os.path.exists('apps.json'):
   with open('apps.json') as source: apps=json.load(source)
  reply(value,{'apps':apps})
 elif method=='app/read': reply(value,{'apps':[{'id':'mail','name':'Mail','description':'Read and manage mail'}]})
 elif method=='app/list':
  if os.path.exists('apps.json'):
   with open('apps.json') as source: apps=json.load(source)
  else: apps=[{'id':'mail','name':'Mail','isAccessible':True,'isEnabled':True},{'id':'disabled-app','name':'Disabled','isAccessible':True,'isEnabled':False},{'id':'inaccessible','name':'Private','isAccessible':False,'isEnabled':True}]
  reply(value,{'data':apps,'nextCursor':None})
 elif method=='plugin/installed': reply(value,{'marketplaces':[{'name':'market','plugins':[{'id':'review@market','name':'review','installed':True,'enabled':True,'interface':{'displayName':'Code review','shortDescription':'Review changes'}},{'id':'disabled-plugin','installed':True,'enabled':False},{'id':'uninstalled','installed':False,'enabled':True},{'id':'admin-blocked','installed':True,'enabled':True,'availability':'DISABLED_BY_ADMIN'}]}]})
 elif method=='collaborationMode/list': reply(value,{'data':[{'name':'Plan','mode':'plan'}]})
 elif method=='experimentalFeature/list': reply(value,{'data':[{'name':'goals','enabled':True}]})
 elif method=='thread/goal/get': reply(value,{'goal':goal})
 elif method=='thread/goal/set':
  assert params['origin']=='user' and params['threadId']=='thread'
  goal={'objective':params['objective'],'status':'active'}
  reply(value,{'goal':goal})
 elif method=='thread/goal/clear':
  assert params['origin']=='user' and params['threadId']=='thread'
  goal=None
  reply(value,{'cleared':True})

 elif method=='get_commands': reply(value,{'commands':[{'name':'review','description':'Review work','source':'extension'},{'name':'skill:project-skill','source':'skill','description':'Project work','sourceInfo':{'path':os.path.join(os.getcwd(),'SKILL.md'),'scope':'project'}}]})
 elif method=='model/list':
  advertised={'id':'catalog-id','model':'available','displayName':'Available','supportedReasoningEfforts':[{'reasoningEffort':'medium'},{'reasoningEffort':'high'}],'defaultReasoningEffort':'medium','isDefault':True,'serviceTiers':[{'id':'fast'}]}
  if os.path.exists('speed-capabilities.json'):
   del advertised['serviceTiers']
   with open('speed-capabilities.json') as source: advertised.update(json.load(source))
  reply(value,{'data':[advertised],'nextCursor':None})
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
 elif method=='apply_flag_settings':
  effort=params['settings'].get('effortLevel',effort)
  fast=params['settings'].get('fastMode',fast)
  reply(value,{})
 elif method=='interrupt': reply(value,{})
 elif method=='set_permission_mode':
  with open('modes','a') as output: output.write(params['mode']+'\n')
  reply(value,{'mode':params['mode']})
 elif method=='get_settings':
  settings={'effective':{'effortLevel':effort,'fastMode':fast}}
  if os.path.exists('claude-settings.json'):
   with open('claude-settings.json') as source: settings=json.load(source)
  reply(value,settings)
 elif method in ['turn/start','turn/steer','prompt']:
  if method=='turn/start':
   emit({'method':'turn/started','params':{'threadId':'thread','turn':{'id':'turn','status':'inProgress','items':[]}}})
   emit({'method':'item/completed','params':{'threadId':'thread','turnId':'turn','item':{'id':'provider-user','type':'userMessage','content':params['input']}}})
  with open('prompt.json','w') as output: json.dump({'params':params,'model':model,'effort':effort,'fast':fast},output)
  if pi: reply(value,{'disposition':'handled'})
  else: reply(value,{'turn':{'id':'turn'}})
 else: raise Exception(method)
";

fn config(root: &Path, provider: AgentKind) -> std::io::Result<NativeSessionConfig> {
    let script = root.join("provider.py");
    fs::write(&script, PROVIDER)?;
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
#[case(AgentKind::Codex, "available", false)]
#[case(AgentKind::Codex, "available", true)]
#[case(AgentKind::Pi, "configured/available", false)]
#[case(AgentKind::Claude, "available", false)]
#[case(AgentKind::Claude, "available", true)]
fn committed_selection_drives_the_next_provider_prompt(
    #[case] provider: AgentKind,
    #[case] model: &str,
    #[case] fast: bool,
) {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let service = NativeAgentService::open(&path).unwrap();
    let mut launch = config(root.path(), provider).unwrap();
    launch.fast_mode = provider != AgentKind::Pi && !fast;
    let record = service.create("binding", "Session", launch).unwrap();
    let models = service.models(&record.target()).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, model);
    assert!(models[0].reasoning_efforts.contains(&"high".to_owned()));
    if provider == AgentKind::Pi {
        assert!(models[0].reasoning_efforts.contains(&"max".to_owned()));
        assert!(!models[0].reasoning_efforts.contains(&"xhigh".to_owned()));
    }
    let selection = NativeModelSelection {
        plan_mode: None,
        fast_mode: Some(fast),
        model: model.to_owned(),
        reasoning_effort: Some("high".to_owned()),
    };
    service.configure(&record.target(), &selection).unwrap();
    let stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["records"][0]["config"]["model"], model);
    assert_eq!(stored["records"][0]["config"]["fast_mode"], fast);
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
        assert_eq!(
            wire["params"]["serviceTier"],
            if fast { json!("fast") } else { Value::Null }
        );
        assert_eq!(wire["params"].get("approvalPolicy"), None);
        assert_eq!(wire["params"].get("sandboxPolicy"), None);
    } else {
        assert_eq!(wire["effort"], "high");
        assert_eq!(wire["model"]["id"], "available");
    }
    if provider == AgentKind::Claude {
        assert_eq!(wire["fast"], fast);
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
                plan_mode: None,
fast_mode: None,
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
                    plan_mode: None,
                    fast_mode: Some(true),
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

/// Claude's handshake catalog: one skill from the user's setup and one of Claude's own commands.
fn advertise_claude_skill(root: &Path) -> std::io::Result<()> {
    fs::write(
        root.join("claude-catalog.json"),
        json!({
            "models": [{"value": "available", "resolvedModel": "claude-test", "displayName": "Available",
                "supportsEffort": true, "supportedEffortLevels": ["medium", "high", "max"]}],
            "commands": [
                {"name": "project-skill", "description": "Project work", "argumentHint": ""},
                {"name": "compact", "description": "Free up context", "argumentHint": "", "builtin": true}
            ]
        })
        .to_string(),
    )
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Pi)]
#[case(AgentKind::Claude)]
fn advertised_skill_chips_use_the_provider_protocol(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    advertise_claude_skill(root.path()).unwrap();
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
    } else if provider == AgentKind::Pi {
        assert_eq!(
            wire["params"]["message"],
            "/skill:project-skill Use \nKeep $unknown literal."
        );
    } else {
        // Claude runs the skill that opens the last text block; the earlier chip reads as `/name`.
        assert_eq!(
            wire["params"]["content"],
            json!([
                {"type": "text", "text": "Use /project-skill\nKeep $unknown literal."},
                {"type": "text", "text": "/project-skill"}
            ])
        );
        let kinds = service
            .completions(&record.target())
            .unwrap()
            .options
            .into_iter()
            .map(|option| (option.name, option.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                (
                    "project-skill".to_owned(),
                    bootty_agents::NativeCompletionKind::Skill
                ),
                (
                    "compact".to_owned(),
                    bootty_agents::NativeCompletionKind::Command
                ),
            ]
        );
    }
    service.shutdown().unwrap();
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Pi)]
#[case(AgentKind::Claude)]
fn skill_text_in_attached_reference_context_does_not_execute(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    advertise_claude_skill(root.path()).unwrap();
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
    } else if provider == AgentKind::Pi {
        assert_eq!(wire["params"]["message"], message);
    } else {
        assert_eq!(wire["params"]["content"], message);
    }
    service.shutdown().unwrap();
}

/// Claude's Plan mode is its own permission mode. A conversation launches in it, switches when the
/// choice changes, and leaves it when Claude's plan is approved, without telling Claude again.
#[rstest]
fn claude_plan_mode_follows_the_choice_and_ends_when_the_plan_is_approved() {
    let root = TempDir::new().unwrap();
    let mut config = config(root.path(), AgentKind::Claude).unwrap();
    config.plan_mode = Some(true);
    config.model = Some("available".to_owned());
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service.create("binding", "Session", config).unwrap();
    let target = record.target();
    let argv: Vec<String> =
        serde_json::from_slice(&fs::read(root.path().join("argv.json")).unwrap()).unwrap();
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--permission-mode", "plan"])
    );
    let modes = || fs::read_to_string(root.path().join("modes")).unwrap_or_default();
    let wait = |ready: &dyn Fn(&bootty_agents::NativeSessionRecord) -> bool| {
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(5))
            .unwrap();
        while !service
            .sessions()
            .iter()
            .any(|record| record.target() == target && ready(record))
        {
            assert!(std::time::Instant::now() < deadline, "timed out");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };

    service.prompt(&target, "plan").unwrap();
    wait(&|record| !record.snapshot.requests.is_empty());
    service
        .approve_decision(
            &target,
            &record_request(&service, &target),
            bootty_agents::NativeApprovalDecision::AllowOnce,
        )
        .unwrap();
    wait(&|record| {
        record.config.plan_mode == Some(false)
            && record.snapshot.status == bootty_agents::NativeSessionStatus::Idle
    });
    service.prompt(&target, "finish").unwrap();
    assert_eq!(modes(), "", "Claude left Plan mode itself");

    for plan in [true, false] {
        wait(&|record| record.snapshot.status == bootty_agents::NativeSessionStatus::Idle);
        service
            .configure(
                &target,
                &NativeModelSelection {
                    plan_mode: Some(plan),
                    fast_mode: None,
                    model: "available".to_owned(),
                    reasoning_effort: None,
                },
            )
            .unwrap();
        service.prompt(&target, "finish").unwrap();
    }
    assert_eq!(modes(), "plan\ndefault\n");
    service.shutdown().unwrap();
}

fn record_request(service: &NativeAgentService, target: &bootty_control::CommandTarget) -> String {
    service
        .sessions()
        .into_iter()
        .find(|record| record.target() == *target)
        .and_then(|record| {
            record
                .snapshot
                .requests
                .first()
                .map(|request| request.id.clone())
        })
        .unwrap_or_default()
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
                    plan_mode: None,
                    fast_mode: None,
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

#[rstest]
fn provider_references_use_the_advertised_account_catalog() {
    use bootty_agents::{
        NativeAgentSession, NativeCompletionKind, NativePrompt, NativeProviderMention,
    };
    let root = TempDir::new().unwrap();
    let session =
        NativeAgentSession::spawn(config(root.path(), AgentKind::Codex).unwrap()).unwrap();
    let catalog = session.completions().unwrap();
    let options: Vec<_> = catalog
        .options
        .iter()
        .filter(|option| {
            matches!(
                option.kind,
                NativeCompletionKind::App | NativeCompletionKind::Plugin
            )
        })
        .cloned()
        .collect();
    assert_eq!(
        options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        vec!["mail", "review@market"]
    );
    assert!(catalog.plan_mode);
    assert!(catalog.goals);
    let text = "@Mail @Code review";
    let prompt = NativePrompt::text(text)
        .unwrap()
        .with_provider_mentions(vec![
            NativeProviderMention {
                option: options[0].clone(),
                prompt_range: 0..5,
            },
            NativeProviderMention {
                option: options[1].clone(),
                prompt_range: 6..18,
            },
        ])
        .unwrap();
    session.send_prompt_input(&prompt).unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(
        wire["params"]["input"][1],
        json!({"type":"mention","name":"Mail","path":"app://mail"})
    );
    assert_eq!(
        wire["params"]["input"][2],
        json!({"type":"mention","name":"Code review","path":"plugin://review@market"})
    );
    session.stop();
}

#[rstest]
fn a_disabled_app_cannot_be_submitted_using_an_old_chip() {
    use bootty_agents::{
        NativeAgentSession, NativeCompletionKind, NativePrompt, NativeProviderMention,
    };
    let root = TempDir::new().unwrap();
    let session =
        NativeAgentSession::spawn(config(root.path(), AgentKind::Codex).unwrap()).unwrap();
    let option = session
        .completions()
        .unwrap()
        .options
        .into_iter()
        .find(|option| option.kind == NativeCompletionKind::App)
        .unwrap();
    fs::write(root.path().join("apps.json"), "[]").unwrap();
    let prompt = NativePrompt::text("@Mail")
        .unwrap()
        .with_provider_mentions(vec![NativeProviderMention {
            option,
            prompt_range: 0..5,
        }])
        .unwrap();
    assert!(
        session
            .send_prompt_input(&prompt)
            .unwrap_err()
            .contains("no longer available")
    );
    assert!(!root.path().join("prompt.json").exists());
    session.stop();
}

#[rstest]
fn queued_provider_references_restore_as_typed_mentions() {
    use bootty_agents::{NativeCompletionKind, NativeProviderMention};
    let root = TempDir::new().unwrap();
    let service = NativeAgentService::open(root.path().join("store.json")).unwrap();
    let record = service
        .create(
            "binding",
            "Mention queue",
            config(root.path(), AgentKind::Codex).unwrap(),
        )
        .unwrap();
    let target = record.target();
    let option = service
        .completions(&target)
        .unwrap()
        .options
        .into_iter()
        .find(|option| option.kind == NativeCompletionKind::App)
        .unwrap();
    service.prompt(&target, "start working").unwrap();
    let prompt = bootty_agents::NativePrompt::text("@Mail")
        .unwrap()
        .with_provider_mentions(vec![NativeProviderMention {
            option,
            prompt_range: 0..5,
        }])
        .unwrap();
    let queued = service.queue_input(&target, &prompt).unwrap();
    service.steer_followup(&target, &queued.id).unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(
        wire["params"]["input"][1],
        json!({"type":"mention","name":"Mail","path":"app://mail"})
    );
    assert_eq!(
        service
            .sessions()
            .iter()
            .find(|record| record.target() == target)
            .unwrap()
            .queued_messages,
        Vec::<bootty_agents::NativeQueuedMessage>::new()
    );
    service.stop(&target).unwrap();
}

#[rstest]
#[case(true)]
#[case(false)]
fn plan_mode_is_persisted_and_sent_as_a_provider_collaboration_mode(#[case] plan: bool) {
    let root = TempDir::new().unwrap();
    let path = root.path().join("store.json");
    let service = NativeAgentService::open(&path).unwrap();
    let record = service
        .create(
            "binding",
            "Planning",
            config(root.path(), AgentKind::Codex).unwrap(),
        )
        .unwrap();
    let target = record.target();
    let selection = NativeModelSelection {
        plan_mode: Some(plan),
        fast_mode: None,
        model: "available".into(),
        reasoning_effort: Some("high".into()),
    };
    service.configure(&target, &selection).unwrap();
    service.prompt(&target, "next turn").unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(
        wire["params"]["collaborationMode"],
        json!({"mode":if plan {"plan"} else {"default"},"settings":{"model":"available","reasoning_effort":"high","developer_instructions":null}})
    );
    let persisted: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(persisted["records"][0]["config"]["plan_mode"], plan);
    service.stop(&target).unwrap();
}

#[rstest]
fn implementing_a_plan_runs_its_turn_outside_plan_mode() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("store.json");
    let service = NativeAgentService::open(&path).unwrap();
    let record = service
        .create(
            "binding",
            "Planning",
            config(root.path(), AgentKind::Codex).unwrap(),
        )
        .unwrap();
    let target = record.target();
    let selection = NativeModelSelection {
        plan_mode: Some(true),
        fast_mode: None,
        model: "available".into(),
        reasoning_effort: None,
    };
    service.configure(&target, &selection).unwrap();
    service.implement_plan(&target).unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(
        (
            &wire["params"]["collaborationMode"]["mode"],
            &wire["params"]["input"][0]["text"]
        ),
        (&json!("default"), &json!("Implement the plan."))
    );
    let persisted: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(persisted["records"][0]["config"]["plan_mode"], false);
    service.stop(&target).unwrap();
}

#[rstest]
fn user_goal_controls_update_and_clear_the_provider_goal_without_sending_a_turn() {
    let root = TempDir::new().unwrap();
    let session =
        bootty_agents::NativeAgentSession::spawn(config(root.path(), AgentKind::Codex).unwrap())
            .unwrap();
    session.set_goal("Improve the composer").unwrap();
    assert_eq!(
        session.completions().unwrap().goal.as_deref(),
        Some("Improve the composer")
    );
    session.set_goal("").unwrap();
    assert_eq!(session.completions().unwrap().goal, None);
    assert!(!root.path().join("prompt.json").exists());
    session.stop();
}

#[rstest]
#[case::codex_fast(AgentKind::Codex, json!({"serviceTiers":[{"id":"fast"}]}), true)]
#[case::codex_legacy(AgentKind::Codex, json!({"additionalSpeedTiers":["fast"]}), true)]
#[case::codex_standard(AgentKind::Codex, json!({"serviceTiers":[{"id":"default"}]}), false)]
#[case::codex_unknown(AgentKind::Codex, json!({}), false)]
#[case::claude_fast(AgentKind::Claude, json!({"supportsFastMode":true}), true)]
#[case::claude_standard(AgentKind::Claude, json!({}), false)]
fn speed_controls_follow_the_captured_models_advertised_capabilities(
    #[case] provider: AgentKind,
    #[case] fields: Value,
    #[case] expected: bool,
) {
    let root = TempDir::new().unwrap();
    if provider == AgentKind::Claude {
        let mut model =
            json!({"value":"available","resolvedModel":"claude-test","displayName":"Available"});
        model
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        fs::write(
            root.path().join("claude-catalog.json"),
            json!({"models":[model]}).to_string(),
        )
        .unwrap();
    } else {
        fs::write(
            root.path().join("speed-capabilities.json"),
            fields.to_string(),
        )
        .unwrap();
    }
    let service = NativeAgentService::open(root.path().join("native.json")).unwrap();
    let record = service
        .create(
            "binding",
            "Speed capabilities",
            config(root.path(), provider).unwrap(),
        )
        .unwrap();
    let catalog = service.models(&record.target()).unwrap();
    assert_eq!(catalog[0].supports_fast_mode, expected);
    let selection = NativeModelSelection {
        plan_mode: None,
        fast_mode: Some(true),
        model: "available".into(),
        reasoning_effort: None,
    };
    assert_eq!(
        service.configure(&record.target(), &selection).is_ok(),
        expected
    );
    assert_eq!(service.sessions()[0].config.fast_mode, expected);
    service.shutdown().unwrap();
}
