//! Form editing and the explicit command boundary for new sessions.

use std::collections::HashMap;

use bootty_control::{CommandTarget, ResourceKind};
use bootty_mux::controller::SpaceId;
use bootty_ui::presentation::new_session_form::{
    NewSessionDraft, NewSessionForm, SessionDestination,
};
use pretty_assertions::{assert_eq, assert_ne};
use rstest::{fixture, rstest};

#[fixture]
fn form() -> NewSessionForm {
    let local = SpaceId::from_persistence(1);
    let another = SpaceId::from_persistence(2);
    let destination = |scope, label: &str, cwd: &str| SessionDestination {
        scope,
        label: label.to_owned(),
        icon: "folder".to_owned(),
        color: [122, 162, 247],
        cwd: cwd.to_owned(),
        remote: None,
        target: CommandTarget {
            kind: ResourceKind::Binding,
            handle: format!("issued-host-{}", scope.persistence_value()),
            generation: 7,
        },
    };
    NewSessionForm::new(
        NewSessionDraft {
            projectless: false,
            scope: local,
            cwd: "/project/current".to_owned(),
            suffix: "a123".to_owned(),
            identity: "0123456789abcdef0123456789abcdef".to_owned(),
            directories: HashMap::new(),
        },
        vec![
            destination(local, "Current host", "/project/current"),
            destination(another, "Another host", "/project/another"),
        ],
    )
}

#[rstest]
fn start_submits_once_until_its_outcome_is_observed(form: NewSessionForm) {
    use bootty_ui::presentation::dialogs::NewSessionPickerEvent;
    let original_target = form.destination().expect("destination").target.clone();
    let mut dialog = ready_dialog(form).expect("project discovery");
    let Some(NewSessionPickerEvent::Submit(invocation)) =
        dialog.apply(&start_intent("start-session"))
    else {
        panic!("creation submits immediately");
    };
    assert!(invocation.arguments[4].starts_with("Terminal in "));
    assert_eq!(
        dialog.apply(&start_intent("start-session")),
        None,
        "accepted Start cannot duplicate creation"
    );
    assert_eq!(invocation.command, "session.create");
    assert_eq!(invocation.target, Some(original_target));
    let (sender, receiver) = std::sync::mpsc::channel();
    dialog.started(receiver);
    sender.send(bootty_control::CommandOutcome::Success {
        value: serde_json::json!({"terminal": {"kind": "terminal", "handle": "issued", "generation": 9}}),
        warnings: Vec::new(),
    }).expect("observed outcome");
    assert!(matches!(
        dialog.poll(),
        Some(NewSessionPickerEvent::Started { .. })
    ));
}

#[rstest]
fn registered_project_selection_moves_the_session_directory(mut form: NewSessionForm) {
    use assert_fs::{TempDir, prelude::*};
    use bootty_ui::{
        gpui::{DialogId, DialogIntent, DialogRole},
        presentation::dialogs::{NEW_SESSION_ID, NewSessionDialog},
    };
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};
    let root = TempDir::new().unwrap();
    root.child("first").create_dir_all().unwrap();
    root.child("second").create_dir_all().unwrap();
    let first = root.child("first").path().to_string_lossy().into_owned();
    let second = root.child("second").path().to_string_lossy().into_owned();
    let (repository, _) =
        bootty_mux::repository::WorkspaceRepository::open(&root.child("config.toml")).unwrap();
    repository
        .register_project(form.draft.scope, &first)
        .unwrap();
    repository
        .register_project(form.draft.scope, &second)
        .unwrap();
    form.set_directory(first.clone());
    let (sender, wakes) = mpsc::channel();
    let repaint: bootty_mux::RepaintHandle = Arc::new(move || {
        let _ = sender.send(());
    });
    let mut dialog = NewSessionDialog::open_registered_form(form, &repaint, repository);
    let deadline = Instant::now().checked_add(Duration::from_secs(2)).unwrap();
    while dialog.spec().projects.len() != 2 || !dialog.spec().rows[0].enabled {
        if dialog.poll().is_some() {
            continue;
        }
        if dialog.spec().projects.len() == 2 && dialog.spec().rows[0].enabled {
            break;
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
    }
    let select = |path: &str| DialogIntent::FieldChanged {
        dialog: DialogId::new(NEW_SESSION_ID),
        field: "project".into(),
        value: path.into(),
    };
    dialog.apply(&select("/unregistered/project"));
    assert_eq!(dialog.draft().unwrap().cwd, first);
    dialog.apply(&select(&second));
    assert_eq!(dialog.spec().role, DialogRole::Prompt);
    assert_eq!(dialog.draft().unwrap().cwd, second);
}

#[rstest]
#[case::dismissed(true)]
#[case::another_picker(false)]
fn project_discovery_waits_for_the_prior_picker(
    mut form: NewSessionForm,
    #[case] dismiss_prior: bool,
) -> anyhow::Result<()> {
    use assert_fs::{TempDir, prelude::*};
    use bootty_ui::presentation::dialogs::NewSessionDialog;
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    let root = TempDir::new()?;
    root.child("project").create_dir_all()?;
    let cwd = root.child("project").path().to_string_lossy().into_owned();
    let (repository, _) =
        bootty_mux::repository::WorkspaceRepository::open(&root.child("config.toml"))?;
    repository.register_project(form.draft.scope, &cwd)?;
    form.set_directory(cwd.clone());
    let (sender, wakes) = mpsc::channel();
    let repaint: bootty_mux::RepaintHandle = Arc::new(move || {
        let _ = sender.send(());
    });

    // Hold the real registry so the first picker cannot finish before the second opens.
    let lock = rusqlite::Connection::open(root.child("session-order.sqlite3"))?;
    lock.execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE")?;
    let prior_form = NewSessionForm::new(
        form.draft.clone(),
        vec![form.destination().expect("destination").clone()],
    );
    let mut prior = Some(NewSessionDialog::open_registered_form(
        prior_form,
        &repaint,
        repository.clone(),
    ));
    if dismiss_prior {
        prior.take();
    }
    let mut dialog = NewSessionDialog::open_registered_form(form, &repaint, repository);
    anyhow::ensure!(
        !dialog.spec().rows[0].enabled,
        "Pending discovery blocks submit"
    );
    while dialog.poll().is_some() {}
    anyhow::ensure!(
        !dialog.spec().rows[0].enabled,
        "Polling retains pending discovery"
    );
    lock.execute_batch("ROLLBACK")?;

    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .ok_or_else(|| anyhow::anyhow!("discovery deadline overflow"))?;
    while !dialog.spec().rows[0].enabled {
        if let Some(prior) = &mut prior {
            prior.poll();
        }
        if dialog.poll().is_some() {
            continue;
        }
        if !dialog.spec().rows[0].enabled {
            wakes.recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        }
    }
    assert_eq!(dialog.spec().projects[0].path, cwd);
    assert_eq!(dialog.spec().hint, None);
    Ok(())
}

#[rstest]
#[case(bootty_control::CommandOutcome::deadline_exceeded())]
#[case(bootty_control::CommandOutcome::Failed { code: "launch".into(), message: "Backend failed".into() })]
fn failed_creation_keeps_the_form_without_reusing_the_failed_task(
    form: NewSessionForm,
    #[case] outcome: bootty_control::CommandOutcome,
) {
    let original = form.draft.identity.clone();
    let mut dialog = ready_dialog(form).expect("discovery");
    let (sender, receiver) = std::sync::mpsc::channel();
    dialog.started(receiver);
    sender.send(outcome).expect("failed attempt");
    assert_eq!(dialog.poll(), None);
    let draft = dialog.draft().expect("draft");
    assert_ne!(draft.identity, original);
    assert!(!dialog.spec().busy);
}

#[rstest]
fn a_registered_project_name_labels_its_directory(mut form: NewSessionForm) {
    use bootty_mux::repository::{ProjectSettings, RegisteredProject};
    form.set_project_defaults(vec![RegisteredProject {
        scope: form.draft.scope,
        cwd: form.draft.cwd.clone(),
        collapsed: false,
        settings: ProjectSettings {
            name: "My project".to_owned(),
            ..ProjectSettings::default()
        },
    }]);
    assert_eq!(
        form.spec(false)
            .project_labels
            .get(&form.draft.cwd)
            .map(String::as_str),
        Some("My project")
    );
}

/// Switching Space keeps each Space's directory, so switching back restores it.
#[rstest]
fn each_space_keeps_its_own_directory(mut form: NewSessionForm) {
    assert!(form.change_field("host", "Another host"));
    assert_eq!(form.draft.cwd, "/project/another");
    form.set_directory("/project/elsewhere".to_owned());
    assert!(form.change_field("host", "Current host"));
    assert_eq!(form.draft.cwd, "/project/current");
    assert!(form.change_field("host", "Another host"));
    assert_eq!(form.draft.cwd, "/project/elsewhere");
    assert!(!form.change_field("host", "Another host"));
}

#[rstest]
fn no_project_keeps_launch_directory_separate_from_assignment(mut form: NewSessionForm) {
    form.set_no_project("/host/home".into());
    let invocation = form.invocation(&form.draft.cwd).unwrap();
    assert_eq!(invocation.arguments[1], "/host/home");
    assert_eq!(invocation.arguments[5], "true");
    form.set_directory("/registered/project".into());
    assert!(!form.draft.projectless);
}

fn ready_dialog(
    form: NewSessionForm,
) -> anyhow::Result<bootty_ui::presentation::dialogs::NewSessionDialog> {
    use std::{
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };
    let (sender, wakes) = mpsc::channel();
    let repaint: bootty_mux::RepaintHandle = Arc::new(move || {
        let _ = sender.send(());
    });
    let mut dialog = bootty_ui::presentation::dialogs::NewSessionDialog::open_form(form, &repaint);
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .ok_or_else(|| anyhow::anyhow!("discovery deadline overflow"))?;
    loop {
        if dialog.poll().is_some() {
            continue;
        }
        if dialog.spec().rows.first().is_some_and(|row| row.enabled) {
            return Ok(dialog);
        }
        wakes.recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    }
}

fn start_intent(action: &str) -> bootty_ui::gpui::DialogIntent {
    bootty_ui::gpui::DialogIntent::Activate {
        dialog: bootty_ui::gpui::DialogId::new(bootty_ui::presentation::dialogs::NEW_SESSION_ID),
        row: bootty_ui::gpui::RowId::new("submit"),
        action: bootty_ui::gpui::ActionId::new(action),
        payload: bootty_ui::gpui::DialogPayload::default(),
    }
}

#[rstest]
#[case::local(None, true)]
#[case::remote(Some(bootty_config::config::SshRemoteConfig::for_host("devbox").into()), false)]
fn footer_abbreviates_only_this_machines_home(
    form: NewSessionForm,
    #[case] remote: Option<bootty_config::config::RemoteConfig>,
    #[case] abbreviated: bool,
) {
    let home = bootty_ui::strings::home_dir().expect("tests run with a home directory");
    let cwd = home.join("dotfiles").display().to_string();
    let mut destination = form.destination().unwrap().clone();
    destination.remote = remote;
    let form = NewSessionForm::new(
        NewSessionDraft {
            cwd: cwd.clone(),
            ..form.draft
        },
        vec![destination],
    );
    let expected = if abbreviated {
        "~/dotfiles".to_owned()
    } else {
        cwd
    };
    assert_eq!(form.spec(false).footer, Some(expected.clone()));
    assert_eq!(form.display_cwd(), expected);
}
