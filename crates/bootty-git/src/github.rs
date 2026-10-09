//! GitHub operations run on the repository's host with that host's existing gh account.

mod checkout;
mod creation;
mod files;
mod metadata;
mod model;
mod queries;
mod stacks;
mod workflows;
pub use checkout::PullRequestCheckoutRequest;
pub use creation::{PullRequestCreationContext, PullRequestCreationRequest};
pub use files::PullRequestDiffRequest;
pub use model::*;
pub use stacks::{StackMergeOperation, StackMergeRequest, StackMergeStatus};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{Git, runner::CommandRunner};

pub struct GitHub<'a, R> {
    runner: &'a R,
    repository: GitHubRepository,
}

impl<R: CommandRunner> Git<R> {
    /// Resolve the remote explicitly; gh never inherits the UI process's working directory.
    /// # Errors
    /// Returns an unresolved repository, unsupported host or host/auth execution error.
    pub fn github(&self, cwd: &str) -> Result<GitHub<'_, R>, String> {
        // Reuse gh's default-repository selection, including forks, upstream, SSH aliases and
        // `gh repo set-default`, rather than maintaining a second remote-ranking policy.
        let output = self
            .runner
            .run_in(
                cwd,
                "gh",
                &["repo".into(), "view".into(), "--json".into(), "url".into()],
            )
            .map_err(|e| e.to_string())?;
        if !output.success {
            return Err(output.stderr.trim().to_owned());
        }
        let value: Value = serde_json::from_str(&output.stdout).map_err(|e| e.to_string())?;
        let remote = value
            .get("url")
            .and_then(Value::as_str)
            .ok_or("GitHub returned no repository URL")?;
        Ok(GitHub {
            runner: &self.runner,
            repository: GitHubRepository::from_remote(remote)?,
        })
    }
}

impl<R: CommandRunner> GitHub<'_, R> {
    #[must_use]
    pub const fn repository(&self) -> &GitHubRepository {
        &self.repository
    }

    /// Read a current pull request and its viewer permissions, threads, files and checks.
    /// # Errors
    /// Returns host/auth/API errors or malformed responses. Bounds are reported as truncation.
    pub fn read(&self, number: u32) -> Result<PullRequestSnapshot, String> {
        let pull_request: PullRequest = self.rest("GET", &self.pull(number)?, None)?;
        if pull_request.number != number {
            return Err("GitHub returned a different pull request".into());
        }
        let mut cursor = Value::Null;
        let mut threads = Vec::new();
        let mut permissions = None;
        let mut checks = Value::Null;
        let mut merge_methods = Vec::new();
        let mut reaction_groups = Vec::new();
        let mut threads_truncated = false;
        for _ in 0..10 {
            let response = self.graphql(queries::THREADS, &json!({"owner":self.repository.owner,"name":self.repository.name,"number":number,"cursor":cursor}))?;
            let repository = response
                .get("repository")
                .ok_or("GitHub returned no repository")?;
            let pr = repository
                .get("pullRequest")
                .filter(|pr| !pr.is_null())
                .ok_or("Pull request is unavailable")?;
            if pr.get("headRefOid").and_then(Value::as_str) != Some(pull_request.head.sha.as_str())
                || pr.get("baseRefOid").and_then(Value::as_str)
                    != Some(pull_request.base.sha.as_str())
            {
                return Err("The pull request changed while loading. Refresh to review it.".into());
            }
            permissions = Some(ViewerPermissions::from_response(repository, pr)?);
            merge_methods = Self::merge_methods(repository);
            reaction_groups = serde_json::from_value(
                pr.get("reactionGroups")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            )
            .map_err(|e| e.to_string())?;
            checks = pr
                .pointer("/commits/nodes/0/commit/statusCheckRollup/contexts")
                .cloned()
                .unwrap_or(Value::Null);
            let page: ThreadConnection = serde_json::from_value(
                pr.get("reviewThreads")
                    .cloned()
                    .ok_or("GitHub returned no review threads")?,
            )
            .map_err(|e| e.to_string())?;
            threads.extend(page.nodes);
            if !page.page_info.has_next_page {
                threads_truncated = false;
                break;
            }
            let next = page
                .page_info
                .end_cursor
                .ok_or("GitHub omitted the next thread cursor")?;
            if cursor.as_str() == Some(&next) {
                return Err("GitHub repeated the review cursor".into());
            }
            cursor = Value::String(next);
            threads_truncated = true;
        }
        let (files, files_truncated) = self.files(number)?;
        let current: PullRequest = self.rest("GET", &self.pull(number)?, None)?;
        if current.head.sha != pull_request.head.sha || current.base.sha != pull_request.base.sha {
            return Err("The pull request changed while loading. Refresh to review it.".into());
        }
        Ok(PullRequestSnapshot {
            repository: self.repository.clone(),
            pull_request,
            permissions: permissions.ok_or("GitHub returned no permissions")?,
            threads,
            threads_truncated,
            files,
            files_truncated,
            checks,
            merge_methods,
            reaction_groups,
        })
    }

    /// List the repository's PRs using GitHub's search, including closed requests when asked.
    /// # Errors
    /// Returns invalid search, host/auth or API errors.
    pub fn search(&self, query: &str) -> Result<Vec<PullRequestSummary>, String> {
        if query.len() > 1024 || query.chars().any(char::is_control) {
            return Err("Invalid pull request search".into());
        }
        let args = vec!["pr".into(), "list".into(), "--repo".into(), self.repository.selector(), "--state".into(), "all".into(), "--limit".into(), "100".into(), "--search".into(), query.into(), "--json".into(), "number,title,state,isDraft,url,headRefName,baseRefName,author,updatedAt,reviewDecision,statusCheckRollup".into()];
        self.command(&args, None)
            .and_then(|output| serde_json::from_str(&output).map_err(|e| e.to_string()))
    }

    fn files(&self, number: u32) -> Result<(Vec<PullRequestFile>, bool), String> {
        let mut files = Vec::new();
        // GitHub's files endpoint caps one PR at 3000 files. Report that boundary.
        for page in 1..=30 {
            let current: Vec<PullRequestFile> = self.rest(
                "GET",
                &format!("{}/files?per_page=100&page={page}", self.pull(number)?),
                None,
            )?;
            let complete = current.len() < 100;
            files.extend(current);
            if complete {
                return Ok((files, false));
            }
        }
        Ok((files, true))
    }

    /// Read another comment page only after verifying the node belongs to this PR.
    /// # Errors
    /// Rejects foreign nodes, invalid cursors and host/API failures.
    pub fn thread_comments(
        &self,
        number: u32,
        id: &str,
        cursor: &str,
    ) -> Result<CommentConnection, String> {
        if number == 0
            || id.is_empty()
            || id.len() > 8192
            || cursor.len() > 8192
            || id.chars().chain(cursor.chars()).any(char::is_control)
        {
            return Err("Invalid review comment page".into());
        }
        let result = self.graphql(
            queries::COMMENTS,
            &json!({"id":id,"cursor":if cursor.is_empty() { Value::Null } else { json!(cursor) }}),
        )?;
        let node = result.get("node").ok_or("Review thread is unavailable")?;
        if node.pointer("/pullRequest/number").and_then(Value::as_u64) != Some(u64::from(number))
            || node
                .pointer("/pullRequest/repository/nameWithOwner")
                .and_then(Value::as_str)
                != Some(format!("{}/{}", self.repository.owner, self.repository.name).as_str())
        {
            return Err("Review thread is not in this pull request".into());
        }
        serde_json::from_value(
            node.get("comments")
                .cloned()
                .ok_or("GitHub returned no comments")?,
        )
        .map_err(|e| e.to_string())
    }

    /// Submit a review against the exact head and source ranges the user saw.
    /// # Errors
    /// Rejects a changed head, invalid selections, own-PR verdicts and host failures.
    pub fn review(&self, number: u32, request: &ReviewRequest) -> Result<Value, String> {
        request.validate()?;
        let snapshot = self.read(number)?;
        if snapshot.pull_request.head.sha != request.expected_head
            || request
                .expected_base
                .as_ref()
                .is_some_and(|base| base != &snapshot.pull_request.base.sha)
        {
            return Err("The pull request changed. Refresh before reviewing.".into());
        }
        if snapshot.permissions.did_author && request.verdict != ReviewVerdict::Comment {
            return Err("You cannot approve or request changes on your own pull request".into());
        }
        for comment in &request.comments {
            let file = snapshot
                .files
                .iter()
                .find(|file| file.filename == comment.anchor.path)
                .ok_or("Comment file is no longer in the pull request")?;
            let diff = crate::diff::FileDiff::parse(
                file.filename.clone(),
                file.previous_filename.clone(),
                file.patch.as_deref(),
            )
            .ok();
            let quote = if let Some(quote) = diff.and_then(|diff| diff.quote(&comment.anchor).ok())
            {
                quote
            } else {
                self.complete_file_diff(
                    file,
                    &PullRequestDiffRequest {
                        head: request.expected_head.clone(),
                        base: snapshot.pull_request.base.sha.clone(),
                        path: file.filename.clone(),
                        context_lines: 100_000,
                    },
                )?
                .quote(&comment.anchor)?
            };
            if quote != comment.quote {
                return Err("Selected code changed. Refresh before commenting.".into());
            }
        }
        let comments = request
            .comments
            .iter()
            .map(|comment| {
                let a = &comment.anchor;
                if a.start_line == a.line {
                    json!({"path":a.path,"side":a.side,"line":a.line,"body":comment.body})
                } else {
                    json!({"path":a.path,"side":a.side,"line":a.line,"body":comment.body,"start_line":a.start_line,"start_side":a.side})
                }
            })
            .collect::<Vec<_>>();
        let current: PullRequest = self.rest("GET", &self.pull(number)?, None)?;
        if current.head.sha != request.expected_head
            || current.base.sha != snapshot.pull_request.base.sha
        {
            return Err("The pull request changed. Refresh before submitting the review.".into());
        }
        self.rest("POST", &format!("{}/reviews",self.pull(number)?), Some(json!({"commit_id":request.expected_head,"event":request.verdict,"body":request.body,"comments":comments})))
    }

    /// Reply or resolve only a thread observed on this exact PR and host.
    /// # Errors
    /// Rejects foreign threads, invalid text, unsupported permissions and host failures.
    pub fn thread(&self, number: u32, request: &ThreadRequest) -> Result<Value, String> {
        let (_, permissions, _) = self.access(number)?;
        let id = match request {
            ThreadRequest::Reply { id, .. } | ThreadRequest::Resolve { id, .. } => id,
        };
        validate_node_id(id)?;
        let scope = self.graphql(queries::THREAD_SCOPE, &json!({"id":id}))?;
        self.verify_thread_scope(
            number,
            scope.get("node").ok_or("Review thread is unavailable")?,
        )?;
        match request {
            ThreadRequest::Reply { body, .. } => {
                validate_body(body, false)?;
                self.graphql(queries::REPLY, &json!({"threadId":id,"body":body}))
            }
            ThreadRequest::Resolve { resolved, .. } => {
                if !permissions.resolve {
                    return Err("Your account cannot resolve this review thread".into());
                }
                self.graphql(
                    if *resolved {
                        queries::RESOLVE
                    } else {
                        queries::UNRESOLVE
                    },
                    &json!({"threadId":id}),
                )
            }
        }
    }

    /// Perform one explicit PR action with host-reported permissions and head identity.
    /// # Errors
    /// Rejects stale heads, disallowed actions and unsuccessful GitHub acknowledgements.
    pub fn action(&self, number: u32, request: &PullRequestActionRequest) -> Result<Value, String> {
        validate_sha(&request.expected_head)?;
        if let Some(stack) = &request.stack {
            return self.stack_action(number, request, stack);
        }
        let (pr, permissions, methods) = self.access(number)?;
        if pr.head.sha != request.expected_head {
            return Err("The pull request changed. Refresh before this action.".into());
        }
        let id = &pr.node_id;
        match request.action {
            PullRequestAction::Merge => self.merge(number, request, &pr, &permissions, &methods),
            PullRequestAction::Close | PullRequestAction::Reopen => {
                if !permissions.can_update
                    || pr.merged
                    || (pr.state == "open") != (request.action == PullRequestAction::Close)
                {
                    return Err("This pull request cannot change to the requested state".into());
                }
                self.rest("PATCH", &self.pull(number)?, Some(json!({"state":if request.action == PullRequestAction::Close { "closed" } else { "open" }})))
            }
            PullRequestAction::Ready | PullRequestAction::Draft => {
                if !permissions.can_update
                    || pr.state != "open"
                    || pr.draft != (request.action == PullRequestAction::Ready)
                {
                    return Err(
                        "This pull request cannot change to the requested review state".into(),
                    );
                }
                self.graphql(
                    if request.action == PullRequestAction::Ready {
                        queries::READY
                    } else {
                        queries::DRAFT
                    },
                    &json!({"id":id}),
                )
            }
            PullRequestAction::UpdateBranch | PullRequestAction::RebaseBranch => {
                if !permissions.can_update_branch || pr.state != "open" {
                    return Err("Your account cannot update this pull request branch".into());
                }
                if request.action == PullRequestAction::RebaseBranch {
                    return self.graphql(
                        queries::REBASE_BRANCH,
                        &json!({"id":id,"sha":request.expected_head}),
                    );
                }
                self.rest(
                    "PUT",
                    &format!("{}/update-branch", self.pull(number)?),
                    Some(json!({"expected_head_sha":request.expected_head})),
                )
            }
            PullRequestAction::EnableAutoMerge | PullRequestAction::DisableAutoMerge => {
                if !permissions.can_write {
                    return Err("Your account cannot merge this pull request".into());
                }
                let method = if request.action == PullRequestAction::EnableAutoMerge {
                    if pr.state != "open" || pr.draft {
                        return Err(
                            "Auto-merge requires an open pull request ready for review".into()
                        );
                    }
                    Self::choose_merge_method(request.merge_method, &methods)?
                } else {
                    MergeMethod::Merge
                };
                self.graphql(
                    if request.action == PullRequestAction::EnableAutoMerge {
                        queries::AUTO_MERGE
                    } else {
                        queries::DISABLE_AUTO_MERGE
                    },
                    &json!({"id":id,"method":method.graphql()}),
                )
            }
            PullRequestAction::ApproveWorkflows => {
                self.approve_workflows(number, &request.expected_head)
            }
            PullRequestAction::Revert => {
                if !permissions.can_write || !pr.merged {
                    return Err("Only a merged pull request can be reverted by an account with write access".into());
                }
                self.graphql(queries::REVERT, &json!({"id":id}))
            }
        }
    }

    fn merge(
        &self,
        number: u32,
        request: &PullRequestActionRequest,
        pr: &PullRequest,
        permissions: &ViewerPermissions,
        methods: &[MergeMethod],
    ) -> Result<Value, String> {
        if !permissions.can_write {
            return Err("Your account cannot merge this pull request".into());
        }
        if pr.state != "open" || pr.draft {
            return Err("Only an open pull request ready for review can be merged".into());
        }
        let method = Self::choose_merge_method(request.merge_method, methods)?;
        let response: Value = self.rest(
            "PUT",
            &format!("{}/merge", self.pull(number)?),
            Some(json!({"sha":request.expected_head,"merge_method":method})),
        )?;
        if response.get("merged") != Some(&Value::Bool(true)) {
            return Err(response
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("GitHub did not merge the pull request")
                .into());
        }
        Ok(response)
    }

    fn access(
        &self,
        number: u32,
    ) -> Result<(PullRequest, ViewerPermissions, Vec<MergeMethod>), String> {
        let pr: PullRequest = self.rest("GET", &self.pull(number)?, None)?;
        if pr.number != number {
            return Err("GitHub returned a different pull request".into());
        }
        let response = self.graphql(queries::ACCESS, &self.variables(number))?;
        let repository = response
            .get("repository")
            .filter(|v| !v.is_null())
            .ok_or("Repository is unavailable")?;
        let current = repository
            .get("pullRequest")
            .filter(|v| !v.is_null())
            .ok_or("Pull request is unavailable")?;
        if current.get("headRefOid").and_then(Value::as_str) != Some(pr.head.sha.as_str()) {
            return Err(
                "The pull request changed while loading. Refresh before continuing.".into(),
            );
        }
        Ok((
            pr,
            ViewerPermissions::from_response(repository, current)?,
            Self::merge_methods(repository),
        ))
    }

    fn variables(&self, number: u32) -> Value {
        json!({"owner":self.repository.owner,"name":self.repository.name,"number":number})
    }

    fn merge_methods(repository: &Value) -> Vec<MergeMethod> {
        [
            ("mergeCommitAllowed", MergeMethod::Merge),
            ("squashMergeAllowed", MergeMethod::Squash),
            ("rebaseMergeAllowed", MergeMethod::Rebase),
        ]
        .into_iter()
        .filter_map(|(key, method)| {
            (repository.get(key) == Some(&Value::Bool(true))).then_some(method)
        })
        .collect()
    }

    fn choose_merge_method(
        method: Option<MergeMethod>,
        methods: &[MergeMethod],
    ) -> Result<MergeMethod, String> {
        let method = method
            .or_else(|| methods.first().copied())
            .ok_or("The repository allows no merge method")?;
        if methods.contains(&method) {
            Ok(method)
        } else {
            Err("The repository does not allow this merge method".into())
        }
    }

    fn verify_thread_scope(&self, number: u32, node: &Value) -> Result<(), String> {
        if node.pointer("/pullRequest/number").and_then(Value::as_u64) != Some(u64::from(number))
            || node
                .pointer("/pullRequest/repository/nameWithOwner")
                .and_then(Value::as_str)
                != Some(format!("{}/{}", self.repository.owner, self.repository.name).as_str())
        {
            return Err("Review thread is not in this pull request".into());
        }
        Ok(())
    }

    fn pull(&self, number: u32) -> Result<String, String> {
        if number == 0 {
            return Err("A pull request number is required".into());
        }
        Ok(format!(
            "repos/{}/{}/pulls/{number}",
            self.repository.owner, self.repository.name
        ))
    }

    fn rest<T: DeserializeOwned>(
        &self,
        method: &str,
        endpoint: &str,
        input: Option<Value>,
    ) -> Result<T, String> {
        let mut args = vec![
            "api".into(),
            "--hostname".into(),
            self.repository.host.clone(),
            "--method".into(),
            method.into(),
            endpoint.into(),
        ];
        if input.is_some() {
            args.extend(["--input".into(), "-".into()]);
        }
        let response = self.command(&args, input)?;
        serde_json::from_str(if response.trim().is_empty() {
            "null"
        } else {
            &response
        })
        .map_err(|e| format!("Invalid GitHub response: {e}"))
    }

    fn graphql(&self, query: &str, variables: &Value) -> Result<Value, String> {
        let args = vec![
            "api".into(),
            "--hostname".into(),
            self.repository.host.clone(),
            "graphql".into(),
            "--input".into(),
            "-".into(),
        ];
        let response = self.command(&args, Some(json!({"query":query,"variables":variables})))?;
        let value: Value = serde_json::from_str(&response).map_err(|e| e.to_string())?;
        if value
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty())
        {
            return Err("GitHub rejected the request".into());
        }
        let data = value
            .get("data")
            .filter(|data| data.is_object())
            .cloned()
            .ok_or("GitHub returned no result")?;
        if query.starts_with("mutation")
            && data
                .as_object()
                .is_none_or(|fields| fields.is_empty() || fields.values().any(Value::is_null))
        {
            return Err("GitHub did not acknowledge the mutation".into());
        }
        Ok(data)
    }

    fn command(&self, args: &[String], input: Option<Value>) -> Result<String, String> {
        let result = match input {
            Some(input) => self.runner.run_with_input(
                "gh",
                args,
                serde_json::to_vec(&input).map_err(|e| e.to_string())?,
            ),
            None => self.runner.run("gh", args),
        }
        .map_err(|e| e.to_string())?;
        if !result.success {
            return Err(result.stderr.trim().to_owned());
        }
        if result.stdout.len() > 16 * 1024 * 1024 {
            return Err("GitHub response exceeds 16 MB".into());
        }
        Ok(result.stdout)
    }
}
