use anyhow::Context as _;
use bootty_git::{
    Git,
    github::{GitHubRepository, PullRequestCreationContext, PullRequestCreationRequest},
    runner::{CommandOutput, CommandRunner},
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc};

const HEAD: &str = "1111111111111111111111111111111111111111";
const OTHER: &str = "2222222222222222222222222222222222222222";

#[derive(Clone, Copy)]
enum State {
    Current,
    LocalMoved,
    RemoteMoved,
    OriginChanged,
    Fork,
}

struct Host {
    state: State,
    writes: Rc<RefCell<Vec<Value>>>,
}
impl Host {
    const fn output(stdout: String) -> CommandOutput {
        CommandOutput {
            success: true,
            stdout,
            stderr: String::new(),
        }
    }
    fn pull() -> Value {
        json!({"number":12,"node_id":"PR_id","title":"Title","body":"Description","html_url":"https://github.enterprise.test/owner/repo/pull/12","state":"open","draft":true,"head":{"sha":HEAD,"ref":"luan/topic"},"base":{"sha":OTHER,"ref":"main"},"user":{"login":"author","avatar_url":null},"merged":false})
    }
}
impl CommandRunner for Host {
    fn run_in(&self, cwd: &str, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        assert_eq!(cwd, "/repo");
        assert_eq!(program, "gh");
        assert_eq!(args, ["repo", "view", "--json", "url"]);
        Ok(Self::output(
            json!({"url":"https://github.enterprise.test/owner/repo"}).to_string(),
        ))
    }
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        if program == "git" {
            assert_eq!(
                args.get(..3).context("Missing Git prefix")?,
                ["--literal-pathspecs", "-C", "/repo"]
            );
            let value = match args.get(3).context("Missing Git operation")?.as_str() {
                "remote" => {
                    if matches!(self.state, State::OriginChanged) {
                        "git@github.enterprise.test:owner/other.git"
                    } else if matches!(self.state, State::Fork) {
                        "git@github.enterprise.test:fork/repo.git"
                    } else {
                        "git@github.enterprise.test:owner/repo.git"
                    }
                }
                "symbolic-ref" => "luan/topic",
                "rev-parse" => {
                    if matches!(self.state, State::LocalMoved) {
                        OTHER
                    } else {
                        HEAD
                    }
                }
                "push" => {
                    assert_eq!(
                        args.get(3..).context("Missing Git push")?,
                        [
                            "push",
                            "--porcelain",
                            "origin",
                            &format!("{HEAD}:refs/heads/luan/topic")
                        ]
                    );
                    self.writes
                        .borrow_mut()
                        .push(json!({"push":args.get(6).context("Missing refspec")?}));
                    "published"
                }
                other => anyhow::bail!("Unexpected git operation {other}"),
            };
            return Ok(Self::output(value.into()));
        }
        assert_eq!(program, "gh");
        let owner = if matches!(self.state, State::Fork) {
            "fork"
        } else {
            "owner"
        };
        assert_eq!(
            args,
            [
                "api",
                "--hostname",
                "github.enterprise.test",
                "--method",
                "GET",
                &format!("repos/{owner}/repo/git/ref/heads/luan%2Ftopic")
            ]
        );
        Ok(Self::output(
            json!({"object":{"sha":if matches!(self.state,State::RemoteMoved){OTHER}else{HEAD}}})
                .to_string(),
        ))
    }
    fn run_with_input(
        &self,
        program: &str,
        args: &[String],
        input: Vec<u8>,
    ) -> anyhow::Result<CommandOutput> {
        assert_eq!(program, "gh");
        assert_eq!(
            args,
            [
                "api",
                "--hostname",
                "github.enterprise.test",
                "--method",
                "POST",
                "repos/owner/repo/pulls",
                "--input",
                "-"
            ]
        );
        self.writes
            .borrow_mut()
            .push(serde_json::from_slice(&input)?);
        Ok(Self::output(Self::pull().to_string()))
    }
}

fn context() -> PullRequestCreationContext {
    PullRequestCreationContext {
        repository: GitHubRepository {
            host: "github.enterprise.test".into(),
            owner: "owner".into(),
            name: "repo".into(),
        },
        head_repository: None,
        branch: "luan/topic".into(),
        head: HEAD.into(),
        base: "main".into(),
    }
}
fn request() -> PullRequestCreationRequest {
    PullRequestCreationRequest {
        context: context(),
        title: "Title".into(),
        body: "Description".into(),
        draft: true,
    }
}

#[rstest]
fn publishing_uses_the_captured_commit_and_never_forces() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    Git::with_runner(Host {
        state: State::Current,
        writes: writes.clone(),
    })
    .publish_pull_request_branch("/repo", &context())
    .unwrap();
    assert_eq!(
        *writes.borrow(),
        vec![json!({"push":format!("{HEAD}:refs/heads/luan/topic")})]
    );
}

#[rstest]
fn creating_preserves_the_selected_base_and_draft_metadata() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let created = Git::with_runner(Host {
        state: State::Current,
        writes: writes.clone(),
    })
    .create_pull_request("/repo", &request())
    .unwrap();
    assert_eq!(created.number, 12);
    assert_eq!(
        *writes.borrow(),
        vec![
            json!({"title":"Title","body":"Description","head":"luan/topic","head_repo":"repo","base":"main","draft":true})
        ]
    );
}

#[rstest]
fn a_fork_publishes_to_origin_and_creates_against_the_selected_base() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let git = Git::with_runner(Host {
        state: State::Fork,
        writes: writes.clone(),
    });
    let mut request = request();
    request.context.head_repository = Some(GitHubRepository {
        owner: "fork".into(),
        ..request.context.repository.clone()
    });
    git.publish_pull_request_branch("/repo", &request.context)
        .unwrap();
    git.create_pull_request("/repo", &request).unwrap();
    assert_eq!(
        *writes.borrow(),
        vec![
            json!({"push":format!("{HEAD}:refs/heads/luan/topic")}),
            json!({"title":"Title","body":"Description","head":"fork:luan/topic","head_repo":"repo","base":"main","draft":true})
        ]
    );
}

#[rstest]
#[case(State::LocalMoved)]
#[case(State::RemoteMoved)]
#[case(State::OriginChanged)]
fn changed_identity_or_revision_cannot_create_a_pull_request(#[case] state: State) {
    let writes = Rc::new(RefCell::new(Vec::new()));
    assert!(
        Git::with_runner(Host {
            state,
            writes: writes.clone()
        })
        .create_pull_request("/repo", &request())
        .is_err()
    );
    assert!(writes.borrow().is_empty());
}
