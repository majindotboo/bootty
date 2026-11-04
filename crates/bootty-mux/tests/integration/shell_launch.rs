use super::*;
use pretty_assertions::assert_eq;

#[rstest]
#[case(false, None)]
#[case(true, None)]
#[case(false, Some("/explicit/shell"))]
#[case(true, Some("/explicit/shell"))]
fn split_preserves_workspace_shell(#[case] remote: bool, #[case] shell: Option<&str>) {
    let launches = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&launches);
    let base = launcher(Outputs::default(), Arc::new(AtomicUsize::new(0)));
    let connector = native::memory::connector(Arc::new(move |size, config| {
        observed.lock().unwrap().push(config.clone());
        base(size, config)
    }))
    .unwrap();
    let backend = NativeBackend::with_identity_verifier(
        Path::new("shell-consistency"),
        connector,
        if remote {
            Some(Arc::new(|_: &str| Ok(())))
        } else {
            None
        },
    );
    backend.configure_launch(bootty_terminal::SessionLaunchConfig {
        shell: shell.map(str::to_owned),
        ..bootty_terminal::SessionLaunchConfig::default()
    });
    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: "session".into(),
            cwd: "/".into(),
            tag: MuxSessionTag::default(),
            argv: None,
        })
        .unwrap();
    backend
        .execute(MuxCommand::SplitPane {
            session_id: "session".into(),
            pane_id: None,
            direction: MuxSplitDirection::Down,
        })
        .unwrap();
    let launches = launches.lock().unwrap();
    assert_eq!(launches.len(), 2);
    assert!(
        launches[0].shell.is_some(),
        "server must resolve the workspace shell"
    );
    assert_eq!(launches[0].shell, launches[1].shell);
    if let Some(shell) = shell {
        assert_eq!(launches[0].shell.as_deref(), Some(shell));
    }
}

#[rstest]
#[case(false)]
#[case(true)]
fn workspace_uses_configured_shell_integration(#[case] enabled: bool) {
    let base = launcher(Outputs::default(), Arc::new(AtomicUsize::new(0)));
    let connector = native::memory::connector(Arc::new(move |size, launch| {
        assert_eq!(launch.shell_integration, enabled);
        base(size, launch)
    }))
    .unwrap();
    let backend = NativeBackend::with_connector(Path::new("integration-defaults"), connector);
    let mut config = bootty_config::config::BoottyConfig::default();
    config.session.shell_integration = enabled;
    let (tx, _) = mpsc::channel();
    backend.configure_launch(
        bootty_mux::terminal_config::terminal_session_config_with_side_effects(
            &config,
            bootty_config::config::AppearanceVariant::Dark,
            &tx,
        )
        .launch,
    );
    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: "session".into(),
            cwd: "/".into(),
            tag: MuxSessionTag::default(),
            argv: None,
        })
        .unwrap();
}
