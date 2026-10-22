use assert_fs::TempDir;
use bootty_agents::{AgentKind, NativeAgentSession, NativeProviderCatalog, NativeSessionConfig};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Value, json};

const PROVIDER: &str = r"#!/usr/bin/env python3
import json, pathlib, sys
for line in sys.stdin:
    request = json.loads(line)
    method = request['request']['subtype']
    if method == 'initialize':
        response = json.loads(pathlib.Path('catalog.json').read_text(encoding='utf-8'))
    elif method == 'get_settings':
        response = {'effective': {}}
    else:
        raise RuntimeError('unexpected discovery request')
    print(json.dumps({'type': 'control_response', 'response': {
        'subtype': 'success', 'request_id': request['request_id'], 'response': response
    }}), flush=True)
";

fn discover(catalog: &Value) -> Result<NativeProviderCatalog, Box<dyn std::error::Error>> {
    let root = TempDir::new()?;
    std::fs::write(
        root.path().join("catalog.json"),
        serde_json::to_vec(catalog)?,
    )?;
    #[cfg(unix)]
    let program = {
        let program = root.path().join("provider");
        bootty_test_support::write_executable(&program, PROVIDER)?;
        program
    };
    #[cfg(windows)]
    let program = {
        std::fs::write(root.path().join("provider.py"), PROVIDER)?;
        let program = root.path().join("provider.cmd");
        std::fs::write(
            &program,
            "@echo off\r\n@python -u \"%~dp0provider.py\" %*\r\n",
        )?;
        program
    };
    let mut config = NativeSessionConfig::new(AgentKind::Claude, root.path());
    config.program = program.to_string_lossy().into_owned();
    config.account_directory = Some(root.path().join("account").to_string_lossy().into_owned());
    NativeAgentSession::discover_catalog(config).map_err(Into::into)
}

// Model-only metadata from Claude Code 2.1.168's initialize response. No account data.
fn advertised_catalog() -> Value {
    json!({"models": [
        {
            "value": "default", "displayName": "Default (recommended)",
            "description": "Opus 4.8 with 1M context · Most capable for complex work",
            "supportsEffort": true,
            "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"],
            "supportsAdaptiveThinking": true, "supportsFastMode": true, "supportsAutoMode": true
        },
        {
            "value": "sonnet", "displayName": "Sonnet",
            "description": "Sonnet 4.6 · Best for everyday tasks",
            "supportsEffort": true, "supportedEffortLevels": ["low", "medium", "high", "max"],
            "supportsAdaptiveThinking": true, "supportsAutoMode": true
        },
        {
            "value": "sonnet[1m]", "displayName": "Sonnet (1M context)",
            "description": "Sonnet 4.6 with 1M context · Draws from usage credits · $3/$15 per Mtok",
            "supportsEffort": true, "supportedEffortLevels": ["low", "medium", "high", "max"],
            "supportsAdaptiveThinking": true, "supportsAutoMode": true
        },
        {"value": "haiku", "displayName": "Haiku", "description": "Haiku 4.5 · Fastest for quick answers"}
    ]})
}

#[test]
fn current_advertised_names_are_not_replaced_by_descriptive_prose() {
    let response = json!({"models": [
        {"value":"default", "displayName":"Default (recommended)", "description":"Opus 5.5 · Best for everyday, complex tasks", "resolvedModel":"claude-opus-5-5"},
        {"value":"opus", "displayName":"Opus 5.5", "description":"For complex work and everyday tasks", "resolvedModel":"claude-opus-5-5"},
        {"value":"sonnet", "displayName":"Sonnet 5.5", "description":"Most efficient for simpler tasks", "resolvedModel":"claude-sonnet-5-5"}
    ]});
    let catalog = discover(&response).unwrap();
    assert_eq!(catalog.models.len(), 2);
    assert_eq!(catalog.models[0].id, "opus");
    assert_eq!(catalog.models[0].display_name, "Opus 5.5");
    assert!(catalog.models[0].is_default);
    assert_eq!(catalog.models[1].display_name, "Sonnet 5.5");
}

#[rstest]
#[case(None)]
#[case(Some(Value::Null))]
fn unresolved_default_keeps_its_advertised_selector_label_and_capabilities(
    #[case] resolved: Option<Value>,
) {
    let mut response = advertised_catalog();
    if let Some(resolved) = resolved {
        response["models"][0]["resolvedModel"] = resolved;
    }
    let catalog = discover(&response).unwrap();
    assert_eq!(
        catalog
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["default", "sonnet", "sonnet[1m]", "haiku"]
    );
    let default = &catalog.models[0];
    assert_eq!(default.display_name, "Opus 4.8 with 1M context");
    assert!(default.is_default);
    assert_eq!(
        catalog
            .models
            .iter()
            .filter(|model| model.is_default)
            .count(),
        1
    );
    assert_eq!(
        default.reasoning_efforts,
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert!(default.supports_fast_mode);
    assert!(!catalog.models[1].supports_fast_mode);
    assert_eq!(catalog.models[3].reasoning_efforts, Vec::<String>::new());
}

#[test]
fn resolved_default_still_deduplicates_the_advertised_alias() {
    let response = json!({"models": [
        {"value":"default", "displayName":"Default (recommended)", "resolvedModel":"claude-opus-4-8"},
        {"value":"opus", "displayName":"Opus 4.8", "resolvedModel":"claude-opus-4-8"}
    ]});
    let catalog = discover(&response).unwrap();
    assert_eq!(catalog.models.len(), 1);
    assert_eq!(catalog.models[0].id, "opus");
    assert_eq!(catalog.models[0].display_name, "Opus 4.8");
    assert!(catalog.models[0].is_default);
}

#[rstest]
#[case("value", json!(""))]
#[case("displayName", Value::Null)]
#[case("displayName", json!("invalid\nlabel"))]
#[case("resolvedModel", json!(""))]
#[case("resolvedModel", json!(42))]
#[case("resolvedModel", json!("invalid\nmodel"))]
fn malformed_advertised_selectors_remain_rejected(#[case] field: &str, #[case] value: Value) {
    let mut response = advertised_catalog();
    response["models"][0][field] = value;
    assert_eq!(
        discover(&response).unwrap_err().to_string(),
        "Provider returned an invalid model selector"
    );
}
