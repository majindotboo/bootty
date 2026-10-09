//! Publish the captured branch and create a PR without changing the checkout.
use super::{Git, GitHub, GitHubRepository, PullRequest, validate_body, validate_sha};
use crate::runner::CommandRunner;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequestCreationContext {
    pub repository: GitHubRepository,
    /// Origin owns the published branch; repository is the PR's base repository.
    /// Omitted by older clients whose origin and base were the same repository.
    #[serde(default)]
    pub head_repository: Option<GitHubRepository>,
    pub branch: String,
    pub head: String,
    pub base: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequestCreationRequest {
    pub context: PullRequestCreationContext,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

impl<R: CommandRunner> Git<R> {
    /// # Errors
    /// Requires a named branch, a valid HEAD and accessible base/origin repositories.
    pub fn pull_request_creation_context(
        &self,
        root: &str,
    ) -> Result<PullRequestCreationContext, String> {
        let branch = self.checked_output(root, &["symbolic-ref", "--short", "HEAD"])?;
        let head = self.checked_output(root, &["rev-parse", "HEAD"])?;
        let github = self.github(root)?;
        let head_repository = self.github_origin(root)?;
        let repository: Value = github.rest("GET", &github.repository_endpoint(), None)?;
        let base = repository
            .get("default_branch")
            .and_then(Value::as_str)
            .ok_or("GitHub returned no default branch")?
            .to_owned();
        let context = PullRequestCreationContext {
            repository: github.repository,
            head_repository: Some(head_repository),
            branch: branch.trim().into(),
            head: head.trim().into(),
            base,
        };
        context.validate()?;
        Ok(context)
    }

    /// # Errors
    /// Rejects a changed checkout or revision; a normal push never force-updates origin.
    pub fn publish_pull_request_branch(
        &self,
        root: &str,
        context: &PullRequestCreationContext,
    ) -> Result<(), String> {
        self.check_creation_context(root, context)?;
        // Push the reviewed commit, even if another process advances the local branch.
        self.checked_output(
            root,
            &[
                "push",
                "--porcelain",
                "origin",
                &format!("{}:refs/heads/{}", context.head, context.branch),
            ],
        )?;
        Ok(())
    }

    /// # Errors
    /// Rejects stale local/remote heads, invalid metadata or a failed GitHub creation.
    pub fn create_pull_request(
        &self,
        root: &str,
        request: &PullRequestCreationRequest,
    ) -> Result<PullRequest, String> {
        self.check_creation_context(root, &request.context)?
            .create(request)
    }

    fn check_creation_context(
        &self,
        root: &str,
        context: &PullRequestCreationContext,
    ) -> Result<GitHub<'_, R>, String> {
        context.validate()?;
        let github = self.github(root)?;
        if github.repository != context.repository {
            return Err("The repository remote changed. Reload before publishing.".into());
        }
        if self.github_origin(root)? != *context.head_repository() {
            return Err("The branch's origin changed. Reload before publishing.".into());
        }
        if self
            .checked_output(root, &["symbolic-ref", "--short", "HEAD"])?
            .trim()
            != context.branch
            || self.checked_output(root, &["rev-parse", "HEAD"])?.trim() != context.head
        {
            return Err("The current branch changed. Reload before publishing.".into());
        }
        Ok(github)
    }

    fn github_origin(&self, root: &str) -> Result<GitHubRepository, String> {
        GitHubRepository::from_remote(
            self.checked_output(root, &["remote", "get-url", "origin"])?
                .trim(),
        )
    }
}

impl PullRequestCreationContext {
    fn validate(&self) -> Result<(), String> {
        validate_sha(&self.head)?;
        validate_branch(&self.branch)?;
        validate_branch(&self.base)?;
        if self.head_repository().host != self.repository.host {
            return Err("The branch and pull request must use the same GitHub host".into());
        }
        Ok(())
    }

    fn head_repository(&self) -> &GitHubRepository {
        self.head_repository.as_ref().unwrap_or(&self.repository)
    }
}

impl<R: CommandRunner> GitHub<'_, R> {
    fn repository_endpoint(&self) -> String {
        format!("repos/{}/{}", self.repository.owner, self.repository.name)
    }

    fn create(&self, request: &PullRequestCreationRequest) -> Result<PullRequest, String> {
        validate_body(&request.body, true)?;
        if request.title.trim().is_empty()
            || request.title.len() > 256
            || request.title.chars().any(char::is_control)
        {
            return Err("Enter a pull request title up to 256 characters".into());
        }
        let head_repository = request.context.head_repository();
        if head_repository == &self.repository && request.context.base == request.context.branch {
            return Err("Choose a different base branch".into());
        }
        let mut encoded = url::Url::parse("https://github.invalid/").map_err(|e| e.to_string())?;
        encoded
            .path_segments_mut()
            .map_err(|()| "Invalid branch")?
            .push(&request.context.branch);
        let reference: Value = self.rest(
            "GET",
            &format!(
                "repos/{}/{}/git/ref/heads/{}",
                head_repository.owner,
                head_repository.name,
                encoded.path().trim_start_matches('/')
            ),
            None,
        )?;
        if reference.pointer("/object/sha").and_then(Value::as_str)
            != Some(request.context.head.as_str())
        {
            return Err("Publish the current branch before creating the pull request".into());
        }
        let head = if head_repository == &self.repository {
            request.context.branch.clone()
        } else {
            format!("{}:{}", head_repository.owner, request.context.branch)
        };
        let created: PullRequest = self.rest("POST", &format!("{}/pulls", self.repository_endpoint()), Some(json!({"title":request.title,"body":request.body,"base":request.context.base,"head":head,"head_repo":head_repository.name,"draft":request.draft})))?;
        if created.number == 0 {
            return Err(
                "GitHub returned no pull request number. Check GitHub before retrying.".into(),
            );
        }
        if created.head.sha != request.context.head || created.base.branch != request.context.base {
            return Err(format!(
                "GitHub created #{} with a changed revision. Check the pull request before retrying.",
                created.number
            ));
        }
        Ok(created)
    }
}

#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Git ref names prohibit the case-sensitive .lock suffix"
)]
fn validate_branch(branch: &str) -> Result<(), String> {
    if branch.is_empty()
        || branch.len() > 1024
        || branch.starts_with(['-', '/'])
        || branch.ends_with(['/', '.'])
        || branch.chars().any(char::is_whitespace)
        || branch.chars().any(char::is_control)
        || branch.contains([':', '~', '^', '?', '*', '[', '\\'])
        || branch.contains("..")
        || branch.contains("@{")
        || branch
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
    {
        return Err("Invalid branch name".into());
    }
    Ok(())
}
