use bootty_config::config::{
    MultiplexerBackendConfig, MultiplexerConfig, RemoteConfig, SshRemoteConfig, WslDistribution,
    WslRemoteConfig,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case(serde_json::json!({"host":"devbox","user":null,"port":null,"program":"ssh","args":[]}))]
#[case(serde_json::json!({"distribution":"Ubuntu 開発"}))]
fn host_target_round_trips_without_changing_the_wire_shape(#[case] value: serde_json::Value) {
    let remote: RemoteConfig = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(remote).unwrap(), value);
}
#[rstest]
#[case(serde_json::json!({"distribution":"Ubuntu", "host":"devbox"}))]
#[case(serde_json::json!({"distribution":"-exec"}))]
#[case(serde_json::json!({"distribution":"Ubuntu\nother"}))]
fn malformed_or_ambiguous_hosts_are_rejected(#[case] value: serde_json::Value) {
    assert!(serde_json::from_value::<RemoteConfig>(value).is_err());
}
#[rstest]
#[case(MultiplexerBackendConfig::Native, false)]
#[case(MultiplexerBackendConfig::Herdr, false)]
#[case(MultiplexerBackendConfig::Rmux, true)]
#[case(MultiplexerBackendConfig::Tmux, true)]
fn wsl_requires_a_linux_client_backend(
    #[case] backend: MultiplexerBackendConfig,
    #[case] supported: bool,
) {
    let config = MultiplexerConfig {
        backend,
        remote: Some(
            WslRemoteConfig {
                distribution: WslDistribution::new("Ubuntu").unwrap(),
            }
            .into(),
        ),
        ..Default::default()
    };
    assert_eq!(config.validate_remote().is_ok(), supported);
}
#[rstest]
fn ssh_and_wsl_can_use_the_same_name_without_sharing_identity() {
    let ssh: RemoteConfig = SshRemoteConfig::for_host("Ubuntu").into();
    let wsl: RemoteConfig = WslRemoteConfig {
        distribution: WslDistribution::new("Ubuntu").unwrap(),
    }
    .into();
    assert_ne!(ssh, wsl);
    assert!(wsl.as_ssh().is_none());
}
proptest! {
    #[test]
    fn distribution_validation_preserves_valid_names(name in "[A-Za-z][A-Za-z0-9 _.-]{0,80}") {
        let distribution = WslDistribution::new(name.clone()).unwrap();
        prop_assert_eq!(distribution.as_str(), name);
    }
}
