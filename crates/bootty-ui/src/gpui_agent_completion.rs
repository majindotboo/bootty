//! Conversation completions capture the exact provider and binding.
use super::*;
use crate::{
    composer_commands::ComposerCommand,
    gpui::CommandAction,
    gpui_composer_completion::{CompletionScope, CompletionSource, ComposerCompletion},
};
use gpui_kit::component::WindowExt as _;

impl NativeAgentSessionView {
    /// Chips for the apps a sent prompt mentioned.
    pub(super) fn render_sent_applications(
        item: &bootty_agents::NativeTranscriptItem,
        cx: &App,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .text_xs()
            .children(item.applications.iter().map(|application| {
                crate::gpui_prompt_attachments::application_chip(
                    &application.bundle_id,
                    &application.name,
                    false,
                    cx,
                )
            }))
    }

    pub(crate) fn configure_speech_account(
        &self,
        codex: &bootty_config::config::AgentProviderConfig,
        cx: &mut Context<Self>,
    ) {
        let config = &self.record.config;
        self.speech.update(cx, |speech, _| {
            if matches!(
                config.provider,
                bootty_agents::AgentKind::Codex | bootty_agents::AgentKind::Pi
            ) {
                speech.set_captured_account(
                    &config.provider.to_string(),
                    config.program.clone(),
                    config.account_directory.clone(),
                );
            } else {
                speech.set_account(codex, "codex");
            }
        });
    }
    pub(crate) fn set_speech_visible(&self, visible: bool, cx: &mut Context<Self>) {
        if !visible {
            self.speech
                .update(cx, super::super::gpui_speech::SpeechInput::stop);
        }
    }
    /// Bind composer completion to the host-issued Binding that resolves files and catalogs.
    pub fn set_completion_binding(
        &mut self,
        binding: Option<CommandTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(binding) = binding else {
            self.completion = None;
            self.completion_subscriptions.clear();
            return;
        };
        let mut catalog = CommandInvocation::new(
            "agents.native.completions",
            vec![self.record.id.clone(), self.record.generation.to_string()],
            Caller::Internal,
        );
        catalog.target = Some(self.record.target());
        let mut files = CommandInvocation::new(
            "files.complete",
            vec![self.record.config.cwd.to_string_lossy().into_owned()],
            Caller::Internal,
        );
        files.target = Some(binding);
        let scope = CompletionScope {
            catalog,
            files,
            applications: self.record.snapshot.application_mentions_supported,
            remote: self
                .record
                .config
                .remote
                .as_ref()
                .map(|remote| remote.host.clone()),
            commands: true,
        };
        if self
            .completion
            .as_ref()
            .is_some_and(|completion| completion.read(cx).scope() == &scope)
        {
            return;
        }
        let editor = self.composer.clone();
        let sender = self.sender.clone();
        // A resume replaces the completion; chips already in the draft keep their app or plugin.
        let mentions = self
            .completion
            .as_ref()
            .and_then(|completion| completion.read(cx).provider_mentions(cx).ok())
            .unwrap_or_default();
        let completion = cx.new(|cx| {
            let mut completion = ComposerCompletion::new(scope, sender, editor, window, cx);
            completion.restore_provider_mentions(&mentions);
            completion
        });
        // Restored prompts show app chips; the completion registered the icon sender above.
        crate::gpui_application_icons::load(
            self.record
                .snapshot
                .transcript
                .iter()
                .flat_map(|item| &item.applications)
                .map(|application| application.bundle_id.clone()),
            cx,
        );
        self.completion_subscriptions.clear();
        self.completion_subscriptions
            .push(cx.observe(&completion, |_, _, cx| cx.notify()));
        self.completion_subscriptions.push(cx.subscribe_in(
            &completion,
            window,
            |this,
             _,
             selection: &crate::gpui_composer_completion::CompletionSelection,
             window,
             cx| {
                this.completion_selected(selection, window, cx);
            },
        ));
        self.completion_subscriptions.push(cx.subscribe_in(
            &completion,
            window,
            |this, _, action: &crate::gpui_composer_completion::ComposerAddAction, window, cx| {
                this.composer_add_action(action, window, cx);
            },
        ));
        self.completion = Some(completion);
        cx.notify();
    }

    fn completion_selected(
        &mut self,
        selection: &crate::gpui_composer_completion::CompletionSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &selection.source {
            CompletionSource::File(_) => {
                self.stage_project_attachment(selection.clone(), window, cx);
            }
            CompletionSource::Application(target) => {
                if self.applications.len() >= 8 {
                    self.error = Some("Mention at most eight application windows".into());
                    cx.notify();
                    return;
                }
                let token = crate::gpui_prompt_attachments::application_token(target);
                let id = token.id().to_string();
                let mention = bootty_agents::NativeApplicationMention {
                    id: id.clone(),
                    target: target.clone(),
                    prompt_range: selection.range.clone(),
                };
                self.applications.retain(|existing| existing.id != id);
                self.applications.push(mention);
                self.composer.update(cx, |input, cx| {
                    input.set_selected_range(selection.range.clone(), cx);
                    if let Err(error) = input.replace_with_token(token, window, cx) {
                        self.error = Some(error.to_string());
                    }
                    input.focus_handle(cx).focus(window, cx);
                });
                cx.notify();
            }
            CompletionSource::Provider(_)
            | CompletionSource::Directory(_)
            | CompletionSource::Composer(_) => {}
        }
    }

    fn composer_add_action(
        &mut self,
        action: &crate::gpui_composer_completion::ComposerAddAction,
        window: &Window,
        cx: &Context<Self>,
    ) {
        match action {
            crate::gpui_composer_completion::ComposerAddAction::Plan(plan) => {
                if let Some(model) = self.models.iter().flatten().find(|model| {
                    self.record
                        .config
                        .model
                        .as_ref()
                        .map_or(model.is_default, |selected| selected == &model.id)
                }) {
                    self.select_model(
                        &bootty_agents::NativeModelSelection {
                            plan_mode: Some(*plan),
                            fast_mode: None,
                            model: model.id.clone(),
                            reasoning_effort: self
                                .record
                                .config
                                .reasoning_effort
                                .clone()
                                .or_else(|| model.default_reasoning_effort.clone()),
                        },
                        window,
                        cx,
                    );
                }
            }
            crate::gpui_composer_completion::ComposerAddAction::Goal(objective) => {
                self.model_command("goal", vec![objective.clone()], window, cx);
            }
        }
    }

    fn stage_project_attachment(
        &mut self,
        selection: crate::gpui_composer_completion::CompletionSelection,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let CompletionSource::File(source) = selection.source else {
            return;
        };
        if self
            .attachments
            .len()
            .saturating_add(self.attachment_imports)
            >= 16
        {
            self.error = Some("Attach at most 16 files".into());
            cx.notify();
            return;
        }
        let sender = self.sender.clone();
        let target = self.record.target();
        self.attachment_imports = self.attachment_imports.saturating_add(1);
        self.error = None;
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { source.stage(&sender) })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                this.attachment_imports = this.attachment_imports.saturating_sub(1);
                if this.record.target() != target
                    || this.composer.read(cx).value().get(selection.range.clone())
                        != Some(&selection.original)
                {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(staged) => {
                        this.composer.update(cx, |input, cx| {
                            input.set_selected_range(selection.range, cx);
                        });
                        this.import_attachment(staged.path, Some(staged.temporary), window, cx);
                    }
                    Err(error) => {
                        this.error = Some(super::diagnostic(error));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn active_applications(
        &self,
        cx: &App,
    ) -> Vec<bootty_agents::NativeApplicationMention> {
        self.composer
            .read(cx)
            .tokens()
            .iter()
            .filter_map(|span| {
                let mut mention = self
                    .applications
                    .iter()
                    .find(|mention| mention.id == span.token().id().as_ref())?
                    .clone();
                mention.prompt_range = span.range();
                Some(mention)
            })
            .fold(BTreeMap::new(), |mut mentions, mention| {
                mentions.insert(mention.id.clone(), mention);
                mentions
            })
            .into_values()
            .collect()
    }

    /// The Bootty command a plain draft names. Attachments, notes, quotes and mentions make it a
    /// message instead.
    pub(super) fn composer_command(&self, cx: &App) -> Option<(ComposerCommand, String)> {
        if !self.attachments.is_empty()
            || !self.annotations.is_empty()
            || !self.applications.is_empty()
            || !self.active_citations(cx).is_empty()
        {
            return None;
        }
        let draft = self.composer.read(cx).value().to_string();
        ComposerCommand::parse(&draft, |name| {
            self.completion
                .as_ref()
                .is_some_and(|completion| completion.read(cx).provider_command(name))
        })
        .map(|(command, argument)| (command, argument.to_owned()))
    }

    /// Runs a Bootty command through the command its button or menu item already uses.
    pub(super) fn run_composer_command(
        &mut self,
        command: ComposerCommand,
        argument: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app_command = match command {
            ComposerCommand::Rename if argument.is_empty() => {
                self.error = Some("Add the new title after /rename.".into());
                cx.notify();
                return;
            }
            ComposerCommand::Rename => {
                let current = self.record.title.clone();
                self.command("rename", vec![argument, current], window, cx);
                None
            }
            ComposerCommand::Stop => {
                self.command("interrupt", Vec::new(), window, cx);
                None
            }
            ComposerCommand::Model => {
                self.perform_completion(
                    CommandAction::Focus(crate::gpui::ComposerControl::Model),
                    window,
                    cx,
                );
                None
            }
            ComposerCommand::New => Some("agents.native.open"),
            ComposerCommand::Diff => Some("show_changes"),
            ComposerCommand::Settings => Some("open_settings"),
        };
        self.composer
            .update(cx, |composer, cx| composer.set_value("", window, cx));
        if let Some(name) = app_command {
            window.dispatch_action(
                Box::new(crate::gpui_actions::InvokeCommand::new(
                    CommandInvocation::from_action(name, Caller::Internal),
                )),
                cx,
            );
        }
        cx.notify();
    }

    pub(crate) fn completion_active(&self, window: &Window, cx: &App) -> bool {
        self.completion
            .as_ref()
            .is_some_and(|completion| completion.read(cx).active(window, cx))
    }
    /// Runs a composer action, as its keybinding does: completion navigation, focus, dictation.
    pub fn perform_completion(
        &self,
        action: CommandAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if matches!(action, CommandAction::Dictate | CommandAction::Voice) {
            if window.has_active_dialog(cx) {
                return false;
            }
            let mode = if action == CommandAction::Dictate {
                crate::gpui_speech::SpeechMode::Dictation
            } else {
                crate::gpui_speech::SpeechMode::Voice
            };
            if mode == crate::gpui_speech::SpeechMode::Voice
                && !self.speech.read(cx).voice_active()
                && (!self.can_prompt() || !self.composer.read(cx).value().trim().is_empty())
            {
                return false;
            }
            self.speech
                .update(cx, |speech, cx| speech.toggle(mode, window, cx));
            return true;
        }
        if matches!(action, CommandAction::EffortDown | CommandAction::EffortUp) {
            if window.has_active_dialog(cx) {
                return false;
            }
            let Some(picker) = &self.model_picker else {
                return false;
            };
            return picker.update(cx, |picker, cx| {
                picker.change_effort(action == CommandAction::EffortUp, cx)
            });
        }
        if let CommandAction::Focus(control) = action {
            if window.has_active_dialog(cx) {
                return false;
            }
            match control {
                crate::gpui::ComposerControl::Model => {
                    let Some(picker) = &self.model_picker else {
                        return false;
                    };
                    picker.update(cx, |picker, cx| picker.open(window, cx));
                    return true;
                }
                crate::gpui::ComposerControl::Effort => {
                    let Some(picker) = &self.model_picker else {
                        return false;
                    };
                    picker.update(cx, |picker, cx| picker.open_reasoning(window, cx));
                    return true;
                }
                crate::gpui::ComposerControl::Permissions => {
                    if self.record.config.provider == bootty_agents::AgentKind::Pi {
                        return false;
                    }
                    return crate::gpui::activate_control_child(
                        &self.permissions_focus,
                        window,
                        cx,
                    );
                }
                _ => return false,
            }
        }
        self.completion.as_ref().is_some_and(|completion| {
            completion.update(cx, |completion, cx| completion.perform(action, window, cx))
        })
    }
}
