use bootty_agents::{AgentKind, terminal_command_descriptors};
use bootty_control::ResourceKind;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(AgentKind::Codex, "Codex", true)]
#[case(AgentKind::Claude, "Claude", true)]
#[case(AgentKind::Pi, "Pi", false)]
fn terminal_palette_offers_supported_provider_pickers(
    #[case] provider: AgentKind,
    #[case] name: &str,
    #[case] fork_picker: bool,
) {
    let descriptors = terminal_command_descriptors();
    for (operation, title, offered) in [
        ("start", format!("Open {name} terminal"), true),
        ("tab", format!("Open {name} tab"), true),
        ("history", format!("{name} session history"), true),
        ("resume", format!("Resume {name} session"), true),
        ("fork", format!("Fork {name} session"), fork_picker),
        ("account.login", format!("Sign in to {name}"), true),
    ] {
        let descriptor = descriptors
            .iter()
            .find(|descriptor| descriptor.id == format!("agents.{provider}.{operation}"))
            .expect("provider command");
        assert_eq!(descriptor.title, title);
        assert_eq!(descriptor.palette, offered);
        assert_eq!(
            descriptor.target,
            Some(if operation == "tab" {
                ResourceKind::Session
            } else {
                ResourceKind::Binding
            })
        );
    }
}
