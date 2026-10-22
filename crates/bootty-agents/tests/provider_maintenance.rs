use assert_fs::prelude::*;
use bootty_agents::{AgentKind, ProviderUpdate, ProviderUse};

#[test]
fn provider_processes_and_updates_exclude_each_other_and_release_on_drop() {
    let root = assert_fs::TempDir::new().unwrap();
    let executable = root.child("claude/versions/provider.exe");
    executable.write_str("MZ fixture").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(executable.path(), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let program = executable.path().to_str().unwrap();
    let first = ProviderUse::acquire(program).unwrap().unwrap();
    let second = ProviderUse::acquire(program).unwrap().unwrap();
    assert!(ProviderUpdate::prepare(AgentKind::Claude, program).is_err());
    drop(first);
    assert!(ProviderUpdate::prepare(AgentKind::Claude, program).is_err());
    drop(second);
    let update = ProviderUpdate::prepare(AgentKind::Claude, program)
        .unwrap()
        .unwrap();
    assert!(ProviderUse::acquire(program).is_err());
    assert!(ProviderUpdate::prepare(AgentKind::Claude, program).is_err());
    drop(update);
    assert!(ProviderUse::acquire(program).unwrap().is_some());
}

#[test]
fn unknown_installations_remain_manual() {
    let root = assert_fs::TempDir::new().unwrap();
    let executable = root.child("manual.exe");
    executable.write_str("MZ fixture").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(executable.path(), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    assert!(
        ProviderUpdate::prepare(AgentKind::Claude, executable.path().to_str().unwrap())
            .unwrap()
            .is_none()
    );
}
