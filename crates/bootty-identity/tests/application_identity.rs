use std::path::Path;

use bootty_identity::{
    ApplicationIdentity, DEVELOPMENT_NAMESPACE_ENV, development_names_for_workspace,
    development_namespace_for_workspace,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case("/workspace/one")]
#[case("/workspace/two")]
fn development_names_are_stable_and_safe(#[case] root: &str) {
    let first = development_names_for_workspace(Path::new(root));
    let second = development_names_for_workspace(Path::new(root));

    assert_eq!(first, second);
    assert!(first.namespace().starts_with("bootty-dev-"));
    assert_eq!(first.namespace().len(), "bootty-dev-".len() + 16);
    assert_eq!(first.cli_name(), first.namespace());
    assert!(first.display_name().starts_with("BoottyDev-"));
    assert!(
        first
            .bundle_identifier()
            .starts_with("dev.bootty.desktop.dev.")
    );
}

#[rstest]
fn different_worktrees_have_different_namespaces() {
    assert_ne!(
        development_namespace_for_workspace(Path::new("/workspace/one")),
        development_namespace_for_workspace(Path::new("/workspace/two"))
    );
}

#[rstest]
fn production_names_do_not_depend_on_the_workspace() {
    let first = ApplicationIdentity::Production.names_for_workspace(Path::new("/workspace/one"));
    let second = ApplicationIdentity::Production.names_for_workspace(Path::new("/workspace/two"));

    assert_eq!(first, second);
    assert_eq!(first.display_name(), "Bootty");
    assert_eq!(first.namespace(), "bootty");
    assert_eq!(first.cli_name(), "bootty");
    assert_eq!(first.bundle_identifier(), "dev.bootty.desktop");
    assert_eq!(
        ApplicationIdentity::Production.development_namespace_environment(),
        None
    );
}

#[rstest]
fn development_child_environment_carries_the_resolved_namespace() {
    assert_eq!(
        ApplicationIdentity::Development.development_namespace_environment(),
        Some((
            DEVELOPMENT_NAMESPACE_ENV,
            ApplicationIdentity::Development.namespace()
        ))
    );
}
