//! Conversation completions capture the exact provider and binding.
use super::*;
use crate::{
    gpui::CommandAction,
    gpui_composer_completion::{CompletionScope, CompletionSource, ComposerCompletion},
};
use gpui_kit::component::WindowExt as _;

impl NativeAgentSessionView {
    pub(crate) fn set_completion_binding(
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
        let completion = cx.new(|cx| ComposerCompletion::new(scope, sender, editor, window, cx));
        self.completion_subscriptions.clear();
        self.completion_subscriptions
            .push(cx.observe(&completion, |_, _, cx| cx.notify()));
        self.completion_subscriptions.push(cx.subscribe_in(
            &completion,
            window,
            |this, _, selection, window, cx| match &selection.source {
                CompletionSource::File(_) => {
                    this.stage_project_attachment(selection.clone(), window, cx);
                }
                CompletionSource::Application(target) => {
                    if this.applications.len() >= 8 {
                        this.error = Some("Mention at most eight application windows".into());
                        cx.notify();
                        return;
                    }
                    let id = format!("application:{}:{}", target.process_id, target.window_id);
                    let label = format!(
                        "@{}",
                        target
                            .bundle_id
                            .rsplit('.')
                            .next()
                            .unwrap_or(&target.bundle_id)
                    );
                    let mention = bootty_agents::NativeApplicationMention {
                        id: id.clone(),
                        target: target.clone(),
                        prompt_range: selection.range.clone(),
                    };
                    this.applications.retain(|existing| existing.id != id);
                    this.applications.push(mention);
                    this.composer.update(cx, |input, cx| {
                        input.set_selected_range(selection.range.clone(), cx);
                        if let Err(error) = input.replace_with_token(
                            gpui_kit::component::input::InlineToken::new(id, label),
                            window,
                            cx,
                        ) {
                            this.error = Some(error.to_string());
                        }
                        input.focus_handle(cx).focus(window, cx);
                    });
                    cx.notify();
                }
                CompletionSource::Provider(_) | CompletionSource::Directory(_) => {}
            },
        ));
        self.completion = Some(completion);
        cx.notify();
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
                        this.error = Some(error);
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

    pub(crate) fn completion_active(&self, window: &Window, cx: &App) -> bool {
        self.completion
            .as_ref()
            .is_some_and(|completion| completion.read(cx).active(window, cx))
    }
    pub(crate) fn perform_completion(
        &self,
        action: CommandAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let CommandAction::Focus(control) = action {
            if window.has_active_dialog(cx) {
                return false;
            }
            match control {
                crate::gpui::ComposerControl::Model => {
                    let Some(picker) = &self.model_picker else {
                        return false;
                    };
                    return crate::gpui::focus_control_child(&picker.focus_handle(cx), window, cx);
                }
                crate::gpui::ComposerControl::Effort => {
                    return crate::gpui::focus_control_child(&self.effort_focus, window, cx);
                }
                crate::gpui::ComposerControl::Permissions => {
                    return crate::gpui::focus_control_child(&self.permissions_focus, window, cx);
                }
                _ => return false,
            }
        }
        self.completion.as_ref().is_some_and(|completion| {
            completion.update(cx, |completion, cx| completion.perform(action, window, cx))
        })
    }
}
