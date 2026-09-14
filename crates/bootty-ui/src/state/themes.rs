use std::{path::Path, sync::mpsc, time::Instant};

use bootty_config::config::{
    AppearanceVariant, parse_theme_source,
    theme_file::{ThemeFile, import_theme, read_theme, save_theme},
};
use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_mux::executor;

use crate::commands::runtime::{CommandDispatch, PendingCommandResult};
use crate::{AppState, commands::ThemeAction, state::AppEffect};

fn failure(message: impl Into<String>) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "theme_failed".to_owned(),
        message: message.into(),
    }
}

impl AppState {
    pub(crate) fn dispatch_theme_command(
        &mut self,
        action: ThemeAction,
        args: &[String],
        execution: Option<(Instant, CommandCancellation)>,
        effects: &mut Vec<AppEffect>,
    ) -> CommandDispatch {
        if matches!(
            action,
            ThemeAction::Restore | ThemeAction::Preview | ThemeAction::Apply
        ) && let Err(error) = executor::begin_synchronous_command(execution.clone())
        {
            return CommandDispatch::Complete(
                crate::commands::runtime::command_outcome_for_mux_error(error),
            );
        }
        let config_dir = self
            .config()
            .config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        match (action, args) {
            (ThemeAction::Read, [name]) => {
                let name = name.clone();
                self.dispatch_theme_worker(execution, move || read_theme(&config_dir, &name))
            }
            (ThemeAction::Import, [path]) => {
                let path = path.clone();
                self.dispatch_theme_worker(execution, move || import_theme(Path::new(&path)))
            }
            (ThemeAction::Save, [name, source] | [name, source, _]) => {
                let (name, source, revision) = (name.clone(), source.clone(), args.get(2).cloned());
                self.dispatch_theme_worker(execution, move || {
                    save_theme(&config_dir, &name, &source, revision.as_deref())
                })
            }
            (ThemeAction::Restore, []) => {
                self.restore_theme_picker_preview();
                self.theme_picker_restore_config = None;
                effects.push(AppEffect::RequestRepaint);
                CommandDispatch::Complete(CommandOutcome::success())
            }
            (ThemeAction::Preview, [source, appearance]) => {
                CommandDispatch::Complete(self.preview_authored_theme(source, appearance, effects))
            }
            (ThemeAction::Apply, [name, appearance]) => {
                CommandDispatch::Complete(self.apply_authored_theme(name, appearance, effects))
            }
            _ => CommandDispatch::Complete(failure("Invalid theme command arguments")),
        }
    }

    fn dispatch_theme_worker(
        &self,
        execution: Option<(Instant, CommandCancellation)>,
        work: impl FnOnce() -> Result<ThemeFile, String> + Send + 'static,
    ) -> CommandDispatch {
        let (deadline, cancellation) = executor::command_execution(execution);
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let outcome = if let Err(error) =
                executor::begin_synchronous_command(Some((deadline, cancellation)))
            {
                crate::commands::runtime::command_outcome_for_mux_error(error)
            } else {
                match work()
                    .and_then(|file| serde_json::to_value(file).map_err(|error| error.to_string()))
                {
                    Ok(value) => CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    },
                    Err(error) => failure(error),
                }
            };
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(result))
    }

    fn preview_authored_theme(
        &mut self,
        source: &str,
        appearance: &str,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        if source.len() > 64 * 1024 {
            return failure("Theme source exceeds 64 KiB");
        }
        match parse_theme_source(source, "draft") {
            Err(error) => failure(error.to_string()),
            Ok(theme) => {
                if self.theme_picker_restore_config.is_none() {
                    self.theme_picker_restore_config = Some(self.config().clone());
                }
                let mut config = self.config().clone();
                let branch = if appearance == "light" {
                    &mut config.appearance.light
                } else {
                    &mut config.appearance.dark
                };
                branch.theme = Some(theme.info.name);
                branch.colors = theme.colors;
                self.config_runtime.replace_preview_config(config);
                self.publish_live_terminal_config(self.active_appearance_variant);
                effects.push(AppEffect::RequestRepaint);
                CommandOutcome::success()
            }
        }
    }

    fn apply_authored_theme(
        &mut self,
        name: &str,
        appearance: &str,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        let mut document = self.config_runtime.document().clone();
        let variant = if appearance == "light" {
            AppearanceVariant::Light
        } else {
            AppearanceVariant::Dark
        };
        let branch = match variant {
            AppearanceVariant::Light => "light",
            AppearanceVariant::Dark => "dark",
        };
        // The authored file already contains the edited colors; stale overrides must not mask it.
        let edited = document
            .remove(&["appearance", branch, "colors"])
            .and_then(|()| document.set_str(&["appearance", branch, "theme"], name));
        match edited.map_err(|error| error.to_string()).and_then(|()| {
            self.commit_settings_document(document)
                .map_err(|error| error.to_string())
        }) {
            Ok((_, _, accepted_effects)) => {
                self.theme_picker_restore_config = None;
                effects.extend(accepted_effects);
                CommandOutcome::success()
            }
            Err(error) => failure(error),
        }
    }
}
