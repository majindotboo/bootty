//! Draft editing and the explicit command boundary for new sessions.

use std::collections::{BTreeMap, HashMap};

use bootty_config::config::{AgentProfileConfig, AgentProvidersConfig};
use bootty_control::{Caller, CommandTarget, ResourceKind};
use bootty_mux::controller::SpaceId;
use bootty_ui::presentation::new_session_form::{
    NewSessionAttachment, NewSessionDraft, NewSessionForm, NewSessionMode, SessionDestination,
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
        worktrees: true,
    };
    let mut providers = AgentProvidersConfig::default();
    "/provider/custom-codex".clone_into(&mut providers.codex.program);
    providers.codex.profiles.insert(
        "work".to_owned(),
        AgentProfileConfig {
            name: "Work".to_owned(),
            directory: Some("/account/work".to_owned()),
            arguments: vec!["--model".to_owned(), "configured-model".to_owned()],
        },
    );
    NewSessionForm::new(
        NewSessionDraft {
            scope: local,
            cwd: "/project/current".to_owned(),
            mode: NewSessionMode::Agent,
            prompt: String::new(),
            applications: Vec::new(),
            attachments: Vec::new(),
            command: String::new(),
            provider: "codex".to_owned(),
            profiles: BTreeMap::new(),
            model_selection: None,
            permissions: bootty_agents::NativePermissionMode::ProviderDefault,
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
        providers,
    )
}

#[rstest]
fn prompt_and_command_survive_mode_and_host_switches(form: NewSessionForm) {
    let form = std::cell::RefCell::new(form);
    let strategy = ("[^\\x00]{0,256}", "[^\\x00]{0,256}");
    TestRunner::new(Config {
        cases: 32,
        ..Config::default()
    })
    .run(&strategy, |(prompt, command)| {
        let mut form = form.borrow_mut();
        form.change_field("mode", "Agent");
        form.change_text(&prompt);
        form.change_field("mode", "Terminal");
        form.change_text(&command);
        assert!(form.change_field("host", "Another host"));
        form.set_directory("/project/another/selected".to_owned());
        assert!(form.change_field("host", "Current host"));
        assert_eq!(form.draft.cwd, "/project/current");
        assert_eq!(&form.draft.prompt, &prompt);
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
fn native_start_keeps_destination_profile_and_prompt_separate_from_argv(mut form: NewSessionForm) {
    form.change_text("Fix 'quotes'; $HOME `uname`\nand preserve this line");
    form.change_field("profile", "Work (work)");
    let invocation = form.invocation(&form.draft.cwd).expect("valid launch");
    assert_eq!(invocation.command, "agents.native.start");
    assert_eq!(invocation.caller, Caller::Internal);
    assert_eq!(
        invocation.target,
        form.destination()
            .map(|destination| destination.target.clone())
    );
    assert_eq!(
        &invocation.arguments[..3],
        ["codex", "/project/current", "/provider/custom-codex"]
    );
    let argv: Vec<String> = serde_json::from_str(&invocation.arguments[3]).expect("literal argv");
    assert_eq!(argv, ["--model", "configured-model"]);
    assert_eq!(
        &invocation.arguments[4..9],
        [
            "task-a123",
            "work",
            form.draft.identity.as_str(),
            "Fix 'quotes'; $HOME `uname`",
            "Fix 'quotes'; $HOME `uname`\nand preserve this line"
        ]
    );
    assert_eq!(invocation.arguments.get(14).map(String::as_str), None);
    assert_eq!(form.title(), "Fix 'quotes'; $HOME `uname`");
    assert!(form.spec(false).multiline);
    assert_eq!(form.spec(false).rows[0].label, "Start");
}

#[rstest]
fn initial_attachments_are_separate_local_paths_and_survive_mode_switches(form: NewSessionForm) {
    let form = std::cell::RefCell::new(form);
    TestRunner::new(Config {
        cases: 32,
        ..Config::default()
    })
    .run(
        &proptest::collection::vec("/tmp/[a-z]{1,24}\\.png", 1..17),
        |paths| {
            let mut form = form.borrow_mut();
            form.draft.attachments = paths
                .iter()
                .map(|path| NewSessionAttachment {
                    path: path.as_str().into(),
                    size_bytes: 10,
                    prompt_ranges: Vec::new(),
                    preview: None,
                    temporary: None,
                })
                .collect();
            form.change_field("mode", "Terminal");
            assert_eq!(form.spec(false).attachments.len(), 0);
            form.change_field("mode", "Agent");
            assert_eq!(form.spec(false).attachments.len(), paths.len());
            let invocation = form
                .invocation(&form.draft.cwd)
                .expect("local attachment draft");
            assert_eq!(
                invocation.arguments[9], "",
                "account remains a separate optional argument"
            );
            let submitted: Vec<String> =
                serde_json::from_str(&invocation.arguments[10]).expect("paths");
            assert_eq!(submitted, paths);
            assert!(
                invocation.arguments[8].is_empty(),
                "file-only draft keeps the prompt literal"
            );
            Ok(())
        },
    )
    .expect("attachment draft properties");
}

#[rstest]
fn attachment_only_enter_starts_an_agent_without_shell_fallback(form: NewSessionForm) {
    use bootty_ui::{
        gpui::{DialogId, DialogIntent},
        presentation::dialogs::{NEW_SESSION_ID, NewSessionPickerEvent},
    };
    let mut dialog = ready_dialog(form).expect("project discovery");
    let attachment = NewSessionAttachment {
        path: "/tmp/screenshot.png".into(),
        size_bytes: 10,
        prompt_ranges: Vec::new(),
        preview: None,
        temporary: None,
    };
    dialog.apply(
        &DialogIntent::AttachmentsChanged {
            dialog: DialogId::new(NEW_SESSION_ID),
            attachments: vec![attachment.clone()],
        },
        &[],
    );
    assert_eq!(dialog.draft().expect("draft").attachments, vec![attachment]);
    let Some(NewSessionPickerEvent::Submit(invocation)) =
        dialog.apply(&start_intent("enter-session"), &[])
    else {
        panic!("a file-only draft submits an agent without arming shell fallback");
    };
    assert_eq!(invocation.command, "agents.native.start");
    assert_eq!(invocation.arguments[10], "[\"/tmp/screenshot.png\"]");
    assert_eq!(
        dialog
            .draft()
            .expect("retained until launch completes")
            .attachments
            .len(),
        1
    );
}

#[rstest]
#[case("")]
#[case("  \n\t")]
#[case("printf 'one\\ntwo'\necho '$HOME'")]
fn terminal_text_distinguishes_shell_from_command(mut form: NewSessionForm, #[case] text: &str) {
    form.change_field("mode", "Terminal");
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
fn accepted_worktree_is_retained_when_provider_start_fails(
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
    assert_eq!(first.command, "agents.native.start");
    assert_eq!(first.arguments[1], "/project/accepted-checkout");
    let (sender, reply) = std::sync::mpsc::channel();
    dialog.started(reply);
    sender.send(CommandOutcome::Failed {
        code: "provider_failed".into(),
        message: "Provider could not start".into(),
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
    assert_eq!(retry.command, "agents.native.start");
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
    form.set_generated_names(bootty_agents::GeneratedSessionNames {
        title: "Improve composer".into(),
        slug: "improve-composer".into(),
    })
    .expect("valid generated names");
    assert_eq!(form.title(), "Improve composer");
    assert_eq!(form.draft.branch, "luan/improve-composer");
    assert_eq!(form.draft.folder, "current-improve-composer");
    for (field, value) in [
        ("branch", "  luan/custom  "),
        ("folder", " custom-checkout "),
        ("start-ref", " main "),
    ] {
        form.change_field(field, value);
    }
    assert_eq!(
        form.worktree_request(),
        Ok(Some(bootty_git::WorktreeRequest {
            branch: "luan/custom".to_owned(),
            name: Some("custom-checkout".to_owned()),
            start_ref: Some("main".to_owned()),
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
fn native_provider_choices_keep_codex_profile_and_disabled_start_keeps_draft(
    mut form: NewSessionForm,
) {
    form.change_text("A useful task title");
    form.change_field("profile", "Work (work)");
    form.change_field("provider", "pi");
    form.change_field("provider", "Claude");
    assert_eq!(form.draft.provider, "claude");
    assert_eq!(
        form.invocation(&form.draft.cwd)
            .expect("Claude launch")
            .arguments[0],
        "claude"
    );
    form.change_field("provider", "codex");
    let spec = form.spec(false);
    let provider = spec
        .fields
        .iter()
        .find(|field| field.id == "provider")
        .expect("provider choice");
    assert_eq!(
        provider.kind,
        bootty_ui::gpui::DialogFieldKind::Choice(vec![
            "Pi".to_owned(),
            "Codex".to_owned(),
            "Claude".to_owned()
        ])
    );
    assert_eq!(
        form.invocation(&form.draft.cwd).expect("launch").arguments[5],
        "work"
    );
    form.error = Some("The host is unavailable".to_owned());
    let spec = form.spec(true);
    assert!(!spec.rows[0].enabled);
    assert_eq!(
        spec.rows[0].detail.as_deref(),
        Some("The host is unavailable")
    );
    assert_eq!(form.draft.prompt, "A useful task title");
}

#[rstest]
#[case::claude("claude", "claude")]
#[case::pi("pi", "pi")]
#[case::codex("codex", "codex")]
#[case::unknown("unknown", "codex")]
fn retained_native_provider_keeps_supported_selection_and_account_drafts(
    mut form: NewSessionForm,
    #[case] provider: &str,
    #[case] expected: &str,
) {
    form.change_text("Keep this native task");
    provider.clone_into(&mut form.draft.provider);
    form.draft
        .profiles
        .insert(provider.to_owned(), "saved-account".to_owned());
    let destination = form.destination().expect("current destination").clone();
    let mut providers = AgentProvidersConfig::default();
    for (id, config) in [
        ("codex", &mut providers.codex),
        ("claude", &mut providers.claude),
        ("pi", &mut providers.pi),
    ] {
        config.profiles.insert(
            "saved-account".to_owned(),
            AgentProfileConfig {
                name: "Saved account".to_owned(),
                directory: Some(format!("/account/{id}/retained")),
                arguments: vec!["--model".to_owned(), format!("saved-{id}-model")],
            },
        );
    }
    let target = destination.target.clone();
    let form = NewSessionForm::new(form.draft, vec![destination], providers);
    assert_eq!(form.draft.provider, expected);
    assert_eq!(form.draft.prompt, "Keep this native task");
    assert_eq!(
        form.draft.profiles.get(provider).map(String::as_str),
        Some("saved-account")
    );
    let launch = form.invocation(&form.draft.cwd).expect("native launch");
    assert_eq!(launch.command, "agents.native.start");
    assert_eq!(launch.target, Some(target));
    assert_eq!(launch.arguments[0], expected);
    assert_eq!(launch.arguments[8], "Keep this native task");
    let argv: Vec<String> = serde_json::from_str(&launch.arguments[3]).expect("profile argv");
    if provider == expected {
        assert_eq!(launch.arguments[5], "saved-account");
        assert_eq!(
            argv,
            ["--model".to_owned(), format!("saved-{provider}-model")]
        );
    } else {
        assert_eq!(launch.arguments[5], "");
        assert_eq!(argv, Vec::<String>::new());
    }
}

#[rstest]
#[case("")]
#[case("printf '%s' '$HOME'")]
fn remote_host_preserves_native_and_terminal_creation(
    mut form: NewSessionForm,
    #[case] command: &str,
) {
    form.change_text("Retain the agent prompt");
    let mut destination = form.destination().expect("destination").clone();
    destination.remote =
        Some(bootty_config::config::SshRemoteConfig::for_host("remote-host").into());
    let mut form = NewSessionForm::new(
        form.draft,
        vec![destination.clone()],
        AgentProvidersConfig::default(),
    );
    let spec = form.spec(false);
    assert!(spec.rows[0].enabled);
    let invocation = form
        .invocation(&form.draft.cwd)
        .expect("remote native launch");
    assert_eq!(invocation.command, "agents.native.start");
    assert_eq!(invocation.target, Some(destination.target.clone()));
    form.change_field("mode", "Terminal");
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
    form.change_field("mode", "Agent");
    assert_eq!(form.draft.prompt, "Retain the agent prompt");
    assert_eq!(form.draft.command, command);
    assert!(form.spec(false).rows[0].enabled);
}

fn ready_dialog(
    form: NewSessionForm,
) -> anyhow::Result<bootty_ui::presentation::dialogs::NewSessionDialog> {
    ready_session_dialog(form, false)
}

fn ready_session_dialog(
    form: NewSessionForm,
    native: bool,
) -> anyhow::Result<bootty_ui::presentation::dialogs::NewSessionDialog> {
    use std::{
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };
    let (sender, wakes) = mpsc::channel();
    let repaint: bootty_mux::RepaintHandle = Arc::new(move || {
        let _ = sender.send(());
    });
    let mut dialog = if native {
        bootty_ui::presentation::dialogs::NewSessionDialog::open_native_form(form, &repaint)
    } else {
        bootty_ui::presentation::dialogs::NewSessionDialog::open_form(form, &repaint)
    };
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
fn empty_enter_arms_visibly_and_edits_reset_shell_fallback(mut form: NewSessionForm) {
    use bootty_ui::{
        gpui::{DialogId, DialogIntent},
        presentation::dialogs::{NEW_SESSION_ID, NewSessionPickerEvent},
    };
    "saved command draft".clone_into(&mut form.draft.command);
    let original_target = form.destination().expect("destination").target.clone();
    let mut dialog = ready_dialog(form).expect("project discovery");
    let enter = start_intent("enter-session");
    assert_eq!(dialog.apply(&enter, &[]), None);
    assert!(
        dialog
            .spec()
            .hint
            .expect("visible hint")
            .contains("Enter again")
    );
    for value in ["edited prompt", ""] {
        assert_eq!(
            dialog.apply(
                &DialogIntent::TextChanged {
                    dialog: DialogId::new(NEW_SESSION_ID),
                    value: value.to_owned()
                },
                &[]
            ),
            None
        );
    }
    assert_eq!(
        dialog.apply(
            &DialogIntent::TextChanged {
                dialog: DialogId::new(NEW_SESSION_ID),
                value: String::new(),
            },
            &[]
        ),
        None
    );
    assert_eq!(
        dialog.apply(&enter, &[]),
        None,
        "editing reset the first Enter"
    );
    let Some(NewSessionPickerEvent::Submit(invocation)) = dialog.apply(&enter, &[]) else {
        panic!("second empty Enter submits one shell");
    };
    assert_eq!(invocation.command, "session.create");
    assert_eq!(invocation.target, Some(original_target));
    assert_eq!(invocation.arguments[2], "[]");
    assert_eq!(
        dialog.draft().expect("draft").command,
        "saved command draft"
    );
    assert_eq!(dialog.draft().expect("draft").mode, NewSessionMode::Agent);
    assert_eq!(
        dialog.apply(&enter, &[]),
        None,
        "accepted Start cannot duplicate creation"
    );
}

#[rstest]
fn unchanged_composer_echoes_preserve_empty_enter_confirmation(form: NewSessionForm) {
    use bootty_ui::{
        gpui::{DialogId, DialogIntent},
        presentation::dialogs::{NEW_SESSION_ID, NewSessionPickerEvent},
    };
    let mut dialog = ready_dialog(form).expect("project discovery");
    let enter = start_intent("enter-session");
    assert_eq!(dialog.apply(&enter, &[]), None);
    for event in [
        DialogIntent::TextChanged {
            dialog: DialogId::new(NEW_SESSION_ID),
            value: String::new(),
        },
        DialogIntent::AttachmentsChanged {
            dialog: DialogId::new(NEW_SESSION_ID),
            attachments: Vec::new(),
        },
    ] {
        assert_eq!(dialog.apply(&event, &[]), None);
    }
    assert!(
        matches!(dialog.apply(&enter, &[]), Some(NewSessionPickerEvent::Submit(invocation)) if invocation.command == "session.create")
    );
}

#[rstest]
#[case("enter-session", true)]
#[case("start-session", true)]
#[case("start-session-background", false)]
fn launch_direction_is_retained_until_observed_success(
    mut form: NewSessionForm,
    #[case] action: &str,
    #[case] foreground: bool,
) {
    use bootty_ui::presentation::dialogs::NewSessionPickerEvent;
    form.change_text("Fix the native composer");
    let original_target = form.destination().expect("destination").target.clone();
    let mut dialog = ready_dialog(form).expect("project discovery");
    let Some(NewSessionPickerEvent::Submit(invocation)) = dialog.apply(&start_intent(action), &[])
    else {
        panic!("Ordinary creation submits immediately without waiting for naming");
    };
    assert_eq!(invocation.arguments[8], "Fix the native composer");
    assert_eq!(dialog.apply(&start_intent(action), &[]), None);
    assert_eq!(invocation.command, "agents.native.start");
    assert_eq!(invocation.target, Some(original_target));
    let (sender, receiver) = std::sync::mpsc::channel();
    dialog.started(receiver);
    sender.send(bootty_control::CommandOutcome::Success {
        value: serde_json::json!({"terminal": {"kind": "terminal", "handle": "issued", "generation": 9}}),
        warnings: Vec::new(),
    }).expect("observed outcome");
    assert!(
        matches!(dialog.poll(), Some(NewSessionPickerEvent::Started { foreground: observed, .. }) if observed == foreground)
    );
}

#[rstest]
#[case("enter-session")]
#[case("start-session")]
fn explicit_native_launcher_starts_without_a_prompt_and_keeps_account_and_terminal_drafts(
    mut form: NewSessionForm,
    #[case] action: &str,
) {
    form.draft.mode = NewSessionMode::Terminal;
    "saved terminal command".clone_into(&mut form.draft.command);
    form.change_field("profile", "Work (work)");
    let target = form.destination().expect("destination").target.clone();
    let cwd = form.draft.cwd.clone();
    let identity = form.draft.identity.clone();
    let mut dialog = ready_session_dialog(form, true).expect("discovery");
    let spec = dialog.spec();
    assert!(
        spec.fields
            .iter()
            .all(|field| matches!(field.id.as_str(), "provider" | "profile" | "permissions"))
    );
    assert!(spec.fields.iter().any(|field| field.id == "permissions"));
    dialog.apply(
        &bootty_ui::gpui::DialogIntent::FieldChanged {
            dialog: bootty_ui::gpui::DialogId::new(
                bootty_ui::presentation::dialogs::NEW_SESSION_ID,
            ),
            field: "permissions".to_owned(),
            value: "Supervised".to_owned(),
        },
        &[],
    );
    assert_eq!(
        dialog.draft().expect("draft").permissions,
        bootty_agents::NativePermissionMode::Supervised
    );
    assert!(spec.rows.iter().all(|row| row.id.0 != "choose-project"));
    for (field, value) in [("host", "elsewhere"), ("isolation", "New worktree")] {
        dialog.apply(
            &bootty_ui::gpui::DialogIntent::FieldChanged {
                dialog: bootty_ui::gpui::DialogId::new(
                    bootty_ui::presentation::dialogs::NEW_SESSION_ID,
                ),
                field: field.to_owned(),
                value: value.to_owned(),
            },
            &[],
        );
    }

    let Some(bootty_ui::presentation::dialogs::NewSessionPickerEvent::Submit(invocation)) =
        dialog.apply(&start_intent(action), &[])
    else {
        panic!("explicit native launch submits on its first activation");
    };
    assert_eq!(invocation.command, "agents.native.tab");
    assert!(invocation.arguments[7].starts_with("codex in "));
    assert_eq!(invocation.arguments[1], cwd);
    assert_eq!(invocation.arguments[6], identity);
    assert_eq!(invocation.target, Some(target));
    assert_eq!(invocation.arguments[0], "codex");
    assert_eq!(invocation.arguments[2], "/provider/custom-codex");
    assert_eq!(invocation.arguments[5], "work");
    assert_eq!(invocation.arguments[8], "");
    assert_eq!(
        dialog.draft().expect("draft").command,
        "saved terminal command"
    );
}

#[rstest]
#[case(Caller::CommandPalette)]
#[case(Caller::Keybinding)]
#[case(Caller::Cli)]
#[case(Caller::Socket)]
fn native_launcher_is_discoverable_and_resolves_through_each_shared_caller(#[case] caller: Caller) {
    let catalog = bootty_ui::commands::CommandCatalog::default();
    let invocation = bootty_control::CommandInvocation::from_action("agents.native.open", caller);
    let resolved = catalog.resolve(invocation).expect("shared native launcher");
    assert!(resolved.descriptor.palette);
    assert_eq!(
        resolved.descriptor.target,
        Some(ResourceKind::ApplicationWindow)
    );
    assert_eq!(resolved.invocation.caller, caller);
    assert!(matches!(
        resolved.executor,
        bootty_ui::commands::CommandExecutor::Core(
            bootty_ui::commands::CoreCommandExecutor::Keybind(
                bootty_ui::app_actions::KeybindAction::App(
                    bootty_ui::app_actions::AppAction::NewNativeAgentTab
                )
            )
        )
    ));
}

#[rstest]
#[case(false, "Provider model")]
#[case(true, "Provider model (provider-model)")]
fn discovered_creation_controls_send_provider_selectors_and_reset_on_profile_change(
    mut form: NewSessionForm,
    #[case] duplicate_name: bool,
    #[case] model_label: &str,
) {
    let model = bootty_agents::NativeModelOption {
        id: "provider-model".to_owned(),
        display_name: "Provider model".to_owned(),
        reasoning_efforts: vec!["medium".to_owned(), "high".to_owned()],
        default_reasoning_effort: Some("medium".to_owned()),
        is_default: true,
        is_legacy: false,
        is_favorite: false,
    };
    let mut catalog = vec![model.clone()];
    if duplicate_name {
        catalog.push(bootty_agents::NativeModelOption {
            id: "another-provider-model".to_owned(),
            is_default: false,
            is_legacy: false,
            is_favorite: false,
            ..model
        });
    }
    form.set_model_catalog(Ok(catalog));
    form.change_field("model", model_label);
    form.change_field("reasoning", "high");
    let spec = form.spec(false);
    assert_eq!(
        spec.fields
            .iter()
            .find(|field| field.id == "model")
            .unwrap()
            .value,
        model_label
    );
    assert_eq!(
        spec.fields
            .iter()
            .find(|field| field.id == "reasoning")
            .unwrap()
            .value,
        "high"
    );
    let invocation = form.invocation(&form.draft.cwd).unwrap();
    assert_eq!(invocation.arguments[9], "");
    assert_eq!(invocation.arguments[10], "");
    let selected: bootty_agents::NativeModelSelection =
        serde_json::from_str(&invocation.arguments[11]).unwrap();
    assert_eq!(selected.model, "provider-model");
    assert_eq!(selected.reasoning_effort.as_deref(), Some("high"));
    let mut surface = invocation.clone();
    "surface.create_agent".clone_into(&mut surface.command);
    surface.arguments.insert(0, "41".to_owned());
    surface.target = None;
    let resolved = bootty_ui::commands::CommandCatalog::default()
        .resolve(surface)
        .expect("captured tab/split admits the same attachment and model arguments");
    assert!(matches!(
        resolved.executor,
        bootty_ui::commands::CommandExecutor::Core(
            bootty_ui::commands::CoreCommandExecutor::Surface(
                bootty_ui::commands::SurfaceCommand::CreateAgent { id: 41, arguments }
            )
        ) if arguments == invocation.arguments
    ));
    let catalog = form.model_catalog_invocation().unwrap();
    assert_eq!(catalog.command, "agents.native.catalog-info");
    assert_eq!(catalog.arguments.len(), 6);
    assert_eq!(catalog.target, invocation.target);
    form.change_field("profile", "Work (work)");
    assert!(form.draft.model_selection.is_none());
    assert_eq!(form.model_options, []);
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
    let provider = form.draft.provider.clone();
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
        dialog.draft().unwrap().prompt,
        "Keep the draft when changing projects"
    );
    assert_eq!(dialog.draft().unwrap().provider, provider);
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
        AgentProvidersConfig::default(),
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
fn permission_choices_reset_with_provider_and_use_the_shared_invocation(mut form: NewSessionForm) {
    form.change_text("Keep this prompt");
    form.change_field("permissions", "Auto");
    assert_eq!(
        form.invocation(&form.draft.cwd).unwrap().arguments[14],
        "auto"
    );
    form.change_field("provider", "Claude");
    assert_eq!(
        form.draft.permissions,
        bootty_agents::NativePermissionMode::ProviderDefault
    );
    let spec = form.spec(false);
    let permissions = spec
        .fields
        .iter()
        .find(|field| field.id == "permissions")
        .unwrap();
    let bootty_ui::gpui::DialogFieldKind::Choice(options) = &permissions.kind else {
        panic!("Permission menu")
    };
    assert!(options.iter().any(|option| option == "Auto"));
    assert!(options.iter().any(|option| option == "Supervised"));
    form.change_field("permissions", "Supervised");
    assert_eq!(
        form.invocation(&form.draft.cwd).unwrap().arguments[14],
        "supervised"
    );
    assert_eq!(form.draft.prompt, "Keep this prompt");
}

#[rstest]
#[case(bootty_control::CommandOutcome::deadline_exceeded())]
#[case(bootty_control::CommandOutcome::Failed { code: "launch".into(), message: "Provider failed".into() })]
fn failed_creation_can_switch_provider_without_reusing_the_failed_task(
    mut form: NewSessionForm,
    #[case] outcome: bootty_control::CommandOutcome,
) {
    form.change_text("Keep this prompt");
    let original = form.draft.identity.clone();
    let mut dialog = ready_dialog(form).expect("discovery");
    let (sender, receiver) = std::sync::mpsc::channel();
    dialog.started(receiver);
    sender.send(outcome).expect("failed attempt");
    assert_eq!(dialog.poll(), None);
    let draft = dialog.draft().expect("draft");
    assert_ne!(draft.identity, original);
    assert_eq!(draft.prompt, "Keep this prompt");
    dialog.apply(
        &bootty_ui::gpui::DialogIntent::FieldChanged {
            dialog: dialog.spec().id,
            field: "provider".into(),
            value: "Pi".into(),
        },
        &[],
    );
    assert_eq!(dialog.draft().expect("draft").provider, "pi");
    assert!(!dialog.spec().busy);
}

#[rstest]
#[case("preferred", "high")]
#[case("unavailable", "medium")]
fn composer_defaults_resolve_advertised_values_and_preserve_explicit_choices(
    form: NewSessionForm,
    #[case] preferred: &str,
    #[case] effort: &str,
) {
    let mut providers = AgentProvidersConfig::default();
    providers.codex.default_model = preferred.to_owned();
    providers.codex.default_effort = "high".to_owned();
    let destination = form.destination().unwrap().clone();
    let mut form = NewSessionForm::new(form.draft, vec![destination], providers);
    let recommended = bootty_agents::NativeModelOption {
        id: "recommended".into(),
        display_name: "Recommended".into(),
        reasoning_efforts: vec!["medium".into()],
        default_reasoning_effort: Some("medium".into()),
        is_default: true,
        is_legacy: false,
        is_favorite: false,
    };
    let catalog = vec![
        recommended.clone(),
        bootty_agents::NativeModelOption {
            id: "preferred".into(),
            display_name: "Preferred".into(),
            reasoning_efforts: vec!["medium".into(), "high".into()],
            is_default: false,
            ..recommended
        },
    ];
    form.set_model_catalog(Ok(catalog.clone()));
    let selected = form.draft.model_selection.as_ref().unwrap();
    assert_eq!(
        selected.model,
        if preferred == "preferred" {
            "preferred"
        } else {
            "recommended"
        }
    );
    assert_eq!(selected.reasoning_effort.as_deref(), Some(effort));
    form.change_field("model", "Preferred");
    form.change_field("reasoning", "medium");
    form.set_model_catalog(Ok(catalog));
    let selected: bootty_agents::NativeModelSelection =
        serde_json::from_str(&form.invocation(&form.draft.cwd).unwrap().arguments[11]).unwrap();
    assert_eq!(selected.model, "preferred");
    assert_eq!(selected.reasoning_effort.as_deref(), Some("medium"));
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
            provider: "claude".to_owned(),
            isolated: true,
            branch_prefix: "work/".to_owned(),
            start_ref: "main".to_owned(),
            ..ProjectSettings::default()
        },
    }]);
    assert_eq!(form.draft.provider, "claude");
    assert_eq!(
        form.spec(false)
            .project_labels
            .get(&form.draft.cwd)
            .map(String::as_str),
        Some("My project")
    );
    assert!(form.draft.isolated);
    assert_eq!(form.draft.start_ref, "main");
    let selection = bootty_agents::NativeModelSelection {
        model: "chosen".to_owned(),
        reasoning_effort: None,
    };
    form.draft.model_selection = Some(selection.clone());
    form.set_directory("/new/worktree".to_owned());
    assert_eq!(form.draft.provider, "claude");
    assert_eq!(
        form.draft
            .model_selection
            .as_ref()
            .map(|value| &value.model),
        Some(&selection.model)
    );
}

#[rstest]
#[case("provider", "Claude")]
#[case("profile", "Work (work)")]
fn permission_defaults_follow_the_selected_account_without_overriding_its_policy(
    mut form: NewSessionForm,
    #[case] field: &str,
    #[case] value: &str,
) {
    use bootty_agents::{NativePermissionMode, NativeProviderCatalog};
    form.change_text("Keep the draft");
    form.set_provider_catalog(Ok(NativeProviderCatalog {
        models: Vec::new(),
        permissions: Some(NativePermissionMode::FullAccess),
    }));
    assert_eq!(
        form.permission_selection(),
        NativePermissionMode::FullAccess
    );
    assert_eq!(
        form.draft.permissions,
        NativePermissionMode::ProviderDefault
    );
    assert_eq!(
        form.invocation(&form.draft.cwd).unwrap().arguments.get(14),
        None
    );
    let spec = form.spec(false);
    assert_eq!(
        spec.fields
            .iter()
            .find(|field| field.id == "permissions")
            .unwrap()
            .value,
        "Full access"
    );
    form.change_field("permissions", "Supervised");
    // An asynchronous catalog refresh must preserve an explicit user choice.
    form.set_provider_catalog(Ok(NativeProviderCatalog {
        models: Vec::new(),
        permissions: Some(NativePermissionMode::FullAccess),
    }));
    assert_eq!(
        form.permission_selection(),
        NativePermissionMode::Supervised
    );
    form.change_field(field, value);
    assert_eq!(
        form.draft.permissions,
        NativePermissionMode::ProviderDefault
    );
    assert_eq!(
        form.permission_selection(),
        NativePermissionMode::ProviderDefault
    );
    form.set_provider_catalog(Ok(NativeProviderCatalog {
        models: Vec::new(),
        permissions: Some(NativePermissionMode::AutoAcceptEdits),
    }));
    assert_eq!(
        form.permission_selection(),
        NativePermissionMode::AutoAcceptEdits
    );
    assert_eq!(
        form.invocation(&form.draft.cwd).unwrap().arguments.get(14),
        None
    );
    assert_eq!(form.draft.prompt, "Keep the draft");
}

#[rstest]
fn provider_catalogs_restore_only_for_the_exact_provider_profile_and_target(
    mut form: NewSessionForm,
) {
    use bootty_agents::{NativeModelOption, NativePermissionMode, NativeProviderCatalog};

    let model = |id: &str| NativeModelOption {
        id: id.to_owned(),
        display_name: id.to_owned(),
        reasoning_efforts: Vec::new(),
        default_reasoning_effort: None,
        is_default: true,
        is_legacy: false,
        is_favorite: false,
    };
    let catalog = |id: &str, permissions| NativeProviderCatalog {
        models: vec![model(id)],
        permissions,
    };

    let codex_default = form
        .model_catalog_invocation()
        .expect("current catalog target");
    form.set_provider_catalog_for(
        &codex_default,
        Ok(catalog(
            "codex-default",
            Some(NativePermissionMode::FullAccess),
        )),
    );
    form.change_field("provider", "Claude");
    assert_eq!(
        form.model_options,
        Vec::<bootty_agents::NativeModelOption>::new()
    );
    let claude = form
        .model_catalog_invocation()
        .expect("Claude catalog target");
    form.set_provider_catalog_for(
        &claude,
        Ok(catalog(
            "claude-default",
            Some(NativePermissionMode::AutoAcceptEdits),
        )),
    );
    form.change_field("provider", "Codex");
    assert_eq!(form.model_options[0].id, "codex-default");
    assert_eq!(
        form.permission_selection(),
        NativePermissionMode::FullAccess
    );

    form.change_field("profile", "Work (work)");
    assert_eq!(
        form.model_options,
        Vec::<bootty_agents::NativeModelOption>::new()
    );
    let codex_work = form
        .model_catalog_invocation()
        .expect("profile catalog target");
    assert_ne!(codex_work, codex_default);
    form.set_provider_catalog_for(
        &codex_work,
        Ok(catalog(
            "codex-work",
            Some(NativePermissionMode::Supervised),
        )),
    );
    assert_eq!(form.model_options[0].id, "codex-work");

    form.set_directory("/project/other".to_owned());
    assert_eq!(
        form.model_options,
        Vec::<bootty_agents::NativeModelOption>::new()
    );
    let other_project = form
        .model_catalog_invocation()
        .expect("project catalog target");
    assert_ne!(other_project, codex_work);
    assert!(form.models_loading);
}

#[rstest]
fn pi_uses_provider_default_and_hides_permission_selection(mut form: NewSessionForm) {
    use bootty_agents::NativePermissionMode;

    form.change_field("provider", "Pi");
    assert_eq!(
        form.draft.permissions,
        NativePermissionMode::ProviderDefault
    );
    form.draft.permissions = NativePermissionMode::Supervised;
    assert!(
        form.spec(false)
            .fields
            .iter()
            .all(|field| field.id != "permissions")
    );
    assert_eq!(
        form.invocation(&form.draft.cwd)
            .expect("Pi launch")
            .arguments
            .get(14),
        None
    );
}

#[rstest]
fn refreshing_a_warm_catalog_preserves_choices_on_failure(mut form: NewSessionForm) {
    let invocation = form.model_catalog_invocation().unwrap();
    form.set_provider_catalog_for(
        &invocation,
        Ok(bootty_agents::NativeProviderCatalog {
            models: vec![bootty_agents::NativeModelOption {
                id: "current".into(),
                display_name: "Current model".into(),
                reasoning_efforts: vec!["medium".into()],
                default_reasoning_effort: Some("medium".into()),
                is_default: true,
                is_legacy: false,
                is_favorite: false,
            }],
            permissions: Some(bootty_agents::NativePermissionMode::FullAccess),
        }),
    );
    form.set_provider_catalog_for(&invocation, Err("Provider unavailable".into()));
    assert_eq!(form.model_options[0].id, "current");
    assert!(!form.models_loading);
    assert_eq!(form.model_error.as_deref(), Some("Provider unavailable"));
}
