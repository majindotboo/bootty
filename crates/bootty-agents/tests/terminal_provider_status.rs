#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt as _};

use bootty_agents::{
    AgentKind, TerminalAgentService, terminal_account_status_in, terminal_provider_status,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

const ACCOUNT_DIRECTORY: &str = "/selected account/with spaces";

fn executable(directory: &assert_fs::TempDir, script: &str) -> std::io::Result<String> {
    let path = directory.path().join("provider");
    fs::write(&path, format!("#!/bin/sh\n{script}\n"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    path.to_str().map(str::to_owned).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "fixture provider path is not UTF-8",
        )
    })
}

fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn provider_script(provider: AgentKind, response: &Value, exit: i32) -> String {
    let scope = match provider {
        AgentKind::Claude => {
            "[ \"$CLAUDE_CONFIG_DIR\" = '/selected account/with spaces' ] || exit 9\n[ \"$*\" = 'auth status --json' ] || exit 9"
        }
        AgentKind::Pi => {
            "[ \"$PI_CODING_AGENT_DIR\" = '/selected account/with spaces' ] || exit 9\n[ \"$*\" = 'auth check --provider openai-codex --json --no-refresh' ] || exit 9"
        }
        AgentKind::Codex => {
            r#"[ "$CODEX_HOME" = '/selected account/with spaces' ] || exit 9
[ "$*" = 'app-server --listen stdio://' ] || exit 9
IFS= read -r initialize
case "$initialize" in *'"method":"initialize"'*) ;; *) exit 9 ;; esac
printf '%s\n' '{"id":1,"result":{}}'
IFS= read -r initialized
case "$initialized" in *'"method":"initialized"'*) ;; *) exit 9 ;; esac
IFS= read -r account
case "$account" in *'"method":"account/read"'*'"refreshToken":false'*) ;; *) exit 9 ;; esac
printf '%s\n' '{"method":"account/updated","params":{"accessToken":"discarded-notification"}}'"#
        }
    };
    let response = if provider == AgentKind::Codex {
        json!({"id":2,"result":response})
    } else {
        response.clone()
    };
    format!(
        "if [ \"$*\" = '--version' ]; then printf '%s\\n' 'test-provider 1'; exit 0; fi\n{scope}\nprintf '%s\\n' {}\nexit {exit}",
        quoted(&response.to_string())
    )
}

#[rstest]
#[case(AgentKind::Codex, json!({"account":{"type":"chatgpt","email":"user@example.com","planType":"pro","accessToken":"secret"},"requiresOpenaiAuth":true}), 0, Some("user@example.com"), Some("chatgpt"), Some("pro"))]
#[case(AgentKind::Codex, json!({"account":{"type":"apiKey","apiKey":"secret","planType":"pro"},"requiresOpenaiAuth":true}), 0, None, Some("api_key"), None)]
#[case(AgentKind::Claude, json!({"loggedIn":true,"authMethod":"claude.ai","email":"user@example.com","subscriptionType":"max","accessToken":"secret"}), 0, Some("user@example.com"), Some("claude.ai"), Some("max"))]
#[case(AgentKind::Claude, json!({"loggedIn":true,"authMethod":"api_key","subscriptionType":"max","apiKey":"secret"}), 0, None, Some("api_key"), None)]
#[case(AgentKind::Claude, json!({"loggedIn":true,"authMethod":"oauth_token","orgName":"Example organization","subscriptionType":null}), 0, Some("Example organization"), Some("oauth_token"), None)]
#[case(AgentKind::Pi, json!({"status":"ready","provider":"openai-codex","authType":"oauth","credentials":"secret","subscriptionType":"pro"}), 0, None, Some("oauth"), None)]
fn provider_checks_use_exact_scope_and_only_supported_account_metadata(
    #[case] provider: AgentKind,
    #[case] response: Value,
    #[case] exit: i32,
    #[case] account: Option<&str>,
    #[case] method: Option<&str>,
    #[case] subscription: Option<&str>,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let program = executable(&directory, &provider_script(provider, &response, exit)).unwrap();
    let status = terminal_provider_status(
        provider,
        &program,
        Some(ACCOUNT_DIRECTORY),
        Some("openai-codex"),
    );
    assert_eq!(status.authenticated, Some(true));
    assert_eq!(status.account.as_deref(), account);
    assert_eq!(status.auth_method.as_deref(), method);
    assert_eq!(status.subscription.as_deref(), subscription);
    assert_eq!(status.version.as_deref(), Some("test-provider 1"));
    assert!(!serde_json::to_string(&status).unwrap().contains("secret"));
}

#[rstest]
#[case(AgentKind::Codex, json!({"account":null,"requiresOpenaiAuth":true}), 0, Some(false))]
#[case(AgentKind::Codex, json!({"account":null,"requiresOpenaiAuth":false}), 0, None)]
#[case(AgentKind::Codex, json!({"account":{"type":"amazonBedrock","credentialSource":"awsManaged"},"requiresOpenaiAuth":false}), 0, None)]
#[case(AgentKind::Claude, json!({"loggedIn":false,"authMethod":"none","email":"stale@example.com","subscriptionType":"max"}), 1, Some(false))]
#[case(AgentKind::Pi, json!({"status":"not_ready","provider":"openai-codex","reason":"credentials_not_configured"}), 1, Some(false))]
#[case(AgentKind::Pi, json!({"status":"not_ready","provider":"openai-codex","reason":"provider_not_found"}), 1, None)]
#[case(AgentKind::Pi, json!({"status":"invalid","provider":"openai-codex","reason":"invalid_state"}), 2, None)]
#[case(AgentKind::Pi, json!({"status":"ready","provider":"another-provider","authType":"oauth"}), 0, None)]
#[case(AgentKind::Claude, json!({"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max"}), 1, None)]
#[case(AgentKind::Codex, json!({"account":{"type":"futureAuth","planType":"pro"}}), 0, None)]
fn signed_out_is_distinct_from_unavailable_or_unverified(
    #[case] provider: AgentKind,
    #[case] response: Value,
    #[case] exit: i32,
    #[case] authenticated: Option<bool>,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let program = executable(&directory, &provider_script(provider, &response, exit)).unwrap();
    let status = terminal_provider_status(
        provider,
        &program,
        Some(ACCOUNT_DIRECTORY),
        Some("openai-codex"),
    );
    assert_eq!(status.authenticated, authenticated);
    assert_eq!(status.account, None);
    assert_eq!(status.subscription, None);
    if authenticated.is_none() {
        assert!(status.message.is_some());
    }
}

#[rstest]
#[case("printf '%s' '{bad secret-response'")]
#[case("printf '%s' '{\"id\":1,\"error\":{\"message\":\"secret-response\"}}'")]
#[case("exit 1")]
fn codex_transport_errors_do_not_publish_raw_response(#[case] script: &str) {
    let directory = assert_fs::TempDir::new().unwrap();
    let program = executable(&directory, script).unwrap();
    let error = terminal_account_status_in(AgentKind::Codex, &program, None, None).unwrap_err();
    assert!(!error.contains("secret-response"));
}

#[rstest]
fn account_response_and_label_bounds_preserve_readiness_without_unsafe_labels() {
    let directory = assert_fs::TempDir::new().unwrap();
    let response = json!({"loggedIn":true,"authMethod":"claude.ai","email":"x".repeat(257),"subscriptionType":"max\nsecret"});
    let program = executable(
        &directory,
        &provider_script(AgentKind::Claude, &response, 0),
    )
    .unwrap();
    let status =
        terminal_account_status_in(AgentKind::Claude, &program, None, Some(ACCOUNT_DIRECTORY))
            .unwrap();
    assert_eq!(status.authenticated, Some(true));
    assert_eq!(status.account, None);
    assert_eq!(status.subscription, None);
    let response = json!({"loggedIn":true,"unused":"x".repeat(64 * 1024)});
    let program = executable(
        &directory,
        &provider_script(AgentKind::Claude, &response, 0),
    )
    .unwrap();
    assert!(
        terminal_account_status_in(AgentKind::Claude, &program, None, Some(ACCOUNT_DIRECTORY))
            .is_err()
    );
}

#[rstest]
#[case(None, json!({"defaultProvider":"anthropic","defaultModel":"selected-model"}), "anthropic", true)]
#[case(Some("openai-codex"), json!({"defaultProvider":"anthropic","defaultModel":"selected-model"}), "openai-codex", true)]
#[case(None, json!({"defaultProvider":"anthropic","defaultModel":"selected-model"}), "openai-codex", false)]
#[case(None, json!({"defaultProvider":"anthropic"}), "anthropic", false)]
#[case(None, json!({"defaultProvider":"-invalid","defaultModel":"selected-model"}), "anthropic", false)]
fn pi_selected_account_defaults_and_explicit_provider_stay_in_scope(
    #[case] explicit: Option<&str>,
    #[case] settings: Value,
    #[case] reported_provider: &str,
    #[case] ready: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let account = directory.path().join("custom account");
    fs::create_dir(&account).unwrap();
    fs::write(account.join("settings.json"), settings.to_string()).unwrap();
    let account_directory = account.to_str().unwrap();
    let selected = explicit.unwrap_or("anthropic");
    let model = if explicit.is_none() {
        " --model selected-model"
    } else {
        ""
    };
    let arguments = format!("auth check --provider {selected}{model} --json --no-refresh");
    let response = json!({"status":"ready","provider":reported_provider,"authType":"oauth"});
    let script = format!(
        "[ \"$PI_CODING_AGENT_DIR\" = {} ] || exit 9\n[ \"$*\" = {} ] || exit 9\nprintf '%s\\n' {}",
        quoted(account_directory),
        quoted(&arguments),
        quoted(&response.to_string())
    );
    let program = executable(&directory, &script).unwrap();
    let status =
        terminal_account_status_in(AgentKind::Pi, &program, explicit, Some(account_directory));
    assert_eq!(status.is_ok(), ready);
    if let Ok(status) = status {
        assert_eq!(status.authenticated, Some(true));
        assert!(status.detail.unwrap().contains(selected));
    }
}

#[rstest]
fn pi_explicit_provider_does_not_read_or_require_default_settings() {
    let directory = assert_fs::TempDir::new().unwrap();
    fs::write(
        directory.path().join("settings.json"),
        "malformed private settings",
    )
    .unwrap();
    let program = executable(
        &directory,
        "printf '%s' '{\"status\":\"ready\",\"provider\":\"openai-codex\",\"authType\":\"oauth\"}'",
    )
    .unwrap();
    let account_directory = directory.path().to_str().unwrap();
    let status = terminal_account_status_in(
        AgentKind::Pi,
        &program,
        Some("openai-codex"),
        Some(account_directory),
    )
    .unwrap();
    assert_eq!(status.authenticated, Some(true));
    assert!(
        terminal_account_status_in(AgentKind::Pi, &program, None, Some(account_directory)).is_err()
    );
    fs::write(
        directory.path().join("settings.json"),
        "x".repeat(1024 * 1024 + 1),
    )
    .unwrap();
    assert!(
        terminal_account_status_in(AgentKind::Pi, &program, None, Some(account_directory)).is_err()
    );
}

#[rstest]
fn pi_defaults_use_provider_environment_and_home() {
    // Each subprocess owns its environment; no unsafe process-global environment mutation.
    if let Ok(program) = std::env::var("BOOTTY_PI_STATUS_TEST_PROGRAM") {
        let status = terminal_account_status_in(AgentKind::Pi, &program, None, None).unwrap();
        assert_eq!(status.authenticated, Some(true));
        assert!(status.detail.unwrap().contains("selected-provider"));
        return;
    }
    let directory = assert_fs::TempDir::new().unwrap();
    for inherited_directory in [true, false] {
        let account = if inherited_directory {
            directory.path().join("inherited account")
        } else {
            directory.path().join(".pi/agent")
        };
        fs::create_dir_all(&account).unwrap();
        fs::write(
            account.join("settings.json"),
            json!({"defaultProvider":"selected-provider","defaultModel":"selected-model"})
                .to_string(),
        )
        .unwrap();
        let account_directory = account.to_str().unwrap();
        let script = format!(
            "[ \"$PI_CODING_AGENT_DIR\" = {} ] || exit 9\n[ \"$*\" = 'auth check --provider selected-provider --model selected-model --json --no-refresh' ] || exit 9\nprintf '%s' '{{\"status\":\"ready\",\"provider\":\"selected-provider\",\"authType\":\"oauth\"}}'",
            quoted(account_directory)
        );
        let program = executable(&directory, &script).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "pi_defaults_use_provider_environment_and_home",
                "--test-threads=1",
            ])
            .env("BOOTTY_PI_STATUS_TEST_PROGRAM", &program)
            .env("HOME", directory.path());
        if inherited_directory {
            child.env("PI_CODING_AGENT_DIR", &account);
        } else {
            child.env_remove("PI_CODING_AGENT_DIR");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[rstest]
fn pi_default_cache_never_adopts_a_different_account_directory() {
    let directory = assert_fs::TempDir::new().unwrap();
    let first = directory.path().join("first");
    let other = directory.path().join("other");
    for (account, provider) in [(&first, "first-provider"), (&other, "other-provider")] {
        fs::create_dir(account).unwrap();
        fs::write(
            account.join("settings.json"),
            json!({"defaultProvider":provider,"defaultModel":"selected-model"}).to_string(),
        )
        .unwrap();
    }
    let program = executable(
        &directory,
        "printf '%s' '{\"status\":\"ready\",\"provider\":\"first-provider\",\"authType\":\"oauth\"}'",
    )
    .unwrap();
    let service = TerminalAgentService::open(directory.path().join("catalog.json")).unwrap();
    let status = service.inspect_provider(AgentKind::Pi, &program, first.to_str(), None);
    assert_eq!(status.authenticated, Some(true));
    assert!(
        service
            .provider_status(AgentKind::Pi, &program, other.to_str(), None)
            .is_none()
    );
    assert!(
        service
            .provider_status(
                AgentKind::Pi,
                &program,
                first.to_str(),
                Some("other-provider")
            )
            .is_none()
    );
    assert!(terminal_account_status_in(AgentKind::Pi, &program, None, other.to_str()).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]
    #[test]
    fn cached_status_never_crosses_account_or_model_provider_scope(other in "[a-z]{1,20}") {
        let directory = assert_fs::TempDir::new().unwrap();
        let service = TerminalAgentService::open(directory.path().join("catalog.json")).unwrap();
        let response = json!({"status":"ready","provider":"openai-codex","authType":"oauth"});
        let program = executable(&directory, &provider_script(AgentKind::Pi, &response, 0))
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        service.inspect_provider(AgentKind::Pi, &program, Some(ACCOUNT_DIRECTORY), Some("openai-codex"));
        prop_assert!(service.provider_status(AgentKind::Pi, &program, Some(ACCOUNT_DIRECTORY), Some("openai-codex")).is_some());
        prop_assert!(service.provider_status(AgentKind::Pi, &program, Some(&other), Some("openai-codex")).is_none());
        prop_assert!(service.provider_status(AgentKind::Pi, &program, Some(ACCOUNT_DIRECTORY), Some(&other)).is_none());
        prop_assert!(service.provider_status(AgentKind::Pi, &other, Some(ACCOUNT_DIRECTORY), Some("openai-codex")).is_none());
        prop_assert!(service.provider_status(AgentKind::Pi, &program, None, Some("openai-codex")).is_none());
    }
}

#[rstest]
#[case(vec!["--model", "openai-codex/gpt-selected"], "auth check --model openai-codex/gpt-selected --json --no-refresh", "openai-codex", true)]
#[case(vec!["--provider", "anthropic", "--model", "claude-selected"], "auth check --provider anthropic --model claude-selected --json --no-refresh", "anthropic", true)]
#[case(vec!["--provider", "anthropic", "--model", "other/selected"], "auth check --provider anthropic --model other/selected --json --no-refresh", "other", false)]
#[case(vec!["--provider", "openrouter", "--model", "anthropic/claude-selected"], "auth check --provider openrouter --model anthropic/claude-selected --json --no-refresh", "openrouter", true)]
#[case(vec!["--provider", "older", "--provider", "openai-codex", "--model", "older", "--model", "gpt-selected"], "auth check --provider openai-codex --model gpt-selected --json --no-refresh", "openai-codex", true)]
#[case(vec!["--model", "OpenAI-Codex/gpt-selected"], "auth check --model OpenAI-Codex/gpt-selected --json --no-refresh", "openai-codex", true)]
fn pi_launch_selectors_delegate_exact_model_and_provider_without_default_fallback(
    #[case] arguments: Vec<&str>,
    #[case] expected_command: &str,
    #[case] reported_provider: &str,
    #[case] ready: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let account = directory.path().join("selected-account");
    fs::create_dir(&account).unwrap();
    fs::write(
        account.join("settings.json"),
        "malformed unrelated defaults",
    )
    .unwrap();
    let account = account.to_str().unwrap();
    let arguments = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
    let selector = bootty_agents::PiAccountSelector::from_arguments(&arguments)
        .unwrap()
        .unwrap();
    let response = json!({"status":"ready","provider":reported_provider,"authType":"oauth","credentials":"secret","subscriptionType":"invented-plan"});
    let script = format!(
        "[ \"$PI_CODING_AGENT_DIR\" = {} ] || exit 9\n[ \"$*\" = {} ] || exit 9\nprintf '%s' {}",
        quoted(account),
        quoted(expected_command),
        quoted(&response.to_string())
    );
    let program = executable(&directory, &script).unwrap();
    let result = bootty_agents::terminal_account_status_with_pi_selector_in(
        AgentKind::Pi,
        &program,
        Some(&selector),
        Some(account),
    );
    assert_eq!(result.is_ok(), ready);
    if let Ok(status) = result {
        assert_eq!(status.authenticated, Some(true));
        assert_eq!(status.auth_method.as_deref(), Some("oauth"));
        assert_eq!(status.subscription, None);
        assert!(!serde_json::to_string(&status).unwrap().contains("secret"));
    }
}

#[rstest]
#[case(vec!["--model", "unqualified"])]
#[case(vec!["--model", "provider/"])]
#[case(vec!["--model"])]
#[case(vec!["--provider", "--model"])]
#[case(vec!["--model=provider/id"])]
#[case(vec!["--provider=provider"])]
fn pi_ambiguous_or_invalid_launch_scope_stays_unverified(#[case] arguments: Vec<&str>) {
    let arguments = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert!(bootty_agents::PiAccountSelector::from_arguments(&arguments).is_err());
}

#[rstest]
fn pi_selector_cache_tracks_exact_account_and_model_arguments() {
    let directory = assert_fs::TempDir::new().unwrap();
    let program = executable(
        &directory,
        "printf '%s' '{\"status\":\"ready\",\"provider\":\"openai-codex\",\"authType\":\"oauth\"}'",
    )
    .unwrap();
    let first = bootty_agents::PiAccountSelector::from_arguments(&[
        "--model".to_owned(),
        "openai-codex/first".to_owned(),
    ])
    .unwrap()
    .unwrap();
    let other = bootty_agents::PiAccountSelector::from_arguments(&[
        "--model".to_owned(),
        "openai-codex/other".to_owned(),
    ])
    .unwrap()
    .unwrap();
    let mixed = bootty_agents::PiAccountSelector::from_arguments(&[
        "--provider".to_owned(),
        "openai-codex".to_owned(),
        "--model".to_owned(),
        "first".to_owned(),
    ])
    .unwrap()
    .unwrap();
    let service = TerminalAgentService::open(directory.path().join("catalog.json")).unwrap();
    let status = service.inspect_provider_with_pi_selector(
        AgentKind::Pi,
        &program,
        Some(ACCOUNT_DIRECTORY),
        Some(&first),
    );
    assert_eq!(status.authenticated, Some(true));
    assert!(
        service
            .provider_status_with_pi_selector(
                AgentKind::Pi,
                &program,
                Some(ACCOUNT_DIRECTORY),
                Some(&first)
            )
            .is_some()
    );
    for (account, selector) in [
        (Some("/other-account"), Some(&first)),
        (Some(ACCOUNT_DIRECTORY), Some(&other)),
        (Some(ACCOUNT_DIRECTORY), Some(&mixed)),
        (Some(ACCOUNT_DIRECTORY), None),
    ] {
        assert!(
            service
                .provider_status_with_pi_selector(AgentKind::Pi, &program, account, selector)
                .is_none()
        );
    }
    assert!(
        service
            .provider_status(
                AgentKind::Pi,
                &program,
                Some(ACCOUNT_DIRECTORY),
                Some("openai-codex")
            )
            .is_none()
    );
}

#[rstest]
fn pi_literal_prompt_arguments_do_not_change_account_scope() {
    let selector = bootty_agents::PiAccountSelector::from_arguments(&[
        "--provider".to_owned(),
        "selected".to_owned(),
        "--".to_owned(),
        "--model".to_owned(),
        "other/model".to_owned(),
    ])
    .unwrap();
    assert_eq!(
        selector,
        Some(bootty_agents::PiAccountSelector::provider("selected"))
    );
}
