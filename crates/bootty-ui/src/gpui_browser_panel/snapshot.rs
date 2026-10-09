use std::time::Instant;

use bootty_control::CommandOutcome;

use super::*;
use crate::commands::BrowserRequest;

struct SnapshotPage {
    id: u64,
    revisions: (u64, u64),
    address: String,
    document: Option<String>,
    window: bootty_control::CommandTarget,
}

impl BrowserPanel {
    pub(crate) fn snapshot_request(
        &self,
        request: BrowserRequest,
        window: &Window,
        cx: &Context<Self>,
    ) {
        let prepared = self.prepare_snapshot(&request);
        let (page, receiver) = match prepared {
            Ok(prepared) => prepared,
            Err(outcome) => {
                request.complete(outcome);
                return;
            }
        };
        cx.spawn_in(window, async move |owner, cx| {
            let outcome = read_snapshot(&owner, cx, &request, &page, receiver)
                .await
                .unwrap_or_else(|outcome| outcome);
            request.complete(outcome);
        })
        .detach();
    }

    fn prepare_snapshot(
        &self,
        request: &BrowserRequest,
    ) -> Result<
        (
            SnapshotPage,
            async_channel::Receiver<Result<String, bootty_browser::CredentialError>>,
        ),
        CommandOutcome,
    > {
        let BrowserAction::Snapshot(document) = &request.action else {
            return Err(stale_snapshot());
        };
        let target = request.target.as_ref().ok_or_else(stale_snapshot)?;
        if self.window_target.as_ref() != Some(target) {
            return Err(stale_snapshot());
        }
        if request.capture_cancelled() {
            return Err(CommandOutcome::cancelled());
        }
        if request
            .capture_deadline()
            .is_none_or(|deadline| Instant::now() >= deadline)
        {
            return Err(CommandOutcome::deadline_exceeded());
        }
        if self.resetting {
            return Err(CommandOutcome::Unavailable {
                message: "Browser site data is being reset.".into(),
            });
        }
        let tab = self
            .tabs
            .iter()
            .find(|tab| Some(tab.id) == request.page)
            .ok_or_else(stale_snapshot)?;
        let view = tab.view.as_ref().filter(|_| !tab.loading).ok_or_else(|| {
            CommandOutcome::Unavailable {
                message: "Open a loaded browser page before reading it.".into(),
            }
        })?;
        let address = view.current_address().map_err(|_| stale_snapshot())?;
        let receiver = view
            .capture_credential_document()
            .map_err(|_| stale_snapshot())?;
        Ok((
            SnapshotPage {
                id: tab.id,
                revisions: (tab.view_revision, tab.load_revision),
                address,
                document: document.clone(),
                window: target.clone(),
            },
            receiver,
        ))
    }

    fn snapshot_current(&self, request: &BrowserRequest, page: &SnapshotPage) -> bool {
        !self.resetting
            && !request.capture_cancelled()
            && request
                .capture_deadline()
                .is_some_and(|deadline| Instant::now() < deadline)
            && self.window_target.as_ref() == Some(&page.window)
            && self.tabs.iter().any(|tab| {
                tab.id == page.id
                    && !tab.loading
                    && (tab.view_revision, tab.load_revision) == page.revisions
                    && tab
                        .view
                        .as_ref()
                        .and_then(|view| view.current_address().ok())
                        .as_ref()
                        == Some(&page.address)
            })
    }
}

#[allow(
    clippy::future_not_send,
    reason = "Native browser views remain on GPUI's UI executor"
)]
async fn read_snapshot(
    owner: &gpui_kit::WeakEntity<BrowserPanel>,
    cx: &mut gpui_kit::AsyncWindowContext,
    request: &BrowserRequest,
    page: &SnapshotPage,
    receiver: async_channel::Receiver<Result<String, bootty_browser::CredentialError>>,
) -> Result<CommandOutcome, CommandOutcome> {
    let deadline = request.capture_deadline().unwrap_or_else(Instant::now);
    let document = receive(receiver, deadline, cx.background_executor()).await?;
    if page
        .document
        .as_ref()
        .is_some_and(|expected| expected != &document)
    {
        return Err(stale_snapshot());
    }
    let receiver = owner
        .update(cx, |this, _| {
            if !this.snapshot_current(request, page) {
                return Err(stale_snapshot());
            }
            this.tabs
                .iter()
                .find(|tab| tab.id == page.id)
                .and_then(|tab| tab.view.as_ref())
                .ok_or_else(stale_snapshot)?
                .document_snapshot(&document, &page.address)
                .map_err(|_| stale_snapshot())
        })
        .map_err(|_| stale_snapshot())??;
    let snapshot = receive(receiver, deadline, cx.background_executor()).await?;
    let final_document = owner
        .update(cx, |this, _| {
            if !this.snapshot_current(request, page) {
                return Err(stale_snapshot());
            }
            this.tabs
                .iter()
                .find(|tab| tab.id == page.id)
                .and_then(|tab| tab.view.as_ref())
                .ok_or_else(stale_snapshot)?
                .capture_credential_document()
                .map_err(|_| stale_snapshot())
        })
        .map_err(|_| stale_snapshot())??;
    if receive(final_document, deadline, cx.background_executor()).await? != document {
        return Err(stale_snapshot());
    }
    owner
        .update(cx, |this, _| {
            if !this.snapshot_current(request, page) {
                return Err(stale_snapshot());
            }
            // Admit only after both observations; a timed-out or cancelled read publishes nothing.
            request
                .begin()
                .map_err(crate::commands::runtime::command_outcome_for_mux_error)
        })
        .map_err(|_| stale_snapshot())??;
    Ok(CommandOutcome::Success {
        value: serde_json::to_value(snapshot).map_err(|_| stale_snapshot())?,
        warnings: Vec::new(),
    })
}

#[allow(
    clippy::future_not_send,
    reason = "Receivers are awaited on GPUI's UI executor"
)]
pub(super) async fn receive<T, E>(
    receiver: async_channel::Receiver<Result<T, E>>,
    deadline: Instant,
    executor: &gpui_kit::BackgroundExecutor,
) -> Result<T, CommandOutcome> {
    match futures::future::select(
        Box::pin(receiver.recv()),
        Box::pin(executor.timer(deadline.saturating_duration_since(Instant::now()))),
    )
    .await
    {
        futures::future::Either::Left((Ok(Ok(value)), _)) => Ok(value),
        futures::future::Either::Left(_) => Err(stale_snapshot()),
        futures::future::Either::Right(_) => Err(CommandOutcome::deadline_exceeded()),
    }
}

fn stale_snapshot() -> CommandOutcome {
    CommandOutcome::StaleTarget {
        message: "The browser document changed before it could be read.".into(),
    }
}
