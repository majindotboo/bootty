//! Native Changes and Diff panels; every Git operation enters the app command mailbox.

mod diff;
mod pull_request;

pub use diff::{DiffComment, GitDiffPanel};

use crate::gpui::{ChangeKind, ElementTooltip as _, change_row, line_counts};
use std::time::{Duration, Instant};

use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
    CommandTarget,
};
use bootty_git::changes::{ChangeGroup, ChangedFile, RepositoryChanges, RepositoryOverview};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Icon, IconName, Selectable as _,
    Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent},
    input::{Input, InputState},
    menu::ContextMenuExt as _,
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Pixels,
    Render, SharedString, Styled, Window, div, prelude::*, px,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPanelContext {
    pub terminal: Option<CommandTarget>,
    pub target: CommandTarget,
    pub directory: String,
    pub host: String,
    pub host_identity: String,
    pub remote: Option<bootty_config::config::RemoteConfig>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Panel visibility, read requests and mutations vary independently"
)]
pub struct GitChangesPanel {
    host_visible: bool,
    context: GitPanelContext,
    sender: BoundAppCommandSender,
    /// The dock's diff panel, where pull request files open.
    diff: Entity<GitDiffPanel>,
    /// The diff this panel shows in place of its list, or beside it when there is room.
    detail: Entity<GitDiffPanel>,
    /// What the shown diff is, like a file's path or "All changes"; without one the list shows.
    detail_title: Option<String>,
    /// The file the keyboard is on.
    cursor: Option<(ChangeGroup, String)>,
    list_focus: FocusHandle,
    /// The panel's width, which decides whether the list and a diff fit side by side.
    width: Pixels,
    message: Entity<InputState>,
    changes: Option<RepositoryChanges>,
    overview: Option<RepositoryOverview>,
    branch_name: Entity<InputState>,
    branch_start: Entity<InputState>,
    /// Narrows the branch list to names containing it.
    branch_filter: Entity<InputState>,
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
    pull_requests: Option<Entity<pull_request::PullRequestView>>,
    pull_request_subscription: Option<gpui_kit::Subscription>,
    showing_pull_requests: bool,
    _subscriptions: [gpui_kit::Subscription; 5],
    /// Diffs leave out whitespace changes, as the diff panel asked.
    ignore_whitespace: bool,
    /// Codex is writing a commit message for the staged changes.
    writing: bool,
    /// The branch last asked about, when, and its pull request once GitHub answered.
    pull_request: Option<(
        String,
        Instant,
        Option<bootty_git::github::PullRequestStatus>,
    )>,
}

pub struct OpenDiff;
impl EventEmitter<OpenDiff> for GitChangesPanel {}
impl EventEmitter<DiffComment> for GitChangesPanel {}

/// A change group's name in Git commands.
const fn group_key(group: ChangeGroup) -> &'static str {
    match group {
        ChangeGroup::Staged => "staged",
        ChangeGroup::Unstaged => "unstaged",
        ChangeGroup::Untracked => "untracked",
    }
}

impl GitChangesPanel {
    pub(crate) fn has_pending_work(&self, cx: &App) -> bool {
        self.pull_requests
            .as_ref()
            .is_some_and(|view| view.read(cx).has_draft(cx))
            || self.mutating
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
        Self::poll_worktree(window, cx);
        let message =
            cx.new(|cx| InputState::new(window, cx).placeholder(crate::i18n::t(cx, "git-message")));
        let detail = cx.new(|cx| {
            let mut detail = GitDiffPanel::new(window, cx);
            detail.hide_toolbar();
            detail
        });
        let branch_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Find a branch"));
        // Hunks chosen in the diff stage or unstage through this panel's repository, hiding
        // whitespace asks Git again for the shown diff, a comment goes on to the conversation,
        // and the commit buttons follow the message.
        let subscriptions = [
            cx.subscribe_in(
                &detail,
                window,
                |this, _, hunk: &diff::StageHunk, window, cx| {
                    this.request(
                        "git.stage-hunk",
                        vec![hunk.patch.clone(), hunk.reverse.to_string()],
                        window,
                        cx,
                    );
                },
            ),
            cx.subscribe_in(
                &detail,
                window,
                |this, _, changed: &diff::WhitespaceChanged, window, cx| {
                    this.ignore_whitespace = changed.0;
                    if this.selected_diff.is_some() {
                        this.refresh_selected_diff(window, cx);
                    } else {
                        this.request_inner("git.diff-all", Vec::new(), false, window, cx);
                    }
                },
            ),
            cx.subscribe(&detail, |_, _, comment: &DiffComment, cx| {
                cx.emit(comment.clone());
            }),
            cx.observe(&message, |_, _, cx| cx.notify()),
            cx.observe(&branch_filter, |_, _, cx| cx.notify()),
        ];
        Self {
            host_visible: true,
            context,
            sender,
            diff,
            detail,
            detail_title: None,
            cursor: None,
            list_focus: cx.focus_handle(),
            width: px(0.),
            active: false,
            local_git,
            revision: 0,
            pending_directory: None,
            pending_read: None,
            messages: std::collections::HashMap::new(),
            message,
            changes: None,
            overview: None,
            branch_name: cx.new(|cx| InputState::new(window, cx).placeholder("new branch")),
            branch_start: cx.new(|cx| InputState::new(window, cx).placeholder("start ref (HEAD)")),
            branch_filter,
            stash_message: cx
                .new(|cx| InputState::new(window, cx).placeholder("stash message (optional)")),
            pending: false,
            request_revision: 0,
            mutating: false,
            selected_diff: None,
            error: None,
            pull_requests: None,
            pull_request_subscription: None,
            showing_pull_requests: false,
            _subscriptions: subscriptions,
            ignore_whitespace: false,
            writing: false,
            pull_request: None,
        }
    }

    /// Every two seconds while it shows, asks Git again when the worktree changed.
    fn poll_worktree(window: &Window, cx: &Context<Self>) {
        cx.spawn_in(window, async move |weak, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if weak
                    .update_in(cx, |this, window, cx| {
                        if (!this.active && !this.diff.read(cx).active)
                            || this.showing_pull_requests
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
            if self
                .pull_requests
                .as_ref()
                .is_some_and(|view| view.read(cx).has_draft(cx))
            {
                self.error=Some("Submit or discard the current pull request review before changing repositories.".into());
                cx.notify();
                return;
            }
            self.pull_requests = None;
            self.pull_request_subscription = None;
            self.showing_pull_requests = false;
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
            self.detail_title = None;
            self.cursor = None;
            self.error = None;
            self.revision = 0;
        }
        self.refresh(window, cx);
    }

    pub(crate) const fn set_host_visible(&mut self, visible: bool) {
        self.host_visible = visible;
    }

    pub(crate) fn refresh(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.showing_pull_requests {
            if let Some(view) = &self.pull_requests {
                view.update(cx, |view, cx| view.refresh(window, cx));
            }
            return;
        }
        if !self.pending {
            self.request_inner("git.status", Vec::new(), false, window, cx);
        }
    }

    fn refresh_selected_diff(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some((path, group)) = self.selected_diff.clone() else {
            return;
        };
        let file = self
            .changes
            .as_ref()
            .and_then(|changes| changes.files.iter().find(|file| file.path == path));
        let groups = file.map_or_else(Default::default, |file| {
            file.groups()
                .map(|group| match group {
                    ChangeGroup::Staged => "staged",
                    ChangeGroup::Unstaged => "unstaged",
                    ChangeGroup::Untracked => "untracked",
                })
                .collect::<Vec<_>>()
        });
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
            // The file has no changes left, so it is gone from the list too.
            self.selected_diff = None;
            self.detail_title = None;
            cx.notify();
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
        } else if command == "git.diff-all" {
            self.selected_diff = None;
            return Ok("All changes".to_owned());
        }
        Ok(self.selected_diff.as_ref().map_or_else(
            || "Diff".to_owned(),
            |(path, group)| format!("{path} · {group}"),
        ))
    }

    /// A changed file's actions: show it in the file manager or open it in an installed editor
    /// when it's on this computer, and copy its path.
    fn file_menu(
        owner: &gpui_kit::WeakEntity<Self>,
        path: &str,
        menu: gpui_kit::component::menu::PopupMenu,
        cx: &App,
    ) -> gpui_kit::component::menu::PopupMenu {
        let Some(panel) = owner.upgrade() else {
            return menu;
        };
        let panel = panel.read(cx);
        let root = panel
            .changes
            .as_ref()
            .map_or(&panel.context.directory, |changes| &changes.root);
        let file = format!("{}/{path}", root.trim_end_matches('/'));
        let local = panel.context.remote.is_none();
        let menu = if local {
            let (reveal, path) = (owner.clone(), file.clone());
            menu.item(
                gpui_kit::component::menu::PopupMenuItem::new(crate::commands::reveal_label())
                    .on_click(move |_, window, cx| {
                        _ = reveal.update(cx, |this, cx| {
                            this.hand_to_desktop(
                                "files.reveal",
                                vec![path.clone()],
                                "Could not show the file",
                                window,
                                cx,
                            );
                        });
                    }),
            )
        } else {
            menu
        };
        let copied = file.clone();
        let menu = menu.item(
            gpui_kit::component::menu::PopupMenuItem::new("Copy Path").on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(copied.clone()));
            }),
        );
        // Editors differ by computer, so they follow the rows every file has.
        let editors = if local {
            crate::editors::installed()
        } else {
            &[]
        };
        editors
            .iter()
            .enumerate()
            .fold(menu, |menu, (index, editor)| {
                let (edit, path) = (owner.clone(), file.clone());
                let menu = if index == 0 { menu.separator() } else { menu };
                menu.item(
                    gpui_kit::component::menu::PopupMenuItem::new(format!(
                        "Open in {}",
                        editor.name
                    ))
                    .on_click(move |_, window, cx| {
                        _ = edit.update(cx, |this, cx| {
                            this.hand_to_desktop(
                                "files.open_in",
                                vec![path.clone(), editor.id.to_owned()],
                                "Could not open the file",
                                window,
                                cx,
                            );
                        });
                    }),
                )
            })
            .separator()
            .item({
                let (discard, path) = (owner.clone(), path.to_owned());
                gpui_kit::component::menu::PopupMenuItem::new("Discard Changes…").on_click(
                    move |_, window, cx| {
                        _ = discard.update(cx, |this, cx| {
                            this.confirm_discard(Some(path.clone()), window, cx);
                        });
                    },
                )
            })
    }

    /// Asks before throwing changes away: one file's, or every change when `path` is `None`.
    fn confirm_discard(&self, path: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let (title, detail) = path.as_ref().map_or_else(
            || {
                (
                    "Discard all changes?".to_owned(),
                    "Staged and unstaged changes go back to the last commit, and untracked files are deleted. Ignored files stay.",
                )
            },
            |path| {
                (
                    format!("Discard the changes to {path}?"),
                    "The file goes back to the last commit, or is deleted if the last commit doesn't have it.",
                )
            },
        );
        let answer = crate::gpui::prompt(
            &title,
            Some(detail),
            &[
                gpui_kit::PromptButton::Other("Discard".into()),
                gpui_kit::PromptButton::Cancel("Cancel".into()),
            ],
            window,
            cx,
        );
        let directory = self.context.directory.clone();
        cx.spawn_in(window, async move |weak, cx| {
            if answer.await == Ok(0) {
                _ = weak.update_in(cx, |this, window, cx| {
                    if this.context.directory != directory {
                        return;
                    }
                    match path {
                        Some(path) => this.request("git.discard", vec![path], window, cx),
                        None => this.request("git.discard-all", Vec::new(), window, cx),
                    }
                });
            }
        })
        .detach();
    }

    /// Hands a local file to this computer: `files.reveal` or `files.open_in`. A refusal
    /// shows as `failure`.
    fn hand_to_desktop(
        &self,
        command: &str,
        arguments: Vec<String>,
        failure: &'static str,
        window: &Window,
        cx: &Context<Self>,
    ) {
        let mut invocation = CommandInvocation::new(command, arguments, Caller::Internal);
        invocation.target = Some(self.context.target.clone());
        let Ok(receiver) = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) else {
            return;
        };
        cx.spawn_in(window, async move |weak, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update(cx, |this, cx| {
                if let Ok(outcome) = outcome
                    && let Some(message) = crate::commands::command_outcome_message(&outcome)
                {
                    this.error = Some(
                        crate::error_catalog::ErrorNotice::failed(failure, message).to_string(),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
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
            "git.status" | "git.diff" | "git.diff-all" | "git.overview" | "git.commit-diff"
        );
        if self.pending && (!mutation || self.mutating) {
            if reveal
                && matches!(
                    command,
                    "git.diff" | "git.diff-all" | "git.commit-diff" | "git.overview"
                )
            {
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
        let mut args = args;
        if matches!(command, "git.diff" | "git.diff-all") && self.ignore_whitespace {
            args.push("ignore".to_owned());
        }
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
                        if let Err(error) = this
                            .receive_command_value(command, value, diff_title, reveal, window, cx)
                        {
                            this.error = Some(error.to_string());
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

    fn receive_command_value(
        &mut self,
        command: &str,
        value: serde_json::Value,
        diff_title: String,
        reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), serde_json::Error> {
        match command {
            "git.status" => {
                let changes = serde_json::from_value(value)?;
                if self.changes.as_ref() != Some(&changes) {
                    self.changes = Some(changes);
                    self.keep_cursor();
                }
                self.ask_pull_request(window, cx);
                if !self.showing_pull_requests {
                    self.refresh_selected_diff(window, cx);
                }
            }
            "git.overview" => self.overview = Some(serde_json::from_value(value)?),
            "git.diff" | "git.diff-all" | "git.commit-diff" => {
                if self.showing_pull_requests {
                    return Ok(());
                }
                let contents = value.as_str().unwrap_or_default().to_owned();
                let group = (command == "git.diff")
                    .then(|| self.selected_diff.as_ref().map(|(_, group)| group.clone()))
                    .flatten();
                let hide_whitespace = self.ignore_whitespace;
                // A refresh keeps a shown diff current; it never opens one.
                if !reveal && self.detail_title.is_none() {
                    return Ok(());
                }
                self.detail_title = Some(diff_title.clone());
                self.detail.update(cx, |diff, cx| {
                    diff.show(diff_title, contents, window, cx);
                    if command != "git.commit-diff" {
                        diff.mark_from_changes(hide_whitespace, cx);
                    }
                    if let Some(group) = &group {
                        diff.allow_staging(group, cx);
                    }
                });
            }
            _ => {
                self.overview = None;
                if matches!(command, "git.commit" | "git.amend") {
                    self.message
                        .update(cx, |message, cx| message.set_value("", window, cx));
                }
                self.refresh(window, cx);
            }
        }
        Ok(())
    }

    /// Every changed file in the list's order, under the group it shows in. A file with staged
    /// and unstaged changes shows in both.
    fn listed(&self) -> Vec<(ChangeGroup, &ChangedFile)> {
        let Some(changes) = &self.changes else {
            return Vec::new();
        };
        [
            ChangeGroup::Staged,
            ChangeGroup::Unstaged,
            ChangeGroup::Untracked,
        ]
        .into_iter()
        .flat_map(|group| {
            changes
                .files
                .iter()
                .filter(move |file| file.groups().any(|candidate| candidate == group))
                .map(move |file| (group, file))
        })
        .collect()
    }

    /// Moves the keyboard to a listed file when the one it was on is gone.
    fn keep_cursor(&mut self) {
        let listed = self.listed();
        let kept = self.cursor.as_ref().and_then(|(group, path)| {
            listed
                .iter()
                .find(|(candidate, file)| candidate == group && file.path == *path)
                .or_else(|| listed.iter().find(|(_, file)| file.path == *path))
        });
        let cursor = kept
            .or_else(|| self.cursor.is_some().then(|| listed.first()).flatten())
            .map(|(group, file)| (*group, file.path.clone()));
        self.cursor = cursor;
    }

    fn open_change(
        &mut self,
        group: ChangeGroup,
        path: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.cursor = Some((group, path.to_owned()));
        self.request(
            "git.diff",
            vec![path.to_owned(), group_key(group).to_owned()],
            window,
            cx,
        );
    }

    /// Stages a file from the unstaged and untracked groups, or unstages it from the staged one.
    fn toggle_staged(
        &mut self,
        group: ChangeGroup,
        path: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.request(
            if group == ChangeGroup::Staged {
                "git.unstage"
            } else {
                "git.stage"
            },
            vec![path.to_owned()],
            window,
            cx,
        );
    }

    /// Opens the file `delta` places from the shown one in the list.
    fn step_change(&mut self, delta: isize, window: &Window, cx: &mut Context<Self>) {
        let listed = self.listed();
        let Some((path, group)) = &self.selected_diff else {
            return;
        };
        let Some(position) = listed
            .iter()
            .position(|(candidate, file)| group_key(*candidate) == group && file.path == *path)
        else {
            return;
        };
        let Some((group, file)) = position
            .checked_add_signed(delta)
            .and_then(|next| listed.get(next))
        else {
            return;
        };
        let (group, path) = (*group, file.path.clone());
        self.open_change(group, &path, window, cx);
    }

    /// Steps the keyboard through the list.
    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let listed = self.listed();
        let position = self.cursor.as_ref().and_then(|(group, path)| {
            listed
                .iter()
                .position(|(candidate, file)| candidate == group && file.path == *path)
        });
        let next = position.map_or(0, |position| {
            position
                .saturating_add_signed(delta)
                .min(listed.len().saturating_sub(1))
        });
        if let Some((group, file)) = listed.get(next) {
            self.cursor = Some((*group, file.path.clone()));
            cx.notify();
        }
    }

    /// Back from a shown diff to the list.
    fn close_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.detail_title = None;
        self.selected_diff = None;
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    /// Shows `contents`, a Git diff such as a turn's changes, titled `title`, until going back to
    /// the list.
    pub(crate) fn show_diff(
        &mut self,
        title: String,
        contents: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.showing_pull_requests = false;
        self.overview = None;
        self.selected_diff = None;
        self.detail_title = Some(title.clone());
        self.detail
            .update(cx, |diff, cx| diff.show(title, contents, window, cx));
        cx.notify();
    }

    /// One group's heading, with what it does to all of its files.
    fn render_group_heading(
        &self,
        group: ChangeGroup,
        count: usize,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let key = group_key(group);
        let pending = self.mutating;
        let (stage, label) = if group == ChangeGroup::Staged {
            ("git.unstage-all", "Unstage all")
        } else {
            ("git.stage-all", "Stage all")
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .h_7()
            .px_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(div().flex_1().min_w_0().truncate().child(format!(
                "{} · {count}",
                crate::i18n::t(cx, &format!("git-{key}"))
            )))
            .when(group == ChangeGroup::Unstaged, |row| {
                row.child(
                    Button::new("discard-all")
                        .debug_selector(|| "git-discard-all".into())
                        .icon(IconName::Delete)
                        .ghost()
                        .xsmall()
                        .disabled(pending)
                        .accessibility_label("Discard all changes")
                        .element_tooltip("Discard all changes")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_discard(None, window, cx);
                        })),
                )
            })
            // Staging everything in the unstaged group would sweep up untracked files too.
            .when(
                group != ChangeGroup::Untracked || !self.has_group(ChangeGroup::Unstaged),
                |row| {
                    row.child(
                        Button::new(SharedString::from(format!("stage-group:{key}")))
                            .debug_selector(move || format!("git-stage-group:{key}"))
                            .icon(if group == ChangeGroup::Staged {
                                IconName::Minus
                            } else {
                                IconName::Plus
                            })
                            .ghost()
                            .xsmall()
                            .disabled(pending)
                            .accessibility_label(label)
                            .element_tooltip(label)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.request(stage, Vec::new(), window, cx);
                            })),
                    )
                },
            )
    }

    fn has_group(&self, group: ChangeGroup) -> bool {
        self.listed()
            .iter()
            .any(|(candidate, _)| *candidate == group)
    }

    /// A changed file's row: clicking shows its diff, and its button stages or unstages it.
    fn render_file_row(
        &self,
        group: ChangeGroup,
        file: &ChangedFile,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let path = file.path.clone();
        let key = group_key(group);
        let (kind, lines) = match group {
            ChangeGroup::Staged => (ChangeKind::from_status(file.index), file.staged_lines),
            ChangeGroup::Unstaged => (ChangeKind::from_status(file.worktree), file.unstaged_lines),
            ChangeGroup::Untracked => (ChangeKind::Added, None),
        };
        let selected = self
            .cursor
            .as_ref()
            .is_some_and(|(candidate, cursor)| *candidate == group && *cursor == path)
            || self
                .selected_diff
                .as_ref()
                .is_some_and(|(selected, selected_group)| {
                    *selected == path && selected_group == key
                });
        let staged = group == ChangeGroup::Staged;
        let pending = self.mutating;
        let owner = cx.weak_entity();
        let (menu_owner, menu_path) = (owner.clone(), path.clone());
        let stage_path = path.clone();
        let trailing = div()
            .flex()
            .items_center()
            .gap_2()
            .children(lines.map(|lines| line_counts(lines, cx)))
            .child(
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
                    .element_tooltip(if staged {
                        "Unstage file (Space)"
                    } else {
                        "Stage file (Space)"
                    })
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        _ = owner.update(cx, |this, cx| {
                            this.toggle_staged(group, &stage_path, window, cx);
                        });
                    }),
            );
        change_row(
            SharedString::from(format!("{key}:{path}")),
            kind,
            &path,
            trailing,
            cx,
        )
        .debug_selector({
            let path = path.clone();
            move || format!("git-change:{key}:{path}")
        })
        .selected(selected)
        .on_click(cx.listener(move |this, _, window, cx| {
            this.open_change(group, &path, window, cx);
        }))
        .context_menu(move |menu, _, cx| Self::file_menu(&menu_owner, &menu_path, menu, cx))
    }

    fn file_list(&self, cx: &Context<Self>) -> impl IntoElement {
        let listed = self.listed();
        let mut list = div()
            .id("git-files")
            .track_focus(&self.list_focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .role(gpui_kit::Role::List)
            .aria_label("Changed files")
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    let cursor = this.cursor.clone();
                    match event.keystroke.key.as_str() {
                        "up" => this.move_cursor(-1, cx),
                        "down" => this.move_cursor(1, cx),
                        "enter" => {
                            if let Some((group, path)) = cursor {
                                this.open_change(group, &path, window, cx);
                            }
                        }
                        "space" => {
                            if let Some((group, path)) = cursor {
                                this.toggle_staged(group, &path, window, cx);
                            }
                        }
                        _ => return,
                    }
                    cx.stop_propagation();
                }),
            );
        let mut group = None;
        for (ix, (file_group, file)) in listed.iter().enumerate() {
            if group != Some(*file_group) {
                group = Some(*file_group);
                let count = listed
                    .get(ix..)
                    .unwrap_or_default()
                    .iter()
                    .take_while(|(candidate, _)| candidate == file_group)
                    .count();
                list = list.child(self.render_group_heading(*file_group, count, cx));
            }
            list = list.child(self.render_file_row(*file_group, file, cx));
        }
        list
    }

    /// The shown diff with its heading: back to the list, what it shows, the files next to it,
    /// and the diff's view controls.
    fn render_detail(&self, cx: &Context<Self>) -> gpui_kit::Stateful<gpui_kit::Div> {
        let file = self.selected_diff.is_some();
        let heading = div()
            .flex()
            .items_center()
            .gap_1()
            .h_9()
            .px_1()
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("diff-back")
                    .debug_selector(|| "git-diff-back".into())
                    .icon(Icon::default().path("icons/arrow-left.svg"))
                    .ghost()
                    .small()
                    .accessibility_label("Back to changes")
                    .element_tooltip("Back to changes (Escape)")
                    .on_click(cx.listener(|this, _, window, cx| this.close_detail(window, cx))),
            )
            .child(self.render_detail_title(cx))
            .when(file, |row| {
                row.child(
                    Button::new("diff-previous")
                        .icon(Icon::default().path("icons/chevron-up.svg"))
                        .ghost()
                        .xsmall()
                        .accessibility_label("Previous file")
                        .element_tooltip("Previous file")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.step_change(-1, window, cx)),
                        ),
                )
                .child(
                    Button::new("diff-next")
                        .icon(Icon::default().path("icons/chevron-down.svg"))
                        .ghost()
                        .xsmall()
                        .accessibility_label("Next file")
                        .element_tooltip("Next file")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.step_change(1, window, cx)),
                        ),
                )
            })
            .children(self.detail.read(cx).controls(self.detail.downgrade(), cx));
        div()
            .id("git-detail")
            .debug_selector(|| "git-detail".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" && !this.detail.read(cx).is_commenting() {
                        this.close_detail(window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .child(heading)
            .child(div().flex_1().min_h_0().child(self.detail.clone()))
    }

    /// What the shown diff is: a file's name, its folder and its lines, or a title like
    /// "All changes".
    fn render_detail_title(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let shown = self.selected_diff.as_ref().and_then(|(path, group)| {
            self.listed()
                .into_iter()
                .find(|(candidate, file)| group_key(*candidate) == group && file.path == *path)
                .map(|(group, file)| (group, file.clone()))
        });
        let Some((group, file)) = shown else {
            return div()
                .flex_1()
                .min_w_0()
                .truncate()
                .px_1()
                .text_sm()
                .child(self.detail_title.clone().unwrap_or_default());
        };
        let (folder, name) = file.path.rsplit_once('/').unwrap_or(("", &file.path));
        let lines = match group {
            ChangeGroup::Staged => file.staged_lines,
            ChangeGroup::Unstaged => file.unstaged_lines,
            ChangeGroup::Untracked => None,
        };
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_2()
            .px_1()
            .text_sm()
            .child(div().min_w_0().truncate().child(name.to_owned()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(folder.to_owned()),
            )
            .children(lines.map(|lines| line_counts(lines, cx)))
    }

    /// Whether anything is staged to commit.
    fn has_staged(&self) -> bool {
        self.changes.as_ref().is_some_and(|changes| {
            changes
                .files
                .iter()
                .any(|file| file.groups().any(|group| group == ChangeGroup::Staged))
        })
    }

    /// Asks Codex for a message describing the staged changes, into an empty message box; a
    /// message typed meanwhile stays.
    fn write_message(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.writing {
            return;
        }
        let receiver = match self.submit_command("git.write-message", Vec::new()) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.error = Some(format!("Git command unavailable: {error:?}"));
                cx.notify();
                return;
            }
        };
        self.writing = true;
        self.error = None;
        cx.notify();
        let directory = self.context.directory.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update_in(cx, |this, window, cx| {
                this.writing = false;
                if this.context.directory != directory {
                    return;
                }
                match outcome {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        let written = value
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        if this.message.read(cx).value().trim().is_empty() {
                            this.message
                                .update(cx, |message, cx| message.set_value(written, window, cx));
                        }
                    }
                    Ok(outcome) => {
                        this.error = Some(
                            super::commands::command_outcome_message(&outcome)
                                .unwrap_or_else(|| "Codex couldn't write a message".to_owned()),
                        );
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn commit(&mut self, amend: bool, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.message.read(cx).value().to_string();
        if !amend {
            self.request("git.commit", vec![message], window, cx);
            return;
        }
        let directory = self.context.directory.clone();
        let answer = crate::gpui::prompt(
            crate::i18n::t(cx, "git-amend-confirm").as_str(),
            Some(crate::i18n::t(cx, "git-amend-detail").as_str()),
            &[
                gpui_kit::PromptButton::Other(crate::i18n::t(cx, "git-amend-action").into()),
                gpui_kit::PromptButton::Cancel(crate::i18n::t(cx, "common-cancel").into()),
            ],
            window,
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
        let answer = crate::gpui::prompt(
            &format!("Drop {reference}?"),
            Some("The saved changes will be removed from your stash list."),
            &[
                gpui_kit::PromptButton::Other("Drop stash".into()),
                gpui_kit::PromptButton::Cancel("Cancel".into()),
            ],
            window,
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
        content = content.child("Branches").child(crate::gpui::focus_input(
            &self.branch_filter,
            Input::new(&self.branch_filter)
                .context_menu(crate::gpui::text_menu(&self.branch_filter))
                .aria_label("Find a branch"),
        ));
        let filter = self.branch_filter.read(cx).value().to_lowercase();
        let matching = branches
            .iter()
            .filter(|branch| branch.name.to_lowercase().contains(&filter))
            .collect::<Vec<_>>();
        // Changes travel to another branch only in a stash the person chose to make.
        let dirty = self
            .changes
            .as_ref()
            .is_some_and(|changes| !changes.files.is_empty());
        for branch in matching.iter().take(MAX_SHOWN_BRANCHES) {
            content = content.child(self.branch_row(branch, dirty, cx));
        }
        if let Some(more) = matching
            .len()
            .checked_sub(MAX_SHOWN_BRANCHES)
            .filter(|more| *more > 0)
        {
            content = content.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{more} more; narrow the search to see them")),
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
                    Input::new(&self.branch_name)
                        .context_menu(crate::gpui::text_menu(&self.branch_name)),
                ))
                .child(crate::gpui::focus_input(
                    &self.branch_start,
                    Input::new(&self.branch_start)
                        .context_menu(crate::gpui::text_menu(&self.branch_start)),
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

    /// A branch with its distance from its upstream, and switching to it: directly from a clean
    /// tree, or by stashing the changes first.
    fn branch_row(
        &self,
        branch: &bootty_git::changes::GitBranch,
        dirty: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let name = branch.name.clone();
        let distance = branch_distance(branch);
        let (label, command, tooltip) = if dirty {
            (
                "Stash and switch",
                "git.branch-stash-switch",
                "Stash your changes, untracked files included, then switch. Apply the stash when you want them back.",
            )
        } else {
            ("Check out", "git.branch-checkout", "Switch to this branch")
        };
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
            .when(!distance.is_empty(), |row| {
                row.child(
                    div()
                        .debug_selector({
                            let name = name.clone();
                            let distance = distance.clone();
                            move || format!("branch-distance:{name}:{distance}")
                        })
                        .flex_none()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(distance.clone()),
                )
            })
            .child(
                Button::new(format!("checkout-branch:{}", branch.name))
                    .label(label)
                    .small()
                    .element_tooltip(tooltip)
                    .disabled(self.mutating || branch.current)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.request(command, vec![name.clone()], window, cx);
                    })),
            )
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
                    Input::new(&self.stash_message)
                        .context_menu(crate::gpui::text_menu(&self.stash_message)),
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

    /// Fetch, and pull or push the commits the branch and its upstream don't share.
    fn render_sync(&self, cx: &Context<Self>) -> Vec<Button> {
        let Some(upstream) = self
            .changes
            .as_ref()
            .and_then(|changes| changes.upstream.as_ref())
        else {
            return Vec::new();
        };
        let commits = |count: u32| if count == 1 { "commit" } else { "commits" };
        let sync = |id: &'static str, command: &'static str| {
            Button::new(id)
                .debug_selector(move || id.into())
                .ghost()
                .xsmall()
                .disabled(self.mutating)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.request(command, Vec::new(), window, cx);
                }))
        };
        let mut buttons = vec![
            sync("git-fetch", "git.fetch")
                .icon(Icon::default().path("icons/cloud-download.svg"))
                .accessibility_label("Fetch")
                .element_tooltip(format!("Fetch the remote of {}", upstream.name)),
        ];
        if upstream.behind > 0 {
            buttons.push(
                sync("git-pull", "git.pull")
                    .label(format!("↓{}", upstream.behind))
                    .element_tooltip(format!(
                        "Pull {} {} from {}",
                        upstream.behind,
                        commits(upstream.behind),
                        upstream.name
                    )),
            );
        }
        if upstream.ahead > 0 {
            buttons.push(
                sync("git-push", "git.push")
                    .label(format!("↑{}", upstream.ahead))
                    .element_tooltip(format!(
                        "Push {} {} to {}",
                        upstream.ahead,
                        commits(upstream.ahead),
                        upstream.name
                    )),
            );
        }
        buttons
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
                .children(self.render_pull_request(cx))
                .children(self.render_sync(cx))
                .child(
                    Button::new("all-changes")
                        .debug_selector(|| "git-all-changes".into())
                        .label("All changes")
                        .ghost()
                        .small()
                        .disabled(
                            self.changes
                                .as_ref()
                                .is_none_or(|changes| changes.files.is_empty()),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.request("git.diff-all", Vec::new(), window, cx);
                        })),
                )
                .child(
                    Button::new("refresh")
                        .icon(IconName::RotateCw)
                        .ghost()
                        .small()
                        .accessibility_label("Refresh changes")
                        .element_tooltip("Refresh changes")
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                )
                .into_any_element(),
            div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!(
                    "{} · {}",
                    self.context.host,
                    root.trim_end_matches(['/', '\\'])
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or("Repository")
                ))
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
impl GitChangesPanel {
    fn render_modes(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div().child(
            div()
                .flex()
                .gap_1()
                .child(
                    Button::new("show-local-changes")
                        .label("Changes")
                        .disabled(self.mutating)
                        .selected(!self.showing_pull_requests)
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.showing_pull_requests = false;
                            this.refresh(window, cx);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("show-pull-requests")
                        .label("Pull requests")
                        .disabled(self.mutating)
                        .selected(self.showing_pull_requests)
                        .icon(gpui_kit::assets::IconName::GitPullRequest)
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_pull_requests(None, window, cx);
                        })),
                ),
        )
    }

    /// Switches to pull requests: to `number`'s, or to the list when opening them first.
    fn show_pull_requests(
        &mut self,
        number: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = &self.pull_requests {
            if let Some(number) = number {
                view.update(cx, |view, cx| view.open(number, window, cx));
            }
        } else {
            let view = cx.new(|cx| {
                pull_request::PullRequestView::new(
                    self.context.clone(),
                    self.sender.clone(),
                    self.diff.clone(),
                    window,
                    cx,
                )
            });
            self.pull_request_subscription = Some(cx.subscribe(&view, |_, _, _: &OpenDiff, cx| {
                cx.emit(OpenDiff);
            }));
            view.update(cx, |view, cx| match number {
                Some(number) => view.open(number, window, cx),
                None => view.find(window, cx),
            });
            self.pull_requests = Some(view);
        }
        self.showing_pull_requests = true;
        cx.notify();
    }

    /// Asks GitHub about the current branch's pull request when the branch is new here, or the
    /// last answer is a minute old.
    fn ask_pull_request(&mut self, window: &Window, cx: &Context<Self>) {
        let Some(branch) = self
            .changes
            .as_ref()
            .and_then(|changes| changes.branch.clone())
        else {
            self.pull_request = None;
            return;
        };
        if self.pull_request.as_ref().is_some_and(|(asked, at, _)| {
            *asked == branch && at.elapsed() < bootty_git::facts::FOCUSED_PULL_REQUEST_INTERVAL
        }) {
            return;
        }
        let Ok(receiver) = self.submit_command("git.pull-request", Vec::new()) else {
            return;
        };
        let known = self
            .pull_request
            .take()
            .filter(|(asked, _, _)| *asked == branch)
            .and_then(|(_, _, status)| status);
        self.pull_request = Some((branch.clone(), Instant::now(), known));
        cx.spawn_in(window, async move |weak, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update(cx, |this, cx| {
                // A failed ask keeps what GitHub said last.
                if let Ok(CommandOutcome::Success { value, .. }) = outcome
                    && let Ok(status) = serde_json::from_value(value)
                    && let Some((asked, _, known)) = &mut this.pull_request
                    && *asked == branch
                {
                    *known = status;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The branch's pull request, which opens among the pull requests.
    fn render_pull_request(&self, cx: &Context<Self>) -> Option<Button> {
        let status = self.pull_request.as_ref()?.2.clone()?;
        let number = status.number;
        let state = match status.state {
            bootty_git::github::PullRequestState::Open => "Open",
            bootty_git::github::PullRequestState::Draft => "Draft",
            bootty_git::github::PullRequestState::Merged => "Merged",
            bootty_git::github::PullRequestState::Closed => "Closed",
        };
        let checks = match status.checks {
            bootty_git::github::ChecksState::None => "",
            bootty_git::github::ChecksState::Pending => ", checks running",
            bootty_git::github::ChecksState::Passing => ", checks passing",
            bootty_git::github::ChecksState::Failing => ", checks failing",
        };
        Some(
            Button::new("branch-pull-request")
                .debug_selector(move || format!("git-pull-request:{number}"))
                .icon(gpui_kit::assets::IconName::GitPullRequest)
                .label(format!("#{number}"))
                .ghost()
                .small()
                .element_tooltip(format!("Pull request #{number}: {state}{checks}"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.show_pull_requests(Some(number), window, cx);
                })),
        )
    }
}

impl GitChangesPanel {
    /// Commit and Amend, which need a message and Commit staged changes too, and writing the
    /// message with Codex from the staged changes.
    fn render_commit_controls(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let empty = self.message.read(cx).value().trim().is_empty();
        let staged = self.has_staged();
        div()
            .flex()
            .gap_2()
            .child(
                Button::new("commit")
                    .label(crate::i18n::t(cx, "git-commit"))
                    .small()
                    .disabled(self.mutating || empty || !staged)
                    .when(!staged, |button| {
                        button.element_tooltip("Stage changes to commit first")
                    })
                    .when(staged && empty, |button| {
                        button.element_tooltip("Write a message first")
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.commit(false, window, cx))),
            )
            .child(
                Button::new("amend")
                    .label(crate::i18n::t(cx, "git-amend"))
                    .small()
                    .disabled(self.mutating || empty)
                    .when(empty, |button| {
                        button.element_tooltip("Write a message first")
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.commit(true, window, cx))),
            )
            .child(
                Button::new("write-message")
                    .icon(gpui_kit::assets::IconName::Sparkles)
                    .small()
                    .ghost()
                    .loading(self.writing)
                    .disabled(self.mutating || !staged)
                    .accessibility_label("Write the message with Codex")
                    .element_tooltip(if staged {
                        "Write the message with Codex"
                    } else {
                        "Stage changes for Codex to describe first"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.write_message(window, cx))),
            )
    }
}

impl Render for GitChangesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let detail = self.detail_title.is_some() && !self.showing_pull_requests;
        // A wide panel keeps the list beside the diff; a narrow one shows one at a time.
        let beside = self.width >= px(720.);
        let root = div()
            .id("git-changes")
            .flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .on_prepaint(move |bounds, _, cx| {
                _ = owner.update(cx, |this, cx| {
                    if this.width != bounds.size.width {
                        this.width = bounds.size.width;
                        cx.notify();
                    }
                });
            });
        if detail && !beside {
            return root.child(self.render_detail(cx)).into_any_element();
        }
        let list = self.render_list(cx);
        if !detail {
            return root.child(list.flex_1()).into_any_element();
        }
        root.child(
            list.w(px(320.))
                .flex_shrink_0()
                .border_r_1()
                .border_color(cx.theme().border),
        )
        .child(self.render_detail(cx))
        .into_any_element()
    }
}

impl GitChangesPanel {
    /// The repository's heading, its changed files or history, and the commit controls.
    fn render_list(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let mut body = div()
            .flex()
            .flex_col()
            .h_full()
            .min_w_0()
            .gap_2()
            .p_2()
            .overflow_hidden()
            .children(self.render_header(cx));
        body = body.child(self.render_modes(cx));
        body = body.when_some(self.error.as_ref(), |body, error| {
            body.child(gpui_kit::component::alert::Alert::error(
                "git-error",
                error.clone(),
            ))
        });
        if self.showing_pull_requests {
            return body.when_some(self.pull_requests.as_ref(), |body, view| {
                body.child(view.clone())
            });
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
            Input::new(&self.message)
                .context_menu(crate::gpui::text_menu(&self.message))
                .disabled(self.mutating),
        ))
        .child(self.render_commit_controls(cx))
    }
}

/// How far a branch is from its upstream, like "↑2 ↓1"; empty when even or without one.
pub fn branch_distance(branch: &bootty_git::changes::GitBranch) -> String {
    [("↑", branch.ahead), ("↓", branch.behind)]
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(arrow, count)| format!("{arrow}{count}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Branches the list shows at most; the search finds the rest.
// Raise it if long lists turn out to be browsed rather than searched.
const MAX_SHOWN_BRANCHES: usize = 50;

fn short_commit(commit: &str) -> String {
    commit.chars().take(10).collect()
}
