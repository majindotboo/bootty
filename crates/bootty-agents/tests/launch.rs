use bootty_agents::{AgentKind, AgentLaunch, LaunchShell};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn launch(args: &[&str]) -> AgentLaunch {
    AgentLaunch {
        program: "agent".to_owned(),
        cwd: Some("/tmp/project".to_owned()),
        arguments: args.iter().map(|arg| (*arg).to_owned()).collect(),
        ephemeral: false,
        account_directory: None,
    }
}

#[rstest]
#[case("line one\nline two\tquoted '$HOME'", true)]
#[case("embedded\0nul", false)]
#[case("bounded", true)]
fn literal_argument_validation_preserves_text(#[case] argument: &str, #[case] valid: bool) {
    assert_eq!(launch(&[argument]).validate().is_ok(), valid);
}

#[rstest]
#[case(vec!["x".repeat(8193)])]
#[case(vec!["x".to_owned(); 65])]
#[case(vec!["x".repeat(8192); 9])]
fn argument_limits_remain_authoritative(#[case] arguments: Vec<String>) {
    let mut launch = launch(&[]);
    launch.arguments = arguments;
    assert!(launch.validate().is_err());
}
#[rstest]
#[case(AgentKind::Pi, false, vec!["--model", "model", "--session", "session"]) ]
#[case(AgentKind::Pi, true, vec!["--model", "model", "--fork", "session"]) ]
#[case(AgentKind::Codex, false, vec!["resume", "session", "--model", "model"]) ]
#[case(AgentKind::Codex, true, vec!["fork", "session", "--model", "model"]) ]
#[case(AgentKind::Claude, false, vec!["--model", "model", "--resume", "session"]) ]
#[case(AgentKind::Claude, true, vec!["--model", "model", "--resume", "session", "--fork-session"]) ]
fn session_launch_retains_configuration_but_never_prompts_or_secrets(
    #[case] provider: AgentKind,
    #[case] fork: bool,
    #[case] expected: Vec<&str>,
) {
    let launch = launch(&[
        "--model",
        "model",
        "--api-key",
        "secret",
        "prompt",
        "--session",
        "old",
    ]);
    assert_eq!(
        launch.session_arguments(provider, "session", fork).unwrap(),
        expected
    );
    let retained = serde_json::to_string(&launch.retained(provider)).unwrap();
    assert!(!retained.contains("secret"));
    assert!(!retained.contains("prompt"));
}

#[rstest]
#[case(vec!["--extension", "/owned/QA questions.ts"])]
#[case(vec!["-e", "/owned/QA questions.ts"])]
#[case(vec!["--extension=/owned/QA questions.ts"])]
fn pi_resume_preserves_configured_extensions_as_literal_arguments(#[case] extension: Vec<&str>) {
    let mut args = extension.clone();
    args.extend(["--api-key", "secret", "--session", "old", "prompt"]);
    let captured = launch(&args);
    assert_eq!(captured.retained(AgentKind::Pi).arguments, extension);
    let mut expected = extension;
    expected.extend(["--session", "captured-session"]);
    assert_eq!(
        captured
            .session_arguments(AgentKind::Pi, "captured-session", false)
            .unwrap(),
        expected
    );
}
#[rstest]
#[case("--no-session")]
#[case("--no-session-persistence")]
#[case("--ephemeral")]
fn ephemeral_sessions_cannot_be_resumed(#[case] flag: &str) {
    let saved = launch(&[flag]).retained(AgentKind::Pi);
    assert!(saved.session_arguments(AgentKind::Pi, "id", false).is_err());
}

#[rstest]
#[case("--no-session")]
#[case("--no-session-persistence")]
#[case("--ephemeral")]
fn prompt_text_after_the_option_separator_does_not_disable_session_persistence(#[case] text: &str) {
    let saved = launch(&["--", text]).retained(AgentKind::Pi);
    assert!(!saved.ephemeral);
    assert!(saved.session_arguments(AgentKind::Pi, "id", false).is_ok());
}
#[rstest]
#[case("")]
#[case("--last")]
#[case("session\nexec")]
fn session_selectors_cannot_change_the_requested_operation(#[case] session: &str) {
    assert!(
        launch(&[])
            .session_arguments(AgentKind::Codex, session, false)
            .is_err()
    );
}
#[cfg(unix)]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn posix_launch_preserves_literal_arguments(value in "[ -~]{0,80}") {
        let launch = AgentLaunch { program: "/usr/bin/printf".to_owned(), cwd: None, arguments: vec!["%s".to_owned(), value.clone()], ephemeral: false, account_directory: None };
        let command = launch.shell_command(AgentKind::Pi, LaunchShell::Posix).unwrap();
        let output = std::process::Command::new("/bin/sh").args(["-c", &command]).output().unwrap();
        prop_assert!(output.status.success());
        prop_assert_eq!(output.stdout, value.into_bytes());
    }
}
#[rstest]
fn windows_launch_uses_encoded_arguments_instead_of_cmd_interpolation() {
    use base64::Engine as _;
    let launch = launch(&["a&b", "x'y", "$HOME", "%PATH%"]);
    let command = launch
        .shell_command(AgentKind::Claude, LaunchShell::Windows)
        .unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(command.split_whitespace().last().unwrap())
        .unwrap();
    let utf16 = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let script = String::from_utf16(&utf16).unwrap();
    assert!(script.contains("& 'agent' 'a&b' 'x''y' '$HOME' '%PATH%'"));
    assert!(!command.contains("%PATH%"));
}
