//! Settings uses the same account catalog and command path as the agent composers.

use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use bootty_agents::{AgentKind, NativeModelOption};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
    ResourceKind,
};
use gpui_kit::component::{Sizable as _, button::Button};
use gpui_kit::{AnyElement, Context, IntoElement as _, Window};

use super::{
    GpuiSettings, ScalarValue, SettingsChoice, SettingsIntent,
    components::ValueRow,
    picker::{SelectControl, SelectStyle},
};

pub(super) struct ProviderCatalog {
    key: String,
    invocation: CommandInvocation,
    sender: BoundAppCommandSender,
    models: Vec<NativeModelOption>,
    loading: bool,
    error: Option<String>,
}

impl GpuiSettings {
    pub(crate) fn sync_provider_catalogs(
        &mut self,
        state: &crate::state::AppState,
        cx: &Context<Self>,
    ) {
        let Some(target) =
            state.current_command_target_for("agents.native.catalog", ResourceKind::Binding)
        else {
            return;
        };
        for provider in AgentKind::ALL {
            let Some(preferences) = state
                .config()
                .agents
                .provider(&provider.to_string())
                .filter(|p| p.enabled)
            else {
                continue;
            };
            let profile = preferences.selected_profile();
            let invocation = CommandInvocation {
                target: Some(target.clone()),
                ..CommandInvocation::new(
                    "agents.native.catalog",
                    vec![
                        provider.to_string(),
                        state
                            .mux()
                            .session_by_id_or_name(
                                state
                                    .workspace
                                    .active
                                    .binding
                                    .current_window_id()
                                    .session_id(),
                            )
                            .and_then(|session| session.anchor.cwd.clone())
                            .unwrap_or_else(|| {
                                crate::strings::home_dir().map_or_else(
                                    || "/".to_owned(),
                                    |home| home.to_string_lossy().into_owned(),
                                )
                            }),
                        preferences.program.clone(),
                        serde_json::to_string(&profile.map_or(&[][..], |p| p.arguments.as_slice()))
                            .unwrap_or_default(),
                        String::new(),
                        preferences.selected.clone(),
                    ],
                    Caller::Internal,
                )
            };
            let key = format!(
                "{target:?}:{:?}:{}:{}:{profile:?}",
                invocation.arguments.get(1),
                preferences.program,
                preferences.selected
            );
            if self
                .provider_catalogs
                .get(&provider)
                .is_some_and(|catalog| catalog.key == key)
            {
                continue;
            }
            self.provider_catalogs.insert(
                provider,
                ProviderCatalog {
                    key,
                    invocation,
                    sender: state.app_command_sender(Caller::Internal),
                    models: Vec::new(),
                    loading: false,
                    error: None,
                },
            );
            self.request_provider_catalog(provider, cx);
        }
    }

    pub(crate) fn provider_effort_options(&self, id: &str) -> Option<Vec<String>> {
        let name = id
            .strip_prefix("agents.")?
            .strip_suffix(".available-efforts")?;
        let provider = AgentKind::ALL
            .into_iter()
            .find(|provider| provider.to_string() == name)?;
        let catalog = self.provider_catalogs.get(&provider)?;
        if catalog.models.is_empty() {
            return None;
        }
        let mut efforts = Vec::new();
        for effort in catalog
            .models
            .iter()
            .flat_map(|model| &model.reasoning_efforts)
        {
            if !efforts.contains(effort) {
                efforts.push(effort.clone());
            }
        }
        Some(efforts)
    }

    pub(crate) fn reset_unsupported_provider_effort(&mut self, id: &str) {
        let Some(provider) = id
            .strip_prefix("agents.")
            .and_then(|id| id.strip_suffix(".default-model"))
        else {
            return;
        };
        let Some(kind) = AgentKind::ALL
            .into_iter()
            .find(|kind| kind.to_string() == provider)
        else {
            return;
        };
        let Some(catalog) = self.provider_catalogs.get(&kind) else {
            return;
        };
        let configured = self.draft.value(id);
        let model = catalog
            .models
            .iter()
            .find(|model| {
                configured.as_ref().and_then(ScalarValue::as_str) == Some(model.id.as_str())
            })
            .or_else(|| catalog.models.iter().find(|model| model.is_default));
        let effort_id = format!("agents.{provider}.default-effort");
        if let Some(model) = model
            && let Some(effort) = self.draft.value(&effort_id)
            && effort.as_str().is_some_and(|effort| {
                !effort.is_empty()
                    && !model
                        .reasoning_efforts
                        .iter()
                        .any(|supported| supported == effort)
            })
        {
            self.draft.set_value(
                &effort_id,
                &bootty_config::settings_schema::SettingValue::Text(String::new()),
            );
        }
    }

    fn request_provider_catalog(&mut self, provider: AgentKind, cx: &Context<Self>) {
        let Some(catalog) = self.provider_catalogs.get_mut(&provider) else {
            return;
        };
        if catalog.loading {
            return;
        }
        let deadline = {
            let now = Instant::now();
            now.checked_add(Duration::from_secs(60)).unwrap_or(now)
        };
        let receiver = match catalog.sender.submit(
            catalog.invocation.clone(),
            deadline,
            CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                catalog.error = Some(format!("Couldn't load models: {error:?}"));
                return;
            }
        };
        catalog.loading = true;
        catalog.error = None;
        let key = catalog.key.clone();
        cx.spawn(async move |owner, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            let _ = owner.update(cx, |this, cx| {
                let Some(catalog) = this
                    .provider_catalogs
                    .get_mut(&provider)
                    .filter(|catalog| catalog.key == key)
                else {
                    return;
                };
                catalog.loading = false;
                let models = match outcome {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        serde_json::from_value::<Vec<NativeModelOption>>(value)
                            .map_err(|error| error.to_string())
                    }
                    Ok(outcome) => Err(crate::commands::command_outcome_message(&outcome)
                        .unwrap_or_else(|| "Couldn't load models".to_owned())),
                    Err(_) => Err("Model discovery stopped".to_owned()),
                };
                match models {
                    Ok(models) if !models.is_empty() => catalog.models = models,
                    Ok(_) => {
                        catalog.error = Some("No models available for this account".to_owned());
                    }
                    Err(error) => catalog.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_provider_default(
        &self,
        row: ValueRow<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let ValueRow {
            id,
            label,
            help,
            value,
            ..
        } = row;
        let quick_model = id == "agents.quick-model";
        let (provider, leaf) = if quick_model {
            ("codex", "default-model")
        } else {
            id.strip_prefix("agents.")?.split_once('.')?
        };
        if !matches!(leaf, "default-model" | "default-effort") {
            return None;
        }
        let provider = AgentKind::ALL
            .into_iter()
            .find(|kind| kind.to_string() == provider)?;
        let catalog = self.provider_catalogs.get(&provider);
        if let Some(error) = catalog.and_then(|catalog| catalog.error.as_ref()) {
            return Some(Self::render_catalog_retry(provider, error, cx));
        }
        let models = catalog.map_or(&[][..], |catalog| catalog.models.as_slice());
        let current = value.as_str().unwrap_or_default();
        let choices = if quick_model {
            self.model_choices(provider, leaf, models, current)
        } else {
            let mut choices = vec![self.provider_settings_choice(provider, leaf, models)];
            choices.extend(self.model_choices(provider, leaf, models, current));
            choices
        };
        let id = id.to_owned();
        Some(Self::render_select(
            SelectControl {
                selector: format!("settings-choice-{id}"),
                label,
                description: help,
                current,
                choices: &choices,
                enabled: !models.is_empty(),
            },
            SelectStyle::Searchable { catalog: "models" },
            Rc::new(move |value| SettingsIntent::SetValue {
                id: id.clone(),
                value: ScalarValue::Text(value),
            }),
            window,
            cx,
        ))
    }

    fn render_catalog_retry(provider: AgentKind, error: &str, cx: &Context<Self>) -> AnyElement {
        Button::new(gpui_kit::SharedString::from(format!(
            "models-retry-{provider}"
        )))
        .outline()
        .small()
        .label("Retry loading models")
        .tooltip(error.to_owned())
        .on_click(cx.listener(move |this, _, _, cx| {
            this.request_provider_catalog(provider, cx);
            cx.notify();
        }))
        .into_any_element()
    }

    /// The configured model, or the provider's default when none is configured.
    fn current_model<'a>(
        &self,
        provider: AgentKind,
        models: &'a [NativeModelOption],
    ) -> Option<&'a NativeModelOption> {
        let configured = self
            .draft
            .value(&format!("agents.{provider}.default-model"));
        models
            .iter()
            .find(|model| {
                configured.as_ref().and_then(ScalarValue::as_str) == Some(model.id.as_str())
            })
            .or_else(|| models.iter().find(|model| model.is_default))
    }

    /// The empty choice, which leaves the model or effort to the provider's own settings.
    fn provider_settings_choice(
        &self,
        provider: AgentKind,
        leaf: &str,
        models: &[NativeModelOption],
    ) -> SettingsChoice {
        let label = if models.is_empty() {
            "Loading models…".to_owned()
        } else if leaf == "default-model" {
            models.iter().find(|model| model.is_default).map_or_else(
                || "Use provider settings".to_owned(),
                |model| format!("Use provider settings ({})", model.display_name),
            )
        } else {
            self.current_model(provider, models)
                .and_then(|model| model.default_reasoning_effort.as_deref())
                .map_or_else(
                    || "Use provider settings".to_owned(),
                    |effort| format!("Use provider settings ({effort})"),
                )
        };
        SettingsChoice {
            token: String::new(),
            label,
            description: None,
        }
    }

    /// The models, or the current model's available efforts, plus a configured value the
    /// catalog no longer lists.
    fn model_choices(
        &self,
        provider: AgentKind,
        leaf: &str,
        models: &[NativeModelOption],
        current: &str,
    ) -> Vec<SettingsChoice> {
        let mut choices = Vec::new();
        if leaf == "default-model" {
            choices.extend(models.iter().map(|model| SettingsChoice {
                token: model.id.clone(),
                label: model.display_name.clone(),
                description: Some(model.id.clone()),
            }));
        } else if let Some(model) = self.current_model(provider, models) {
            let available = self
                .draft
                .string_list(&format!("agents.{provider}.available-efforts"))
                .unwrap_or_default();
            choices.extend(
                model
                    .reasoning_efforts
                    .iter()
                    .filter(|effort| available.is_empty() || available.contains(effort))
                    .map(|effort| SettingsChoice {
                        token: effort.clone(),
                        label: crate::gpui_agent_session::reasoning_label(effort),
                        description: None,
                    }),
            );
        }
        if !current.is_empty() && !choices.iter().any(|choice| choice.token == current) {
            choices.push(SettingsChoice {
                token: current.to_owned(),
                label: current.to_owned(),
                description: Some("Configured value".to_owned()),
            });
        }
        choices
    }
}
