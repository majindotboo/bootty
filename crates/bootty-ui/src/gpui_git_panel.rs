//! Native Changes and Diff panels; every Git operation enters the app command mailbox.

use std::time::{Duration, Instant};

use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
    CommandTarget,
};
use bootty_git::changes::{ChangeGroup, RepositoryChanges, RepositoryOverview};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent},
    input::{EditorState, Input, InputState, TextDecoration, TextDecorationCollection},
    list::ListItem,
    tree::{Tree, TreeEntry, TreeItem, TreeState},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle, Hsla, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, div, prelude::*,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPanelContext {
    pub terminal: Option<CommandTarget>,
    pub target: CommandTarget,
    pub directory: String,
    pub host: String,
    pub host_identity: String,
}

pub struct GitDiffPanel {
    active: bool,
    pub(crate) group: Option<gpui_kit::WeakEntity<gpui_kit::component::dock::TabGroup>>,
    editor: Entity<EditorState>,
    title: Option<String>,
    decorations: TextDecorationCollection,
    decoration_colors: Option<[Hsla; 3]>,
}

impl GitDiffPanel {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("diff")
                .soft_wrap(false)
        });
        let decorations = editor.update(cx, |editor, cx| {
            editor.create_decorations_collection(Vec::new(), cx)
        });
        Self {
            active: false,
            group: None,
            editor,
            decorations,
            decoration_colors: None,
            title: None,
        }
    }

    fn show(
        &mut self,
        title: String,
        contents: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.title.as_ref() == Some(&title) && self.editor.read(cx).value().as_str() == contents
        {
            return;
        }
        self.title = Some(title);
        self.decoration_colors = None;
        self.editor
            .update(cx, |editor, cx| editor.set_value(contents, window, cx));
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }
}

impl EventEmitter<PanelEvent> for GitDiffPanel {}
impl Focusable for GitDiffPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}
impl BasePanel for GitDiffPanel {
    fn visible(&self, _: &App) -> bool {
        self.title.is_some()
    }

    fn set_active(&mut self, active: bool, _: &mut Window, _: &mut Context<Self>) {
        self.active = active;
    }
    fn on_added_to(
        &mut self,
        group: gpui_kit::WeakEntity<gpui_kit::component::dock::TabGroup>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.group = Some(group);
    }
    fn on_removed(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.group = None;
        self.active = false;
    }
    fn panel_name(&self) -> &'static str {
        "bootty.diff"
    }
}
impl Panel for GitDiffPanel {
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        Some(
            self.title
                .clone()
                .unwrap_or_else(|| crate::i18n::t(cx, "panel-diff"))
                .into(),
        )
    }
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tab_name(cx).unwrap_or_default()
    }
}
impl Render for GitDiffPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = [
            cx.theme().success,
            cx.theme().danger,
            cx.theme().muted_foreground,
        ];
        if self.decoration_colors != Some(colors) {
            self.decoration_colors = Some(colors);
            let contents = self.editor.read(cx).value();
            let mut offset = 0_usize;
            let decorations = contents
                .split_inclusive('\n')
                .filter_map(|line| {
                    let range = offset..offset.saturating_add(line.len());
                    offset = range.end;
                    let color = if line.starts_with("+++")
                        || line.starts_with("---")
                        || line.starts_with("@@")
                        || line.starts_with("diff ")
                        || line.starts_with("index ")
                    {
                        colors[2]
                    } else if line.starts_with('+') {
                        colors[0]
                    } else if line.starts_with('-') {
                        colors[1]
                    } else {
                        return None;
                    };
                    Some(TextDecoration::new(
                        range,
                        HighlightStyle {
                            color: Some(color),
                            ..Default::default()
                        },
                    ))
                })
                .collect();
            self.decorations.set(decorations, cx);
        }
        crate::gpui::readonly_editor(&self.editor, "Git diff")
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Panel visibility, read requests and mutations vary independently"
)]
pub struct GitChangesPanel {
    host_visible: bool,
    context: GitPanelContext,
    sender: BoundAppCommandSender,
    diff: Entity<GitDiffPanel>,
    message: Entity<InputState>,
    changes: Option<RepositoryChanges>,
    tree: Entity<TreeState>,
    overview: Option<RepositoryOverview>,
    branch_name: Entity<InputState>,
    branch_start: Entity<InputState>,
    stash_message: Entity<InputState>,
    pending: bool,
    request_revision: u64,
    mutating: bool,
    selected_diff: Option<(String, String)>,
    error: Option<String>,
    active: bool,
    local_git: Option<bootty_git::GitFactsCache>,
    revision: u64,
    pending_directory: Option<String>,
    pending_read: Option<(&'static str, Vec<String>)>,
    messages: std::collections::HashMap<String, String>,
}

pub struct OpenDiff;
impl EventEmitter<OpenDiff> for GitChangesPanel {}

impl GitChangesPanel {
    pub(crate) fn has_pending_work(&self, cx: &App) -> bool {
        self.mutating
            || self.error.is_some()
            || [
                &self.message,
                &self.branch_name,
                &self.branch_start,
                &self.stash_message,
            ]
            .into_iter()
            .any(|input| !input.read(cx).value().is_empty())
            || self.messages.values().any(|message| !message.is_empty())
    }

    pub(crate) fn decoration(&self, path: &str) -> String {
        let Some(changes) = &self.changes else {
            return String::new();
        };
        let Some(relative) = path
            .strip_prefix(&changes.root)
            .and_then(|path| path.strip_prefix('/').or_else(|| path.strip_prefix('\\')))
        else {
            return String::new();
        };
        changes
            .files
            .iter()
            .find(|file| file.path == relative)
            .map_or_else(String::new, |file| {
                format!("  {}{}", file.index, file.worktree)
            })
    }

    pub(crate) fn new(
        context: GitPanelContext,
        sender: BoundAppCommandSender,
        diff: Entity<GitDiffPanel>,
        local_git: Option<bootty_git::GitFactsCache>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.spawn_in(window, async move |weak, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if weak
                    .update_in(cx, |this, window, cx| {
                        if (!this.active && !this.diff.read(cx).active)
                            || !this.host_visible
                            || this.pending
                        {
                            return;
                        }
                        let root = this
                            .changes
                            .as_ref()
                            .map_or(&this.context.directory, |changes| &changes.root);
                        let revision = this
                            .local_git
                            .as_ref()
                            .map(|cache| cache.worktree_revision(root));
                        if revision
                            .is_none_or(|revision| revision == 0 || revision != this.revision)
                        {
                            this.revision = revision.unwrap_or_default();
                            this.refresh(window, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            host_visible: true,
            context,
            sender,
            diff,
            active: false,
            local_git,
            revision: 0,
            pending_directory: None,
            pending_read: None,
            messages: std::collections::HashMap::new(),
            message: cx.new(|cx| {
                InputState::new(window, cx).placeholder(crate::i18n::t(cx, "git-message"))
            }),
            changes: None,
            tree: cx.new(|cx| TreeState::new(cx)),
            overview: None,
            branch_name: cx.new(|cx| InputState::new(window, cx).placeholder("new branch")),
            branch_start: cx.new(|cx| InputState::new(window, cx).placeholder("start ref (HEAD)")),
            stash_message: cx
                .new(|cx| InputState::new(window, cx).placeholder("stash message (optional)")),
            pending: false,
            request_revision: 0,
            mutating: false,
            selected_diff: None,
            error: None,
        }
    }

    pub(crate) fn browse(
        &mut self,
        directory: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending {
            self.pending_directory = Some(directory);
            return;
        }
        if self.context.directory != directory {
            self.messages.insert(
                self.context.directory.clone(),
                self.message.read(cx).value().to_string(),
            );
            self.context.directory = directory;
            let message = self
                .messages
                .remove(&self.context.directory)
                .unwrap_or_default();
            self.message
                .update(cx, |input, cx| input.set_value(message, window, cx));
            self.changes = None;
            self.overview = None;
            self.selected_diff = None;
            self.error = None;
            self.revision = 0;
            self.tree
                .update(cx, |tree, cx| tree.set_items(Vec::new(), cx));
        }
        self.refresh(window, cx);
    }

    pub(crate) const fn set_host_visible(&mut self, visible: bool) {
        self.host_visible = visible;
    }

    pub(crate) fn refresh(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.pending {
            self.request_inner("git.status", Vec::new(), false, window, cx);
        }
    }

    fn refresh_selected_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, group)) = self.selected_diff.clone() else {
            return;
        };
        let file = self
            .changes
            .as_ref()
            .and_then(|changes| changes.files.iter().find(|file| file.path == path));
        let groups = file
            .map(|file| {
                file.groups()
                    .map(|group| match group {
                        ChangeGroup::Staged => "staged",
                        ChangeGroup::Unstaged => "unstaged",
                        ChangeGroup::Untracked => "untracked",
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some(group) = groups
            .iter()
            .find(|candidate| **candidate == group)
            .or_else(|| groups.first())
        {
            self.request_inner(
                "git.diff",
                vec![path, (*group).to_owned()],
                false,
                window,
                cx,
            );
        } else {
            self.selected_diff = None;
            self.diff.update(cx, |diff, cx| {
                diff.show(
                    path,
                    "No changes remaining for this file.".to_owned(),
                    window,
                    cx,
                );
            });
        }
    }

    fn request(
        &mut self,
        command: &'static str,
        args: Vec<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.request_inner(command, args, true, window, cx);
    }

    fn prepare_diff_selection(
        &mut self,
        command: &str,
        args: &[String],
    ) -> Result<String, &'static str> {
        if command == "git.diff" {
            let [path, group] = args else {
                return Err("A diff path and change group are required");
            };
            self.selected_diff = Some((path.clone(), group.clone()));
        } else if command == "git.commit-diff" {
            let [commit] = args else {
                return Err("A commit is required");
            };
            self.selected_diff = None;
            return Ok(format!("Commit {}", short_commit(commit)));
        }
        Ok(self.selected_diff.as_ref().map_or_else(
            || "Diff".to_owned(),
            |(path, group)| format!("{path} · {group}"),
        ))
    }

    fn submit_command(
        &self,
        command: &str,
        mut args: Vec<String>,
    ) -> Result<std::sync::mpsc::Receiver<CommandOutcome>, bootty_control::AppCommandSendError>
    {
        let root = self
            .changes
            .as_ref()
            .map_or(&self.context.directory, |changes| &changes.root)
            .clone();
        args.insert(0, root);
        let mut invocation = CommandInvocation::from_action(command, Caller::Internal);
        invocation.arguments = args;
        invocation.target = Some(self.context.target.clone());
        let cancellation = CommandCancellation::new();
        self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_mins(2))
                .unwrap_or_else(Instant::now),
            cancellation,
        )
    }

    fn request_inner(
        &mut self,
        command: &'static str,
        args: Vec<String>,
        reveal: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let mutation = !matches!(
            command,
            "git.status" | "git.diff" | "git.overview" | "git.commit-diff"
        );
        if self.pending && (!mutation || self.mutating) {
            if reveal && matches!(command, "git.diff" | "git.commit-diff" | "git.overview") {
                self.pending_read = Some((command, args));
            }
            return;
        }
        let diff_title = match self.prepare_diff_selection(command, &args) {
            Ok(title) => title,
            Err(error) => {
                self.error = Some(error.to_owned());
                cx.notify();
                return;
            }
        };
        let receiver = match self.submit_command(command, args) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.error = Some(format!("Git command unavailable: {error:?}"));
                cx.notify();
                return;
            }
        };
        self.pending = true;
        self.mutating = mutation;
        self.request_revision = self.request_revision.wrapping_add(1);
        let revision = self.request_revision;
        if reveal {
            self.error = None;
        }
        cx.notify();
        cx.spawn_in(window, async move |weak, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update_in(cx, |this, window, cx| {
                // A write supersedes an in-flight preview. Its stale result cannot
                // clear the write's busy state or replace the resulting diff.
                if revision != this.request_revision {
                    return;
                }
                this.pending = false;
                this.mutating = false;
                match outcome {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        if command == "git.status" {
                            match serde_json::from_value(value) {
                                Ok(changes) => {
                                    if this.changes.as_ref() != Some(&changes) {
                                        this.changes = Some(changes);
                                        this.sync_tree(cx);
                                    }
                                    this.refresh_selected_diff(window, cx);
                                }
                                Err(error) => this.error = Some(error.to_string()),
                            }
                        } else if command == "git.overview" {
                            match serde_json::from_value(value) {
                                Ok(overview) => this.overview = Some(overview),
                                Err(error) => this.error = Some(error.to_string()),
                            }
                        } else if matches!(command, "git.diff" | "git.commit-diff") {
                            let contents = value.as_str().unwrap_or_default().to_owned();
                            this.diff
                                .update(cx, |diff, cx| diff.show(diff_title, contents, window, cx));
                            if reveal {
                                cx.emit(OpenDiff);
                            }
                        } else {
                            this.overview = None;
                            if matches!(command, "git.commit" | "git.amend") {
                                this.message
                                    .update(cx, |message, cx| message.set_value("", window, cx));
                            }
                            this.refresh(window, cx);
                        }
                    }
                    Ok(outcome) => {
                        this.error = Some(
                            super::commands::command_outcome_message(&outcome)
                                .unwrap_or_else(|| "Git command failed".to_owned()),
                        );
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                if let Some(directory) = this.pending_directory.take() {
                    this.pending_read = None;
                    this.browse(directory, window, cx);
                } else if let Some((command, args)) = this.pending_read.take() {
                    this.request(command, args, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn sync_tree(&mut self, cx: &mut Context<Self>) {
        let Some(changes) = &self.changes else {
            return;
        };
        let items = [
            (ChangeGroup::Staged, "staged"),
            (ChangeGroup::Unstaged, "unstaged"),
            (ChangeGroup::Untracked, "untracked"),
        ]
        .into_iter()
        .filter_map(|(group, key)| {
            let files = changes
                .files
                .iter()
                .filter(|file| file.groups().any(|candidate| candidate == group))
                .map(|file| TreeItem::new(format!("{key}:{}", file.path), file.path.clone()))
                .collect::<Vec<_>>();
            if files.is_empty() {
                return None;
            }
            let expanded = self
                .tree
                .read(cx)
                .index_of(&key.into())
                .and_then(|ix| self.tree.read(cx).entry(ix))
                .is_none_or(gpui_kit::base::TreeEntry::is_expanded);
            Some(
                TreeItem::new(
                    key,
                    format!(
                        "{} · {}",
                        crate::i18n::t(cx, &format!("git-{key}")),
                        files.len()
                    ),
                )
                .children(files)
                .expanded(expanded),
            )
        })
        .collect::<Vec<_>>();
        let selected = self.tree.update(cx, |tree, cx| {
            let selected = tree.selected_item().map(|item| item.id.clone());
            tree.set_items(items, cx);
            if let Some(id) = selected {
                let index = tree.index_of(&id).or_else(|| {
                    let (_, path) = id.split_once(':')?;
                    ["staged", "unstaged", "untracked"]
                        .into_iter()
                        .find_map(|group| tree.index_of(&format!("{group}:{path}").into()))
                });
                tree.set_selected_index(index, cx);
            }
            tree.selected_item().map(|item| item.id.clone())
        });
        if let Some(id) = selected
            && let Some((group, path)) = id.split_once(':')
            && self
                .selected_diff
                .as_ref()
                .is_some_and(|(selected, _)| selected == path)
        {
            self.selected_diff = Some((path.to_owned(), group.to_owned()));
        }
    }

    fn open_change(&mut self, id: &str, window: &Window, cx: &mut Context<Self>) {
        if let Some((group, path)) = id.split_once(':') {
            self.request(
                "git.diff",
                vec![path.to_owned(), group.to_owned()],
                window,
                cx,
            );
        }
    }

    fn file_row(
        entry: &TreeEntry,
        selected: bool,
        owner: &gpui_kit::WeakEntity<Self>,
        pending: bool,
        cx: &App,
    ) -> ListItem {
        let item = entry.item();
        let id = item.id.clone();
        let owner = owner.clone();
        let mut row = ListItem::new(id.clone())
            .role(gpui_kit::Role::TreeItem)
            .aria_label(item.label.clone())
            .aria_selected(selected)
            .w_full()
            .h_7()
            .px_2()
            .py_0()
            .text_sm();
        if entry.is_folder() {
            return row.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Icon::new(if entry.is_expanded() {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall(),
                    )
                    .child(item.label.clone()),
            );
        }
        let (group, path) = id.split_once(':').unwrap_or(("", &item.label));
        let (directory, name) = path.rsplit_once('/').unwrap_or(("", path));
        let staged = group == "staged";
        let stage_path = path.to_owned();
        let stage_owner = owner.clone();
        row = row
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .flex_1()
                    .child(
                        Icon::new(IconName::File)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(div().min_w_0().flex_1().truncate().child(name.to_owned()))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(directory.to_owned()),
                    ),
            )
            .suffix(move |_, _| {
                let path = stage_path.clone();
                let owner = stage_owner.clone();
                Button::new("stage-change")
                    .icon(if staged {
                        IconName::Minus
                    } else {
                        IconName::Plus
                    })
                    .ghost()
                    .xsmall()
                    .disabled(pending)
                    .accessibility_label(if staged { "Unstage file" } else { "Stage file" })
                    .tooltip(if staged {
                        "Unstage file (Space)"
                    } else {
                        "Stage file (Space)"
                    })
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        _ = owner.update(cx, |this, cx| {
                            this.request(
                                if staged { "git.unstage" } else { "git.stage" },
                                vec![path.clone()],
                                window,
                                cx,
                            );
                        });
                    })
            })
            .on_click(move |_, window, cx| {
                _ = owner.update(cx, |this, cx| this.open_change(&id, window, cx));
            });
        row
    }

    fn file_list(&self, cx: &Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let pending = self.mutating;
        let tree = Tree::new(&self.tree, move |_, entry, selected, _, cx| {
            Self::file_row(entry, selected, &owner, pending, cx)
        });
        let focus_tree = self.tree.clone();
        div()
            .id("git-files")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .role(gpui_kit::Role::Tree)
            .aria_label("Changed files")
            .on_mouse_down(gpui_kit::MouseButton::Left, move |_, window, cx| {
                focus_tree.update(cx, |tree, cx| tree.focus(window, cx));
            })
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        let id = this
                            .tree
                            .read(cx)
                            .selected_item()
                            .map(|item| item.id.clone());
                        if let Some(id) = id {
                            if event.keystroke.key == "enter" {
                                this.open_change(&id, window, cx);
                            } else if let Some((group, path)) = id.split_once(':') {
                                this.request(
                                    if group == "staged" {
                                        "git.unstage"
                                    } else {
                                        "git.stage"
                                    },
                                    vec![path.to_owned()],
                                    window,
                                    cx,
                                );
                            }
                            cx.stop_propagation();
                        }
                    }
                }),
            )
            .child(tree)
    }

    fn commit(&mut self, amend: bool, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.message.read(cx).value().to_string();
        if !amend {
            self.request("git.commit", vec![message], window, cx);
            return;
        }
        let directory = self.context.directory.clone();
        let answer = window.prompt(
            gpui_kit::PromptLevel::Warning,
            crate::i18n::t(cx, "git-amend-confirm").as_str(),
            Some(crate::i18n::t(cx, "git-amend-detail").as_str()),
            &[
                gpui_kit::PromptButton::Other(crate::i18n::t(cx, "git-amend-action").into()),
                gpui_kit::PromptButton::Cancel(crate::i18n::t(cx, "common-cancel").into()),
            ],
            cx,
        );
        cx.spawn_in(window, async move |weak, cx| {
            if answer.await == Ok(0) {
                _ = weak.update_in(cx, |this, window, cx| {
                    if this.context.directory != directory {
                        this.error = Some("Repository changed while confirming the amend. Retry from its Changes panel.".to_owned());
                        cx.notify();
                        return;
                    }
                    this.request("git.amend", vec![message], window, cx);
                });
            }
        })
        .detach();
    }

    fn drop_stash(
        &self,
        reference: String,
        commit: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let directory = self.context.directory.clone();
        let answer = window.prompt(
            gpui_kit::PromptLevel::Warning,
            &format!("Drop {reference}?"),
            Some("The saved changes will be removed from your stash list."),
            &[
                gpui_kit::PromptButton::Other("Drop stash".into()),
                gpui_kit::PromptButton::Cancel("Cancel".into()),
            ],
            cx,
        );
        cx.spawn_in(window, async move |weak, cx| {
            if answer.await == Ok(0) {
                _ = weak.update_in(cx, |this, window, cx| {
                    if this.context.directory != directory {
                        this.error = Some("Repository changed while confirming the stash deletion. Retry from its Changes panel.".to_owned());
                        cx.notify();
                        return;
                    }
                    this.request("git.stash-drop", vec![reference, commit], window, cx);
                });
            }
        })
        .detach();
    }
}

impl GitChangesPanel {
    fn append_branches(
        &self,
        mut content: gpui_kit::Stateful<gpui_kit::Div>,
        branches: &[bootty_git::changes::GitBranch],
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        content = content.child("Branches");
        for branch in branches {
            let name = branch.name.clone();
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .min_w_0()
                    .gap_1()
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(format!(
                        "{}{} · {}",
                        if branch.current { "● " } else { "" },
                        branch.name,
                        short_commit(&branch.commit)
                    )))
                    .child(
                        Button::new(format!("checkout-branch:{}", branch.name))
                            .label("Check out")
                            .small()
                            .disabled(self.mutating || branch.current)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.request("git.branch-checkout", vec![name.clone()], window, cx);
                            })),
                    ),
            );
        }
        content = content.child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .gap_1()
                .child(crate::gpui::focus_input(
                    &self.branch_name,
                    Input::new(&self.branch_name),
                ))
                .child(crate::gpui::focus_input(
                    &self.branch_start,
                    Input::new(&self.branch_start),
                ))
                .child(
                    Button::new("create-branch")
                        .label("Create branch")
                        .small()
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let name = this.branch_name.read(cx).value().to_string();
                            let start = this.branch_start.read(cx).value().to_string();
                            this.request(
                                "git.branch-create",
                                vec![
                                    name,
                                    if start.is_empty() {
                                        "HEAD".into()
                                    } else {
                                        start
                                    },
                                ],
                                window,
                                cx,
                            );
                        })),
                ),
        );
        content
    }

    fn append_history(
        &self,
        mut content: gpui_kit::Stateful<gpui_kit::Div>,
        history: &[bootty_git::changes::GitCommit],
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        content = content.child("Recent history");
        for commit in history {
            let id = commit.id.clone();
            content = content.child(
                Button::new(format!("commit-diff:{}", commit.id))
                    .ghost()
                    .w_full()
                    .justify_start()
                    .child(div().flex_1().min_w_0().truncate().child(format!(
                        "{} · {} · {}",
                        short_commit(&commit.id),
                        commit.author,
                        commit.subject
                    )))
                    .small()
                    .disabled(self.mutating)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.request("git.commit-diff", vec![id.clone()], window, cx);
                    })),
            );
        }
        content
    }

    fn append_stashes(
        &self,
        mut content: gpui_kit::Stateful<gpui_kit::Div>,
        stashes: &[bootty_git::changes::GitStash],
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        content = content.child("Stashes");
        for stash in stashes {
            let apply = stash.reference.clone();
            let apply_commit = stash.commit.clone();
            let drop_commit = stash.commit.clone();
            let drop = stash.reference.clone();
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .child(format!("{} · {}", stash.reference, stash.subject)),
                    )
                    .child(
                        Button::new(format!("stash-apply:{}", stash.commit))
                            .label("Apply")
                            .small()
                            .disabled(self.mutating)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.request(
                                    "git.stash-apply",
                                    vec![apply.clone(), apply_commit.clone()],
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new(format!("stash-drop:{}", stash.commit))
                            .label("Drop")
                            .small()
                            .disabled(self.mutating)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.drop_stash(drop.clone(), drop_commit.clone(), window, cx);
                            })),
                    ),
            );
        }
        content = content.child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .gap_1()
                .child(crate::gpui::focus_input(
                    &self.stash_message,
                    Input::new(&self.stash_message),
                ))
                .child(
                    Button::new("stash-tracked")
                        .label("Stash tracked")
                        .small()
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let message = this.stash_message.read(cx).value().to_string();
                            this.request(
                                "git.stash-push",
                                vec![message, "false".into()],
                                window,
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new("stash-all")
                        .label("Stash including untracked")
                        .small()
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let message = this.stash_message.read(cx).value().to_string();
                            this.request(
                                "git.stash-push",
                                vec![message, "true".into()],
                                window,
                                cx,
                            );
                        })),
                ),
        );
        content
    }

    fn render_header(&self, cx: &Context<Self>) -> [gpui_kit::AnyElement; 2] {
        let root = self
            .changes
            .as_ref()
            .map_or(self.context.directory.as_str(), |changes| {
                changes.root.as_str()
            });
        let branch = self
            .changes
            .as_ref()
            .and_then(|changes| changes.branch.clone())
            .unwrap_or_else(|| "Repository".to_owned());
        [
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .flex_shrink_0()
                .child(div().flex_1().min_w_0().truncate().text_sm().child(branch))
                .child(
                    Button::new("refresh")
                        .icon(IconName::RotateCw)
                        .ghost()
                        .small()
                        .accessibility_label("Refresh changes")
                        .tooltip("Refresh changes")
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                )
                .into_any_element(),
            div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{} · {root}", self.context.host))
                .into_any_element(),
        ]
    }
}

impl EventEmitter<PanelEvent> for GitChangesPanel {}
impl Focusable for GitChangesPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.message.focus_handle(cx)
    }
}
impl BasePanel for GitChangesPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.changes"
    }
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if active {
            self.refresh(window, cx);
        }
    }
    fn on_removed(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.active = false;
    }
}
impl Panel for GitChangesPanel {
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        Some(crate::i18n::t(cx, "panel-changes").into())
    }
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::i18n::t(cx, "panel-changes")
    }
}
impl Render for GitChangesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = div()
            .id("git-changes")
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .gap_2()
            .p_2()
            .overflow_hidden()
            .children(self.render_header(cx));
        if let Some(error) = &self.error {
            body = body.child(gpui_kit::component::alert::Alert::error(
                "git-error",
                error.clone(),
            ));
        }
        if self
            .changes
            .as_ref()
            .is_some_and(|changes| changes.files.is_empty())
        {
            body = body.child(
                div()
                    .p_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No changes"),
            );
        }
        if self.overview.is_none() {
            body = body.child(self.file_list(cx));
        }
        body = body.child(
            div().flex().gap_1().child(
                Button::new("git-overview")
                    .label(if self.overview.is_some() {
                        "Back to changes"
                    } else {
                        "History, branches & stashes"
                    })
                    .ghost()
                    .small()
                    .disabled(self.mutating)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.overview.take().is_some() {
                            cx.notify();
                        } else {
                            this.request("git.overview", vec!["100".into()], window, cx);
                        }
                    })),
            ),
        );
        if let Some(overview) = &self.overview {
            let mut content = div()
                .id("repository-history")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .gap_2()
                .overflow_y_scroll();
            content = self.append_branches(content, &overview.branches, cx);
            content = self.append_history(content, &overview.history, cx);
            content = self.append_stashes(content, &overview.stashes, cx);
            body = body.child(content);
        }
        body.child(crate::gpui::focus_input(
            &self.message,
            Input::new(&self.message).disabled(self.mutating),
        ))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("commit")
                        .label(crate::i18n::t(cx, "git-commit"))
                        .small()
                        .disabled(self.mutating)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.commit(false, window, cx)),
                        ),
                )
                .child(
                    Button::new("amend")
                        .label(crate::i18n::t(cx, "git-amend"))
                        .small()
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, window, cx| this.commit(true, window, cx))),
                ),
        )
    }
}

fn short_commit(commit: &str) -> String {
    commit.chars().take(10).collect()
}
