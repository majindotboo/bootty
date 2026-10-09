use anyhow::{Context as _, ensure};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use bootty_git::{
    Git,
    diff::{DiffAnchor, DiffSide},
    github::{
        CodeComment, GitHubRepository, MergeMethod, MetadataRequest, PullRequestAction,
        PullRequestActionRequest, Reaction, ReviewRequest, ReviewVerdict, ThreadRequest,
    },
    runner::{CommandOutput, CommandRunner},
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Value, json};

const HEAD: &str = "1111111111111111111111111111111111111111";
const NEXT: &str = "2222222222222222222222222222222222222222";

#[derive(Clone, Copy, PartialEq, Eq)]
enum BaseChange {
    Threads,
    Files,
}

#[rstest]
#[case("git@github.com:owner/repo.git", "github.com")]
#[case(
    "https://github.enterprise.test/owner/repo.git",
    "github.enterprise.test"
)]
#[case("ssh://git@github.com/owner/repo.git", "github.com")]
fn repository_preserves_the_exact_remote_host(#[case] remote: &str, #[case] host: &str) {
    assert_eq!(
        GitHubRepository::from_remote(remote).unwrap(),
        GitHubRepository {
            host: host.into(),
            owner: "owner".into(),
            name: "repo".into()
        }
    );
}

#[rstest]
#[case("/local/repository")]
#[case("https://github.com/owner/repo/other")]
#[case("https://github.com/owner/repo?other=repo")]
#[case("git@github.com:owner/../repo")]
fn ambiguous_remotes_are_rejected(#[case] remote: &str) {
    assert!(GitHubRepository::from_remote(remote).is_err());
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent host failure and permission scenarios"
)]
#[derive(Default)]
struct Host {
    writes: Rc<RefCell<Vec<Value>>>,
    different_head: bool,
    base_change: Option<BaseChange>,
    pull_reads: Cell<usize>,
    own_pr: bool,
    scope_match: bool,
    role: Option<&'static str>,
    reject_write: bool,
    ambiguous_fork: bool,
    merge_response: Option<Value>,
    merge_poll: Option<Value>,
    polls: Rc<RefCell<Vec<String>>>,
}

impl Host {
    fn output(value: &Value) -> CommandOutput {
        CommandOutput {
            success: true,
            stdout: value.to_string(),
            stderr: String::new(),
        }
    }
    fn pull() -> Value {
        json!({"number":12,"node_id":"PR_id","title":"Test","body":"Description","html_url":"https://github.enterprise.test/owner/repo/pull/12","state":"open","draft":false,"head":{"sha":HEAD,"ref":"branch","repo":{"full_name":"fork/repo"}},"base":{"sha":NEXT,"ref":"main"},"user":{"login":"author","avatar_url":null},"merged":false})
    }
}

impl CommandRunner for Host {
    fn run_in(&self, cwd: &str, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        assert_eq!(cwd, "/repo");
        assert_eq!(program, "gh");
        assert_eq!(args, ["repo", "view", "--json", "url"]);
        Ok(Self::output(
            &json!({"url":"https://github.enterprise.test/owner/repo"}),
        ))
    }

    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        if program == "git" {
            assert_eq!(
                args,
                [
                    "--literal-pathspecs",
                    "-C",
                    "/repo",
                    "remote",
                    "get-url",
                    "origin"
                ]
            );
            return Ok(CommandOutput {
                success: true,
                stdout: "git@github.enterprise.test:owner/repo.git\n".into(),
                stderr: String::new(),
            });
        }
        assert_eq!(program, "gh");
        if args.first().is_some_and(|arg| arg == "pr" || arg == "run") {
            ensure!(
                args.windows(2)
                    .any(|pair| pair == ["--repo", "github.enterprise.test/owner/repo"]),
                "Unexpected repository"
            );
            if args.first().is_some_and(|arg| arg == "pr") {
                let head = json!({"number":12,"headRefOid":HEAD,"isCrossRepository":true,"headRepositoryOwner":{"login":"fork"}});
                return Ok(Self::output(&if self.ambiguous_fork {
                    json!([head.clone(), head])
                } else {
                    json!([head])
                }));
            }
            ensure!(
                args.windows(2).any(|pair| pair == ["--commit", HEAD]),
                "Wrong workflow commit"
            );
            ensure!(
                args.windows(2)
                    .any(|pair| pair == ["--event", "pull_request"]),
                "Wrong workflow event"
            );
            return Ok(Self::output(
                &json!([{"databaseId":7,"workflowName":"CI","url":"https://github.enterprise.test/owner/repo/actions/runs/7"}]),
            ));
        }
        if args.get(4).is_some_and(|method| method == "POST")
            && args
                .last()
                .is_some_and(|endpoint| endpoint.ends_with("/approve"))
        {
            self.writes
                .borrow_mut()
                .push(json!({"endpoint":args.last()}));
            return Ok(Self::output(&Value::Null));
        }
        assert_eq!(
            args.get(2).map(String::as_str),
            Some("github.enterprise.test")
        );
        let endpoint = args.last().context("Missing API endpoint")?;
        if endpoint.contains("/merge-async/") {
            self.polls.borrow_mut().push(endpoint.clone());
            return Ok(Self::output(
                self.merge_poll
                    .as_ref()
                    .context("Missing merge poll response")?,
            ));
        }
        Ok(Self::output(&if endpoint.contains("/stacks?") {
            json!([{"number":7,"url":"https://github.enterprise.test/owner/repo/stacks/7","base":{"ref":"main"},"pull_requests":[{"number":12,"title":"Test","head":{"sha":HEAD,"ref":"branch"},"state":"open","draft":false}]}])
        } else if endpoint.contains("/files?") {
            json!([{"filename":"file.rs","previous_filename":null,"status":"modified","additions":1,"deletions":1,"patch":"@@ -1 +1 @@\n-old\n+new\n"}])
        } else {
            let reads = self.pull_reads.get();
            self.pull_reads.set(reads.saturating_add(1));
            let mut pull = Self::pull();
            if self.base_change == Some(BaseChange::Files) && reads > 0 {
                *pull
                    .pointer_mut("/base/sha")
                    .context("Missing base revision")? = json!(HEAD);
            }
            pull
        }))
    }
    fn run_with_input(
        &self,
        program: &str,
        args: &[String],
        input: Vec<u8>,
    ) -> anyhow::Result<CommandOutput> {
        assert_eq!(program, "gh");
        assert_eq!(
            args.get(2).map(String::as_str),
            Some("github.enterprise.test")
        );
        let input: Value = serde_json::from_slice(&input)?;
        if let Some(query) = input.get("query").and_then(Value::as_str) {
            if query.starts_with("mutation") {
                self.writes.borrow_mut().push(input.clone());
                return Ok(Self::output(&if self.reject_write {
                    json!({"errors":[{"message":"Rejected"}]})
                } else {
                    json!({"data":{"accepted":{"id":"result"}}})
                }));
            }
            if input.pointer("/variables/id").is_some() {
                return Ok(Self::output(&json!({"data":{
                    "repository":{"pullRequest":{"id":"PR_id"}},
                    "node":{"id":"comment", "__typename":"IssueComment", "viewerCanUpdate":true,
                        "pullRequest":{"id":if self.scope_match { "PR_id" } else { "other" },"number":if self.scope_match {12} else {99},"repository":{"nameWithOwner":"owner/repo"}}}
                }})));
            }

            assert_eq!(input.pointer("/variables/owner"), Some(&json!("owner")));
            assert_eq!(input.pointer("/variables/name"), Some(&json!("repo")));
            return Ok(Self::output(
                &json!({"data":{"repository":{"viewerPermission":self.role.unwrap_or("WRITE"),"mergeCommitAllowed":true,"squashMergeAllowed":false,"rebaseMergeAllowed":false,"pullRequest":{"headRefOid":if self.different_head { NEXT } else { HEAD },"baseRefOid":if self.base_change == Some(BaseChange::Threads) { HEAD } else { NEXT },"viewerCanUpdate":true,"viewerDidAuthor":self.own_pr,"viewerCanUpdateBranch":true,"reviewThreads":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}},"commits":{"nodes":[]}}}}}),
            ));
        }
        self.writes.borrow_mut().push(input);
        Ok(if self.reject_write {
            CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "GitHub refused the write".into(),
            }
        } else {
            Self::output(&if args.iter().any(|arg| arg.ends_with("/merge-async")) {
                self.merge_response
                    .clone()
                    .unwrap_or_else(|| json!({"status":"merged"}))
            } else {
                json!({"id":42})
            })
        })
    }
}

fn review(quote: &str) -> ReviewRequest {
    ReviewRequest {
        expected_base: Some(NEXT.into()),
        expected_head: HEAD.into(),
        verdict: ReviewVerdict::Comment,
        body: String::new(),
        comments: vec![CodeComment {
            anchor: DiffAnchor {
                path: "file.rs".into(),
                side: DiffSide::Right,
                start_line: 1,
                line: 1,
            },
            quote: quote.into(),
            body: "Please change this".into(),
        }],
    }
}

#[rstest]
#[case(None)]
#[case(Some(BaseChange::Threads))]
#[case(Some(BaseChange::Files))]
fn review_loading_keeps_both_revisions_consistent(#[case] base_change: Option<BaseChange>) {
    let git = Git::with_runner(Host {
        base_change,
        ..Host::default()
    });
    let result = git.github("/repo").unwrap().read(12);
    if base_change.is_some() {
        assert!(result.unwrap_err().contains("changed"));
    } else {
        let snapshot = result.unwrap();
        assert_eq!(snapshot.pull_request.head.sha, HEAD);
        assert_eq!(snapshot.pull_request.base.sha, NEXT);
    }
}

#[rstest]
fn review_submits_one_atomic_source_anchored_payload_on_the_repository_host() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        writes: writes.clone(),
        different_head: false,
        own_pr: false,
        ..Host::default()
    });
    git.github("/repo")
        .unwrap()
        .review(12, &review("new"))
        .unwrap();
    assert_eq!(
        *writes.borrow(),
        vec![
            json!({"commit_id":HEAD,"event":"COMMENT","body":"","comments":[{"path":"file.rs","side":"RIGHT","line":1,"body":"Please change this"}]})
        ]
    );
}

#[rstest]
#[case(true, false, "new", ReviewVerdict::Comment)]
#[case(false, false, "stale code", ReviewVerdict::Comment)]
#[case(false, true, "new", ReviewVerdict::Approve)]
fn stale_heads_quotes_and_own_pr_approval_never_write(
    #[case] different_head: bool,
    #[case] own_pr: bool,
    #[case] quote: &str,
    #[case] verdict: ReviewVerdict,
) {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        writes: writes.clone(),
        different_head,
        own_pr,
        ..Host::default()
    });
    let mut request = review(quote);
    request.verdict = verdict;
    assert!(git.github("/repo").unwrap().review(12, &request).is_err());
    assert!(writes.borrow().is_empty());
}

#[rstest]
fn a_thread_from_a_different_pr_cannot_be_replied_to_or_resolved() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        writes: writes.clone(),
        different_head: false,
        own_pr: false,
        ..Host::default()
    });
    for request in [
        ThreadRequest::Reply {
            id: "foreign".into(),
            body: "Reply".into(),
        },
        ThreadRequest::Resolve {
            id: "foreign".into(),
            resolved: true,
        },
    ] {
        assert!(git.github("/repo").unwrap().thread(12, &request).is_err());
    }
    assert!(writes.borrow().is_empty());
}

#[rstest]
#[case(MetadataRequest::React { id:Some("foreign".into()), content:Reaction::Heart, reacted:true })]
#[case(MetadataRequest::EditComment { id:"foreign".into(),body:"Edited".into() })]
fn foreign_subjects_never_mutate(#[case] request: MetadataRequest) {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        writes: writes.clone(),
        ..Host::default()
    });
    assert!(git.github("/repo").unwrap().metadata(12, &request).is_err());
    assert!(writes.borrow().is_empty());
}

#[rstest]
fn triage_can_add_labels_without_being_able_to_request_reviewers() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        writes: writes.clone(),
        role: Some("TRIAGE"),
        ..Host::default()
    });
    let github = git.github("/repo").unwrap();
    github
        .metadata(
            12,
            &MetadataRequest::Labels {
                names: vec!["needs review".into()],
                applied: true,
            },
        )
        .unwrap();
    assert!(
        github
            .metadata(
                12,
                &MetadataRequest::Reviewers {
                    logins: vec!["reviewer".into()],
                    teams: vec![],
                    requested: true
                }
            )
            .is_err()
    );
    assert_eq!(*writes.borrow(), vec![json!({"labels":["needs review"]})]);
}

#[rstest]
fn editing_one_pr_field_preserves_the_other() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        writes: writes.clone(),
        ..Host::default()
    });
    git.github("/repo")
        .unwrap()
        .metadata(
            12,
            &MetadataRequest::Edit {
                title: Some("New title".into()),
                body: None,
            },
        )
        .unwrap();
    assert_eq!(*writes.borrow(), vec![json!({"title":"New title"})]);
}

#[rstest]
fn a_disallowed_merge_method_never_writes() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        writes: writes.clone(),
        ..Host::default()
    });
    assert!(
        git.github("/repo")
            .unwrap()
            .action(
                12,
                &PullRequestActionRequest {
                    expected_head: HEAD.into(),
                    action: PullRequestAction::Merge,
                    merge_method: Some(MergeMethod::Squash),
                    stack: None
                }
            )
            .is_err()
    );
    assert!(writes.borrow().is_empty());
}

#[rstest]
fn host_rejection_is_reported_instead_of_acknowledging_success() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        writes: writes.clone(),
        scope_match: true,
        reject_write: true,
        ..Host::default()
    });
    let github = git.github("/repo").unwrap();
    assert!(github.review(12, &review("new")).is_err());
    assert!(
        github
            .metadata(
                12,
                &MetadataRequest::React {
                    id: Some("comment".into()),
                    content: Reaction::Heart,
                    reacted: true
                }
            )
            .is_err()
    );
    assert_eq!(writes.borrow().len(), 2);
}

#[rstest]
#[case(false, true)]
#[case(true, false)]
fn workflow_approval_refuses_an_ambiguous_fork_before_any_write(
    #[case] ambiguous: bool,
    #[case] accepted: bool,
) {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        ambiguous_fork: ambiguous,
        writes: writes.clone(),
        ..Host::default()
    });
    let result = git.github("/repo").unwrap().action(
        12,
        &PullRequestActionRequest {
            expected_head: HEAD.into(),
            action: PullRequestAction::ApproveWorkflows,
            merge_method: None,
            stack: None,
        },
    );
    assert_eq!(result.is_ok(), accepted);
    assert_eq!(writes.borrow().len(), usize::from(accepted));
    if accepted {
        assert_eq!(result.unwrap()["approved"], json!([7]));
    }
}

#[rstest]
#[case(0)]
#[case(1)]
#[case(2)]
#[case(3)]
fn stack_actions_require_the_observed_membership_and_revisions(#[case] fault: u8) {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        writes: writes.clone(),
        ..Host::default()
    });
    let github = git.github("/repo").unwrap();
    let stack = github.stack(12).unwrap().unwrap();
    assert_eq!(stack.layers.len(), 1);
    assert_eq!(stack.layers[0].head, HEAD);
    let head = bootty_git::github::StackHead {
        number: 12,
        head: if fault == 1 { NEXT } else { HEAD }.into(),
    };
    let request = PullRequestActionRequest {
        expected_head: HEAD.into(),
        action: PullRequestAction::Merge,
        merge_method: None,
        stack: Some(bootty_git::github::StackActionContext {
            number: if fault == 0 { 8 } else { 7 },
            heads: if fault == 2 {
                vec![head.clone(), head]
            } else {
                vec![head]
            },
        }),
    };
    assert_eq!(github.action(12, &request).is_ok(), fault == 3);
    assert_eq!(writes.borrow().len(), usize::from(fault == 3));
}

#[rstest]
#[case(json!({"status":"merged"}), true)]
#[case(json!({"status":"enqueued"}), true)]
#[case(json!({"status":"failed"}), true)]
#[case(json!({"status":"pending","details":{"uuid":"operation-1"}}), true)]
#[case(json!({"status":"pending","details":{"uuid":"operation-2"}}), false)]
#[case(json!({"status":"merged","details":{"uuid":"operation-2"}}), false)]
#[case(json!({"status":"pending","details":{}}), false)]
#[case(json!({"status":"unknown"}), false)]
fn acknowledged_stack_merge_is_observed_without_another_write(
    #[case] response: Value,
    #[case] accepted: bool,
) {
    use bootty_git::github::{StackActionContext, StackHead, StackMergeRequest, StackMergeStatus};
    let writes = Rc::new(RefCell::new(Vec::new()));
    let polls = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        writes: writes.clone(),
        polls: polls.clone(),
        merge_response: Some(json!({"status":"pending","details":{"uuid":"operation-1"}})),
        merge_poll: Some(response),
        ..Host::default()
    });
    let github = git.github("/repo").unwrap();
    let result = github
        .action(
            12,
            &PullRequestActionRequest {
                expected_head: HEAD.into(),
                action: PullRequestAction::Merge,
                merge_method: None,
                stack: Some(StackActionContext {
                    number: 7,
                    heads: vec![StackHead {
                        number: 12,
                        head: HEAD.into(),
                    }],
                }),
            },
        )
        .unwrap();
    let StackMergeStatus::Pending { details } = StackMergeStatus::from_response(result).unwrap()
    else {
        panic!("Pending merge lost its operation");
    };
    let mut request = StackMergeRequest {
        repository: github.repository().clone(),
        number: 12,
        operation: details.uuid,
    };
    assert_eq!(github.stack_merge_status(&request).is_ok(), accepted);
    assert_eq!(writes.borrow().len(), 1);
    assert_eq!(
        *polls.borrow(),
        vec!["repos/owner/repo/pulls/12/merge-async/operation-1"]
    );
    // A changed repository and an endpoint-shaped ID fail before contacting GitHub.
    request.repository.owner = "other".into();
    assert!(github.stack_merge_status(&request).is_err());
    request.repository = github.repository().clone();
    request.operation = "../operation-1".into();
    assert!(github.stack_merge_status(&request).is_err());
    assert_eq!(polls.borrow().len(), 1);
    assert_eq!(writes.borrow().len(), 1);
}
