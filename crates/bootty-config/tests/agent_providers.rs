use assert_fs::prelude::*;
use bootty_config::{config::load_config_from_path, settings_schema::SettingsSchema};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(false)]
#[case(true)]
fn agent_spawning_is_an_explicit_scalar_permission(#[case] enabled: bool) {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("config.toml");
    file.write_str("").unwrap();
    assert!(
        !load_config_from_path(file.path())
            .unwrap()
            .agents
            .allow_spawn
    );
    file.write_str(&format!("[agents]\nallow-spawn = {enabled}\n"))
        .unwrap();
    let config = load_config_from_path(file.path()).unwrap();
    assert_eq!(config.agents.allow_spawn, enabled);
    let schema = SettingsSchema::builtin();
    let spec = schema.get("agents.allow-spawn").unwrap();
    assert_eq!(spec.page.as_ref(), "permissions");
    assert!(matches!(
        spec.kind,
        bootty_config::settings_schema::SettingKind::Bool
    ));
    assert_eq!(
        spec.default_value(&config),
        Some(bootty_config::settings_schema::SettingValue::Bool(enabled))
    );
}

#[rstest]
#[case("codex")]
#[case("claude")]
#[case("pi")]
fn account_and_launch_preferences_round_trip(#[case] provider: &str) {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("config.toml");
    file.write_str(&format!("[agents.{provider}]\nenabled = false\nprogram = '/bin/provider'\nselected = 'work'\n[agents.{provider}.profiles.work]\nname = 'Work'\ndirectory = '/account/store'\narguments = ['--model', 'literal; $HOME']\n")).unwrap();
    let config = load_config_from_path(file.path()).unwrap();
    let preferences = config.agents.provider(provider).unwrap();
    assert!(!preferences.enabled);
    assert_eq!(preferences.program, "/bin/provider");
    let selected = preferences.selected_profile().unwrap();
    assert_eq!(selected.name, "Work");
    assert_eq!(selected.directory.as_deref(), Some("/account/store"));
    assert_eq!(selected.arguments, ["--model", "literal; $HOME"]);
    let schema = SettingsSchema::builtin();
    for field in ["name", "directory", "arguments"] {
        assert!(schema.allows_write_path(&["agents", provider, "profiles", "work", field]));
    }
}

#[rstest]
#[case("[agents.codex]\nselected = 'missing'")]
#[case("[agents.codex]\nprogram = '--not-a-program'")]
#[case("[agents.unknown]\nenabled = true")]
#[case("[agents.codex.profiles.work]\nname = 'Work'\ndirectory = 'relative/path'")]
#[case("[agents.codex.profiles.'work.with.dot']\nname = 'Work'")]
#[case("[agents.codex.profiles.work]\nname = ''")]
fn invalid_provider_preferences_do_not_load(#[case] source: &str) {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("config.toml");
    file.write_str(source).unwrap();
    load_config_from_path(file.path()).expect_err("invalid provider preferences");
    assert_eq!(std::fs::read_to_string(file.path()).unwrap(), source);
}
