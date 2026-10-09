//! Bounded full-file reads use immutable revisions, including renames and deletions.
use super::{GitHub, PullRequest, PullRequestFile, validate_sha};
use crate::{
    diff::{DiffHunk, DiffLine, FileDiff},
    runner::CommandRunner,
};
use serde::{Deserialize, Serialize};
use std::{
    io::Write as _,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequestDiffRequest {
    pub head: String,
    pub base: String,
    pub path: String,
    pub context_lines: u32,
}

impl<R: CommandRunner> GitHub<'_, R> {
    /// # Errors
    /// Rejects changed revisions, foreign files, binary/oversized contents and malformed patches.
    pub fn file_diff(
        &self,
        number: u32,
        request: &PullRequestDiffRequest,
    ) -> Result<FileDiff, String> {
        validate_sha(&request.head)?;
        validate_sha(&request.base)?;
        if request.context_lines > 100_000 {
            return Err("Diff context exceeds the file limit".into());
        }
        let current: PullRequest = self.rest("GET", &self.pull(number)?, None)?;
        check_revisions(&current, request)?;
        let (files, _) = self.files(number)?;
        let file = files
            .iter()
            .find(|file| file.filename == request.path)
            .ok_or("File is no longer in this pull request")?;
        let diff = self.complete_file_diff(file, request)?;
        let current: PullRequest = self.rest("GET", &self.pull(number)?, None)?;
        check_revisions(&current, request)?;
        Ok(diff)
    }

    pub(super) fn complete_file_diff(
        &self,
        file: &PullRequestFile,
        request: &PullRequestDiffRequest,
    ) -> Result<FileDiff, String> {
        FileDiff::parse(file.filename.clone(), file.previous_filename.clone(), None)?;
        let old = if file.status == "added" {
            String::new()
        } else {
            self.file_contents(
                &request.base,
                file.previous_filename.as_deref().unwrap_or(&file.filename),
            )?
        };
        let new = if file.status == "removed" {
            String::new()
        } else {
            self.file_contents(&request.head, &file.filename)?
        };
        // Git computes a host-neutral patch locally from private temporary copies. No checkout,
        // object database or remote workspace is modified, and the copies disappear on return.
        let mut old_file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        let mut new_file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        old_file
            .write_all(old.as_bytes())
            .map_err(|e| e.to_string())?;
        new_file
            .write_all(new.as_bytes())
            .map_err(|e| e.to_string())?;
        let runner = bootty_host::CancellableCommandRunner::with_deadline(
            bootty_host::CommandCancellation::default(),
            Instant::now()
                .checked_add(Duration::from_secs(10))
                .ok_or("Diff deadline overflow")?,
        );
        let output = runner
            .run(
                "git",
                &[
                    "diff".into(),
                    "--no-index".into(),
                    "--no-ext-diff".into(),
                    "--no-textconv".into(),
                    "--text".into(),
                    format!("--unified={}", request.context_lines),
                    "--".into(),
                    old_file.path().to_string_lossy().into_owned(),
                    new_file.path().to_string_lossy().into_owned(),
                ],
            )
            .map_err(|e| e.to_string())?;
        // git diff exits 1 for differences; an execution failure has no valid unified patch.
        if !output.success && (!output.stderr.trim().is_empty() || !output.stdout.contains("@@ ")) {
            return Err(format!(
                "Could not compare file contents: {}",
                output.stderr.trim()
            ));
        }
        let mut diff = FileDiff::parse(
            file.filename.clone(),
            file.previous_filename.clone(),
            Some(&output.stdout),
        )?;
        if old == new && request.context_lines == 100_000 && !new.is_empty() {
            let lines = new
                .lines()
                .enumerate()
                .map(|(index, text)| {
                    let number = u32::try_from(index.saturating_add(1))
                        .map_err(|_| "File has too many lines")?;
                    Ok(DiffLine {
                        old_line: Some(number),
                        new_line: Some(number),
                        text: text.into(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            diff.hunks.push(DiffHunk {
                header: format!("@@ -1,{} +1,{} @@", lines.len(), lines.len()),
                lines,
            });
        }
        Ok(diff)
    }

    fn file_contents(&self, revision: &str, path: &str) -> Result<String, String> {
        let mut url = url::Url::parse("https://github.invalid/").map_err(|e| e.to_string())?;
        {
            let mut segments = url.path_segments_mut().map_err(|()| "Invalid file path")?;
            for segment in path.split('/') {
                segments.push(segment);
            }
        }
        let endpoint = format!(
            "repos/{}/{}/contents/{}?ref={revision}",
            self.repository.owner,
            self.repository.name,
            url.path().trim_start_matches('/')
        );
        let contents = self.command(
            &[
                "api".into(),
                "--hostname".into(),
                self.repository.host.clone(),
                "--header".into(),
                "Accept: application/vnd.github.raw+json".into(),
                endpoint,
            ],
            None,
        )?;
        if contents.len() > 1024 * 1024 {
            return Err("File exceeds the 1 MB review limit".into());
        }
        if contents.contains('\0') {
            return Err("Binary files cannot be expanded as source".into());
        }
        Ok(contents)
    }
}

fn check_revisions(pr: &PullRequest, request: &PullRequestDiffRequest) -> Result<(), String> {
    if pr.head.sha != request.head || pr.base.sha != request.base {
        return Err("The pull request changed. Refresh before expanding the file.".into());
    }
    Ok(())
}
