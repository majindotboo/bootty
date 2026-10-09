// Ported from T3 Code's GitHubPullRequestCli and gitHubPullRequestJson queries.
// MIT License
// Copyright (c) 2026 T3 Tools Inc.
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.
pub(super) const THREADS: &str = r"query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
  repository(owner:$owner,name:$name) {
    viewerPermission
    mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed
    pullRequest(number:$number) {
      headRefOid baseRefOid viewerCanUpdate viewerDidAuthor viewerCanUpdateBranch
      reactionGroups { content viewerHasReacted users { totalCount } }
      reviewThreads(first:100,after:$cursor) {
        pageInfo { hasNextPage endCursor }
        nodes { id isResolved isOutdated path line diffSide
          comments(first:10) { totalCount pageInfo { hasNextPage endCursor }
            nodes { id body createdAt url viewerCanUpdate author { login avatarUrl } reactionGroups { content viewerHasReacted users { totalCount } } }
          }
        }
      }
      commits(last:1) { nodes { commit { statusCheckRollup { contexts(first:100) {
        pageInfo { hasNextPage endCursor }
        nodes {
          ... on CheckRun { name status conclusion detailsUrl }
          ... on StatusContext { context state description targetUrl }
        }
      } } } } }
    }
  }
}";
pub(super) const REPLY: &str = r"mutation($threadId:ID!,$body:String!) { addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$threadId,body:$body}) { comment { id } } }";
pub(super) const COMMENTS: &str = r"query($id:ID!,$cursor:String) {
  node(id:$id) { ... on PullRequestReviewThread {
    pullRequest { number repository { nameWithOwner } }
    comments(first:100,after:$cursor) { totalCount pageInfo { hasNextPage endCursor }
      nodes { id body createdAt url viewerCanUpdate author { login avatarUrl } reactionGroups { content viewerHasReacted users { totalCount } } }
    }
  } }
}";
pub(super) const RESOLVE: &str = r"mutation($threadId:ID!) { resolveReviewThread(input:{threadId:$threadId}) { thread { isResolved } } }";
pub(super) const UNRESOLVE: &str = r"mutation($threadId:ID!) { unresolveReviewThread(input:{threadId:$threadId}) { thread { isResolved } } }";
pub(super) const READY: &str = r"mutation($id:ID!) { markPullRequestReadyForReview(input:{pullRequestId:$id}) { pullRequest { id } } }";
pub(super) const DRAFT: &str = r"mutation($id:ID!) { convertPullRequestToDraft(input:{pullRequestId:$id}) { pullRequest { id } } }";
pub(super) const AUTO_MERGE: &str = r"mutation($id:ID!,$method:PullRequestMergeMethod!) { enablePullRequestAutoMerge(input:{pullRequestId:$id,mergeMethod:$method}) { pullRequest { id } } }";
pub(super) const DISABLE_AUTO_MERGE: &str = r"mutation($id:ID!) { disablePullRequestAutoMerge(input:{pullRequestId:$id}) { pullRequest { id } } }";

pub(super) const ACCESS: &str = r"query($owner:String!,$name:String!,$number:Int!) {
 repository(owner:$owner,name:$name) {
  viewerPermission mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed
  pullRequest(number:$number) { id headRefOid viewerCanUpdate viewerDidAuthor viewerCanUpdateBranch }
 }
}";
pub(super) const THREAD_SCOPE: &str = r"query($id:ID!) {
 node(id:$id) { ... on PullRequestReviewThread { pullRequest { number repository { nameWithOwner } } } }
}";
pub(super) const SUBJECT_SCOPE: &str = r"query($owner:String!,$name:String!,$number:Int!,$id:ID!) {
 repository(owner:$owner,name:$name) { pullRequest(number:$number) { id } }
 node(id:$id) { id __typename
  ... on IssueComment { viewerCanUpdate pullRequest { id } }
  ... on PullRequestReviewComment { viewerCanUpdate pullRequest { id } }
  ... on PullRequestReview { pullRequest { id } }
 }
}";
pub(super) const REVERT: &str = r"mutation($id:ID!) { revertPullRequest(input:{pullRequestId:$id}) { revertPullRequest { id number url } } }";
pub(super) const ADD_REACTION: &str = r"mutation($id:ID!,$content:ReactionContent!) { addReaction(input:{subjectId:$id,content:$content}) { reaction { content } } }";
pub(super) const REMOVE_REACTION: &str = r"mutation($id:ID!,$content:ReactionContent!) { removeReaction(input:{subjectId:$id,content:$content}) { reaction { content } } }";
pub(super) const UPDATE_ISSUE_COMMENT: &str = r"mutation($id:ID!,$body:String!) { updateIssueComment(input:{id:$id,body:$body}) { issueComment { id } } }";
pub(super) const UPDATE_REVIEW_COMMENT: &str = r"mutation($id:ID!,$body:String!) { updatePullRequestReviewComment(input:{pullRequestReviewCommentId:$id,body:$body}) { pullRequestReviewComment { id } } }";
pub(super) const MARK_VIEWED: &str = r"mutation($id:ID!,$path:String!) { markFileAsViewed(input:{pullRequestId:$id,path:$path}) { pullRequest { id } } }";
pub(super) const UNMARK_VIEWED: &str = r"mutation($id:ID!,$path:String!) { unmarkFileAsViewed(input:{pullRequestId:$id,path:$path}) { pullRequest { id } } }";
pub(super) const VIEWED: &str = r"query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
 repository(owner:$owner,name:$name) { pullRequest(number:$number) {
  headRefOid files(first:100,after:$cursor) {
   pageInfo { hasNextPage endCursor } nodes { path viewerViewedState }
  }
 } }
}";

pub(super) const ACTIVITY: &str = r"query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
 repository(owner:$owner,name:$name) { pullRequest(number:$number) {
  timelineItems(first:100,after:$cursor,itemTypes:[ISSUE_COMMENT,PULL_REQUEST_REVIEW,PULL_REQUEST_COMMIT,MERGED_EVENT,CLOSED_EVENT,REOPENED_EVENT,READY_FOR_REVIEW_EVENT,CONVERT_TO_DRAFT_EVENT]) {
   pageInfo { hasNextPage endCursor }
   nodes {
    __typename
    ... on IssueComment { id body createdAt viewerCanUpdate author { login avatarUrl } reactionGroups { content viewerHasReacted users { totalCount } } }
    ... on PullRequestReview { id body createdAt state author { login avatarUrl } reactionGroups { content viewerHasReacted users { totalCount } } }
    ... on PullRequestCommit { id commit { oid messageHeadline committedDate } }
    ... on MergedEvent { id createdAt actor { login avatarUrl } }
    ... on ClosedEvent { id createdAt actor { login avatarUrl } }
    ... on ReopenedEvent { id createdAt actor { login avatarUrl } }
    ... on ReadyForReviewEvent { id createdAt actor { login avatarUrl } }
    ... on ConvertToDraftEvent { id createdAt actor { login avatarUrl } }
   }
  }
 } }
}";

pub(super) const REBASE_BRANCH: &str = r"mutation($id:ID!,$sha:GitObjectID!) { updatePullRequestBranch(input:{pullRequestId:$id,expectedHeadOid:$sha,updateMethod:REBASE}) { pullRequest { headRefOid } } }";
