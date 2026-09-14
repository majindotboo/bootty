pub fn config(
    config_path: std::path::PathBuf,
    backend: bootty_config::config::MultiplexerBackendConfig,
) -> bootty_config::config::BoottyConfig {
    let mut config = bootty_config::config::BoottyConfig {
        config_path,
        ..Default::default()
    };
    config.multiplexer.backend = backend;
    config
}
