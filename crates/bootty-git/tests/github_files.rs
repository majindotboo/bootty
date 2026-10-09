use anyhow::Context as _;
use bootty_git::{
    Git,
    diff::{DiffAnchor, DiffSide},
    github::PullRequestDiffRequest,
    runner::{CommandOutput, CommandRunner},
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::json;
use std::cell::Cell;

const HEAD: &str = "1111111111111111111111111111111111111111";
const BASE: &str = "2222222222222222222222222222222222222222";

struct Host {
    status: &'static str,
    contents: &'static str,
    moved: bool,
    reads: Cell<usize>,
}
impl CommandRunner for Host {
    fn run_in(&self, cwd: &str, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        assert_eq!(cwd, "/repo");
        assert_eq!(program, "gh");
        assert_eq!(args, ["repo", "view", "--json", "url"]);
        Ok(CommandOutput {
            success: true,
            stdout: json!({"url":"https://github.enterprise.test/owner/repo"}).to_string(),
            stderr: String::new(),
        })
    }

    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        let output = if program == "git" {
            "git@github.enterprise.test:owner/repo.git".into()
        } else {
            assert_eq!(program, "gh");
            assert_eq!(
                args.get(2).context("Missing host")?,
                "github.enterprise.test"
            );
            let endpoint = args.last().context("Missing endpoint")?;
            if endpoint.contains("/files?") {
                json!([{"filename":"new file.rs","previous_filename":"old file.rs","status":self.status,"additions":1,"deletions":1,"patch":null}]).to_string()
            } else if endpoint.contains("/contents/") {
                anyhow::ensure!(
                    endpoint.contains("/contents/new%20file.rs?")
                        || endpoint.contains("/contents/old%20file.rs?")
                );
                if endpoint.ends_with(BASE) {
                    "unchanged\nold\nafter\n".into()
                } else {
                    anyhow::ensure!(endpoint.ends_with(HEAD));
                    self.contents.into()
                }
            } else {
                let reads = self.reads.get();
                self.reads.set(reads.saturating_add(1));
                json!({"number":12,"node_id":"PR_id","title":"Title","body":"Description","html_url":"https://github.enterprise.test/owner/repo/pull/12","state":"open","draft":false,"head":{"sha":if self.moved && reads > 0 { BASE } else { HEAD },"ref":"branch"},"base":{"sha":BASE,"ref":"main"},"user":{"login":"author","avatar_url":null},"merged":false}).to_string()
            }
        };
        Ok(CommandOutput {
            success: true,
            stdout: output,
            stderr: String::new(),
        })
    }
}

fn request() -> PullRequestDiffRequest {
    PullRequestDiffRequest {
        head: HEAD.into(),
        base: BASE.into(),
        path: "new file.rs".into(),
        context_lines: 100_000,
    }
}

#[rstest]
#[case("modified", DiffSide::Right, "new")]
#[case("renamed", DiffSide::Right, "new")]
#[case("added", DiffSide::Right, "new")]
#[case("removed", DiffSide::Left, "old")]
fn missing_patches_expand_from_the_exact_source_revision(
    #[case] status: &'static str,
    #[case] side: DiffSide,
    #[case] expected: &str,
) {
    let git = Git::with_runner(Host {
        status,
        contents: "unchanged\nnew\nafter\n",
        moved: false,
        reads: Cell::new(0),
    });
    let diff = git
        .github("/repo")
        .unwrap()
        .file_diff(12, &request())
        .unwrap();
    assert_eq!(
        diff.quote(&DiffAnchor {
            path: "new file.rs".into(),
            side,
            start_line: 2,
            line: 2
        })
        .unwrap(),
        expected
    );
    assert_eq!(diff.previous_path.as_deref(), Some("old file.rs"));
}

#[rstest]
fn changing_a_revision_during_the_read_discards_the_comparison() {
    let git = Git::with_runner(Host {
        status: "modified",
        contents: "unchanged\nnew\nafter\n",
        moved: true,
        reads: Cell::new(0),
    });
    assert!(
        git.github("/repo")
            .unwrap()
            .file_diff(12, &request())
            .unwrap_err()
            .contains("changed")
    );
}

#[rstest]
fn binary_contents_are_not_rendered_as_text() {
    let git = Git::with_runner(Host {
        status: "modified",
        contents: "binary\0data",
        moved: false,
        reads: Cell::new(0),
    });
    assert!(
        git.github("/repo")
            .unwrap()
            .file_diff(12, &request())
            .unwrap_err()
            .contains("Binary")
    );
}

#[rstest]
fn full_file_context_shows_a_rename_without_content_changes() {
    let git = Git::with_runner(Host {
        status: "renamed",
        contents: "unchanged\nold\nafter\n",
        moved: false,
        reads: Cell::new(0),
    });
    let diff = git
        .github("/repo")
        .unwrap()
        .file_diff(12, &request())
        .unwrap();
    assert_eq!(
        diff.quote(&DiffAnchor {
            path: "new file.rs".into(),
            side: DiffSide::Right,
            start_line: 1,
            line: 3
        })
        .unwrap(),
        "unchanged\nold\nafter"
    );
}
