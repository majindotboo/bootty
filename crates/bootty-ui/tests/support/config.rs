pub fn config(
    config_path: std::path::PathBuf,
    backend: bootty_config::config::MultiplexerBackendConfig,
) -> bootty_config::config::BoottyConfig {
    let mut config = bootty_config::config::BoottyConfig {
        config_path,
        ..Default::default()
    };
    config.multiplexer.backend = backend;
    // Fixture clocks pass the updater's first delay, which would run the real installers on PATH.
    config.agents.auto_update = false;
    // The user's login shell and its startup files are outside the fixture.
    config.session.shell = Some("/bin/sh".to_owned());
    config
}
