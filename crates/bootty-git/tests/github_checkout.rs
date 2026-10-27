#[path = "support/repository.rs"]
mod repository_support;
#[path = "support/seed.rs"]
mod seed;

#[path = "support/process.rs"]
mod runner;

use anyhow::Context as _;
use assert_fs::{TempDir, prelude::*};
use bootty_git::{
    Git,
    runner::{CommandOutput, CommandRunner},
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::json;
use std::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    None,
    Dirty,
    IgnoredCollision,
    FetchMoved,
    LocalMoved,
    RepositoryMoved,
}

struct Host<'a> {
    root: &'a str,
    head: &'a str,
    base: &'a str,
    change: Cell<Change>,
}

impl CommandRunner for Host<'_> {
    fn run_in(&self, cwd: &str, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        assert_eq!(
            std::fs::canonicalize(cwd)?,
            std::fs::canonicalize(self.root)?
        );
        assert_eq!(program, "gh");
        assert_eq!(args, ["repo", "view", "--json", "url"]);
        Ok(CommandOutput { success: true, stdout: json!({"url":if self.change.get() == Change::RepositoryMoved { "https://github.com/owner/other" } else { "https://github.com/owner/repo" }}).to_string(), stderr: String::new() })
    }

    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        if program == "gh" {
            assert_eq!(
                args,
                [
                    "api",
                    "--hostname",
                    "github.com",
                    "--method",
                    "GET",
                    "repos/owner/repo/pulls/12"
                ]
            );
            return Ok(CommandOutput { success: true, stdout: json!({"number":12,"node_id":"PR_id","title":"Title","body":"Description","html_url":"https://github.com/owner/repo/pull/12","state":"open","draft":false,"head":{"sha":self.head,"ref":"topic"},"base":{"sha":self.base,"ref":"main"},"user":{"login":"author","avatar_url":null},"merged":false}).to_string(), stderr: String::new() });
        }
        assert_eq!(program, "git");
        let mut args = args.to_vec();
        if args.get(3).is_some_and(|arg| arg == "fetch") {
            assert_eq!(
                args.get(3..).context("Git fetch arguments")?,
                [
                    "fetch",
                    "--no-tags",
                    "--",
                    "https://github.com/owner/repo.git",
                    "refs/pull/12/head"
                ]
            );
            // Only the network dependency is replaced; real Git fetch/switch protects the files.
            *args.get_mut(6).context("Git fetch remote")? = self.root.into();
        }
        runner::RepositoryRunner.run(program, &args)
    }
}

fn git(root: &str, args: &[&str]) -> anyhow::Result<String> {
    let args = ["-C", root]
        .into_iter()
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let output = runner::RepositoryRunner.run("git", &args)?;
    anyhow::ensure!(output.success, "{}", output.stderr);
    Ok(output.stdout.trim().into())
}

#[rstest]
#[case(Change::None)]
#[case(Change::Dirty)]
#[case(Change::IgnoredCollision)]
#[case(Change::FetchMoved)]
#[case(Change::LocalMoved)]
#[case(Change::RepositoryMoved)]
fn checkout_preserves_files_and_branches_across_changed_state(
    #[case] change: Change,
) -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let root = directory.path().to_str().context("UTF-8 checkout")?;
    seed::repository(directory.path(), "pull-request", false)?;
    let base = std::fs::read_to_string(directory.path().join(".git/HEAD"))?
        .trim()
        .to_owned();
    let head = std::fs::read_to_string(directory.path().join(".git/refs/pull/12/head"))?
        .trim()
        .to_owned();
    let original_branch = head.clone();
    let host = Host {
        root,
        head: &head,
        base: &base,
        change: Cell::new(Change::None),
    };
    let request = Git::with_runner(&host)
        .pull_request_checkout_context(root, 12, &head)
        .map_err(anyhow::Error::msg)?;
    match change {
        Change::Dirty => directory.child("source.txt").write_str("user changes\n")?,
        Change::IgnoredCollision => directory
            .child("ignored.txt")
            .write_str("user ignored file\n")?,
        Change::FetchMoved => {
            git(root, &["update-ref", "refs/pull/12/head", &base])?;
        }
        Change::LocalMoved => {
            git(root, &["switch", "--detach", &head])?;
        }
        _ => {}
    }
    host.change.set(change);
    let outcome = Git::with_runner(&host).checkout_pull_request(root, &request);
    assert_eq!(outcome.is_ok(), change == Change::None, "{outcome:?}");
    assert_eq!(
        git(root, &["rev-parse", "refs/heads/main"])?,
        original_branch
    );
    if change == Change::IgnoredCollision {
        directory.child("ignored.txt").assert("user ignored file\n");
    }
    if change == Change::Dirty {
        directory.child("source.txt").assert("user changes\n");
    }
    let expected = if matches!(change, Change::None | Change::LocalMoved) {
        &head
    } else {
        &base
    };
    assert_eq!(git(root, &["rev-parse", "HEAD"])?, *expected);
    Ok(())
}

impl CommandRunner for &Host<'_> {
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        Host::run(self, program, args)
    }
    fn run_in(&self, cwd: &str, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        Host::run_in(self, cwd, program, args)
    }
}
