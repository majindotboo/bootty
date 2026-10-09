//! Review a captured PR revision without rewriting a local branch or discarding files.
use super::{Git, GitHubRepository, PullRequest, validate_sha};
use crate::runner::CommandRunner;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequestCheckoutRequest {
    pub repository: GitHubRepository,
    pub number: u32,
    pub expected_head: String,
    pub expected_local_head: String,
}

impl<R: CommandRunner> Git<R> {
    /// # Errors
    /// Rejects a dirty checkout or a PR that no longer matches the displayed revision.
    pub fn pull_request_checkout_context(
        &self,
        root: &str,
        number: u32,
        expected_head: &str,
    ) -> Result<PullRequestCheckoutRequest, String> {
        validate_sha(expected_head)?;
        let root = self.require_clean(root)?;
        let github = self.github(&root)?;
        let pull: PullRequest = github.rest("GET", &github.pull(number)?, None)?;
        if pull.head.sha != expected_head || pull.number != number {
            return Err("The pull request changed. Refresh before checking out.".into());
        }
        Ok(PullRequestCheckoutRequest {
            repository: github.repository,
            number,
            expected_head: expected_head.into(),
            expected_local_head: self
                .checked_output(&root, &["rev-parse", "HEAD"])?
                .trim()
                .into(),
        })
    }

    /// # Errors
    /// Rejects changed local/remote revisions or files Git cannot preserve while switching.
    pub fn checkout_pull_request(
        &self,
        root: &str,
        request: &PullRequestCheckoutRequest,
    ) -> Result<(), String> {
        validate_sha(&request.expected_local_head)?;
        if self.github(root)?.repository != request.repository {
            return Err(
                "The repository changed. Reload before checking out the pull request.".into(),
            );
        }
        let current =
            self.pull_request_checkout_context(root, request.number, &request.expected_head)?;
        if current.repository != request.repository
            || current.expected_local_head != request.expected_local_head
        {
            return Err(
                "The checkout changed. Reload before checking out the pull request.".into(),
            );
        }
        let remote = format!(
            "https://{}/{}/{}.git",
            request.repository.host, request.repository.owner, request.repository.name
        );
        self.checked_output(
            root,
            &[
                "fetch",
                "--no-tags",
                "--",
                &remote,
                &format!("refs/pull/{}/head", request.number),
            ],
        )?;
        if self
            .checked_output(root, &["rev-parse", "FETCH_HEAD"])?
            .trim()
            != request.expected_head
        {
            return Err("The fetched pull request changed. Refresh before checking out.".into());
        }
        self.require_clean(root)?;
        if self.checked_output(root, &["rev-parse", "HEAD"])?.trim() != request.expected_local_head
        {
            return Err("The checkout changed while fetching the pull request.".into());
        }
        // A review checkout stays detached so it cannot reset a user's local branch. Branch
        // creation remains the existing explicit command; switch also preserves ignored files.
        self.checked_output(
            root,
            &[
                "switch",
                "--no-overwrite-ignore",
                "--detach",
                &request.expected_head,
            ],
        )
        .map(|_| ())
    }
}
