pub fn config(config_path: std::path::PathBuf) -> bootty_config::config::BoottyConfig {
    bootty_test_support::native_mux::local(&config_path).expect("in-process native mux");
    let mut config = bootty_config::config::BoottyConfig {
        config_path,
        ..Default::default()
    };
    // The user's login shell and its startup files are outside the fixture.
    config.session.shell = Some("/bin/sh".to_owned());
    config
}
