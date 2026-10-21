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
        // A Codex conversation speaks for its own account; any other uses the selected one.
        self.speech.update(cx, |speech, cx| {
            if config.provider == bootty_agents::AgentKind::Codex {
                speech.set_captured_account(
                    config.program.clone(),
                    config.account_directory.clone(),
                    cx,
                );
            } else {
                speech.set_account(codex, cx);
            }
        });
    }
    /// Take the voice call's offer to this conversation's Codex session, then connect its answer.
    pub(super) fn start_voice_call(
        &self,
        offer: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(epoch) = self.speech.read(cx).voice_epoch() else {
            return;
        };
        let target = self.record.target();
        let mut invocation = CommandInvocation::new(
            "agents.native.voice-start",
            vec![
                self.record.id.clone(),
                self.record.generation.to_string(),
                offer,
            ],
            Caller::Internal,
        );
        invocation.target = Some(target.clone());
        let receiver = match self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                crate::gpui_speech::notify_error(
                    "Could not start voice mode",
                    crate::error_catalog::command_send_message(error),
                    window,
                    cx,
                );
                self.speech
                    .update(cx, super::super::gpui_speech::SpeechInput::stop);
                return;
            }
        };
        cx.spawn_in(window, async move |owner, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            let answer = match &outcome {
                Ok(CommandOutcome::Success { value, .. }) => {
                    value.get("sdp").and_then(Value::as_str).map(str::to_owned)
                }
                _ => None,
            };
            _ = owner.update_in(cx, |this, window, cx| {
                // A stopped call's reply must never connect or stop its replacement.
                if this.record.target() != target
                    || this.speech.read(cx).voice_epoch() != Some(epoch)
                {
                    return;
                }
                if let Some(answer) = answer {
                    this.speech
                        .update(cx, |speech, cx| speech.answer(answer, cx));
                    return;
                }
                let message = outcome
                    .ok()
                    .and_then(|outcome| crate::commands::command_outcome_message(&outcome))
                    .unwrap_or_else(|| "Codex couldn't start voice mode".to_owned());
                crate::gpui_speech::notify_error(
                    "Could not start voice mode",
                    &message,
                    window,
                    cx,
                );
                this.speech
                    .update(cx, super::super::gpui_speech::SpeechInput::stop);
            });
        })
        .detach();
    }

    /// End Codex's side of a voice call that ended here.
    pub(super) fn end_voice_call(&self) {
        let mut invocation = CommandInvocation::new(
            "agents.native.voice-stop",
            vec![self.record.id.clone(), self.record.generation.to_string()],
            Caller::Internal,
        );
        invocation.target = Some(self.record.target());
        _ = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        );
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
        let conversations = self.mentionable_conversations.clone();
        let completion = cx.new(|cx| {
            let mut completion = ComposerCompletion::new(scope, sender, editor, window, cx);
            completion.restore_provider_mentions(&mentions);
            completion.set_conversations(conversations);
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
            CompletionSource::Conversation(conversation) => {
                if self.conversation_mentions.len() >= bootty_agents::MAX_MENTIONED_CONVERSATIONS {
                    self.error = Some("Mention at most four conversations in one message".into());
                    cx.notify();
                    return;
                }
                let token = gpui_kit::component::input::InlineToken::new(
                    format!("conversation:{}", conversation.target.handle),
                    format!("@{}", conversation.name),
                );
                self.conversation_mentions
                    .retain(|mention| mention.target != conversation.target);
                self.conversation_mentions.push(conversation.clone());
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
            | CompletionSource::File(_)
            | CompletionSource::Directory(_)
            | CompletionSource::Composer(_) => {}
        }
    }

    /// Puts the composer's words aside, or brings the newest stashed prompt back into an empty
    /// composer. Attachments stay in the composer.
    fn stash_or_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !cx.has_global::<crate::prompt_stash::PromptStash>() {
            return false;
        }
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            return self.restore_stashed(None, window, cx);
        }
        let now = chrono::Utc::now().timestamp();
        match cx
            .update_global(|stash: &mut crate::prompt_stash::PromptStash, _| stash.stash(text, now))
        {
            Ok(()) => {
                self.composer.update(cx, |composer, cx| {
                    composer.set_value("", window, cx);
                });
                self.draft_replaced(cx);
            }
            Err(error) => self.error = Some(super::diagnostic("Could not stash the prompt", error)),
        }
        cx.notify();
        true
    }

    /// Takes a stashed prompt, the newest when `id` is `None`, back into the composer: in place
    /// of an empty draft, or where the cursor is.
    pub(super) fn restore_stashed(
        &mut self,
        id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let taken =
            cx.update_global(|stash: &mut crate::prompt_stash::PromptStash, _| stash.take(id));
        match taken {
            Ok(Some(entry)) if self.composer.read(cx).value().trim().is_empty() => {
                self.restore_draft(&entry.text, &[], &[], window, cx);
            }
            Ok(Some(entry)) => self.composer.update(cx, |composer, cx| {
                composer.insert(entry.text, window, cx);
            }),
            Ok(None) => return false,
            Err(error) => {
                self.error = Some(super::diagnostic("Could not restore the prompt", error));
            }
        }
        cx.notify();
        true
    }

    /// The stashed prompts, from a badge beside the composer's controls when there are any.
    /// Gives the composer the conversation's room to write in, or shrinks it back. Either way
    /// the words stay where they are and keep focus.
    pub(super) fn set_composer_expanded(
        &mut self,
        expanded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.composer_expanded != expanded {
            self.composer_expanded = expanded;
            self.composer
                .update(cx, |composer, cx| composer.focus(window, cx));
            cx.notify();
        }
    }

    fn toggle_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_composer_expanded(!self.composer_expanded, window, cx);
    }

    pub(super) fn render_shrink(cx: &Context<Self>) -> impl IntoElement + use<> {
        Button::new("native-composer-shrink")
            .debug_selector(|| "native-composer-shrink".into())
            .ghost()
            .xsmall()
            .icon(IconName::Minimize)
            .text_color(cx.theme().muted_foreground)
            .accessibility_label("Shrink the composer")
            .element_tooltip("Shrink the composer")
            .on_click(cx.listener(|this, _, window, cx| this.toggle_composer(window, cx)))
    }

    pub(super) fn render_stash(cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let entries = cx
            .try_global::<crate::prompt_stash::PromptStash>()?
            .entries()
            .to_vec();
        if entries.is_empty() {
            return None;
        }
        let owner = cx.weak_entity();
        Some(
            Button::new("native-stash")
                .debug_selector(|| "native-stash".into())
                .ghost()
                .xsmall()
                .icon(gpui_kit::component::Icon::default().path("icons/bookmark.svg"))
                .label(entries.len().to_string())
                .text_color(cx.theme().muted_foreground)
                .accessibility_label("Stashed prompts")
                .dropdown_menu(move |menu, _, _| {
                    entries.iter().fold(menu, |menu, entry| {
                        let (owner, id) = (owner.clone(), entry.id.clone());
                        let words = entry.text.split_whitespace().collect::<Vec<_>>().join(" ");
                        let snippet = match words.char_indices().nth(60) {
                            Some((end, _)) => format!("{}…", words.get(..end).unwrap_or(&words)),
                            None => words,
                        };
                        menu.item(
                            gpui_kit::component::menu::PopupMenuItem::new(snippet).on_click(
                                move |_, window, cx| {
                                    _ = owner.update(cx, |this, cx| {
                                        this.restore_stashed(Some(&id), window, cx);
                                    });
                                },
                            ),
                        )
                    })
                }),
        )
    }

    /// The conversations the composer still mentions, in the order they were chosen.
    pub(super) fn active_conversation_mentions(&self, cx: &App) -> Vec<CommandTarget> {
        let tokens: Vec<String> = self
            .composer
            .read(cx)
            .tokens()
            .iter()
            .map(|span| span.token().id().to_string())
            .collect();
        self.conversation_mentions
            .iter()
            .filter(|mention| {
                tokens
                    .iter()
                    .any(|id| *id == format!("conversation:{}", mention.target.handle))
            })
            .map(|mention| mention.target.clone())
            .collect()
    }

    /// The other conversations this composer can mention.
    pub fn set_mentionable_conversations(
        &mut self,
        conversations: Vec<crate::gpui_composer_completion::MentionableConversation>,
        cx: &mut Context<Self>,
    ) {
        if self.mentionable_conversations == conversations {
            return;
        }
        self.mentionable_conversations.clone_from(&conversations);
        if let Some(completion) = &self.completion {
            completion.update(cx, |completion, _| {
                completion.set_conversations(conversations);
            });
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
                self.set_plan_mode(*plan, window, cx);
            }
            crate::gpui_composer_completion::ComposerAddAction::Goal(objective) => {
                self.model_command("goal", vec![objective.clone()], window, cx);
            }
        }
    }

    /// Plan mode rides on the model selection, so a toggle made before models load waits for
    /// them instead of doing nothing.
    pub(super) fn set_plan_mode(&mut self, plan: bool, window: &Window, cx: &Context<Self>) {
        let Some(model) = self.models.iter().flatten().find(|model| {
            self.record
                .config
                .model
                .as_ref()
                .map_or(model.is_default, |selected| selected == &model.id)
        }) else {
            self.pending_plan = Some(plan);
            self.load_models(window, cx);
            return;
        };
        self.select_model(
            &bootty_agents::NativeModelSelection {
                plan_mode: Some(plan),
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
            ComposerCommand::Plan => {
                let plan = !self.record.config.plan_mode.unwrap_or(false);
                self.set_plan_mode(plan, window, cx);
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
        self.draft_replaced(cx);
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
        &mut self,
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
        if action == CommandAction::TogglePlan {
            let offered = self
                .completion
                .as_ref()
                .is_some_and(|completion| completion.read(cx).plan_mode());
            if window.has_active_dialog(cx) || !offered || !self.provider_enabled() {
                return false;
            }
            let plan = !self.record.config.plan_mode.unwrap_or(false);
            self.set_plan_mode(plan, window, cx);
            return true;
        }
        if action == CommandAction::ToggleFast {
            if window.has_active_dialog(cx) || !self.provider_enabled() {
                return false;
            }
            return self.toggle_fast_mode(window, cx);
        }
        if matches!(
            action,
            CommandAction::FocusComposer | CommandAction::Stash | CommandAction::Expand
        ) {
            if window.has_active_dialog(cx) || self.subagent.is_some() {
                return false;
            }
            match action {
                CommandAction::Stash => return self.stash_or_restore(window, cx),
                CommandAction::Expand => self.toggle_composer(window, cx),
                _ => self
                    .composer
                    .update(cx, |composer, cx| composer.focus(window, cx)),
            }
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
