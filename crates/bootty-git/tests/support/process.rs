//! Real Git behavior without the host runner's cancellation polling interval.
use bootty_git::{CommandOutput, CommandRunner};
use std::{
    io::Write as _,
    process::{Command, Output, Stdio},
};

pub struct RepositoryRunner;

fn output(output: Output) -> anyhow::Result<CommandOutput> {
    Ok(CommandOutput {
        success: output.status.success(),
        stdout: String::from_utf8(output.stdout)?,
        stderr: String::from_utf8(output.stderr)?,
    })
}

impl CommandRunner for RepositoryRunner {
    fn run_with_env(
        &self,
        program: &str,
        args: &[String],
        env: &[(&str, &str)],
    ) -> anyhow::Result<CommandOutput> {
        output(
            Command::new(program)
                .args(args)
                .envs(env.iter().copied())
                .output()?,
        )
    }

    fn remove_file(&self, path: &str) -> anyhow::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        output(Command::new(program).args(args).output()?)
    }

    fn run_with_input(
        &self,
        program: &str,
        args: &[String],
        input: Vec<u8>,
    ) -> anyhow::Result<CommandOutput> {
        use anyhow::Context as _;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().context("Git input pipe")?;
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let captured = child.wait_with_output()?;
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("Git input writer stopped"))??;
        output(captured)
    }

    fn run_shell_in(&self, cwd: &str, command: &str) -> anyhow::Result<CommandOutput> {
        // Teardown tests exercise the command, never the developer's login profile or tools.
        #[cfg(unix)]
        let mut shell = {
            let mut shell = Command::new("/bin/sh");
            shell.args(["-c", command]).env("PATH", "/usr/bin:/bin");
            shell
        };
        #[cfg(windows)]
        let mut shell = {
            let system = std::path::PathBuf::from(
                std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()),
            )
            .join("System32");
            let mut shell = Command::new(system.join("cmd.exe"));
            shell.args(["/D", "/S", "/C", command]).env("PATH", system);
            shell
        };
        output(
            shell
                .current_dir(cwd)
                .env("HOME", cwd)
                .env("USERPROFILE", cwd)
                .env("ENV", "")
                .env_remove("BASH_ENV")
                .output()?,
        )
    }
}
