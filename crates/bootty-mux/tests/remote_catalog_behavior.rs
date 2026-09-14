use bootty_config::config::MultiplexerBackendConfig;
use bootty_mux::RemoteSpaceSummary;
use pretty_assertions::assert_eq;

#[test]
fn remote_space_summary_preserves_the_versioned_wire_shape() {
    for (backend, token) in [
        (MultiplexerBackendConfig::Herdr, "herdr"),
        (MultiplexerBackendConfig::Rmux, "rmux"),
        (MultiplexerBackendConfig::Native, "native"),
        (MultiplexerBackendConfig::Tmux, "tmux"),
    ] {
        let summary = RemoteSpaceSummary {
            catalog_version: 3,
            id: "space-1".to_owned(),
            name: "Development".to_owned(),
            backend,
        };
        let wire = serde_json::to_value(&summary).expect("serialize summary");
        assert_eq!(wire["backend"], token);
        assert_eq!(
            serde_json::from_value::<RemoteSpaceSummary>(wire).unwrap(),
            summary
        );
    }
}
