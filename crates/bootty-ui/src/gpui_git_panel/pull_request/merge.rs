//! Observe GitHub's acknowledged merge; retries only read the retained operation.
use std::time::{Duration, Instant};

use bootty_git::github::{StackMergeRequest, StackMergeStatus};
use gpui_kit::{Context, Window};

use super::PullRequestView;

pub(super) struct PendingMerge {
    request: StackMergeRequest,
    deadline: Instant,
    attempt: u32,
}

impl PullRequestView {
    pub(super) fn receive_merge(
        &mut self,
        command: &str,
        value: serde_json::Value,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let status = StackMergeStatus::from_response(value)?;
        match status {
            StackMergeStatus::Pending { details } => {
                if command == "git.github.merge-status" {
                    if self
                        .merge
                        .as_ref()
                        .is_none_or(|merge| merge.request.operation != details.uuid)
                    {
                        return Err("GitHub returned a different stack merge operation".into());
                    }
                } else {
                    let snapshot = self
                        .snapshot
                        .as_ref()
                        .ok_or("The pull request disappeared")?;
                    self.merge = Some(PendingMerge {
                        request: StackMergeRequest {
                            repository: snapshot.repository.clone(),
                            number: snapshot.pull_request.number,
                            operation: details.uuid,
                        },
                        deadline: observation_deadline(),
                        attempt: 0,
                    });
                }
                self.schedule_merge(window, cx);
            }
            StackMergeStatus::Merged | StackMergeStatus::Enqueued => {
                self.merge = None;
                self.merge_poll = None;
                if let Some(number) = self
                    .snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.pull_request.number)
                {
                    self.open(number, window, cx);
                }
            }
            StackMergeStatus::Failed => {
                self.merge = None;
                self.merge_poll = None;
                return Err("GitHub refused the stack merge. Check its branch rules and merge requirements.".into());
            }
        }
        Ok(())
    }

    pub(super) fn resume_merge(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some(merge) = &mut self.merge {
            merge.deadline = observation_deadline();
            merge.attempt = 0;
        }
        self.poll_merge(window, cx);
    }

    fn schedule_merge(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(merge) = &mut self.merge else {
            return;
        };
        // Bound polling to 1/2/4/8/10-second backoff and a five-minute observation window.
        let delay = Duration::from_secs((1_u64 << merge.attempt.min(4)).min(10));
        merge.attempt = merge.attempt.saturating_add(1);
        if Instant::now()
            .checked_add(delay)
            .is_none_or(|next| next >= merge.deadline)
        {
            self.error = Some(
                "GitHub is still merging the stack. Refresh to check the same operation.".into(),
            );
            self.merge_poll = None;
            cx.notify();
            return;
        }
        self.merge_poll = Some(cx.spawn_in(window, async move |owner, cx| {
            cx.background_executor().timer(delay).await;
            _ = owner.update_in(cx, |this, window, cx| {
                if this.pending {
                    this.schedule_merge(window, cx);
                } else {
                    this.poll_merge(window, cx);
                }
            });
        }));
    }

    fn poll_merge(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(merge) = &self.merge else {
            return;
        };
        if self.pending {
            return;
        }
        match serde_json::to_string(&merge.request) {
            Ok(request) => self.request("git.github.merge-status", vec![request], window, cx),
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            }
        }
    }
}

fn observation_deadline() -> Instant {
    Instant::now()
        .checked_add(Duration::from_mins(5))
        .unwrap_or_else(Instant::now)
}
