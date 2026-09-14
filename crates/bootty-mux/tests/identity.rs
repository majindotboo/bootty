use bootty_config::ApplicationIdentity;
use bootty_mux::tmux::local_server_args;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn production_uses_the_users_default_tmux_server() {
    assert_eq!(
        local_server_args(ApplicationIdentity::Production),
        Vec::<String>::new()
    );
}

#[rstest]
fn development_uses_its_worktrees_tmux_server() {
    assert_eq!(
        local_server_args(ApplicationIdentity::Development),
        vec![
            "-L".to_owned(),
            ApplicationIdentity::Development.namespace().to_owned()
        ]
    );
}

#[rstest]
fn preparing_the_daemon_keeps_the_callers_environment_unchanged() -> anyhow::Result<()> {
    let names = [
        bootty_config::APPLICATION_IDENTITY_ENV,
        bootty_config::DEVELOPMENT_NAMESPACE_ENV,
        rmux_sdk::bootstrap::discovery::SDK_DAEMON_BINARY_ENV,
    ];
    let before = names.map(std::env::var_os);
    bootty_mux::rmux::prepare_local_rmux_daemon(ApplicationIdentity::for_process())?;
    assert_eq!(names.map(std::env::var_os), before);
    Ok(())
}
