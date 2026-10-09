//! Fork workflow approval checks reject ambiguous PRs and recheck each run.
use super::{GitHub, PullRequest, WorkflowApproval, WorkflowHead, validate_sha};
use crate::runner::CommandRunner;
use serde_json::{Value, json};

impl<R: CommandRunner> GitHub<'_, R> {
    /// Read approval candidates only for a unique open fork PR at the reviewed revision.
    /// # Errors
    /// Rejects changed heads, ambiguous forks, truncated results and host errors.
    pub fn workflow_approvals(
        &self,
        number: u32,
        head: &str,
    ) -> Result<Vec<WorkflowApproval>, String> {
        validate_sha(head)?;
        let (pr, _, _) = self.access(number)?;
        self.workflow_runs(&pr, head)
    }

    fn workflow_runs(&self, pr: &PullRequest, head: &str) -> Result<Vec<WorkflowApproval>, String> {
        if pr.head.sha != head || pr.state != "open" {
            return Err("The pull request changed. Refresh before approving workflows.".into());
        }
        let head_repo = pr
            .head
            .repo
            .as_ref()
            .ok_or("The fork repository is unavailable")?;
        let base_repo = format!("{}/{}", self.repository.owner, self.repository.name);
        if head_repo.full_name.eq_ignore_ascii_case(&base_repo) {
            return Ok(Vec::new());
        }
        let owner = head_repo
            .full_name
            .split_once('/')
            .ok_or("Invalid fork repository")?
            .0;
        let args = vec![
            "pr".into(),
            "list".into(),
            "--repo".into(),
            self.repository.selector(),
            "--state".into(),
            "open".into(),
            "--head".into(),
            pr.head.branch.clone(),
            "--limit".into(),
            "101".into(),
            "--json".into(),
            "number,headRefOid,isCrossRepository,headRepositoryOwner".into(),
        ];
        let heads: Vec<WorkflowHead> =
            serde_json::from_str(&self.command(&args, None)?).map_err(|e| e.to_string())?;
        if heads.len() > 100 {
            return Err("The fork head list was truncated. Approve workflows on GitHub.".into());
        }
        let exact = heads
            .iter()
            .filter(|candidate| {
                candidate.head_ref_oid == head
                    && candidate.is_cross_repository
                    && candidate
                        .head_repository_owner
                        .as_ref()
                        .is_some_and(|account| account.login.eq_ignore_ascii_case(owner))
            })
            .collect::<Vec<_>>();
        if exact.len() != 1
            || exact
                .first()
                .is_none_or(|candidate| candidate.number != pr.number)
        {
            return Err("This fork revision belongs to more than one pull request. Approve workflows on GitHub.".into());
        }
        let args = vec![
            "run".into(),
            "list".into(),
            "--repo".into(),
            self.repository.selector(),
            "--commit".into(),
            head.into(),
            "--branch".into(),
            pr.head.branch.clone(),
            "--event".into(),
            "pull_request".into(),
            "--status".into(),
            "action_required".into(),
            "--limit".into(),
            "101".into(),
            "--json".into(),
            "databaseId,workflowName,url".into(),
        ];
        let runs: Vec<WorkflowApproval> =
            serde_json::from_str(&self.command(&args, None)?).map_err(|e| e.to_string())?;
        if runs.len() > 100 || runs.iter().any(|run| run.database_id == 0) {
            return Err("GitHub returned an invalid or truncated workflow list".into());
        }
        Ok(runs)
    }

    pub(super) fn approve_workflows(&self, number: u32, head: &str) -> Result<Value, String> {
        let (pr, permissions, _) = self.access(number)?;
        if !permissions.can_write {
            return Err("Your account cannot approve workflows for this repository".into());
        }
        let runs = self.workflow_runs(&pr, head)?;
        let mut approved = Vec::new();
        for run in runs {
            let current = self.workflow_approvals(number, head)?;
            if !current
                .iter()
                .any(|current| current.database_id == run.database_id)
            {
                return Err(format!(
                    "Workflow approvals changed after {} approvals. Refresh before continuing.",
                    approved.len()
                ));
            }
            self.rest::<Value>(
                "POST",
                &format!(
                    "repos/{}/{}/actions/runs/{}/approve",
                    self.repository.owner, self.repository.name, run.database_id
                ),
                None,
            )?;
            approved.push(run.database_id);
        }
        Ok(json!({"approved":approved}))
    }
}
