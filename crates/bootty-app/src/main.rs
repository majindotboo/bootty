use anyhow::Result;

fn main() -> Result<()> {
    // Correct a stale `$SHELL` to the OS login shell before any child inherits
    // it; tmux otherwise bakes the wrong shell into the server's default-shell.
    bootty_app::shell_env::align_shell_env();
    // Recover the user's PATH and shell exports before anything reads the
    // environment; a Finder-launched .app starts with launchd's minimal PATH.
    bootty_app::shell_env::hydrate_from_login_shell();

    let config =
        bootty_app::config::load_config_from_path(bootty_app::config::default_config_path())?;
    let options = bootty_app::platform::native_options_for_config(&config);

    bootty_app::native_host::run(options, config)
}
