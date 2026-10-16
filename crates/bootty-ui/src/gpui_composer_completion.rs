//! Both composers share a retained, provider-scoped completion menu.
use crate::{
    completion::{CompletionKind, CompletionTrigger},
    gpui::CommandAction,
};
use bootty_agents::{NativeCompletionCatalog, NativeCompletionKind, NativeCompletionOption};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use gpui_kit::component::{
    ActiveTheme as _, IndexPath,
    command::{Command, CommandItem, CommandState},
    input::{InlineToken, TextareaState},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, Focusable as _, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Task, Window, deferred, div, prelude::*,
};
#[path = "gpui_composer_add.rs"]
mod add;
pub use add::ComposerAddAction;
pub use add::ComposerFileActions;

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionScope {
    pub catalog: CommandInvocation,
    pub files: CommandInvocation,
    pub applications: bool,
    /// Offer Bootty's own slash commands; only a live conversation can run them.
    pub commands: bool,
}

#[derive(Clone)]
pub enum CompletionSource {
    Provider(NativeCompletionOption),
    /// A project file, by its path from the project root.
    File(String),
    Directory(String),
    Application(bootty_computer::ComputerTarget),
    Composer(crate::composer_commands::ComposerCommand),
}
#[derive(Clone)]
struct CompletionItem {
    label: String,
    detail: String,
    icon: &'static str,
    source: CompletionSource,
}

#[derive(Clone)]
pub struct CompletionSelection {
    pub source: CompletionSource,
    pub range: std::ops::Range<usize>,
}

pub struct ComposerCompletion {
    scope: CompletionScope,
    sender: BoundAppCommandSender,
    editor: Entity<TextareaState>,
    menu: Entity<CommandState>,
    trigger: Option<CompletionTrigger>,
    dismissed: Option<(CompletionKind, usize)>,
    catalog: Option<NativeCompletionCatalog>,
    applications: Option<Vec<bootty_computer::ComputerTarget>>,
    provider_mentions: BTreeMap<String, NativeCompletionOption>,
    plan_mode: bool,
    draft_goal: Option<String>,
    add_menus: Vec<(
        gpui_kit::WeakEntity<gpui_kit::component::menu::PopupMenu>,
        ComposerFileActions,
    )>,
    files: Vec<CompletionItem>,
    file_snapshot: Option<(Instant, String, bootty_host::files::FileCompletionPage)>,
    items: Vec<CompletionItem>,
    pending: BTreeMap<&'static str, u64>,
    request: u64,
    file_query: Option<Task<()>>,
    file_cancellation: Option<CommandCancellation>,
    reset_selection: bool,
    confirming: bool,
    omitted: usize,
    error: Option<String>,
    _subscription: Subscription,
}

impl ComposerCompletion {
    pub(super) fn new(
        scope: CompletionScope,
        sender: BoundAppCommandSender,
        editor: Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        crate::gpui_application_icons::register(&sender, cx);
        let subscription = cx.observe_in(&editor, window, |this, _, window, cx| {
            this.sync(window, cx);
            if this.trigger.is_some() {
                cx.notify();
            }
        });
        let mut this = Self {
            scope,
            sender,
            editor,
            menu: cx.new(|cx| CommandState::new(window, cx)),
            trigger: None,
            dismissed: None,
            catalog: None,
            applications: None,
            provider_mentions: BTreeMap::new(),
            add_menus: Vec::new(),
            plan_mode: false,
            draft_goal: None,
            files: Vec::new(),
            file_snapshot: None,
            items: Vec::new(),
            pending: BTreeMap::new(),
            request: 0,
            file_query: None,
            file_cancellation: None,
            reset_selection: false,
            confirming: false,
            omitted: 0,
            error: None,
            _subscription: subscription,
        };
        this.load_add_catalog(window, cx);
        this
    }

    pub(super) fn restore_provider_mentions(
        &mut self,
        mentions: &[bootty_agents::NativeProviderMention],
    ) {
        self.provider_mentions = mentions
            .iter()
            .map(|mention| (mention.id(), mention.option.clone()))
            .collect();
    }

    pub(crate) fn provider_kind(&self, token_id: &str) -> Option<NativeCompletionKind> {
        self.provider_mentions
            .get(token_id)
            .map(|option| option.kind)
    }

    pub(super) fn provider_mentions(
        &self,
        cx: &App,
    ) -> Result<Vec<bootty_agents::NativeProviderMention>, String> {
        let mut mentions = Vec::new();
        for span in self.editor.read(cx).tokens() {
            let id = span.token().id().as_ref();
            if !id.starts_with("provider:") {
                continue;
            }
            let option = self
                .provider_mentions
                .get(id)
                .ok_or("Remove or reattach the app or plugin after changing accounts")?;
            mentions.push(bootty_agents::NativeProviderMention {
                option: option.clone(),
                prompt_range: span.range(),
            });
        }
        Ok(mentions)
    }

    pub(super) const fn scope(&self) -> &CompletionScope {
        &self.scope
    }
    pub(super) fn active(&self, window: &Window, cx: &App) -> bool {
        self.trigger.is_some() && self.editor.focus_handle(cx).is_focused(window)
    }

    /// Enter accepts a suggestion only when one exists. Text whose last token merely starts with
    /// a trigger character, such as `/new` or `costs $5`, still submits.
    pub(super) fn blocks_submission(&self, window: &Window, cx: &App) -> bool {
        self.confirming || (self.active(window, cx) && !self.items.is_empty())
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.editor.read(cx);
        let selection = state.selected_range();
        let token = state.tokens().iter().any(|span| {
            span.range().start < selection.start && selection.start <= span.range().end
        });
        let trigger = if token {
            None
        } else {
            CompletionTrigger::at(&state.value(), selection)
        };
        if trigger
            .as_ref()
            .map(|trigger| (trigger.kind, trigger.range.start))
            != self.dismissed
        {
            self.dismissed = None;
        }
        let trigger =
            trigger.filter(|trigger| self.dismissed != Some((trigger.kind, trigger.range.start)));
        if self.trigger == trigger {
            return;
        }
        self.file_query = None;
        if let Some(cancellation) = self.file_cancellation.take() {
            _ = cancellation.cancel();
        }
        self.pending.remove("files");
        self.reset_selection = true;
        self.trigger = trigger;
        self.files.clear();
        self.omitted = 0;
        self.error = None;
        let Some(request) = self.request.checked_add(1) else {
            self.trigger = None;
            return;
        };
        self.request = request;
        let Some(trigger) = self.trigger.clone() else {
            self.file_snapshot = None;
            self.items.clear();
            cx.notify();
            return;
        };
        if matches!(
            trigger.kind,
            CompletionKind::Command | CompletionKind::Skill | CompletionKind::Mention
        ) && self.catalog.is_none()
            && !self.pending.contains_key("catalog")
        {
            self.fetch(self.scope.catalog.clone(), "catalog", request, window, cx);
        }
        if trigger.kind == CompletionKind::Mention {
            self.load_files(&trigger.query, request, window, cx);
            if self.scope.applications
                && cfg!(target_os = "macos")
                && self.applications.is_none()
                && !self.pending.contains_key("applications")
            {
                self.fetch(
                    CommandInvocation::new("computer.targets", Vec::new(), Caller::Internal),
                    "applications",
                    request,
                    window,
                    cx,
                );
            }
        }
        self.rebuild(window, cx);
    }

    fn load_files(&mut self, query: &str, request: u64, window: &Window, cx: &Context<Self>) {
        // Reuse at most 100 complete candidates for one second while narrowing. Fetch a fresh
        // snapshot for broader queries; a filesystem watch can replace this expiry when needed.
        let cached = self
            .file_snapshot
            .as_ref()
            .filter(|(at, _, _)| at.elapsed() < Duration::from_secs(1))
            .and_then(|(_, previous, page)| page.narrow(previous, query));
        if let Some(page) = cached {
            self.install_files(page);
        } else {
            let mut files = self.scope.files.clone();
            files.arguments.push(query.to_owned());
            self.pending.insert("files", request);
            self.file_query = Some(cx.spawn_in(window, async move |owner, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                _ = owner.update_in(cx, |this, window, cx| {
                    if request == this.request {
                        this.fetch(files, "files", request, window, cx);
                    }
                });
            }));
        }
    }

    fn fetch(
        &mut self,
        invocation: CommandInvocation,
        source: &'static str,
        request: u64,
        window: &Window,
        cx: &Context<Self>,
    ) {
        let cancellation = CommandCancellation::new();
        if source == "files" {
            self.file_cancellation = Some(cancellation.clone());
        }
        let Ok(receiver) = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            cancellation,
        ) else {
            self.pending.remove(source);
            self.error = Some("Completion host is unavailable".into());
            return;
        };
        self.pending.insert(source, request);
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                if this.pending.get(source) == Some(&request) {
                    this.pending.remove(source);
                }
                // Provider capabilities are scoped to this entity; file queries are cursor-scoped.
                if source == "files" && request != this.request {
                    return;
                }
                match result {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        if let Err(error) = this.receive(source, value) {
                            this.error = Some(error);
                        } else if source == "applications" {
                            this.load_application_icons(cx);
                        } else if source == "catalog" {
                            crate::gpui_application_icons::load_remote(
                                this.catalog
                                    .iter()
                                    .flat_map(|catalog| &catalog.options)
                                    .filter_map(|option| option.icon_url.clone()),
                                cx,
                            );
                        }
                    }
                    Ok(outcome) => this.error = crate::commands::command_outcome_message(&outcome),
                    Err(_) => this.error = Some("Completion host disconnected".into()),
                }
                this.rebuild(window, cx);
                this.refresh_add_menus(window, cx);
            });
        })
        .detach();
    }

    fn load_application_icons(&self, cx: &mut Context<Self>) {
        let bundle_ids = self
            .applications
            .iter()
            .flatten()
            .map(|target| target.bundle_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        crate::gpui_application_icons::load(bundle_ids, cx);
    }

    fn receive(&mut self, source: &str, value: serde_json::Value) -> Result<(), String> {
        match source {
            "catalog" => {
                self.catalog = Some(serde_json::from_value(value).map_err(|e| e.to_string())?);
            }
            "files" => {
                let bootty_host::files::FileResponse::Completions(page) =
                    serde_json::from_value(value).map_err(|e| e.to_string())?
                else {
                    return Err("File host returned no completions".into());
                };
                if let Some(trigger) = &self.trigger {
                    self.file_snapshot =
                        Some((Instant::now(), trigger.query.clone(), page.clone()));
                }
                self.install_files(page);
            }
            "applications" => {
                let targets: Vec<bootty_computer::ComputerTarget> = serde_json::from_value(
                    value
                        .get("targets")
                        .cloned()
                        .ok_or("Host returned no application windows")?,
                )
                .map_err(|e| e.to_string())?;
                // Mentioning an app agents may not use would grant nothing.
                self.applications = Some(
                    targets
                        .into_iter()
                        .filter(crate::commands::agent_may_use)
                        .collect(),
                );
            }
            _ => return Err("Unknown completion source".into()),
        }
        Ok(())
    }

    fn install_files(&mut self, page: bootty_host::files::FileCompletionPage) {
        self.omitted = page.omitted;
        self.files = page
            .files
            .into_iter()
            .map(|path| CompletionItem {
                icon: if path.ends_with('/') {
                    "folder"
                } else {
                    crate::gpui_prompt_attachments::icon(&path)
                },
                label: path.clone(),
                detail: if path.ends_with('/') {
                    "Folder"
                } else {
                    "File"
                }
                .into(),
                source: if path.ends_with('/') {
                    CompletionSource::Directory(path)
                } else {
                    CompletionSource::File(path)
                },
            })
            .collect();
    }

    /// Whether the provider advertises a slash command with this name.
    pub(crate) fn provider_command(&self, name: &str) -> bool {
        self.catalog.as_ref().is_some_and(|catalog| {
            catalog
                .options
                .iter()
                .any(|option| option.kind == NativeCompletionKind::Command && option.name == name)
        })
    }

    /// `/` lists Bootty's commands, then the provider's commands and skills; `$` lists only
    /// skills. Apps and plugins are `@` mentions.
    fn command_candidates(&self, kind: CompletionKind) -> Vec<CompletionItem> {
        let composer = (kind == CompletionKind::Command && self.scope.commands)
            .then_some(crate::composer_commands::ComposerCommand::ALL)
            .into_iter()
            .flatten()
            // Only a provider with plan mode offers /plan.
            .filter(|command| {
                *command != crate::composer_commands::ComposerCommand::Plan
                    || self
                        .catalog
                        .as_ref()
                        .is_some_and(|catalog| catalog.plan_mode)
            })
            .map(|command| CompletionItem {
                label: format!("/{}", command.label(|name| self.provider_command(name))),
                detail: command.description().to_owned(),
                icon: "terminal",
                source: CompletionSource::Composer(command),
            });
        let provider = self
            .catalog
            .as_ref()
            .into_iter()
            .flat_map(|catalog| &catalog.options)
            .filter(|option| match option.kind {
                NativeCompletionKind::Skill => true,
                NativeCompletionKind::Command => kind == CompletionKind::Command,
                NativeCompletionKind::App | NativeCompletionKind::Plugin => false,
            })
            .map(|option| {
                let skill = option.kind == NativeCompletionKind::Skill;
                CompletionItem {
                    label: format!("{}{}", if skill { "$" } else { "/" }, option.name),
                    detail: option
                        .description
                        .clone()
                        .or_else(|| option.argument_hint.clone())
                        .unwrap_or_else(|| if skill { "Skill" } else { "Command" }.into()),
                    icon: if skill { "sparkles" } else { "terminal" },
                    source: CompletionSource::Provider(option.clone()),
                }
            });
        composer.chain(provider).collect()
    }

    fn integration_candidates(&self) -> Vec<CompletionItem> {
        self.catalog
            .as_ref()
            .into_iter()
            .flat_map(|catalog| &catalog.options)
            .filter(|option| {
                matches!(
                    option.kind,
                    NativeCompletionKind::App | NativeCompletionKind::Plugin
                )
            })
            .map(|option| CompletionItem {
                label: format!("@{}", option.display_name),
                detail: option.description.clone().unwrap_or_default(),
                icon: if option.kind == NativeCompletionKind::App {
                    "app-window"
                } else {
                    "plug"
                },
                source: CompletionSource::Provider(option.clone()),
            })
            .collect()
    }

    fn rebuild(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(trigger) = self.trigger.clone() else {
            self.items.clear();
            cx.notify();
            return;
        };
        // Local apps, then connected apps, then files: a bare `@` in a busy directory would
        // otherwise show only files. Ranking keeps this order among equal scores.
        let mut candidates = Vec::new();
        if matches!(
            trigger.kind,
            CompletionKind::Command | CompletionKind::Skill
        ) {
            candidates.extend(self.command_candidates(trigger.kind));
        }
        if trigger.kind == CompletionKind::Mention {
            candidates.extend(
                self.applications
                    .as_ref()
                    .into_iter()
                    .flatten()
                    .map(|target| {
                        let name = application_name(target);
                        CompletionItem {
                            label: format!("@{name}"),
                            detail: target
                                .title
                                .clone()
                                .unwrap_or_else(|| target.bundle_id.clone()),
                            icon: "app-window",
                            source: CompletionSource::Application(target.clone()),
                        }
                    }),
            );
            candidates.extend(self.integration_candidates());
        }
        candidates.extend(self.files.iter().cloned());
        let mut ranked = candidates
            .into_iter()
            .filter_map(|item| {
                let score = match &item.source {
                    CompletionSource::File(_) | CompletionSource::Directory(_) => {
                        crate::product_dialogs::searchable::fuzzy_match(&item.label, &trigger.query)
                            .map(|matched| matched.score)
                    }
                    _ => trigger.score(&item.label, &item.detail),
                };
                score.map(|score| (score, item))
            })
            .collect::<Vec<_>>();
        // Stable sorting keeps the provider's intentional order when no query is present.
        ranked.sort_by_key(|item| std::cmp::Reverse(item.0));
        self.reset_selection |= self.items.is_empty();
        self.items = ranked.into_iter().take(50).map(|(_, item)| item).collect();
        cx.notify();
    }

    fn entries(&self) -> Vec<CommandItem> {
        self.items
            .iter()
            .map(|item| {
                let label = item.label.clone();
                let detail = item.detail.clone();
                let icon = item.icon;
                let artwork = match &item.source {
                    CompletionSource::Application(target) => Some(target.bundle_id.clone()),
                    CompletionSource::Provider(option) => option.icon_url.clone(),
                    _ => None,
                };
                CommandItem::new().label(label.clone()).child(move |_, cx| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .w_full()
                        .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, _| {
                            window.prevent_default();
                        })
                        .child(artwork.as_deref().map_or_else(
                            || {
                                crate::gpui::sized_icon(
                                    icon,
                                    crate::gpui::IconSize::Small,
                                    cx.theme().muted_foreground,
                                )
                            },
                            |key| {
                                crate::gpui_application_icons::render(
                                    key,
                                    crate::gpui::IconSize::Small,
                                    cx.theme().muted_foreground,
                                    cx,
                                )
                            },
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .max_w(gpui_kit::relative(0.45))
                                        .flex_shrink_0()
                                        .text_sm()
                                        .text_ellipsis()
                                        .child(label.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .text_ellipsis()
                                        .child(detail.clone()),
                                ),
                        )
                })
            })
            .collect::<Vec<_>>()
    }

    pub(super) fn perform(
        &mut self,
        action: CommandAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.active(window, cx) {
            return false;
        }
        let index = self
            .menu
            .read(cx)
            .selected_index()
            .map_or(0, |index| index.row);
        match action {
            CommandAction::Cancel => {
                self.dismissed = self
                    .trigger
                    .as_ref()
                    .map(|trigger| (trigger.kind, trigger.range.start));
                self.trigger = None;
                self.items.clear();
                cx.notify();
            }
            CommandAction::Confirm => {
                self.confirming = true;
                self.select(index, window, cx);
                // Textarea emits PressEnter before the same key's propagated action finishes.
                // Keep that notification from submitting the newly accepted completion.
                cx.defer_in(window, |this, _, _| this.confirming = false);
            }
            CommandAction::Next | CommandAction::Previous => {
                if self.items.is_empty() {
                    return true;
                }
                let next = if action == CommandAction::Next {
                    index
                        .saturating_add(1)
                        .checked_rem(self.items.len())
                        .unwrap_or_default()
                } else {
                    index
                        .saturating_add(self.items.len())
                        .saturating_sub(1)
                        .checked_rem(self.items.len())
                        .unwrap_or_default()
                };
                self.menu.update(cx, |menu, cx| {
                    menu.set_selected_index(Some(IndexPath::new(next)), window, cx);
                });
            }
            CommandAction::EffortDown
            | CommandAction::EffortUp
            | CommandAction::ToggleFavorite
            | CommandAction::Focus(_)
            | CommandAction::Dictate
            | CommandAction::Voice => return false,
        }
        true
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(index).cloned() else {
            return;
        };
        let Some(trigger) = self.trigger.clone() else {
            return;
        };
        if CompletionTrigger::at(
            &self.editor.read(cx).value(),
            self.editor.read(cx).selected_range(),
        )
        .as_ref()
            != Some(&trigger)
        {
            return;
        }
        self.trigger = None;
        if self
            .editor
            .read(cx)
            .value()
            .get(trigger.range.clone())
            .is_none()
        {
            return;
        }
        self.dismissed = Some((trigger.kind, trigger.range.start));
        let text = match &item.source {
            CompletionSource::Directory(path) => Some(format!("@{}/", path.trim_end_matches('/'))),
            // The agent reads the project file itself, so the prompt names it, not a copy.
            CompletionSource::File(path) if path.contains(char::is_whitespace) => {
                Some(format!("@\"{path}\" "))
            }
            CompletionSource::File(path) => Some(format!("@{path} ")),
            CompletionSource::Composer(command) => Some(format!(
                "/{} ",
                command.label(|name| self.provider_command(name))
            )),
            _ => None,
        };
        if let Some(text) = text {
            // A folder drills in, so its completion stays open.
            let folder = matches!(item.source, CompletionSource::Directory(_));
            if folder {
                self.dismissed = None;
            } else {
                self.items.clear();
            }
            self.editor.update(cx, |input, cx| {
                input.set_selected_range(trigger.range.clone(), cx);
                input.replace(text, window, cx);
                input.focus_handle(cx).focus(window, cx);
            });
            cx.notify();
            return;
        }
        if let CompletionSource::Provider(option) = &item.source {
            if matches!(
                option.kind,
                NativeCompletionKind::App | NativeCompletionKind::Plugin
            ) {
                let mention = bootty_agents::NativeProviderMention {
                    option: option.clone(),
                    prompt_range: trigger.range.clone(),
                };
                let id = mention.id();
                self.provider_mentions.insert(id.clone(), option.clone());
                self.editor.update(cx, |input, cx| {
                    input.set_selected_range(trigger.range.clone(), cx);
                    _ = input.replace_with_token(
                        InlineToken::new(id, format!("@{}", option.display_name)),
                        window,
                        cx,
                    );
                    input.focus_handle(cx).focus(window, cx);
                });
            } else {
                self.editor.update(cx, |input, cx| {
                    input.set_selected_range(trigger.range.clone(), cx);
                    if option.kind == NativeCompletionKind::Skill {
                        let token = InlineToken::new(
                            format!("skill:{}", option.name),
                            format!("${}", option.name),
                        );
                        _ = input.replace_with_token(token, window, cx);
                    } else {
                        input.replace(format!("/{} ", option.name), window, cx);
                    }
                    input.focus_handle(cx).focus(window, cx);
                });
            }
        } else {
            cx.emit(CompletionSelection {
                source: item.source,
                range: trigger.range,
            });
        }
        self.items.clear();
        cx.notify();
    }
}

impl Drop for ComposerCompletion {
    fn drop(&mut self) {
        if let Some(cancellation) = &self.file_cancellation {
            _ = cancellation.cancel();
        }
    }
}

impl EventEmitter<CompletionSelection> for ComposerCompletion {}
impl Render for ComposerCompletion {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.active(window, cx) {
            return div().hidden().into_any_element();
        }
        let owner = cx.weak_entity();
        if self.reset_selection {
            self.reset_selection = false;
            let menu = self.menu.clone();
            // Command installs its new items during this render, before resetting selection.
            window.defer(cx, move |window, cx| {
                menu.update(cx, |menu, cx| {
                    menu.set_selected_index(Some(IndexPath::default()), window, cx);
                });
            });
        }
        let entries = self.entries();
        let bounds = self.editor.read(cx).input_bounds();
        let rem = f32::from(window.rem_size());
        // Leave room above the editor for the title bar and the narrowing hint.
        let height = gpui_kit::px(
            rem.mul_add(-3., f32::from(bounds.origin.y))
                .max(rem.mul_add(2., 0.))
                .min(rem.mul_add(16., 0.)),
        );
        deferred(
            gpui_kit::anchored()
                .anchor(gpui_kit::Anchor::BottomLeft)
                .position(bounds.origin)
                .snap_to_window()
                .child(
                    div()
                        .w(bounds.size.width)
                        .min_w_0()
                        .occlude()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().popover)
                        .rounded(cx.theme().radius)
                        .shadow_md()
                        .children((!self.items.is_empty()).then(|| {
                            Command::new(&self.menu)
                                .searchable(false)
                                .filterable(false)
                                .bordered(false)
                                .max_h(height)
                                .items(entries)
                                .on_confirm(move |index, window, cx| {
                                    _ = owner
                                        .update(cx, |this, cx| this.select(index.row, window, cx));
                                })
                        }))
                        .when(self.items.is_empty(), |view| {
                            view.child(
                                div()
                                    .p_2()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if self.pending.is_empty() {
                                        self.error
                                            .clone()
                                            .unwrap_or_else(|| "No matching suggestions".into())
                                    } else {
                                        "Loading…".into()
                                    }),
                            )
                        })
                        .when(self.omitted > 0, |view| {
                            view.child(
                                div()
                                    .p_1()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Type more to narrow files"),
                            )
                        }),
                ),
        )
        .with_priority(1)
        .into_any_element()
    }
}

/// The readable application name, falling back to the bundle identifier's last segment.
pub fn application_name(target: &bootty_computer::ComputerTarget) -> &str {
    target
        .application_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            target
                .bundle_id
                .rsplit('.')
                .next()
                .unwrap_or(&target.bundle_id)
        })
}
