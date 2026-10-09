use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::diff::DiffAnchor;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GitHubRepository {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl GitHubRepository {
    /// # Errors
    /// Accepts HTTPS, SSH URLs and Git's scp-style SSH syntax, with an exact repository path.
    pub fn from_remote(remote: &str) -> Result<Self, String> {
        let (host, path) = if let Some(path) = remote.strip_prefix("git@") {
            let (host, path) = path.split_once(':').ok_or("Invalid GitHub SSH remote")?;
            (host.to_owned(), path.to_owned())
        } else {
            let url = url::Url::parse(remote).map_err(|_| "Unsupported GitHub remote")?;
            if !matches!(url.scheme(), "https" | "ssh")
                || url
                    .port()
                    .is_some_and(|port| url.scheme() != "ssh" || port != 22)
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("Unsupported GitHub remote".into());
            }
            (
                url.host_str().ok_or("Missing GitHub hostname")?.to_owned(),
                url.path().trim_start_matches('/').to_owned(),
            )
        };
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        {
            return Err("Invalid GitHub hostname".into());
        }
        let path = path.strip_suffix(".git").unwrap_or(&path);
        let (owner, name) = path.split_once('/').ok_or("Missing GitHub repository")?;
        if [owner, name].into_iter().any(|part| {
            part.is_empty()
                || part.starts_with('-')
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        }) {
            return Err("Invalid GitHub repository".into());
        }
        Ok(Self {
            host,
            owner: owner.into(),
            name: name.into(),
        })
    }

    #[must_use]
    pub fn selector(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.name)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestActor {
    pub login: String,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestSummary {
    pub number: u32,
    pub title: String,
    pub state: String,
    pub is_draft: bool,
    pub url: String,
    pub head_ref_name: String,
    pub base_ref_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestRef {
    pub sha: String,
    #[serde(rename = "ref")]
    pub branch: String,
    #[serde(default)]
    pub repo: Option<PullRequestRepository>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestRepository {
    pub full_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequest {
    pub number: u32,
    pub node_id: String,
    pub title: String,
    pub body: Option<String>,
    pub html_url: String,
    pub state: String,
    pub draft: bool,
    pub head: PullRequestRef,
    pub base: PullRequestRef,
    pub user: PullRequestActor,
    pub merged: bool,
    #[serde(default)]
    pub auto_merge: Option<serde_json::Value>,
    #[serde(default)]
    pub labels: Vec<GitHubLabel>,
    #[serde(default)]
    pub requested_reviewers: Vec<PullRequestActor>,
    #[serde(default)]
    pub requested_teams: Vec<PullRequestTeam>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestTeam {
    pub slug: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestFile {
    pub filename: String,
    pub previous_filename: Option<String>,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub patch: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewComment {
    pub id: String,
    pub author: Option<ReviewActor>,
    pub body: String,
    pub created_at: String,
    pub url: String,
    #[serde(default)]
    pub viewer_can_update: bool,
    #[serde(default)]
    pub reaction_groups: Vec<ReactionGroup>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewActor {
    pub login: String,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentConnection {
    pub total_count: u32,
    pub nodes: Vec<ReviewComment>,
    pub page_info: PageInfo,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThread {
    pub id: String,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub path: String,
    pub line: Option<u32>,
    pub diff_side: crate::diff::DiffSide,
    pub comments: CommentConnection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadConnection {
    pub nodes: Vec<ReviewThread>,
    pub page_info: PageInfo,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent permissions come from GitHub's viewer, repository role and PR authorship"
)]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ViewerPermissions {
    pub can_write: bool,
    pub can_update: bool,
    pub did_author: bool,
    pub can_update_branch: bool,
    pub resolve: bool,
    pub labels: bool,
}

impl ViewerPermissions {
    pub(super) fn from_response(repository: &Value, pr: &Value) -> Result<Self, String> {
        let role = repository
            .get("viewerPermission")
            .and_then(Value::as_str)
            .ok_or("GitHub returned no viewer role")?;
        let can_write = matches!(role, "WRITE" | "MAINTAIN" | "ADMIN");
        let can_update = pr
            .get("viewerCanUpdate")
            .and_then(Value::as_bool)
            .ok_or("GitHub returned no update permission")?;
        let did_author = pr
            .get("viewerDidAuthor")
            .and_then(Value::as_bool)
            .ok_or("GitHub returned no author identity")?;
        Ok(Self {
            can_write,
            can_update,
            did_author,
            can_update_branch: pr
                .get("viewerCanUpdateBranch")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            resolve: can_write || did_author,
            labels: can_write || role == "TRIAGE",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestSnapshot {
    pub repository: GitHubRepository,
    pub pull_request: PullRequest,
    pub permissions: ViewerPermissions,
    pub threads: Vec<ReviewThread>,
    pub threads_truncated: bool,
    pub files: Vec<PullRequestFile>,
    pub files_truncated: bool,
    pub checks: Value,
    pub merge_methods: Vec<MergeMethod>,
    #[serde(default)]
    pub reaction_groups: Vec<ReactionGroup>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewVerdict {
    Comment,
    Approve,
    RequestChanges,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodeComment {
    pub anchor: DiffAnchor,
    pub quote: String,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub expected_head: String,
    #[serde(default)]
    pub expected_base: Option<String>,
    pub verdict: ReviewVerdict,
    pub body: String,
    pub comments: Vec<CodeComment>,
}

impl ReviewRequest {
    pub(super) fn validate(&self) -> Result<(), String> {
        validate_sha(&self.expected_head)?;
        if let Some(base) = &self.expected_base {
            validate_sha(base)?;
        }
        validate_body(
            &self.body,
            self.verdict == ReviewVerdict::Approve || !self.comments.is_empty(),
        )?;
        if self.comments.len() > 100 {
            return Err("A review has at most 100 comments".into());
        }
        for comment in &self.comments {
            validate_body(&comment.body, false)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThreadRequest {
    Reply { id: String, body: String },
    Resolve { id: String, resolved: bool },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestAction {
    Merge,
    Ready,
    Draft,
    Close,
    Reopen,
    UpdateBranch,
    RebaseBranch,
    ApproveWorkflows,
    EnableAutoMerge,
    DisableAutoMerge,
    Revert,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub(super) const fn graphql(self) -> &'static str {
        match self {
            Self::Merge => "MERGE",
            Self::Squash => "SQUASH",
            Self::Rebase => "REBASE",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequestActionRequest {
    pub expected_head: String,
    pub action: PullRequestAction,
    pub merge_method: Option<MergeMethod>,
    #[serde(default)]
    pub stack: Option<StackActionContext>,
}

pub(super) fn validate_body(body: &str, allow_empty: bool) -> Result<(), String> {
    if (!allow_empty && body.trim().is_empty()) || body.len() > 65536 || body.contains('\0') {
        return Err("Enter a comment of at most 64 KB".into());
    }
    Ok(())
}

pub(super) fn validate_sha(sha: &str) -> Result<(), String> {
    if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("A complete observed commit identity is required".into());
    }
    Ok(())
}

pub(super) fn validate_node_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 8192 || id.chars().any(char::is_control) {
        return Err("Invalid GitHub subject identity".into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Reaction {
    ThumbsUp,
    ThumbsDown,
    Laugh,
    Hooray,
    Confused,
    Heart,
    Rocket,
    Eyes,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum MetadataRequest {
    Edit {
        title: Option<String>,
        body: Option<String>,
    },
    Labels {
        names: Vec<String>,
        applied: bool,
    },
    Reviewers {
        logins: Vec<String>,
        teams: Vec<String>,
        requested: bool,
    },
    Comment {
        body: String,
    },
    EditComment {
        id: String,
        body: String,
    },
    React {
        id: Option<String>,
        content: Reaction,
        reacted: bool,
    },
    Viewed {
        expected_head: String,
        path: String,
        viewed: bool,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewedFile {
    pub path: String,
    pub viewer_viewed_state: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewedFilesPage {
    pub nodes: Vec<ViewedFile>,
    pub page_info: PageInfo,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Labels,
    Reviewers,
    Teams,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GitHubLabel {
    pub name: String,
    pub color: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReactionGroup {
    pub content: Reaction,
    pub viewer_has_reacted: bool,
    pub users: ReactionCount,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReactionCount {
    pub total_count: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityItem {
    pub id: String,
    #[serde(rename = "__typename")]
    pub kind: String,
    pub created_at: Option<String>,
    pub body: Option<String>,
    pub author: Option<ReviewActor>,
    pub actor: Option<ReviewActor>,
    pub state: Option<String>,
    pub commit: Option<ActivityCommit>,
    #[serde(default)]
    pub viewer_can_update: bool,
    #[serde(default)]
    pub reaction_groups: Vec<ReactionGroup>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityCommit {
    pub oid: String,
    pub message_headline: String,
    pub committed_date: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPage {
    pub nodes: Vec<ActivityItem>,
    pub page_info: PageInfo,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GitHubCandidate {
    pub name: String,
    pub description: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CandidatePage {
    pub candidates: Vec<GitHubCandidate>,
    pub next_page: Option<u32>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowApproval {
    pub database_id: u64,
    pub workflow_name: String,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkflowHead {
    pub number: u32,
    pub head_ref_oid: String,
    pub is_cross_repository: bool,
    pub head_repository_owner: Option<WorkflowOwner>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct WorkflowOwner {
    pub login: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PullRequestStack {
    pub number: u32,
    pub url: String,
    pub base: String,
    pub layers: Vec<StackLayer>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct StackLayer {
    pub number: u32,
    pub title: String,
    pub branch: String,
    pub head: String,
    pub state: String,
    pub draft: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StackActionContext {
    pub number: u32,
    pub heads: Vec<StackHead>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StackHead {
    pub number: u32,
    pub head: String,
}
