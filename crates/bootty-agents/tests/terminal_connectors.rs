//! A provider's MCP connectors read from its own CLI, and its own commands for changing them.

use bootty_agents::{
    AgentKind, ConnectorAction, ProviderConnector, parse_claude_connectors, parse_codex_connectors,
    terminal_connector_launch,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn connector(name: &str, target: &str, status: &str, needs_sign_in: bool) -> ProviderConnector {
    ProviderConnector {
        name: name.to_owned(),
        target: target.to_owned(),
        status: status.to_owned(),
        needs_sign_in,
    }
}

/// Claude names hold colons and its URLs hold "://", and only the ones it can't reach without
/// signing in ask for it.
#[rstest]
fn claude_lists_each_server_with_its_health() {
    let listed = parse_claude_connectors(
        "Checking MCP server health…\n\n\
         plugin:linear:linear: https://mcp.linear.app/mcp (HTTP) - ✔ Connected\n\
         plugin:figma:figma: https://mcp.figma.com/mcp (HTTP) - ! Needs authentication\n\
         local: npx -y some-server (stdio) - ✘ Failed to connect\n",
    );
    assert_eq!(
        listed,
        [
            connector(
                "plugin:linear:linear",
                "https://mcp.linear.app/mcp (HTTP)",
                "Connected",
                false
            ),
            connector(
                "plugin:figma:figma",
                "https://mcp.figma.com/mcp (HTTP)",
                "Needs authentication",
                true
            ),
            connector(
                "local",
                "npx -y some-server (stdio)",
                "Failed to connect",
                false
            ),
        ]
    );
}

/// Codex's JSON says how each server runs and whether it waits on a sign-in.
#[rstest]
fn codex_lists_each_server_with_how_it_runs() {
    let listed = parse_codex_connectors(
        r#"[
          {"name":"docs","enabled":true,"transport":{"type":"streamable_http","url":"https://docs.example/mcp"},"auth_status":"not_logged_in"},
          {"name":"tools","enabled":true,"transport":{"type":"stdio","command":"npx","args":["-y","tools"]},"auth_status":"unsupported"},
          {"name":"old","enabled":false,"transport":{"type":"stdio","command":"old"},"auth_status":"unsupported"}
        ]"#,
    )
    .unwrap();
    assert_eq!(
        listed,
        [
            connector(
                "docs",
                "https://docs.example/mcp",
                "Needs authentication",
                true
            ),
            connector("tools", "npx -y tools", "Configured", false),
            connector("old", "old", "Turned off", false),
        ]
    );
    assert!(parse_codex_connectors("not json").is_err());
}

/// Adding and signing in use the provider's own commands, so the provider keeps its connectors.
#[rstest]
#[case::claude_url(AgentKind::Claude, ConnectorAction::Add { name: "docs", target: "https://docs.example/mcp" }, &["mcp", "add", "--transport", "http", "docs", "https://docs.example/mcp"][..])]
#[case::codex_url(AgentKind::Codex, ConnectorAction::Add { name: "docs", target: "https://docs.example/mcp" }, &["mcp", "add", "docs", "--url", "https://docs.example/mcp"][..])]
#[case::claude_command(AgentKind::Claude, ConnectorAction::Add { name: "tools", target: " npx -y tools " }, &["mcp", "add", "tools", "--", "npx", "-y", "tools"][..])]
#[case::codex_command(AgentKind::Codex, ConnectorAction::Add { name: "tools", target: "npx -y tools" }, &["mcp", "add", "tools", "--", "npx", "-y", "tools"][..])]
#[case::sign_in(AgentKind::Claude, ConnectorAction::SignIn { name: "plugin:figma:figma" }, &["mcp", "login", "plugin:figma:figma"][..])]
fn connector_commands_are_the_providers_own(
    #[case] provider: AgentKind,
    #[case] action: ConnectorAction<'_>,
    #[case] expected: &[&str],
) {
    let launch = terminal_connector_launch(provider, "provider", action).unwrap();
    assert_eq!(launch.arguments, expected);
    assert!(launch.ephemeral, "the terminal closes with the command");
}

#[rstest]
#[case::no_name(AgentKind::Claude, ConnectorAction::SignIn { name: " " }, "Name the connector")]
#[case::flag_name(AgentKind::Codex, ConnectorAction::SignIn { name: "--all" }, "Name the connector")]
#[case::no_target(AgentKind::Codex, ConnectorAction::Add { name: "docs", target: "" }, "Give the connector's URL or command")]
#[case::pi(AgentKind::Pi, ConnectorAction::SignIn { name: "docs" }, "Pi has no connectors to manage")]
fn connector_commands_refuse_what_the_provider_would(
    #[case] provider: AgentKind,
    #[case] action: ConnectorAction<'_>,
    #[case] error: &str,
) {
    assert_eq!(
        terminal_connector_launch(provider, "provider", action).unwrap_err(),
        error
    );
}
