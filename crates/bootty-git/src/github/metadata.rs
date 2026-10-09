//! PR metadata, reactions and host-owned viewed-file operations.

use serde_json::{Value, json};

use super::{
    CandidateKind, GitHub, MetadataRequest, ViewedFilesPage, queries, validate_body,
    validate_node_id, validate_sha,
};
use crate::{diff::FileDiff, runner::CommandRunner};

impl<R: CommandRunner> GitHub<'_, R> {
    /// Apply one explicit metadata mutation to this repository's pull request.
    /// # Errors
    /// Rejects foreign subjects, stale heads, invalid text and denied host permissions.
    pub fn metadata(&self, number: u32, request: &MetadataRequest) -> Result<Value, String> {
        let (pr, permissions, _) = self.access(number)?;
        match request {
            MetadataRequest::Edit { title, body } => {
                if !permissions.can_update || title.is_none() && body.is_none() {
                    return Err("This account cannot edit the pull request".into());
                }
                self.edit(number, title.as_deref(), body.as_deref())
            }
            MetadataRequest::Labels { names, applied } => {
                if !permissions.labels {
                    return Err("This account cannot change labels".into());
                }
                validate_names(names)?;
                let endpoint = self.issue(number, "labels");
                if *applied {
                    self.rest("POST", &endpoint, Some(json!({"labels":names})))
                } else {
                    let mut results = Vec::new();
                    for name in names {
                        results.push(self.rest::<Value>(
                            "DELETE",
                            &encoded_segment(&endpoint, name)?,
                            None,
                        )?);
                    }
                    Ok(json!(results))
                }
            }
            MetadataRequest::Reviewers {
                logins,
                teams,
                requested,
            } => {
                if !permissions.can_write {
                    return Err("This account cannot request reviewers".into());
                }
                validate_names(logins)?;
                validate_names(teams)?;
                if logins.is_empty() && teams.is_empty() {
                    return Err("Select at least one reviewer".into());
                }
                self.rest(
                    if *requested { "POST" } else { "DELETE" },
                    &format!("{}/requested_reviewers", self.pull(number)?),
                    Some(json!({"reviewers":logins,"team_reviewers":teams})),
                )
            }
            MetadataRequest::Comment { body } => {
                validate_body(body, false)?;
                self.rest(
                    "POST",
                    &self.issue(number, "comments"),
                    Some(json!({"body":body})),
                )
            }
            MetadataRequest::EditComment { id, body } => {
                validate_body(body, false)?;
                let subject = self.subject(number, id)?;
                if subject.get("viewerCanUpdate") != Some(&Value::Bool(true)) {
                    return Err("This account cannot edit the comment".into());
                }
                let query = match subject.get("__typename").and_then(Value::as_str) {
                    Some("IssueComment") => queries::UPDATE_ISSUE_COMMENT,
                    Some("PullRequestReviewComment") => queries::UPDATE_REVIEW_COMMENT,
                    _ => return Err("This subject is not an editable comment".into()),
                };
                self.graphql(query, &json!({"id":id,"body":body}))
            }
            MetadataRequest::React {
                id,
                content,
                reacted,
            } => {
                let id = id.as_deref().unwrap_or(&pr.node_id);
                self.subject(number, id)?;
                self.graphql(
                    if *reacted {
                        queries::ADD_REACTION
                    } else {
                        queries::REMOVE_REACTION
                    },
                    &json!({"id":id,"content":content}),
                )
            }
            MetadataRequest::Viewed {
                expected_head,
                path,
                viewed,
            } => self.set_viewed(number, &pr, expected_head, path, *viewed),
        }
    }

    fn set_viewed(
        &self,
        number: u32,
        pr: &super::PullRequest,
        expected_head: &str,
        path: &str,
        viewed: bool,
    ) -> Result<Value, String> {
        validate_sha(expected_head)?;
        FileDiff::parse(path.into(), None, None)?;
        if pr.head.sha != expected_head {
            return Err("The pull request changed. Refresh before marking files viewed.".into());
        }
        let (files, _) = self.files(number)?;
        if !files.iter().any(|file| file.filename == path) {
            return Err("The file is not in this pull request".into());
        }
        self.graphql(
            if viewed {
                queries::MARK_VIEWED
            } else {
                queries::UNMARK_VIEWED
            },
            &json!({"id":pr.node_id,"path":path}),
        )
    }

    fn edit(&self, number: u32, title: Option<&str>, body: Option<&str>) -> Result<Value, String> {
        let mut payload = serde_json::Map::new();
        if let Some(title) = title {
            validate_body(title, false)?;
            if title.chars().any(char::is_control) || title.len() > 1024 {
                return Err("Enter a title of at most 1 KB".into());
            }
            payload.insert("title".into(), json!(title));
        }
        if let Some(body) = body {
            validate_body(body, true)?;
            payload.insert("body".into(), json!(body));
        }
        self.rest("PATCH", &self.pull(number)?, Some(payload.into()))
    }

    /// Read the host's viewed-file state one bounded page at a time.
    /// # Errors
    /// Returns malformed cursor, inaccessible PR or host errors.
    pub fn viewed_files(&self, number: u32, cursor: &str) -> Result<ViewedFilesPage, String> {
        self.pull(number)?;
        if !cursor.is_empty() {
            validate_node_id(cursor)?;
        }
        let variables = json!({"owner":self.repository.owner,"name":self.repository.name,"number":number,"cursor":if cursor.is_empty() { Value::Null } else { json!(cursor) }});
        let response = self.graphql(queries::VIEWED, &variables)?;
        serde_json::from_value(
            response
                .pointer("/repository/pullRequest/files")
                .cloned()
                .ok_or("GitHub returned no viewed files")?,
        )
        .map_err(|e| e.to_string())
    }

    /// Read the timeline using the host's cursor without substituting rendered row positions.
    /// # Errors
    /// Returns invalid cursor, malformed response or host/auth failures.
    pub fn activity(&self, number: u32, cursor: &str) -> Result<super::ActivityPage, String> {
        self.pull(number)?;
        if !cursor.is_empty() {
            validate_node_id(cursor)?;
        }
        let variables = json!({"owner":self.repository.owner,"name":self.repository.name,"number":number,"cursor":if cursor.is_empty() { Value::Null } else { json!(cursor) }});
        let response = self.graphql(queries::ACTIVITY, &variables)?;
        serde_json::from_value(
            response
                .pointer("/repository/pullRequest/timelineItems")
                .cloned()
                .ok_or("GitHub returned no activity")?,
        )
        .map_err(|e| e.to_string())
    }

    /// Read a candidate page on the repository host, preserving pagination.
    /// # Errors
    /// Rejects invalid page numbers and host/auth failures.
    pub fn candidates(
        &self,
        kind: CandidateKind,
        page: u32,
    ) -> Result<super::CandidatePage, String> {
        if !(1..=100).contains(&page) {
            return Err("Invalid candidate page".into());
        }
        let path = match kind {
            CandidateKind::Labels => "labels",
            CandidateKind::Reviewers => "collaborators",
            CandidateKind::Teams => "teams",
        };
        let rows: Vec<Value> = self.rest(
            "GET",
            &format!(
                "repos/{}/{}/{path}?per_page=100&page={page}",
                self.repository.owner, self.repository.name
            ),
            None,
        )?;
        let full = rows.len() == 100;
        if rows.len() > 100 {
            return Err("GitHub returned an oversized candidate page".into());
        }
        let mut candidates = Vec::new();
        for row in rows {
            let field = match kind {
                CandidateKind::Labels => "name",
                CandidateKind::Reviewers => "login",
                CandidateKind::Teams => "slug",
            };
            let name = row
                .get(field)
                .and_then(Value::as_str)
                .filter(|name| {
                    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
                })
                .ok_or("GitHub returned an invalid candidate")?
                .to_owned();
            let description = row
                .get("description")
                .or_else(|| row.get("name"))
                .and_then(Value::as_str)
                .map(|text| text.chars().take(512).collect());
            candidates.push(super::GitHubCandidate { name, description });
        }
        Ok(super::CandidatePage {
            candidates,
            next_page: if full && page < 100 {
                Some(page.saturating_add(1))
            } else {
                None
            },
            truncated: full && page == 100,
        })
    }

    fn issue(&self, number: u32, suffix: &str) -> String {
        format!(
            "repos/{}/{}/issues/{number}/{suffix}",
            self.repository.owner, self.repository.name
        )
    }

    fn subject(&self, number: u32, id: &str) -> Result<Value, String> {
        validate_node_id(id)?;
        let variables = json!({"owner":self.repository.owner,"name":self.repository.name,"number":number,"id":id});
        let response = self.graphql(queries::SUBJECT_SCOPE, &variables)?;
        let expected = response
            .pointer("/repository/pullRequest/id")
            .and_then(Value::as_str)
            .ok_or("Pull request is unavailable")?;
        let subject = response
            .get("node")
            .filter(|v| !v.is_null())
            .ok_or("Comment is unavailable")?;
        let actual = subject
            .pointer("/pullRequest/id")
            .or_else(|| subject.get("id"))
            .and_then(Value::as_str);
        if actual != Some(expected) {
            return Err("This subject is not in this pull request".into());
        }
        Ok(subject.clone())
    }
}

fn validate_names(names: &[String]) -> Result<(), String> {
    if names.len() > 100
        || names
            .iter()
            .any(|name| name.is_empty() || name.len() > 256 || name.chars().any(char::is_control))
    {
        return Err("Invalid label or reviewer selection".into());
    }
    Ok(())
}

fn encoded_segment(endpoint: &str, segment: &str) -> Result<String, String> {
    let mut url =
        url::Url::parse(&format!("https://github.com/{endpoint}")).map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|()| "Invalid API path")?
        .push(segment);
    Ok(url.path().trim_start_matches('/').into())
}
