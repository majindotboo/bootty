use assert_fs::prelude::*;
use bootty_config::{
    config::{BoottyConfig, ComputerConfig, load_config_from_path},
    settings_schema::{SettingKind, SettingValue, SettingsSchema},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn load(source: &str) -> Result<BoottyConfig, Box<dyn std::error::Error>> {
    let directory = assert_fs::TempDir::new()?;
    let path = directory.child("config.toml");
    path.write_str(source)?;
    Ok(load_config_from_path(path.path())?)
}

#[rstest]
#[case("")]
#[case("[computer]\n")]
fn computer_use_is_disabled_when_unspecified(
    #[case] source: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(load(source)?.computer, ComputerConfig::default());
    assert_eq!(
        BoottyConfig::default().computer,
        ComputerConfig {
            enabled: false,
            capture_enabled: false,
            input_enabled: false
        }
    );
    Ok(())
}

proptest! {
    #[test]
    fn computer_policy_switches_load_independently(enabled in any::<bool>(), capture_enabled in any::<bool>(), input_enabled in any::<bool>()) {
        let config = load(&format!("[computer]\nenabled={enabled}\ncapture-enabled={capture_enabled}\ninput-enabled={input_enabled}\n")).map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(config.computer, ComputerConfig { enabled, capture_enabled, input_enabled });
    }
}

#[rstest]
#[case("enabled")]
#[case("capture-enabled")]
#[case("input-enabled")]
fn computer_switch_has_one_scalar_settings_owner(
    #[case] field: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let schema = SettingsSchema::builtin();
    let spec = schema
        .get(&format!("computer.{field}"))
        .ok_or("computer scalar spec")?;
    let SettingKind::Bool = spec.kind else {
        return Err("computer setting must be a scalar boolean".into());
    };
    assert_eq!(spec.page.as_ref(), "permissions");
    assert_eq!(
        spec.default_value(&BoottyConfig::default()),
        Some(SettingValue::Bool(false))
    );
    let configured = load(&format!("[computer]\n{field}=true\n"))?;
    assert_eq!(
        spec.default_value(&configured),
        Some(SettingValue::Bool(true))
    );
    Ok(())
}

#[rstest]
#[case("enabled=\"true\"")]
#[case("capture-enabled=1")]
#[case("input-enabled=\"granted\"")]
#[case("accessibility-granted=true")]
fn computer_policy_rejects_invalid_types_and_fake_os_grants(#[case] source: &str) {
    assert!(load(&format!("[computer]\n{source}\n")).is_err());
}
