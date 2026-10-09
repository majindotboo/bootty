//! Remote stack actions never rewrite the local checkout.
/* Native stack behavior adapted from T3 Code.
MIT License

Copyright (c) 2026 T3 Tools Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/

use super::{
    GitHub, PullRequestAction, PullRequestActionRequest, PullRequestStack, StackActionContext,
    StackHead, StackLayer, validate_sha,
};
use crate::runner::CommandRunner;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StackMergeRequest {
    pub repository: super::GitHubRepository,
    pub number: u32,
    pub operation: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum StackMergeStatus {
    Merged,
    Enqueued,
    Pending { details: StackMergeOperation },
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StackMergeOperation {
    pub uuid: String,
}

impl StackMergeStatus {
    /// Decode only GitHub's supported outcomes, retaining the pending operation identity.
    /// # Errors
    /// Rejects unknown outcomes and missing or unsafe operation identifiers.
    pub fn from_response(value: Value) -> Result<Self, String> {
        let status: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if let Self::Pending { details } = &status {
            validate_operation(&details.uuid)?;
        }
        Ok(status)
    }
}

fn validate_operation(operation: &str) -> Result<(), String> {
    if operation.is_empty()
        || operation.len() > 128
        || !operation
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("GitHub returned an invalid stack merge operation".into());
    }
    Ok(())
}

impl<R: CommandRunner> GitHub<'_, R> {
    /// Read an acknowledged stack merge without submitting another merge.
    /// # Errors
    /// Rejects a changed repository or operation, and reports host/API failures.
    pub fn stack_merge_status(
        &self,
        request: &StackMergeRequest,
    ) -> Result<StackMergeStatus, String> {
        if request.repository != self.repository {
            return Err("The repository changed while observing the stack merge".into());
        }
        validate_operation(&request.operation)?;
        let value: Value = self.rest(
            "GET",
            &format!(
                "{}/merge-async/{}",
                self.pull(request.number)?,
                request.operation
            ),
            None,
        )?;
        if value
            .pointer("/details/uuid")
            .and_then(Value::as_str)
            .is_some_and(|id| id != request.operation)
        {
            return Err("GitHub returned a different stack merge operation".into());
        }
        let status = StackMergeStatus::from_response(value)?;
        Ok(status)
    }
    /// Read GitHub's native stack, in bottom-to-top order.
    /// # Errors
    /// Reports unsupported hosts, malformed membership or missing revisions.
    pub fn stack(&self, number: u32) -> Result<Option<PullRequestStack>, String> {
        self.pull(number)?;
        let endpoint = format!(
            "repos/{}/{}/stacks?pull_request={number}",
            self.repository.owner, self.repository.name
        );
        let stacks: Vec<Value> = self.rest("GET", &endpoint, None)?;
        if stacks.len() > 1 {
            return Err("GitHub returned ambiguous stack membership".into());
        }
        stacks
            .first()
            .map(|stack| decode(stack, number))
            .transpose()
    }

    pub(super) fn stack_action(
        &self,
        number: u32,
        request: &PullRequestActionRequest,
        expected: &StackActionContext,
    ) -> Result<Value, String> {
        if !matches!(
            request.action,
            PullRequestAction::Merge | PullRequestAction::RebaseBranch
        ) {
            return Err("This operation is not supported for a stack".into());
        }
        let stack = self
            .stack(number)?
            .ok_or("The pull request no longer belongs to this stack")?;
        if stack.number != expected.number {
            return Err("The stack changed. Refresh before trying again.".into());
        }
        let target = stack
            .layers
            .iter()
            .position(|layer| layer.number == number)
            .ok_or("The stack changed")?;
        if request.action == PullRequestAction::RebaseBranch
            && target != stack.layers.len().saturating_sub(1)
        {
            return Err("Rebase a stack from its top pull request".into());
        }
        if stack
            .layers
            .get(target)
            .ok_or("The stack target disappeared")?
            .head
            != request.expected_head
        {
            return Err("The pull request changed. Refresh the stack.".into());
        }
        let affected = if request.action == PullRequestAction::Merge {
            stack
                .layers
                .get(..=target)
                .ok_or("The stack target disappeared")?
        } else {
            &stack.layers
        };
        let open = affected
            .iter()
            .filter(|layer| layer.state != "merged")
            .collect::<Vec<_>>();
        if open.is_empty()
            || open.iter().any(|layer| layer.state != "open")
            || expected.heads.len() != open.len()
            || expected
                .heads
                .iter()
                .map(|head| head.number)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != open.len()
            || open.iter().any(|layer| {
                !expected
                    .heads
                    .iter()
                    .any(|head| head.number == layer.number && head.head == layer.head)
            })
        {
            return Err("The stack changed. Refresh before trying again.".into());
        }
        if request.action == PullRequestAction::RebaseBranch {
            return self.rebase_stack(&open);
        }
        if open.iter().any(|layer| layer.draft) {
            return Err("Mark every affected stack layer ready before merging".into());
        }
        let (_, permissions, methods) = self.access(number)?;
        if !permissions.can_write {
            return Err("Your account cannot merge this stack".into());
        }
        let method = Self::choose_merge_method(request.merge_method, &methods)?;
        let result:Value = self.rest("PUT",&format!("{}/merge-async",self.pull(number)?),Some(json!({"merge_method":match method {super::MergeMethod::Merge=>"merge",super::MergeMethod::Squash=>"squash",super::MergeMethod::Rebase=>"rebase"},"merge_action":"default","sha":request.expected_head})))?;
        let status = StackMergeStatus::from_response(result)?;
        if status == StackMergeStatus::Failed {
            return Err(
                "GitHub refused the stack merge. Check its branch rules and merge requirements."
                    .into(),
            );
        }
        serde_json::to_value(status).map_err(|e| e.to_string())
    }

    fn rebase_stack(&self, layers: &[&StackLayer]) -> Result<Value, String> {
        // Check every branch before the first mutation; current branches may not need an update yet.
        for layer in layers {
            let access = self.graphql("query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){headRepository{viewerPermission} maintainerCanModify}}}",&json!({"owner":self.repository.owner,"name":self.repository.name,"number":layer.number}))?;
            let pr = access
                .pointer("/repository/pullRequest")
                .ok_or("Missing stack branch access")?;
            if pr.get("headRepository").is_none_or(Value::is_null)
                || (pr.get("maintainerCanModify") != Some(&Value::Bool(true))
                    && !matches!(
                        pr.pointer("/headRepository/viewerPermission")
                            .and_then(Value::as_str),
                        Some("WRITE" | "MAINTAIN" | "ADMIN")
                    ))
            {
                return Err("You cannot update every branch in this stack".into());
            }
        }
        let mut processed = Vec::<StackHead>::new();
        for layer in layers {
            let head = self.rebase_stack_layer(layer,&processed).map_err(|error|format!("Stack rebase stopped at PR #{} after {} layers: {error}. Earlier updates remain on GitHub.",layer.number,processed.len()))?;
            processed.push(head);
        }
        Ok(json!({"heads":processed}))
    }

    fn rebase_stack_layer(
        &self,
        layer: &StackLayer,
        processed: &[StackHead],
    ) -> Result<StackHead, String> {
        for prior in processed {
            let pr: super::PullRequest = self.rest("GET", &self.pull(prior.number)?, None)?;
            if pr.head.sha != prior.head {
                return Err("An earlier stack layer changed during the rebase".into());
            }
        }
        let result = self.graphql("query($owner:String!,$name:String!,$number:Int!,$sha:String!){repository(owner:$owner,name:$name){pullRequest(number:$number){id headRefOid baseRef{compare(headRef:$sha){behindBy}}}}}",&json!({"owner":self.repository.owner,"name":self.repository.name,"number":layer.number,"sha":layer.head}))?;
        let pr = result
            .pointer("/repository/pullRequest")
            .ok_or("Missing stack branch")?;
        if pr.get("headRefOid").and_then(Value::as_str) != Some(layer.head.as_str()) {
            return Err("This stack layer changed during the rebase".into());
        }
        let behind = pr
            .pointer("/baseRef/compare/behindBy")
            .and_then(Value::as_u64)
            .ok_or("GitHub returned no stack comparison")?;
        let head = if behind == 0 {
            layer.head.clone()
        } else {
            let id = pr
                .get("id")
                .and_then(Value::as_str)
                .ok_or("Missing stack layer identity")?;
            let updated = self.graphql("mutation($id:ID!,$sha:GitObjectID!){updatePullRequestBranch(input:{pullRequestId:$id,expectedHeadOid:$sha,updateMethod:REBASE}){pullRequest{headRefOid}}}",&json!({"id":id,"sha":layer.head}))?;
            let head = updated
                .pointer("/updatePullRequestBranch/pullRequest/headRefOid")
                .and_then(Value::as_str)
                .ok_or("GitHub did not report the rebased revision")?;
            validate_sha(head)?;
            head.into()
        };
        Ok(StackHead {
            number: layer.number,
            head,
        })
    }
}

fn decode(value: &Value, number: u32) -> Result<PullRequestStack, String> {
    let integer = |value: &Value| {
        value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n != 0)
            .ok_or("Invalid stack number")
    };
    let string = |value: &Value| {
        value
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 8192 && !s.chars().any(char::is_control))
            .map(str::to_owned)
            .ok_or("Invalid stack text")
    };
    let raw = value
        .get("pull_requests")
        .and_then(Value::as_array)
        .filter(|layers| !layers.is_empty() && layers.len() <= 100)
        .ok_or("Invalid stack layers")?;
    let mut layers = Vec::new();
    for layer in raw {
        let head = string(layer.pointer("/head/sha").unwrap_or(&Value::Null))?;
        validate_sha(&head)?;
        layers.push(StackLayer {
            number: integer(&layer["number"])?,
            title: layer["title"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(1024)
                .collect(),
            branch: string(layer.pointer("/head/ref").unwrap_or(&Value::Null))?,
            head,
            state: if layer.get("merged_at").is_some_and(|v| !v.is_null()) {
                "merged".into()
            } else {
                string(&layer["state"])?
            },
            draft: layer
                .get("draft")
                .and_then(Value::as_bool)
                .ok_or("Missing stack review state")?,
        });
    }
    if !layers.iter().any(|layer| layer.number == number)
        || layers
            .iter()
            .map(|layer| layer.number)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != layers.len()
    {
        return Err("Invalid stack membership".into());
    }
    Ok(PullRequestStack {
        number: integer(&value["number"])?,
        url: string(
            value
                .get("html_url")
                .filter(|v| !v.is_null())
                .unwrap_or_else(|| &value["url"]),
        )?,
        base: string(if value["base"].is_string() {
            &value["base"]
        } else {
            value.pointer("/base/ref").unwrap_or(&Value::Null)
        })?,
        layers,
    })
}
