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
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionScope {
    pub catalog: CommandInvocation,
    pub files: CommandInvocation,
    pub applications: bool,
    pub remote: Option<bootty_config::config::RemoteConfig>,
}

#[derive(Clone)]
pub enum CompletionSource {
    Provider(NativeCompletionOption),
    File(crate::attachment_source::FileCompletionSource),
    Directory(String),
    Application(bootty_computer::ComputerTarget),
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
    pub original: String,
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
        let subscription = cx.observe_in(&editor, window, |this, _, window, cx| {
            this.sync(window, cx);
            if this.trigger.is_some() {
                cx.notify();
            }
        });
        Self {
            scope,
            sender,
            editor,
            menu: cx.new(|cx| CommandState::new(window, cx)),
            trigger: None,
            dismissed: None,
            catalog: None,
            applications: None,
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
        }
    }

    pub(super) const fn scope(&self) -> &CompletionScope {
        &self.scope
    }
    pub(super) fn active(&self, window: &Window, cx: &App) -> bool {
        self.trigger.is_some() && self.editor.focus_handle(cx).is_focused(window)
    }

    pub(super) fn blocks_submission(&self, window: &Window, cx: &App) -> bool {
        self.confirming || self.active(window, cx)
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
            CompletionKind::Command | CompletionKind::Skill
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
                        }
                    }
                    Ok(outcome) => this.error = crate::commands::command_outcome_message(&outcome),
                    Err(_) => this.error = Some("Completion host disconnected".into()),
                }
                this.rebuild(window, cx);
            });
        })
        .detach();
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
                self.applications = Some(
                    serde_json::from_value(
                        value
                            .get("targets")
                            .cloned()
                            .ok_or("Host returned no application windows")?,
                    )
                    .map_err(|e| e.to_string())?,
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
                    let mut invocation = CommandInvocation::new(
                        "files.source",
                        vec![
                            format!("{}/{path}", page.root.trim_end_matches('/')),
                            page.root.clone(),
                        ],
                        Caller::Internal,
                    );
                    invocation.target.clone_from(&self.scope.files.target);
                    CompletionSource::File(crate::attachment_source::FileCompletionSource {
                        invocation,
                        remote: self.scope.remote.clone(),
                    })
                },
            })
            .collect();
    }

    fn rebuild(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(trigger) = self.trigger.clone() else {
            self.items.clear();
            cx.notify();
            return;
        };
        let mut candidates = self.files.clone();
        if matches!(
            trigger.kind,
            CompletionKind::Command | CompletionKind::Skill
        ) {
            candidates.extend(
                self.catalog
                    .as_ref()
                    .into_iter()
                    .flat_map(|catalog| &catalog.options)
                    .filter(|option| {
                        trigger.kind != CompletionKind::Skill
                            || option.kind == NativeCompletionKind::Skill
                    })
                    .cloned()
                    .map(|option| {
                        let skill = option.kind == NativeCompletionKind::Skill;
                        CompletionItem {
                            label: format!("{}{}", if skill { "$" } else { "/" }, option.name),
                            detail: option
                                .description
                                .clone()
                                .or_else(|| option.argument_hint.clone())
                                .unwrap_or_else(|| {
                                    if skill {
                                        "Skill".into()
                                    } else {
                                        "Command".into()
                                    }
                                }),
                            icon: if skill { "sparkles" } else { "terminal" },
                            source: CompletionSource::Provider(option),
                        }
                    }),
            );
        }
        if trigger.kind == CompletionKind::Mention {
            candidates.extend(
                self.applications
                    .as_ref()
                    .into_iter()
                    .flatten()
                    .map(|target| {
                        let name = target
                            .bundle_id
                            .rsplit('.')
                            .next()
                            .unwrap_or(&target.bundle_id);
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
        }
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
                        .child(crate::gpui::sized_icon(
                            icon,
                            crate::gpui::IconSize::Small,
                            cx.theme().muted_foreground,
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
            CommandAction::ToggleFavorite | CommandAction::Focus(_) => return false,
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
        let text = self.editor.read(cx).value().to_string();
        let Some(original) = text.get(trigger.range.clone()).map(str::to_owned) else {
            return;
        };
        self.dismissed = Some((trigger.kind, trigger.range.start));
        if let CompletionSource::Directory(path) = &item.source {
            self.dismissed = None;
            self.editor.update(cx, |input, cx| {
                input.set_selected_range(trigger.range.clone(), cx);
                input.replace(format!("@{}/", path.trim_end_matches('/')), window, cx);
                input.focus_handle(cx).focus(window, cx);
            });
            cx.notify();
            return;
        }
        if let CompletionSource::Provider(option) = &item.source {
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
        } else {
            cx.emit(CompletionSelection {
                source: item.source,
                range: trigger.range,
                original,
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
            return div().into_any_element();
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
