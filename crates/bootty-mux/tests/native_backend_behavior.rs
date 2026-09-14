use anyhow::{Context as _, Result};
use assert_fs::TempDir;
use bootty_mux::{
    backend::MuxBackend,
    command::{MuxCommand, MuxSplitDirection},
    native::NativeBackend,
    snapshot::{MuxSession, MuxSessionTag},
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

#[derive(Clone, Copy)]
enum PaneOperation {
    Split,
    Kill,
    Close,
}

impl PaneOperation {
    fn command(self, pane_id: String) -> MuxCommand {
        let session_id = "session".to_owned();
        match self {
            Self::Split => MuxCommand::SplitPane {
                session_id,
                pane_id: Some(pane_id),
                direction: MuxSplitDirection::Down,
            },
            Self::Kill => MuxCommand::KillPane {
                session_id,
                pane_id: Some(pane_id),
            },
            Self::Close => MuxCommand::ClosePane {
                session_id,
                pane_id: Some(pane_id),
            },
        }
    }
}

#[fixture]
fn backend() -> Result<(TempDir, NativeBackend)> {
    let directory = TempDir::new().context("private backend namespace")?;
    let mut backend = NativeBackend::for_workspace(directory.path());
    for name in ["foreign", "session"] {
        backend
            .execute(MuxCommand::CreateProjectSession {
                session_id: name.into(),
                cwd: "/source".into(),
                tag: MuxSessionTag::default(),
            })
            .context("create session")?;
    }
    backend
        .execute(MuxCommand::SplitPane {
            session_id: "session".into(),
            pane_id: None,
            direction: MuxSplitDirection::Right,
        })
        .context("split source window")?;
    backend
        .execute(MuxCommand::NewWindow {
            session_id: "session".into(),
            cwd: Some("/destination".into()),
        })
        .context("activate another window")?;
    Ok((directory, backend))
}

fn session(backend: &NativeBackend) -> Result<MuxSession> {
    backend
        .snapshot()
        .context("snapshot")?
        .sessions
        .into_iter()
        .find(|session| session.id == "session")
        .context("session")
}

#[rstest]
#[case(PaneOperation::Split)]
#[case(PaneOperation::Kill)]
#[case(PaneOperation::Close)]
fn explicit_pane_operations_use_the_owning_inactive_window(
    backend: Result<(TempDir, NativeBackend)>,
    #[case] operation: PaneOperation,
) -> Result<()> {
    let (_directory, mut backend) = backend?;
    let before = session(&backend)?;
    let source = &before.windows[0];
    let pane = source.panes[0].pane_id.clone().expect("source pane");
    backend.execute(operation.command(pane))?;
    let after = session(&backend)?;
    assert_eq!(after.windows[1].panes, before.windows[1].panes);
    match operation {
        PaneOperation::Split => {
            assert_eq!(after.windows[0].panes.len(), 3);
            assert_eq!(
                after.windows[0].panes.last().unwrap().cwd,
                source.panes[0].cwd
            );
            assert_eq!(after.active_window_id.as_deref(), Some(source.id.as_str()));
        }
        PaneOperation::Kill | PaneOperation::Close => {
            assert_eq!(after.windows[0].panes, source.panes[1..]);
            assert_eq!(after.active_window_id, before.active_window_id);
        }
    }
    Ok(())
}

#[rstest]
#[case(PaneOperation::Split)]
#[case(PaneOperation::Kill)]
#[case(PaneOperation::Close)]
fn stale_or_foreign_panes_never_fall_back_to_the_active_pane(
    backend: Result<(TempDir, NativeBackend)>,
    #[case] operation: PaneOperation,
) {
    let (_directory, mut backend) = backend.expect("native backend fixture");
    let before = backend.snapshot().expect("snapshot");
    let foreign = before
        .sessions
        .iter()
        .find(|session| session.id == "foreign")
        .unwrap()
        .anchor
        .pane_id
        .clone()
        .unwrap();
    for pane in ["missing".to_owned(), foreign] {
        assert!(backend.execute(operation.command(pane)).is_err());
        assert_eq!(backend.snapshot().expect("unchanged snapshot"), before);
    }
}

#[rstest]
#[case(PaneOperation::Kill, false)]
#[case(PaneOperation::Close, true)]
fn only_close_removes_a_window_with_its_last_pane(
    backend: Result<(TempDir, NativeBackend)>,
    #[case] operation: PaneOperation,
    #[case] removes_window: bool,
) -> Result<()> {
    let (_directory, mut backend) = backend?;
    let before = session(&backend)?;
    let target = &before.windows[1];
    backend.execute(operation.command(target.panes[0].pane_id.clone().unwrap()))?;
    let after = session(&backend)?;
    assert_eq!(
        after.windows.len(),
        before
            .windows
            .len()
            .checked_sub(usize::from(removes_window))
            .context("window count")?
    );
    assert_eq!(after.windows[0].panes, before.windows[0].panes);
    Ok(())
}
