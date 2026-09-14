//! Optional launch hooks. User rc files and custom command arguments remain shell-owned.
use anyhow::Result;
use std::path::Path;

pub struct ShellIntegration {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    directory: tempfile::TempDir,
}

impl ShellIntegration {
    #[must_use]
    pub fn script(shell: &str) -> Option<&'static str> {
        match shell {
            "bash" => Some(include_str!("shell_integration/bash.sh")),
            "zsh" => Some(include_str!("shell_integration/zsh.sh")),
            "fish" => Some(include_str!("shell_integration/fish.fish")),
            _ => None,
        }
    }
    /// Unsupported shells and explicit command arguments retain their original launch behavior.
    ///
    /// # Errors
    /// Returns an I/O error if the temporary shell hooks cannot be created.
    pub fn prepare(
        program: &str,
        args: &[String],
        original_zdotdir: Option<&str>,
    ) -> Result<Option<Self>> {
        if !args.is_empty() {
            return Ok(None);
        }
        let name = Path::new(program)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if !matches!(name, "bash" | "zsh" | "fish") {
            return Ok(None);
        }
        let directory = tempfile::Builder::new().prefix("bootty-shell-").tempdir()?;
        let mut integration = Self {
            args: Vec::new(),
            env: Vec::new(),
            directory,
        };
        let path = integration.directory.path();
        match name {
            "bash" => {
                let file = path.join("bashrc");
                std::fs::write(
                    &file,
                    format!(
                        "[[ -r $HOME/.bashrc ]] && source \"$HOME/.bashrc\"\n{}",
                        include_str!("shell_integration/bash.sh")
                    ),
                )?;
                integration.args = vec![
                    "--rcfile".to_owned(),
                    file.to_string_lossy().into_owned(),
                    "-i".to_owned(),
                ];
            }
            "fish" => {
                let file = path.join("init.fish");
                std::fs::write(&file, include_str!("shell_integration/fish.fish"))?;
                integration.args = vec![
                    "--interactive".to_owned(),
                    "--init-command".to_owned(),
                    format!("source '{}'", file.to_string_lossy().replace('\'', "\\'")),
                ];
            }
            "zsh" => {
                std::fs::write(
                    path.join(".zshenv"),
                    r#"typeset -g __bootty_injected_zdotdir=$ZDOTDIR
if [[ -n ${BOOTTY_ORIGINAL_ZDOTDIR+x} ]]; then
    ZDOTDIR=$BOOTTY_ORIGINAL_ZDOTDIR
else
    unset ZDOTDIR
fi
[[ -r ${ZDOTDIR:-$HOME}/.zshenv ]] && source "${ZDOTDIR:-$HOME}/.zshenv"
typeset -g __bootty_user_zdotdir=${ZDOTDIR-}
typeset -g __bootty_zdotdir_was_set=${ZDOTDIR+x}
[[ -o rcs ]] && ZDOTDIR=$__bootty_injected_zdotdir
unset BOOTTY_ORIGINAL_ZDOTDIR
"#,
                )?;
                std::fs::write(
                    path.join(".zshrc"),
                    format!(
                        "if [[ -n $__bootty_zdotdir_was_set ]]; then ZDOTDIR=$__bootty_user_zdotdir; else unset ZDOTDIR; fi\n[[ -r ${{ZDOTDIR:-$HOME}}/.zshrc ]] && source \"${{ZDOTDIR:-$HOME}}/.zshrc\"\n{}",
                        include_str!("shell_integration/zsh.sh")
                    ),
                )?;
                integration
                    .env
                    .push(("ZDOTDIR".to_owned(), path.to_string_lossy().into_owned()));
                if let Some(original) = original_zdotdir {
                    integration
                        .env
                        .push(("BOOTTY_ORIGINAL_ZDOTDIR".to_owned(), original.to_owned()));
                }
            }
            _ => return Ok(None),
        }
        Ok(Some(integration))
    }
}
