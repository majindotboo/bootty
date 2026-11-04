//! Draft editing and the explicit command boundary for new sessions.

use std::collections::HashMap;

use bootty_control::{CommandTarget, ResourceKind};
use bootty_mux::controller::SpaceId;
use bootty_ui::presentation::new_session_form::{
    NewSessionDraft, NewSessionForm, SessionDestination,
};
use pretty_assertions::{assert_eq, assert_ne};
use proptest::test_runner::{Config, TestRunner};
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
            command: String::new(),
            isolated: false,
            isolation_preference: false,
            branch: String::new(),
            folder: String::new(),
            start_ref: String::new(),
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
fn linked_checkout_uses_its_project_identity_without_changing_the_directory(
    mut form: NewSessionForm,
) {
    form.set_directory("/worktrees/arc/out-of-box-setup".into());
    form.set_worktree_catalog(&[
        bootty_git::WorktreePickerEntry {
            label: "New worktree".into(),
            is_new: true,
            ..Default::default()
        },
        bootty_git::WorktreePickerEntry {
            label: "arc".into(),
            path: Some("/projects/arc".into()),
            ..Default::default()
        },
        bootty_git::WorktreePickerEntry {
            label: "out-of-box-setup".into(),
            path: Some("/worktrees/arc/out-of-box-setup".into()),
            ..Default::default()
        },
    ]);
    let spec = form.spec(false);
    assert_eq!(
        spec.rows
            .iter()
            .find(|row| row.id.0 == "choose-project")
            .unwrap()
            .label,
        "arc…"
    );
    assert_eq!(spec.selected_project.as_deref(), Some("/projects/arc"));
    assert_eq!(form.draft.cwd, "/worktrees/arc/out-of-box-setup");
}

#[rstest]
#[case("Current host", "HEAD", "feature/actual")]
#[case("Another host", "HEAD", "feature/actual")]
#[case("Current host", "configured/base", "configured/base")]
fn base_branch_catalog_names_the_current_ref_and_respects_explicit_choices(
    mut form: NewSessionForm,
    #[case] host: &str,
    #[case] initial: &str,
    #[case] expected: &str,
) {
    form.change_field("host", host);
    form.set_worktree_available(true);
    form.change_field("isolation", "New worktree");
    form.change_field("start-ref", initial);
    let invocation = form.branch_catalog_invocation().unwrap();
    assert_eq!(
        invocation.target.as_ref(),
        form.destination().map(|destination| &destination.target)
    );
    let overview = serde_json::from_value(serde_json::json!({
        "root": form.draft.cwd, "history": [], "stashes": [],
        "branches": [{"name": "feature/actual", "commit": "abc", "current": true, "upstream": null}]
    }))
    .unwrap();
    form.set_branches_for(&invocation, Ok(overview));
    assert_eq!(form.draft.start_ref, expected);
    let spec = form.spec(false);
    let base = spec
        .fields
        .iter()
        .find(|field| field.id == "start-ref")
        .unwrap();
    assert!(
        matches!(&base.kind, bootty_ui::gpui::DialogFieldKind::Choice(options) if options.iter().any(|option| option == "feature/actual"))
    );
    form.set_directory("/another/project".into());
    form.set_branches_for(&invocation, Err("Stale response".into()));
    assert!(
        !form
            .spec(false)
            .hint
            .is_some_and(|hint| hint.contains("Stale response"))
    );
}

fn load_checkout_branches(form: &mut NewSessionForm) -> anyhow::Result<()> {
    form.set_worktree_available(true);
    let invocation = form
        .branch_catalog_invocation()
        .ok_or_else(|| anyhow::anyhow!("current checkout loads branches"))?;
    let overview = serde_json::from_value(serde_json::json!({
        "root": form.draft.cwd, "history": [], "stashes": [],
        "branches": [
            {"name": "main", "commit": "abc", "current": true, "upstream": null,
             "worktree_path": form.draft.cwd},
            {"name": "feature/existing", "commit": "abc", "current": false, "upstream": null},
            {"name": "topic/linked", "commit": "abc", "current": false, "upstream": null,
             "worktree_path": "/worktrees/project/linked"}
        ]
    }))?;
    form.set_branches_for(&invocation, Ok(overview));
    Ok(())
}

#[rstest]
#[case("Current host")]
#[case("Another host")]
fn existing_branch_selection_waits_for_git_and_preserves_failed_drafts(
    mut form: NewSessionForm,
    #[case] host: &str,
    #[values(false, true)] succeeds: bool,
) {
    use bootty_control::CommandOutcome;
    use bootty_ui::{
        gpui::{DialogId, DialogIntent},
        presentation::dialogs::{NEW_SESSION_ID, NewSessionDialog, NewSessionPickerEvent},
    };
    form.change_field("host", host);
    form.change_text("Keep this command");
    load_checkout_branches(&mut form).expect("branch catalog");
    let target = form.destination().unwrap().target.clone();
    let cwd = form.draft.cwd.clone();
    let repaint: bootty_mux::RepaintHandle = std::sync::Arc::new(|| {});
    let mut dialog = NewSessionDialog::open_form(form, &repaint);
    let Some(NewSessionPickerEvent::CheckoutBranch(invocation)) = dialog.apply(
        &DialogIntent::FieldChanged {
            dialog: DialogId::new(NEW_SESSION_ID),
            field: "checkout-branch".into(),
            value: "feature/existing".into(),
        },
        &[],
    ) else {
        panic!("an unused existing branch requests a Git checkout")
    };
    assert_eq!(invocation.command, "git.branch-checkout");
    assert_eq!(invocation.target, Some(target));
    assert_eq!(
        invocation.arguments,
        [cwd.clone(), "feature/existing".into()]
    );
    let (sender, reply) = std::sync::mpsc::channel();
    dialog.checkout_started(invocation, reply);
    let selection = |dialog: &NewSessionDialog| {
        dialog
            .spec()
            .fields
            .into_iter()
            .find(|field| field.id == "checkout-branch")
            .unwrap()
            .value
    };
    assert_eq!(selection(&dialog), "main");
    assert_eq!(dialog.apply(&start_intent("start-session"), &[]), None);
    sender
        .send(if succeeds {
            CommandOutcome::Success {
                value: serde_json::Value::Null,
                warnings: Vec::new(),
            }
        } else {
            CommandOutcome::Failed {
                code: "git_failed".into(),
                message: "Commit or stash your changes".into(),
            }
        })
        .unwrap();
    assert_eq!(
        dialog.poll(),
        None,
        "choosing a branch does not start a session"
    );
    assert_eq!(
        selection(&dialog),
        if succeeds { "feature/existing" } else { "main" }
    );
    assert_eq!(dialog.draft().unwrap().command, "Keep this command");
    assert_eq!(dialog.draft().unwrap().cwd, cwd);
    if !succeeds {
        assert!(
            dialog
                .spec()
                .hint
                .unwrap()
                .contains("Commit or stash your changes")
        );
    }
}

#[rstest]
fn selecting_a_checked_out_branch_reuses_its_directory(
    mut form: NewSessionForm,
    #[values("Current host", "Another host")] host: &str,
) {
    form.change_field("host", host);
    form.change_text("Keep this draft");
    load_checkout_branches(&mut form).expect("branch catalog");
    let project = form.project_path().to_owned();
    assert_eq!(form.select_checkout_branch("topic/linked"), Ok(None));
    assert_eq!(form.draft.cwd, "/worktrees/project/linked");
    assert_eq!(form.project_path(), project);
    assert!(!form.draft.isolated);
    assert_eq!(form.worktree_request(), Ok(None));
    let selection = |form: &NewSessionForm, field: &str| {
        form.spec(false)
            .fields
            .into_iter()
            .find(|entry| entry.id == field)
            .unwrap()
            .value
    };
    assert_eq!(selection(&form, "isolation"), "Current worktree");
    assert_eq!(selection(&form, "checkout-branch"), "topic/linked");
    assert!(
        form.spec(false)
            .branches
            .iter()
            .any(|branch| branch.name == "topic/linked" && branch.current)
    );
    let invocation = form.invocation(&form.draft.cwd).unwrap();
    assert_eq!(invocation.arguments[1], "/worktrees/project/linked");
    assert_eq!(form.select_checkout_branch("main"), Ok(None));
    assert_eq!(form.draft.cwd, project);
    assert_eq!(selection(&form, "isolation"), "Current checkout");
    assert_eq!(selection(&form, "checkout-branch"), "main");
    assert_eq!(form.select_checkout_branch("topic/linked"), Ok(None));
    assert_eq!(form.draft.cwd, "/worktrees/project/linked");
    assert_eq!(form.draft.command, "Keep this draft");
}

#[rstest]
fn explicit_worktree_branch_has_a_known_destination(mut form: NewSessionForm) {
    form.set_worktree_available(true);
    form.change_field("isolation", "New worktree");
    form.change_text("Fix startup");
    form.change_field("branch", "feature/fix-startup");
    assert!(
        form.spec(false)
            .footer
            .unwrap()
            .ends_with("/current/feature-fix-startup")
    );
}

#[rstest]
fn creation_progress_keeps_the_checkout_and_explains_a_start_failure(form: NewSessionForm) {
    use bootty_control::CommandOutcome;
    use bootty_ui::gpui::DialogProgressState;
    let mut dialog = ready_dialog(form).unwrap();
    let (sender, reply) = std::sync::mpsc::channel();
    dialog.worktree_started(reply);
    assert_eq!(
        dialog.spec().progress[0].state,
        DialogProgressState::Running
    );
    sender
        .send(CommandOutcome::Success {
            value: serde_json::json!("/project/retained-checkout"),
            warnings: vec![],
        })
        .unwrap();
    assert!(matches!(
        dialog.poll(),
        Some(bootty_ui::presentation::dialogs::NewSessionPickerEvent::Submit(_))
    ));
    let (sender, reply) = std::sync::mpsc::channel();
    dialog.started(reply);
    let spec = dialog.spec();
    assert_eq!(spec.progress[0].state, DialogProgressState::Complete);
    assert_eq!(spec.progress[1].state, DialogProgressState::Running);
    sender
        .send(CommandOutcome::Failed {
            code: "backend_failed".into(),
            message: "launchctl: Permission denied".into(),
        })
        .unwrap();
    assert_eq!(dialog.poll(), None);
    let spec = dialog.spec();
    assert_eq!(spec.progress[1].state, DialogProgressState::Failed);
    assert_eq!(spec.footer.as_deref(), Some("/project/retained-checkout"));
    assert_eq!(spec.selected_project.as_deref(), Some("/project/current"));
    let error = spec.rows[0].detail.as_deref().unwrap();
    assert!(error.contains("Permission denied"), "{error}");
    assert!(
        spec.hint
            .as_deref()
            .unwrap()
            .contains("Retry uses this checkout")
    );
    dialog.apply(
        &bootty_ui::gpui::DialogIntent::TextChanged {
            dialog: bootty_ui::gpui::DialogId::new(
                bootty_ui::presentation::dialogs::NEW_SESSION_ID,
            ),
            value: "Updated command for retry".into(),
        },
        &[],
    );
    assert!(
        dialog.spec().rows[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("Permission denied")
    );
}

#[rstest]
fn command_survives_host_switches(form: NewSessionForm) {
    let form = std::cell::RefCell::new(form);
    let strategy = "[^\\x00]{0,256}";
    TestRunner::new(Config {
        cases: 32,
        ..Config::default()
    })
    .run(&strategy, |command| {
        let mut form = form.borrow_mut();
        form.change_text(&command);
        assert!(form.change_field("host", "Another host"));
        form.set_directory("/project/another/selected".to_owned());
        assert!(form.change_field("host", "Current host"));
        assert_eq!(form.draft.cwd, "/project/current");
        assert_eq!(&form.draft.command, &command);
        assert_eq!(form.draft.identity, "0123456789abcdef0123456789abcdef");
        assert!(form.change_field("host", "Another host"));
        assert_eq!(form.draft.cwd, "/project/another/selected");
        form.change_field("host", "Current host");
        Ok(())
    })
    .expect("draft preservation property");
}

#[rstest]
#[case("")]
#[case("  \n\t")]
#[case("printf 'one\\ntwo'\necho '$HOME'")]
fn terminal_text_distinguishes_shell_from_command(mut form: NewSessionForm, #[case] text: &str) {
    form.change_text(text);
    let invocation = form.invocation(&form.draft.cwd).expect("terminal launch");
    assert_eq!(invocation.command, "session.create");
    assert_eq!(invocation.arguments[3], form.draft.identity);
    assert_eq!(invocation.arguments[4], form.title());
    let argv: Vec<String> = serde_json::from_str(&invocation.arguments[2]).expect("argv");
    if text.trim().is_empty() {
        assert!(argv.is_empty(), "empty Terminal opens its shell");
        assert_eq!(form.spec(false).text_label.as_deref(), Some("Terminal"));
    } else {
        assert_eq!(argv.last().map(String::as_str), Some(text));
        assert_eq!(form.spec(false).text_label.as_deref(), Some("Command"));
    }
}

#[rstest]
fn accepted_worktree_is_retained_when_session_start_fails(
    form: NewSessionForm,
) -> anyhow::Result<()> {
    use bootty_control::CommandOutcome;
    use bootty_ui::presentation::dialogs::NewSessionPickerEvent;
    let mut dialog = ready_dialog(form)?;
    let original = dialog.draft().expect("draft").identity.clone();
    let (sender, reply) = std::sync::mpsc::channel();
    dialog.worktree_started(reply);
    sender.send(CommandOutcome::Success {
        value: serde_json::json!("/project/accepted-checkout"),
        warnings: Vec::new(),
    })?;
    let Some(NewSessionPickerEvent::Submit(first)) = dialog.poll() else {
        anyhow::bail!("Accepted worktree must submit session creation")
    };
    assert_eq!(first.command, "session.create");
    assert_eq!(first.arguments[1], "/project/accepted-checkout");
    let (sender, reply) = std::sync::mpsc::channel();
    dialog.started(reply);
    sender.send(CommandOutcome::Failed {
        code: "backend_failed".into(),
        message: "The backend could not start the session".into(),
    })?;
    assert_eq!(dialog.poll(), None);
    let draft = dialog.draft().expect("retained draft");
    assert_eq!(draft.cwd, "/project/accepted-checkout");
    anyhow::ensure!(!draft.isolated, "Retry uses the existing checkout");
    assert_ne!(
        draft.identity, original,
        "Failed task identities cannot be reused"
    );
    let Some(NewSessionPickerEvent::Submit(retry)) =
        dialog.apply(&start_intent("start-session"), &[])
    else {
        anyhow::bail!("Retry must submit directly, without creating another worktree")
    };
    assert_eq!(retry.command, "session.create");
    assert_eq!(retry.arguments[1], first.arguments[1]);
    Ok(())
}

#[rstest]
fn worktree_is_optional_generated_editable_and_capability_bounded(mut form: NewSessionForm) {
    assert_eq!(form.worktree_request(), Ok(None));
    assert!(
        !form
            .spec(false)
            .fields
            .iter()
            .any(|field| field.id == "isolation")
    );
    form.set_worktree_available(true);
    form.change_field("isolation", "New worktree");
    assert_eq!(form.draft.branch, "");
    assert_eq!(form.draft.folder, "");
    // Empty names fall back to ones generated from the draft's unique suffix.
    assert_eq!(
        form.worktree_request(),
        Ok(Some(bootty_git::WorktreeRequest {
            branch: "bootty/terminal-a123".to_owned(),
            name: Some("terminal-a123".to_owned()),
            start_ref: Some("HEAD".to_owned()),
            directory: None,
        }))
    );
    let footer = form.spec(false).footer;
    form.set_worktree_directory(Some("/srv/checkouts".to_owned()));
    let namespace = bootty_config::ApplicationIdentity::for_process().namespace();
    let default = if cfg!(windows) {
        format!("%LOCALAPPDATA%\\{namespace}\\worktrees")
    } else {
        format!("~/.local/share/{namespace}/worktrees")
    };
    assert_eq!(
        [footer, form.spec(false).footer],
        [
            Some(format!("{default}/current/terminal-a123")),
            Some("/srv/checkouts/current/terminal-a123".to_owned()),
        ]
    );
    for (field, value) in [
        ("branch", "  feature/custom  "),
        ("folder", " custom-checkout "),
        ("start-ref", " main "),
    ] {
        form.change_field(field, value);
    }
    assert_eq!(
        form.worktree_request(),
        Ok(Some(bootty_git::WorktreeRequest {
            branch: "feature/custom".to_owned(),
            name: Some("custom-checkout".to_owned()),
            start_ref: Some("main".to_owned()),
            directory: None,
        }))
    );
    form.change_field("folder", "../unowned");
    assert!(form.worktree_request().is_err());
    form.set_worktree_available(false);
    assert!(
        !form.draft.isolated,
        "unsupported projects still allow the current checkout"
    );
    assert!(
        form.draft.isolation_preference,
        "the next supported project remembers isolation"
    );
    assert_eq!(form.worktree_request(), Ok(None));
}

#[rstest]
#[case("")]
#[case("printf '%s' '$HOME'")]
fn remote_host_runs_terminal_commands_through_its_login_shell(
    form: NewSessionForm,
    #[case] command: &str,
) {
    let mut destination = form.destination().expect("destination").clone();
    destination.remote =
        Some(bootty_config::config::SshRemoteConfig::for_host("remote-host").into());
    let mut form = NewSessionForm::new(form.draft, vec![destination.clone()]);
    form.change_text(command);
    let invocation = form
        .invocation(&form.draft.cwd)
        .expect("remote terminal launch");
    assert!(form.spec(false).rows[0].enabled);
    assert_eq!(invocation.command, "session.create");
    assert_eq!(invocation.target, Some(destination.target));
    let argv: Vec<String> =
        serde_json::from_str(&invocation.arguments[2]).expect("literal terminal argv");
    if command.is_empty() {
        assert_eq!(argv, Vec::<String>::new());
    } else {
        assert_eq!(argv, ["/bin/sh", "-lc", command]);
    }
    assert_eq!(form.draft.command, command);
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
fn start_submits_once_until_its_outcome_is_observed(mut form: NewSessionForm) {
    use bootty_ui::presentation::dialogs::NewSessionPickerEvent;
    form.change_text("cargo test");
    let original_target = form.destination().expect("destination").target.clone();
    let mut dialog = ready_dialog(form).expect("project discovery");
    let Some(NewSessionPickerEvent::Submit(invocation)) =
        dialog.apply(&start_intent("start-session"), &[])
    else {
        panic!("creation submits immediately");
    };
    assert_eq!(invocation.arguments[4], "cargo test");
    assert_eq!(
        dialog.apply(&start_intent("start-session"), &[]),
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
        Some(NewSessionPickerEvent::Started { setup: None, .. })
    ));
}

/// A session that starts in a worktree the dialog just made names that worktree's repository,
/// so its project's setup scripts run in it.
#[rstest]
fn a_session_in_a_new_worktree_runs_its_project_setup(form: NewSessionForm) {
    use bootty_ui::presentation::dialogs::NewSessionPickerEvent;
    let mut dialog = ready_dialog(form).expect("project discovery");
    let create = dialog
        .start_create_worktree(
            "/repo".to_owned(),
            bootty_git::WorktreeRequest {
                branch: "topic".to_owned(),
                name: None,
                start_ref: None,
                directory: None,
            },
        )
        .expect("worktree request");
    assert_eq!(create.command, "worktree.create");
    let (sender, receiver) = std::sync::mpsc::channel();
    dialog.worktree_started(receiver);
    sender
        .send(bootty_control::CommandOutcome::Success {
            value: serde_json::json!("/worktrees/topic"),
            warnings: Vec::new(),
        })
        .expect("observed worktree");
    let Some(NewSessionPickerEvent::Submit(_)) = dialog.poll() else {
        panic!("the session launches in the new worktree");
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    dialog.started(receiver);
    sender
        .send(bootty_control::CommandOutcome::Success {
            value: serde_json::json!({"terminal": {"kind": "terminal", "handle": "issued", "generation": 9}}),
            warnings: Vec::new(),
        })
        .expect("observed session");
    assert!(matches!(
        dialog.poll(),
        Some(NewSessionPickerEvent::Started { setup: Some(repository), .. }) if repository == "/repo"
    ));
}

#[rstest]
fn registered_project_selection_preserves_the_creation_draft(mut form: NewSessionForm) {
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
    form.change_text("Keep the draft when changing projects");
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
    dialog.apply(&select("/unregistered/project"), &[]);
    assert_eq!(dialog.draft().unwrap().cwd, first);
    dialog.apply(&select(&second), &[]);
    assert_eq!(dialog.spec().role, DialogRole::Prompt);
    assert_eq!(dialog.draft().unwrap().cwd, second);
    assert_eq!(
        dialog.draft().unwrap().command,
        "Keep the draft when changing projects"
    );
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
fn failed_creation_keeps_the_draft_without_reusing_the_failed_task(
    mut form: NewSessionForm,
    #[case] outcome: bootty_control::CommandOutcome,
) {
    form.change_text("Keep this command");
    let original = form.draft.identity.clone();
    let mut dialog = ready_dialog(form).expect("discovery");
    let (sender, receiver) = std::sync::mpsc::channel();
    dialog.started(receiver);
    sender.send(outcome).expect("failed attempt");
    assert_eq!(dialog.poll(), None);
    let draft = dialog.draft().expect("draft");
    assert_ne!(draft.identity, original);
    assert_eq!(draft.command, "Keep this command");
    assert!(!dialog.spec().busy);
}

/// A new worktree's branch starts with the configured prefix, unless its project sets its own.
#[rstest]
#[case::configured("", "team/terminal-a123")]
#[case::project("work/", "work/terminal-a123")]
fn worktree_branches_start_with_the_configured_or_project_prefix(
    mut form: NewSessionForm,
    #[case] project_prefix: &str,
    #[case] branch: &str,
) {
    use bootty_mux::repository::{ProjectSettings, RegisteredProject};
    form.set_branch_prefix("team/".to_owned());
    form.set_project_defaults(vec![RegisteredProject {
        scope: form.draft.scope,
        cwd: form.draft.cwd.clone(),
        collapsed: false,
        settings: ProjectSettings {
            branch_prefix: project_prefix.to_owned(),
            ..ProjectSettings::default()
        },
    }]);
    form.set_worktree_available(true);
    form.change_field("isolation", "New worktree");
    assert_eq!(
        form.worktree_request()
            .expect("valid worktree")
            .map(|request| request.branch),
        Some(branch.to_owned())
    );
}

#[rstest]
fn project_defaults_apply_to_creation_and_survive_a_created_worktree(mut form: NewSessionForm) {
    use bootty_mux::repository::{ProjectSettings, RegisteredProject};
    form.set_project_defaults(vec![RegisteredProject {
        scope: form.draft.scope,
        cwd: form.draft.cwd.clone(),
        collapsed: false,
        settings: ProjectSettings {
            name: "My project".to_owned(),
            isolated: true,
            branch_prefix: "work/".to_owned(),
            start_ref: "main".to_owned(),
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
    assert!(form.draft.isolated);
    assert_eq!(form.draft.start_ref, "main");
    form.set_directory("/new/worktree".to_owned());
    assert!(form.draft.isolated);
    assert_eq!(form.draft.start_ref, "main");
}

#[rstest]
fn no_project_keeps_launch_directory_separate_from_assignment(mut form: NewSessionForm) {
    form.set_no_project("/host/home".into());
    form.set_worktree_available(true);
    assert!(!form.worktree_available);
    assert!(!form.draft.isolated);
    let invocation = form.invocation(&form.draft.cwd).unwrap();
    assert_eq!(invocation.arguments[1], "/host/home");
    assert_eq!(invocation.arguments[5], "true");
    form.set_directory("/registered/project".into());
    assert!(!form.draft.projectless);
}
