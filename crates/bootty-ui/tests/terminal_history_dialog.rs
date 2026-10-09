//! Saved metadata, captured account/destination, and explicit history activation.

use std::sync::mpsc;

use bootty_agents::{AgentKind, TerminalHistoryEntry};
use bootty_control::{CommandOutcome, CommandTarget, ResourceKind};
use bootty_ui::gpui::{ActionId, DialogId, DialogIntent, DialogPayload, RowId};
use bootty_ui::presentation::terminal_history::{
    TerminalHistoryContext, TerminalHistoryDialog, TerminalHistoryEvent,
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

#[fixture]
fn dialog() -> TerminalHistoryDialog {
    TerminalHistoryDialog::new(TerminalHistoryContext {
        provider: AgentKind::Pi,
        binding: CommandTarget {
            kind: ResourceKind::Binding,
            handle: "issued-binding".to_owned(),
            generation: 7,
        },
        session: CommandTarget {
            kind: ResourceKind::Session,
            handle: "issued-session".to_owned(),
            generation: 11,
        },
        cwd: Some("/project/current".to_owned()),
        profile: "work".to_owned(),
        program: "/provider/pi".to_owned(),
        arguments: vec!["--model".to_owned(), "configured".to_owned()],
    })
}

fn activate(action: &str, row: &str) -> DialogIntent {
    DialogIntent::Activate {
        dialog: DialogId::new("terminal-agent-history"),
        action: ActionId::new(action),
        row: RowId::new(row),
        payload: DialogPayload::None,
    }
}

#[rstest]
fn query_scope_switch_keeps_exact_account_and_binding(mut dialog: TerminalHistoryDialog) {
    let Some(TerminalHistoryEvent::Request {
        invocation,
        opening,
    }) = dialog.query()
    else {
        panic!("current query")
    };
    assert!(!opening);
    assert_eq!(invocation.arguments, ["/project/current", "work"]);
    let target = invocation.target;
    let Some(TerminalHistoryEvent::Request {
        invocation,
        opening,
    }) = dialog.apply(&activate("toggle-scope", "scope"))
    else {
        panic!("all projects query")
    };
    assert!(!opening);
    assert_eq!(invocation.target, target);
    assert_eq!(invocation.arguments, ["", "work"]);
    assert!(
        dialog
            .spec()
            .footer
            .is_some_and(|footer| footer.contains("All projects"))
    );
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn saved_agent_opens_without_a_terminal_and_keeps_the_provider_handle(#[case] provider: AgentKind) {
    let binding = CommandTarget {
        kind: ResourceKind::Binding,
        handle: "captured-space".into(),
        generation: 7,
    };
    let mut dialog = TerminalHistoryDialog::new(TerminalHistoryContext {
        provider,
        binding: binding.clone(),
        session: binding.clone(),
        cwd: None,
        profile: "work".into(),
        program: "/provider/program".into(),
        arguments: Vec::new(),
    });
    let Some(TerminalHistoryEvent::Request { invocation, .. }) = dialog.query() else {
        panic!("Query")
    };
    assert_eq!(invocation.target, Some(binding.clone()));
    assert_eq!(invocation.arguments, ["", "work"]);
    let entry = TerminalHistoryEntry {
        provider,
        session_id: "external-session".into(),
        title: Some("Previous work".into()),
        created_at: None,
        updated_at: None,
        cwd: "/original/project".into(),
        account_directory: "/account/work".into(),
        resume_id: if provider == AgentKind::Pi {
            "/account/work/sessions/external-session.jsonl".into()
        } else {
            "external-session".into()
        },
    };
    let (sender, receiver) = mpsc::channel();
    dialog.started(receiver, false);
    sender
        .send(CommandOutcome::Success {
            value: serde_json::json!({"entries":[entry],"account_directory":"/account/work"}),
            warnings: Vec::new(),
        })
        .unwrap();
    assert!(dialog.poll().is_none());
    let Some(TerminalHistoryEvent::Request {
        invocation,
        opening,
    }) = dialog.apply(&activate("open-history", "0"))
    else {
        panic!("Open")
    };
    assert!(opening);
    assert_eq!(invocation.command, "agents.native.start");
    assert_eq!(invocation.target, Some(binding));
    assert_eq!(invocation.arguments[1], "/original/project");
    assert_eq!(invocation.arguments[8], "");
    assert_eq!(
        serde_json::from_str::<TerminalHistoryEntry>(&invocation.arguments[16]).unwrap(),
        entry
    );
    let completed = serde_json::json!({"native":{"id":"accepted-conversation","generation":1}});
    let (sender, receiver) = mpsc::channel();
    dialog.started(receiver, true);
    sender
        .send(CommandOutcome::Success {
            value: completed.clone(),
            warnings: Vec::new(),
        })
        .unwrap();
    let Some(TerminalHistoryEvent::Opened(value)) = dialog.poll() else {
        panic!("Accepted conversation must reach the shared focus path")
    };
    assert_eq!(value, completed);
}

#[rstest]
fn selection_never_launches_and_open_keeps_saved_identity_account_and_session(
    mut dialog: TerminalHistoryDialog,
) {
    let (sender, receiver) = mpsc::channel();
    dialog.started(receiver, false);
    assert!(
        dialog
            .spec()
            .rows
            .iter()
            .any(|row| row.label.contains("Loading"))
    );
    let entry = TerminalHistoryEntry {
        provider: AgentKind::Pi,
        session_id: "saved-id".to_owned(),
        title: Some("Saved purpose".to_owned()),
        created_at: Some(1_767_225_600_000),
        updated_at: Some(1_767_225_700_000),
        cwd: "/project/current".into(),
        account_directory: "/account/work".into(),
        resume_id: "/account/work/sessions/project/saved-id.jsonl".to_owned(),
    };
    sender
        .send(CommandOutcome::Success {
            value: serde_json::json!({"entries": [entry], "account_directory": "/account/work"}),
            warnings: Vec::new(),
        })
        .unwrap_or_else(|error| panic!("deliver command outcome: {error}"));
    assert!(dialog.poll().is_none());
    let spec = dialog.spec();
    assert!(spec.rows.iter().any(|row| row.label == "Saved purpose"));
    assert!(spec.rows.iter().any(|row| {
        row.trailing
            .as_ref()
            .is_some_and(|dates| dates.contains("Updated"))
    }));
    let selected = DialogIntent::SelectionChanged {
        dialog: spec.id,
        row: RowId::new("0"),
    };
    assert!(dialog.apply(&selected).is_none());
    let Some(TerminalHistoryEvent::Request {
        invocation,
        opening,
    }) = dialog.apply(&activate("open-history", "0"))
    else {
        panic!("explicit Open")
    };
    assert!(opening);
    assert_eq!(invocation.command, "agents.pi.resume");
    assert_eq!(
        invocation
            .target
            .as_ref()
            .map(|target| (target.handle.as_str(), target.generation)),
        Some(("issued-session", 11))
    );
    assert_eq!(
        invocation.arguments,
        [
            "/account/work/sessions/project/saved-id.jsonl",
            "/project/current",
            "/provider/pi",
            "[\"--model\",\"configured\"]",
            "work",
            "/account/work"
        ]
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn failures_stay_visible_and_opening_waits_for_its_observed_outcome(
    mut dialog: TerminalHistoryDialog,
    #[case] opening: bool,
) {
    let (sender, receiver) = mpsc::channel();
    dialog.started(receiver, opening);
    assert_eq!(dialog.is_in_flight(), opening);
    let dismiss = DialogIntent::Dismiss {
        dialog: DialogId::new("terminal-agent-history"),
    };
    if opening {
        assert!(dialog.apply(&dismiss).is_none());
    }
    sender
        .send(CommandOutcome::Unsupported {
            message: "Remote history is unavailable".to_owned(),
        })
        .unwrap_or_else(|error| panic!("deliver command outcome: {error}"));
    assert!(dialog.poll().is_none());
    assert!(!dialog.is_in_flight());
    assert!(
        dialog
            .spec()
            .rows
            .iter()
            .any(|row| row.label.contains("Remote history is unavailable"))
    );
    assert!(matches!(
        dialog.apply(&dismiss),
        Some(TerminalHistoryEvent::Close)
    ));
}

#[rstest]
fn empty_history_still_freezes_account_before_scope_changes(mut dialog: TerminalHistoryDialog) {
    let (sender, receiver) = mpsc::channel();
    dialog.started(receiver, false);
    sender
        .send(CommandOutcome::Success {
            value: serde_json::json!({"entries": [], "account_directory": "/account/work", "omitted_entries": 17}),
            warnings: Vec::new(),
        })
        .unwrap_or_else(|error| panic!("deliver command outcome: {error}"));
    assert!(dialog.poll().is_none());
    assert!(
        dialog
            .spec()
            .rows
            .iter()
            .any(|row| row.label.contains("17 additional sessions omitted") && !row.enabled)
    );
    assert!(
        dialog
            .spec()
            .rows
            .iter()
            .any(|row| row.label.contains("No saved sessions"))
    );
    let Some(TerminalHistoryEvent::Request {
        invocation,
        opening,
    }) = dialog.apply(&activate("toggle-scope", "scope"))
    else {
        panic!("scope query")
    };
    assert!(!opening);
    assert_eq!(invocation.arguments, ["", "work", "/account/work"]);
    assert_eq!(
        invocation.target.as_ref().map(|target| target.generation),
        Some(7)
    );
}

#[rstest]
fn unified_history_filters_and_resumes_with_each_providers_captured_account() {
    let binding = CommandTarget {
        kind: ResourceKind::Binding,
        handle: "history-host".into(),
        generation: 9,
    };
    let providers = [AgentKind::Codex, AgentKind::Claude, AgentKind::Pi];
    let mut dialog = TerminalHistoryDialog::all_providers(
        providers
            .into_iter()
            .map(|provider| TerminalHistoryContext {
                provider,
                binding: binding.clone(),
                session: binding.clone(),
                cwd: Some("/project".into()),
                profile: format!("{provider}-profile"),
                program: format!("/bin/{provider}"),
                arguments: vec!["--configured".into()],
            })
            .collect(),
    )
    .unwrap();
    let mut request = dialog.query();
    for (index, provider) in providers.into_iter().enumerate() {
        let Some(TerminalHistoryEvent::Request {
            invocation,
            opening,
        }) = request
        else {
            panic!("provider query")
        };
        assert!(!opening);
        assert_eq!(invocation.command, format!("agents.{provider}.history"));
        assert_eq!(invocation.target, Some(binding.clone()));
        assert_eq!(
            invocation.arguments,
            [String::new(), format!("{provider}-profile")]
        );
        let entry = TerminalHistoryEntry {
            provider,
            session_id: format!("saved-{provider}"),
            title: Some(format!("Work with {provider}")),
            created_at: None,
            updated_at: Some(i64::try_from(index).unwrap()),
            cwd: "/project".into(),
            account_directory: format!("/account/{provider}").into(),
            resume_id: if provider == AgentKind::Pi {
                "/account/pi/saved.jsonl".into()
            } else {
                format!("saved-{provider}")
            },
        };
        let (sender, receiver) = mpsc::channel();
        dialog.started(receiver, false);
        sender.send(CommandOutcome::Success {value: serde_json::json!({"entries":[entry], "account_directory":format!("/account/{provider}")}), warnings: Vec::new()}).unwrap();
        request = dialog.poll();
    }
    assert!(request.is_none());
    assert_eq!(dialog.spec().title, "History");
    assert_eq!(
        dialog.spec().placement,
        bootty_ui::gpui::DialogPlacement::FullView
    );
    assert_eq!(
        dialog
            .spec()
            .rows
            .iter()
            .filter(|row| row
                .action
                .as_ref()
                .is_some_and(|action| action.id.0 == "open-history"))
            .count(),
        3
    );
    for provider in providers {
        assert!(
            dialog
                .apply(&DialogIntent::FieldChanged {
                    dialog: DialogId::new("terminal-agent-history"),
                    field: "provider".into(),
                    value: provider.to_string()
                })
                .is_none()
        );
        let Some(TerminalHistoryEvent::Request {
            invocation,
            opening,
        }) = dialog.apply(&activate("open-history", "0"))
        else {
            panic!("resume")
        };
        assert!(opening);
        assert_eq!(invocation.target, Some(binding.clone()));
        assert_eq!(invocation.arguments[0], provider.to_string());
        assert_eq!(invocation.arguments[2], format!("/bin/{provider}"));
        assert_eq!(invocation.arguments[5], format!("{provider}-profile"));
        let resumed: TerminalHistoryEntry =
            serde_json::from_str(&invocation.arguments[16]).unwrap();
        assert_eq!(resumed.provider, provider);
        assert_eq!(
            resumed.account_directory,
            std::path::PathBuf::from(format!("/account/{provider}"))
        );
    }
    let Some(TerminalHistoryEvent::Request { invocation, .. }) =
        dialog.apply(&activate("toggle-scope", "scope"))
    else {
        panic!("scope query")
    };
    assert_eq!(
        invocation.arguments,
        ["/project", "codex-profile", "/account/codex"]
    );
}

#[rstest]
fn unified_history_provider_failures_do_not_leak_into_other_filters() {
    let binding = CommandTarget {
        kind: ResourceKind::Binding,
        handle: "history-host".into(),
        generation: 9,
    };
    let mut dialog = TerminalHistoryDialog::all_providers(
        [AgentKind::Codex, AgentKind::Claude]
            .into_iter()
            .map(|provider| TerminalHistoryContext {
                provider,
                binding: binding.clone(),
                session: binding.clone(),
                cwd: None,
                profile: String::new(),
                program: provider.default_program().into(),
                arguments: Vec::new(),
            })
            .collect(),
    )
    .unwrap();
    assert!(dialog.query().is_some());
    let (sender, receiver) = mpsc::channel();
    dialog.started(receiver, false);
    sender
        .send(CommandOutcome::Success {
            value: serde_json::json!({"entries":[], "account_directory":"/codex"}),
            warnings: Vec::new(),
        })
        .unwrap();
    assert!(dialog.poll().is_some());
    let (sender, receiver) = mpsc::channel();
    dialog.started(receiver, false);
    sender
        .send(CommandOutcome::Failed {
            code: "provider_error".into(),
            message: "private provider payload".into(),
        })
        .unwrap();
    assert!(dialog.poll().is_none());
    assert!(
        dialog
            .spec()
            .rows
            .iter()
            .any(|row| row.label.contains("claude history could not load"))
    );
    assert!(
        !dialog
            .spec()
            .rows
            .iter()
            .any(|row| row.label.contains("private"))
    );
    dialog.apply(&DialogIntent::FieldChanged {
        dialog: DialogId::new("terminal-agent-history"),
        field: "provider".into(),
        value: "codex".into(),
    });
    assert!(
        !dialog
            .spec()
            .rows
            .iter()
            .any(|row| row.label.contains("could not load"))
    );
}
