//! Creation drafts use the same completion owner as running conversations.
use super::*;
use crate::gpui_composer_completion::{CompletionSource, ComposerCompletion};
impl DialogView {
    pub(crate) fn set_completion_sender(&mut self, sender: bootty_control::BoundAppCommandSender) {
        self.completion_sender = Some(sender);
    }
    pub(super) fn sync_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let scope = self
            .spec
            .as_ref()
            .filter(|spec| !spec.busy)
            .and_then(|spec| spec.completion.clone());
        let Some((scope, sender)) = scope.zip(self.completion_sender.clone()) else {
            self.completion = None;
            self.completion_subscriptions.clear();
            return;
        };
        if self
            .completion
            .as_ref()
            .is_some_and(|completion| completion.read(cx).scope() == &scope)
        {
            return;
        }
        let editor = self.prompt_textarea.clone();
        let mentions = self
            .spec
            .as_ref()
            .map_or_default(|spec| spec.provider_mentions.clone());
        let completion = cx.new(|cx| {
            let mut completion = ComposerCompletion::new(scope, sender, editor, window, cx);
            completion.restore_provider_mentions(&mentions);
            completion
        });
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
             cx| match &selection.source {
                CompletionSource::Application(target) => {
                    let Some(spec) = this.spec.as_mut() else {
                        return;
                    };
                    if spec.applications.len() >= 8 {
                        this.attachment_error =
                            Some("Mention at most eight application windows".into());
                        cx.notify();
                        return;
                    }
                    let token = crate::gpui_prompt_attachments::application_token(target);
                    let id = token.id().to_string();
                    spec.applications.retain(|mention| mention.id != id);
                    spec.applications
                        .push(bootty_agents::NativeApplicationMention {
                            id: id.clone(),
                            target: target.clone(),
                            prompt_range: selection.range.clone(),
                        });
                    let dialog = spec.id.clone();
                    let applications = spec.applications.clone();
                    this.prompt_textarea.update(cx, |input, cx| {
                        input.set_selected_range(selection.range.clone(), cx);
                        if let Err(error) = input.replace_with_token(token, window, cx) {
                            this.attachment_error = Some(error.to_string());
                        }
                        input.focus_handle(cx).focus(window, cx);
                    });
                    cx.emit(DialogIntent::ApplicationsChanged {
                        dialog,
                        applications,
                    });
                    cx.notify();
                }
                CompletionSource::Provider(_)
                | CompletionSource::File(_)
                | CompletionSource::Directory(_)
                | CompletionSource::Composer(_) => {}
            },
        ));
        self.completion_subscriptions.push(cx.subscribe_in(
            &completion,
            window,
            |this, _, action: &crate::gpui_composer_completion::ComposerAddAction, _, cx| {
                this.composer_add_action(action, cx);
            },
        ));
        self.completion = Some(completion);
    }
    fn composer_add_action(
        &self,
        action: &crate::gpui_composer_completion::ComposerAddAction,
        cx: &mut Context<Self>,
    ) {
        match action {
            crate::gpui_composer_completion::ComposerAddAction::Plan(plan) => {
                if let Some(spec) = &self.spec {
                    cx.emit(DialogIntent::FieldChanged {
                        dialog: spec.id.clone(),
                        field: "plan-mode".into(),
                        value: plan.to_string(),
                    });
                }
            }
            crate::gpui_composer_completion::ComposerAddAction::Goal(objective) => {
                if let Some(spec) = &self.spec {
                    cx.emit(DialogIntent::FieldChanged {
                        dialog: spec.id.clone(),
                        field: "goal".into(),
                        value: objective.clone(),
                    });
                }
            }
        }
    }

    pub(crate) fn completion_active(&self, window: &Window, cx: &App) -> bool {
        self.completion
            .as_ref()
            .is_some_and(|completion| completion.read(cx).active(window, cx))
    }
    pub(crate) fn completion_blocks_submission(&self, window: &Window, cx: &App) -> bool {
        self.completion
            .as_ref()
            .is_some_and(|completion| completion.read(cx).blocks_submission(window, cx))
    }
    /// Flips the form's plan mode when the provider offers it.
    fn toggle_plan_mode(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let offered = self
            .completion
            .as_ref()
            .is_some_and(|completion| completion.read(cx).plan_mode());
        let Some(spec) = self
            .spec
            .as_ref()
            .filter(|spec| Self::is_agent_session_spec(spec) && !spec.busy)
        else {
            return false;
        };
        if !offered || window.has_active_dialog(cx) {
            return false;
        }
        let plan = spec
            .fields
            .iter()
            .any(|field| field.id == "plan-mode" && field.value == "true");
        cx.emit(DialogIntent::FieldChanged {
            dialog: spec.id.clone(),
            field: "plan-mode".into(),
            value: (!plan).to_string(),
        });
        true
    }

    pub(crate) fn perform_completion(
        &self,
        action: CommandAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if action == CommandAction::Dictate {
            if self
                .spec
                .as_ref()
                .is_none_or(|spec| !Self::is_agent_session_spec(spec) || spec.busy)
            {
                return false;
            }
            self.speech.update(cx, |speech, cx| {
                speech.toggle(crate::gpui_speech::SpeechMode::Dictation, window, cx);
            });
            return true;
        }
        if action == CommandAction::FocusComposer {
            if self
                .spec
                .as_ref()
                .is_none_or(|spec| !Self::is_agent_session_spec(spec) || spec.busy)
                || window.has_active_dialog(cx)
            {
                return false;
            }
            self.prompt_textarea
                .update(cx, |prompt, cx| prompt.focus(window, cx));
            return true;
        }
        if action == CommandAction::TogglePlan {
            return self.toggle_plan_mode(window, cx);
        }
        if matches!(action, CommandAction::EffortDown | CommandAction::EffortUp) {
            if window.has_active_dialog(cx) {
                return false;
            }
            let Some(picker) = &self.model_picker else {
                return false;
            };
            return picker.state.update(cx, |picker, cx| {
                picker.change_effort(action == CommandAction::EffortUp, cx)
            });
        }
        if let CommandAction::Focus(control) = action {
            let Some(spec) = &self.spec else {
                return false;
            };
            if !Self::is_agent_session_spec(spec) || spec.busy || window.has_active_dialog(cx) {
                return false;
            }
            match control {
                ComposerControl::Project => {
                    let Some(picker) = &self.project_picker else {
                        return false;
                    };
                    picker.state.focus_handle(cx).focus(window, cx);
                    window.dispatch_action(
                        Box::new(gpui_kit::base::actions::Confirm { secondary: false }),
                        cx,
                    );
                }
                ComposerControl::Model | ComposerControl::Provider => {
                    let Some(picker) = &self.model_picker else {
                        return false;
                    };
                    picker
                        .state
                        .update(cx, |picker, cx| picker.open(window, cx));
                }
                ComposerControl::Effort => {
                    let Some(picker) = &self.model_picker else {
                        return false;
                    };
                    picker
                        .state
                        .update(cx, |picker, cx| picker.open_reasoning(window, cx));
                }
                _ => {
                    if !spec.fields.iter().any(|field| field.id == control.field()) {
                        return false;
                    }
                    let Some(focus) = self.control_focus.get(control.field()) else {
                        return false;
                    };
                    return crate::gpui::activate_control_child(focus, window, cx);
                }
            }
            return true;
        }
        self.completion.as_ref().is_some_and(|completion| {
            completion.update(cx, |completion, cx| completion.perform(action, window, cx))
        })
    }
}
