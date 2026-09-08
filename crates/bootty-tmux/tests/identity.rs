use bootty_identity::ApplicationIdentity;
use bootty_tmux::local_server_args;
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
