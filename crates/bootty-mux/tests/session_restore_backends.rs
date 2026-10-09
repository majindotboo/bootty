use std::cell::RefCell;

use anyhow::Result;
use bootty_host::{CommandOutput, CommandRunner};
use bootty_mux::{
    command::{MuxCommand, MuxSplitDirection},
    session_snapshot::{SavedTerminalPane, SavedTerminalSession, SavedTerminalWindow},
    snapshot::{MuxPaneLayout, MuxPaneSplitDirection, MuxSessionTag},
    tmux::TmuxBackend,
    tmux_compatible_layout::{parse_with_checksum, restore_window_layout},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};

#[derive(Default)]
struct BackendRunner {
    calls: RefCell<Vec<Vec<String>>>,
    fail: Option<&'static str>,
    separator: Option<&'static str>,
}

impl CommandRunner for BackendRunner {
    fn run(&self, _program: &str, args: &[String]) -> Result<CommandOutput> {
        self.calls.borrow_mut().push(args.to_vec());
        if self
            .fail
            .is_some_and(|command| args.first().is_some_and(|arg| arg == command))
        {
            return Ok(CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "fixture backend failure".into(),
            });
        }
        let stdout = match args.first().map(String::as_str) {
            Some("new-session") => "$9\x1f@10\x1f%20\n".into(),
            Some("split-window") => format!(
                "$9\x1f@10\x1f%{}\n",
                self.calls.borrow().iter()
                    .filter(|args| args.first().is_some_and(|arg| arg == "split-window"))
                    .count().saturating_add(20)
            ),
            Some("show-options") => "/bin/sh\n".into(),
            Some("new-window") => "$9\x1f@11\x1f%23\n".into(),
            Some("list-sessions") => {
                "s\x1f$9\x1fsession\x1flogical\x1fspace\x1f0\x1f1\x1f%20\x1f123\x1f/tmp\x1fsh\np\x1f$9\x1f@10\x1f0\x1ffirst\x1f1\x1f1\x1f%20\x1fhidden\x1f0\x1f/tmp\x1fsh\n".into()
            }
            _ => String::new(),
        };
        Ok(CommandOutput {
            success: true,
            stdout: stdout.replace('\x1f', self.separator.unwrap_or("\x1f")),
            stderr: String::new(),
        })
    }

    fn run_with_input(
        &self,
        program: &str,
        args: &[String],
        _input: Vec<u8>,
    ) -> Result<CommandOutput> {
        self.run(program, args)
    }
}

fn pane(id: &str, cwd: &str) -> SavedTerminalPane {
    SavedTerminalPane {
        native_agent: None,
        id: id.into(),
        backend_id: id.into(),
        cwd: cwd.into(),
        cols: 80,
        rows: 24,
        text: "saved plain history".into(),
        omitted_lines: 0,
    }
}

#[fixture]
fn saved() -> SavedTerminalSession {
    SavedTerminalSession {
        captured_at: 1,
        session_id: "logical".into(),
        backend_id: "$8".into(),
        active_window_id: Some("old-second".into()),
        windows: vec![
            SavedTerminalWindow {
                id: "old-first".into(),
                backend_id: "old-first".into(),
                title: "first".into(),
                focused_pane_id: "old-b".into(),
                layout: Some(MuxPaneLayout::Split {
                    direction: MuxPaneSplitDirection::Right,
                    ratio_millis: 300,
                    first: Box::new(MuxPaneLayout::Pane("old-a".into())),
                    second: Box::new(MuxPaneLayout::Pane("old-b".into())),
                }),
                panes: vec![pane("old-a", "/tmp"), pane("old-b", "/var/tmp")],
            },
            SavedTerminalWindow {
                id: "old-second".into(),
                backend_id: "old-second".into(),
                title: "second".into(),
                focused_pane_id: "old-c".into(),
                layout: None,
                panes: vec![pane("old-c", "/tmp")],
            },
        ],
    }
}

fn tag() -> MuxSessionTag {
    MuxSessionTag {
        identity: Some("logical".into()),
        space: Some("space".into()),
    }
}

#[rstest]
#[case("\x1f")]
#[case("\\037")]
#[case("_")]
fn tmux_restores_saved_topology_using_created_backend_ids(
    saved: SavedTerminalSession,
    #[case] separator: &'static str,
) {
    let mut backend = TmuxBackend::with_runner(
        "tmux",
        BackendRunner {
            separator: Some(separator),
            ..BackendRunner::default()
        },
    );
    backend
        .execute(MuxCommand::RestoreSession {
            session_id: "new-name".into(),
            tag: tag(),
            snapshot: saved,
        })
        .expect("restore");
    let calls = backend.runner().calls.borrow();
    let create = &calls[0];
    assert_eq!(
        &create[0..5],
        &[
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{session_id}\x1f#{window_id}\x1f#{pane_id}"
        ]
    );
    let tags = calls
        .iter()
        .find(|args| args.first().is_some_and(|arg| arg == "set-option"))
        .expect("stamp");
    assert!(tags.windows(2).any(|args| args == ["-t", "$9"]));
    let custom = calls
        .iter()
        .filter(|args| args.first().is_some_and(|arg| arg == "select-layout"))
        .find_map(|args| {
            args.last()
                .and_then(|layout| parse_with_checksum(layout).ok())
        })
        .expect("custom layout");
    let MuxPaneLayout::Split {
        first,
        second,
        direction,
        ..
    } = custom
    else {
        panic!("saved split was dropped");
    };
    assert_eq!(
        (direction, *first, *second),
        (
            MuxPaneSplitDirection::Right,
            MuxPaneLayout::Pane("%21".into()),
            MuxPaneLayout::Pane("%22".into())
        )
    );
    assert!(
        calls
            .iter()
            .any(|args| args == &["select-pane", "-t", "%22"])
    );
    assert_eq!(
        calls.last().expect("focus"),
        &["select-window", "-t", "@11"]
    );
    assert!(
        !calls
            .iter()
            .any(|args| args.iter().any(|arg| arg == "saved plain history"))
    );
}

#[rstest]
fn restore_rejects_a_tag_that_cannot_preserve_saved_identity(saved: SavedTerminalSession) {
    let mut backend = TmuxBackend::with_runner("tmux", BackendRunner::default());
    backend
        .execute(MuxCommand::RestoreSession {
            session_id: "new-name".into(),
            tag: MuxSessionTag::default(),
            snapshot: saved,
        })
        .expect_err("missing logical identity");
    assert!(backend.runner().calls.borrow().is_empty());
}

#[rstest]
#[case("new-session", false)]
#[case("set-option", true)]
#[case("split-window", true)]
#[case("new-window", true)]
#[case("display-message", true)]
#[case("respawn-pane", true)]
fn failed_restore_removes_only_its_successfully_created_session(
    saved: SavedTerminalSession,
    #[case] fail: &'static str,
    #[case] rollback: bool,
) {
    let mut backend = TmuxBackend::with_runner(
        "tmux",
        BackendRunner {
            fail: Some(fail),
            ..BackendRunner::default()
        },
    );
    backend
        .execute(MuxCommand::RestoreSession {
            session_id: "new-name".into(),
            tag: tag(),
            snapshot: saved,
        })
        .expect_err("injected failure");
    let calls = backend.runner().calls.borrow();
    let kills = calls
        .iter()
        .filter(|args| args.first().is_some_and(|arg| arg == "kill-session"))
        .collect::<Vec<_>>();
    if rollback {
        assert_eq!(
            kills,
            vec![&vec![
                "kill-session".to_owned(),
                "-t".to_owned(),
                "$9".to_owned()
            ]]
        );
    } else {
        assert_eq!(kills, Vec::<&Vec<String>>::new());
    }
}

#[rstest]
#[case(vec!["literal;".into()], vec!["'literal;'".into()])]
#[case(vec!["program".into(), "$(literal)".into(), "last;".into()], vec!["program".into(), "$(literal)".into(), "last\\;".into()])]
fn create_pane_keeps_program_arguments_literal_under_exact_parent(
    #[case] argv: Vec<String>,
    #[case] expected: Vec<String>,
) {
    let mut backend = TmuxBackend::with_runner("tmux", BackendRunner::default());
    backend
        .execute(MuxCommand::CreatePane {
            session_id: "$9".into(),
            pane_id: Some("%20".into()),
            direction: MuxSplitDirection::Down,
            cwd: Some("/tmp/path;".into()),
            argv,
        })
        .expect("create pane");
    let calls = backend.runner().calls.borrow();
    let args = calls.last().expect("split request");
    assert_eq!(
        &args[..7],
        &[
            "split-window",
            "-v",
            "-t",
            "%20",
            "-c",
            "/tmp/path\\;",
            "--"
        ]
    );
    assert_eq!(&args[7..], &expected);
}

proptest! {
    #[test]
    fn remapped_custom_layout_preserves_axes_panes_and_ratios(right in any::<bool>(), ratio in 1_u16..1000) {
        let direction = if right { MuxPaneSplitDirection::Right } else { MuxPaneSplitDirection::Down };
        let saved = SavedTerminalWindow { id: "window".into(), backend_id: "window".into(), title: "title".into(), focused_pane_id: "a".into(),
            layout: Some(MuxPaneLayout::Split { direction: direction.clone(), ratio_millis: ratio,
                first: Box::new(MuxPaneLayout::Pane("a".into())), second: Box::new(MuxPaneLayout::Pane("b".into())) }),
            panes: vec![pane("a", "/tmp"), pane("b", "/tmp")] };
        let (cols, rows, encoded) = restore_window_layout(&saved, &["%90".into(), "%91".into()]).expect("encode");
        let MuxPaneLayout::Split { direction: actual_direction, ratio_millis, first, second } = parse_with_checksum(&encoded).expect("parse") else { panic!("missing split"); };
        assert_eq!((actual_direction, *first, *second), (direction, MuxPaneLayout::Pane("%90".into()), MuxPaneLayout::Pane("%91".into())));
        // Cell rounding and the backend minimum of two cells bound the ratio difference.
        let extent = if right { cols } else { rows };
        let tolerance = 2500_u16.checked_div(extent.saturating_sub(1)).expect("nonzero layout extent").saturating_add(1);
        prop_assert!(ratio_millis.abs_diff(ratio) <= tolerance);
    }
}
