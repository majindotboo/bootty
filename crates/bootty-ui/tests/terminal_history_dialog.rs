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
